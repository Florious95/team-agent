from __future__ import annotations

import copy
from pathlib import Path
from typing import Any

from team_agent.paths import runtime_dir
from team_agent.state import load_runtime_state, runtime_state_path
from team_agent.restart.snapshot import load_snapshot_state, state_team_name


def restart_candidates(workspace: Path) -> list[dict[str, Any]]:
    by_session: dict[str, dict[str, Any]] = {}
    snapshots_root = runtime_dir(workspace) / "teams"
    for path in sorted(snapshots_root.glob("*/state.json")) if snapshots_root.exists() else []:
        state = load_snapshot_state(path)
        if not state or not state.get("session_name"):
            continue
        session_name = str(state["session_name"])
        by_session[session_name] = restart_candidate_from_state(state, path)
    active = load_runtime_state(workspace)
    if active.get("session_name"):
        by_session[str(active["session_name"])] = restart_candidate_from_state(active, runtime_state_path(workspace))
    return sorted(by_session.values(), key=lambda item: item.get("session_name") or "")


def restart_candidate_from_state(state: dict[str, Any], state_path: Path) -> dict[str, Any]:
    session_name = str(state.get("session_name") or "")
    return {
        "session_name": session_name,
        "team_name": state_team_name(state),
        "state_path": str(state_path),
        "spec_path": state.get("spec_path"),
        "agents": sorted(state.get("agents", {}).keys()),
        "has_context": state_has_restart_context(state),
        "state": state,
    }


def state_has_restart_context(state: dict[str, Any]) -> bool:
    for agent_state in state.get("agents", {}).values():
        if not isinstance(agent_state, dict):
            continue
        if agent_state.get("session_id") or agent_state.get("rollout_path") or agent_state.get("captured_at"):
            return True
    return bool(state.get("agents"))


def select_restart_state(workspace: Path, team: str | None = None) -> dict[str, Any]:
    from team_agent.runtime import RuntimeError
    candidates = [item for item in restart_candidates(workspace) if item.get("has_context")]
    if team:
        matches = [
            item
            for item in candidates
            if team in {item.get("session_name"), item.get("team_name"), Path(str(item.get("state_path"))).parent.name}
        ]
        if len(matches) == 1:
            return copy.deepcopy(matches[0]["state"])
        if len(matches) > 1:
            raise RuntimeError("restart team selector is ambiguous. " + format_restart_candidates(matches))
        raise RuntimeError(f"restart team {team!r} not found. " + format_restart_candidates(candidates))
    if len(candidates) == 1:
        return copy.deepcopy(candidates[0]["state"])
    if len(candidates) > 1:
        raise RuntimeError(
            "multiple restartable teams found in this workspace; pass --team <session_name> to choose. "
            + format_restart_candidates(candidates)
        )
    return load_runtime_state(workspace)


def format_restart_candidates(candidates: list[dict[str, Any]]) -> str:
    if not candidates:
        return "No restartable team state was found."
    parts = []
    for item in candidates:
        parts.append(
            f"{item.get('session_name')} team={item.get('team_name') or '-'} "
            f"agents={','.join(item.get('agents') or []) or '-'}"
        )
    return "Candidates: " + "; ".join(parts)


def quick_start_existing_context(workspace: Path, session_name: str) -> dict[str, Any] | None:
    for item in restart_candidates(workspace):
        if item.get("session_name") == session_name and item.get("has_context"):
            return item
    return None
