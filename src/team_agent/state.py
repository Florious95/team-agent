from __future__ import annotations

import json
import os
import copy
import uuid
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

from team_agent.paths import runtime_dir
from team_agent.simple_yaml import dumps


SESSION_CAPTURE_FIELDS = [
    "session_id",
    "rollout_path",
    "captured_at",
    "captured_via",
    "attribution_confidence",
]
SESSION_STATE_FIELDS = [
    *SESSION_CAPTURE_FIELDS,
    "spawn_cwd",
]


def runtime_state_path(workspace: Path) -> Path:
    return runtime_dir(workspace) / "state.json"


def normalize_agent_session_state(state: dict[str, Any]) -> None:
    agents = state.get("agents", {})
    if not isinstance(agents, dict):
        return
    for agent_state in agents.values():
        if isinstance(agent_state, dict):
            for field in SESSION_STATE_FIELDS:
                agent_state.setdefault(field, None)


def load_runtime_state(workspace: Path) -> dict[str, Any]:
    path = runtime_state_path(workspace)
    if not path.exists():
        return {"agents": {}, "tasks": [], "session_name": None}
    state = json.loads(path.read_text(encoding="utf-8"))
    normalize_agent_session_state(state)
    return state


def team_state_key(state: dict[str, Any]) -> str:
    for field in ("team_dir", "spec_path"):
        value = state.get(field)
        if not value:
            continue
        path = Path(str(value))
        key = path.name if field == "team_dir" else path.parent.name
        if key and key not in {".team", "runtime"}:
            return key
    return str(state.get("session_name") or "current")


def compact_team_state(state: dict[str, Any]) -> dict[str, Any]:
    compact = copy.deepcopy(state)
    compact.pop("teams", None)
    return compact


def merge_workspace_team_state(existing: dict[str, Any], launched: dict[str, Any]) -> dict[str, Any]:
    launched_key = team_state_key(launched)
    if not existing.get("session_name"):
        merged = copy.deepcopy(launched)
        merged.setdefault("teams", {})[launched_key] = compact_team_state(launched)
        return merged
    existing_key = team_state_key(existing)
    if existing_key == launched_key:
        merged = copy.deepcopy(launched)
        teams = copy.deepcopy(existing.get("teams") or {})
        teams[launched_key] = compact_team_state(launched)
        merged["teams"] = teams
        return merged
    merged = copy.deepcopy(existing)
    teams = merged.setdefault("teams", {})
    teams.setdefault(existing_key, compact_team_state(existing))
    teams[launched_key] = compact_team_state(launched)
    return merged


def team_state_candidates(state: dict[str, Any]) -> dict[str, dict[str, Any]]:
    candidates: dict[str, dict[str, Any]] = {}
    teams = state.get("teams")
    if isinstance(teams, dict):
        for key, value in teams.items():
            if isinstance(value, dict):
                candidates[str(key)] = value
    if state.get("session_name"):
        candidates.setdefault(team_state_key(state), compact_team_state(state))
    return candidates


def format_team_candidates(candidates: dict[str, dict[str, Any]]) -> str:
    if not candidates:
        return "No team state was found."
    parts = []
    for key, state in sorted(candidates.items()):
        agents = ",".join(sorted(state.get("agents", {}).keys())) or "-"
        parts.append(f"{key} session={state.get('session_name') or '-'} agents={agents}")
    return "Candidates: " + "; ".join(parts)


def select_runtime_state(workspace: Path, team: str | None = None) -> dict[str, Any]:
    state = load_runtime_state(workspace)
    candidates = team_state_candidates(state)
    if team:
        matches = [
            value
            for key, value in candidates.items()
            if team in {key, str(value.get("session_name") or ""), str(value.get("team_dir") or "")}
        ]
        if len(matches) == 1:
            return copy.deepcopy(matches[0])
        from team_agent.errors import RuntimeError
        if len(matches) > 1:
            raise RuntimeError("team selector is ambiguous. " + format_team_candidates(candidates))
        raise RuntimeError(f"team {team!r} not found. " + format_team_candidates(candidates))
    if len(candidates) > 1:
        from team_agent.errors import RuntimeError
        raise RuntimeError("multiple teams found in this workspace; pass --team <team> to choose. " + format_team_candidates(candidates))
    return copy.deepcopy(state)


def ambiguous_team_target_result(state: dict[str, Any]) -> dict[str, Any] | None:
    candidates = team_state_candidates(state)
    if len(candidates) <= 1:
        return None
    return {
        "ok": False,
        "status": "refused",
        "reason": "team_target_ambiguous",
        "candidates": sorted(candidates),
        "message": "multiple teams found in this workspace; pass --team <team> to choose. " + format_team_candidates(candidates),
    }


def resolve_team_scoped_state(
    workspace: Path,
    team: str | None,
) -> tuple[dict[str, Any] | None, dict[str, Any] | None]:
    if team is None:
        ambiguous = ambiguous_team_target_result(load_runtime_state(workspace))
        if ambiguous:
            return None, ambiguous
    try:
        from team_agent.errors import RuntimeError as _TeamAgentRuntimeError
        return select_runtime_state(workspace, team), None
    except _TeamAgentRuntimeError as exc:
        return None, {
            "ok": False,
            "status": "refused",
            "reason": "team_target_unresolved",
            "team": team,
            "error": str(exc),
        }


def _caller_identity_from_env() -> dict[str, str]:
    return {
        "pane_id": os.environ.get("TEAM_AGENT_LEADER_PANE_ID") or "",
        "provider": os.environ.get("TEAM_AGENT_LEADER_PROVIDER") or "",
        "machine_fingerprint": os.environ.get("TEAM_AGENT_MACHINE_FINGERPRINT") or "",
    }


def check_team_owner(state: dict[str, Any]) -> dict[str, Any] | None:
    owner = state.get("team_owner") or {}
    if not owner:
        return None
    caller = _caller_identity_from_env()
    if (
        caller["pane_id"] == (owner.get("pane_id") or "")
        and caller["provider"] == (owner.get("provider") or "")
        and caller["machine_fingerprint"] == (owner.get("machine_fingerprint") or "")
    ):
        return None
    return {
        "ok": False,
        "status": "refused",
        "reason": "team_owner_mismatch",
        "error": "not_owner",
        "action": "use team-agent takeover --confirm",
        "team_owner": owner,
        "caller": caller,
    }


def worker_sender_bypasses_owner_gate(state: dict[str, Any], sender: str | None) -> str | None:
    if not sender:
        return None
    leader_id = (state.get("leader") or {}).get("id") or "leader"
    if sender == leader_id or sender in {"leader", "Leader"}:
        return None
    if sender not in (state.get("agents") or {}):
        return None
    env_agent_id = os.environ.get("TEAM_AGENT_ID") or ""
    if env_agent_id and env_agent_id != sender:
        return None
    return env_agent_id or sender


def populate_team_owner_from_env(state: dict[str, Any], source: str = "autopopulate") -> dict[str, Any] | None:
    if state.get("team_owner"):
        return state["team_owner"]
    caller = _caller_identity_from_env()
    if not caller["pane_id"]:
        return None
    owner = {
        "pane_id": caller["pane_id"],
        "provider": caller["provider"],
        "machine_fingerprint": caller["machine_fingerprint"],
        "claimed_at": datetime.now(timezone.utc).isoformat(),
        "claimed_via": source,
    }
    state["team_owner"] = owner
    return owner


def save_runtime_state(workspace: Path, state: dict[str, Any]) -> None:
    path = runtime_state_path(workspace)
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp_path = path.with_name(f"{path.name}.{os.getpid()}.{uuid.uuid4().hex}.tmp")
    try:
        tmp_path.write_text(json.dumps(state, indent=2, ensure_ascii=False), encoding="utf-8")
        os.replace(tmp_path, path)
    finally:
        tmp_path.unlink(missing_ok=True)


def save_team_scoped_state(workspace: Path, team_state: dict[str, Any]) -> None:
    target_key = team_state_key(team_state)
    existing = load_runtime_state(workspace)
    existing_primary_key = team_state_key(existing) if existing.get("session_name") else None
    if (
        existing_primary_key is not None
        and existing_primary_key != target_key
        and existing.get("session_name")
        and existing.get("session_name") == team_state.get("session_name")
    ):
        existing_primary_key = target_key
    existing_teams = existing.get("teams") or {}
    if not existing_teams and existing_primary_key == target_key:
        merged = copy.deepcopy(team_state)
        merged.pop("teams", None)
        save_runtime_state(workspace, merged)
        return
    teams = copy.deepcopy(existing_teams)
    teams[target_key] = compact_team_state(team_state)
    if existing_primary_key is None or existing_primary_key == target_key:
        merged = copy.deepcopy(team_state)
        merged["teams"] = teams
    else:
        merged = copy.deepcopy(existing)
        merged["teams"] = teams
    if not merged.get("teams"):
        merged.pop("teams", None)
    save_runtime_state(workspace, merged)


def write_team_state(workspace: Path, spec: dict[str, Any], runtime: dict[str, Any], results: list[dict[str, Any]] | None = None) -> Path:
    path = workspace / spec.get("context", {}).get("state_file", "team_state.md")
    path.parent.mkdir(parents=True, exist_ok=True)
    lines = [
        "# Team State",
        "",
        f"Updated: {datetime.now(timezone.utc).isoformat()}",
        "",
        "## Objective",
        "",
        spec.get("team", {}).get("objective", ""),
        "",
        "## Team",
        "",
        f"- Name: {spec.get('team', {}).get('name')}",
        f"- Runtime session: {runtime.get('session_name')}",
    ]
    receiver = runtime.get("leader_receiver") or {}
    if receiver:
        if receiver.get("mode") == "direct_tmux":
            lines.append(
                f"- Leader receiver: direct tmux {receiver.get('pane_id')} "
                f"({receiver.get('provider')}, {receiver.get('status')})"
            )
        else:
            lines.append(f"- Leader inbox fallback: {receiver.get('session')}:{receiver.get('window')} ({receiver.get('status')})")
            lines.append(f"- Leader inbox log: {receiver.get('path')}")
    lines.extend(["", "## Agents", ""])
    for agent in spec.get("agents", []):
        status = runtime.get("agents", {}).get(agent["id"], {}).get("status", "unknown")
        lines.append(f"- {agent['id']}: {agent['role']} on {agent['provider']} ({status})")
    lines.extend(["", "## Task Graph", ""])
    for task in runtime.get("tasks", spec.get("tasks", [])):
        deps = ", ".join(task.get("deps", [])) or "none"
        assignee = task.get("assignee") or "unassigned"
        lines.append(f"- {task['id']} [{task.get('status', 'pending')}], assignee={assignee}, deps={deps}: {task['title']}")
        if task.get("last_result_summary"):
            lines.append(f"  Summary: {task['last_result_summary']}")
        if task.get("artifact_refs"):
            for ref in task["artifact_refs"]:
                if isinstance(ref, dict):
                    lines.append(f"  Artifact: {ref.get('path')} - {ref.get('description', '')}")
                else:
                    lines.append(f"  Artifact: INVALID artifact ref {ref!r}")
    lines.extend(["", "## Latest Results", ""])
    for result in results or []:
        envelope = json.loads(result["envelope"]) if isinstance(result.get("envelope"), str) else result
        lines.append(f"- {envelope.get('task_id')} from {envelope.get('agent_id')}: {envelope.get('status')} - {envelope.get('summary')}")
    lines.extend(["", "## Blockers", ""])
    blockers = [
        task
        for task in runtime.get("tasks", spec.get("tasks", []))
        if task.get("status") in {"blocked", "failed", "needs_retry"}
    ]
    if blockers:
        for task in blockers:
            lines.append(f"- {task['id']}: {task.get('last_result_summary', task.get('title'))}")
    else:
        lines.append("- None")
    lines.extend(["", "## Next Step", "", "- Continue routing ready tasks and collect result envelopes."])
    path.write_text("\n".join(lines) + "\n", encoding="utf-8")
    return path


def write_spec(path: Path, spec: dict[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp_path = path.with_suffix(path.suffix + ".tmp")
    tmp_path.write_text(dumps(spec), encoding="utf-8")
    os.replace(tmp_path, path)
