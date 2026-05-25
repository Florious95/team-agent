from __future__ import annotations

import hashlib
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
_UUID_SEPARATOR = "\0"


def derive_leader_session_uuid(machine_fingerprint: str, workspace_abspath: str, os_user: str, team_id: str) -> str:
    parts = [machine_fingerprint, workspace_abspath, os_user, team_id]
    if any(_UUID_SEPARATOR in part for part in parts):
        raise ValueError("leader_session_uuid inputs must not contain NUL")
    return hashlib.sha256(_UUID_SEPARATOR.join(parts).encode("utf-8")).hexdigest()[:32]


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
    if _migrate_state_identity(state, workspace):
        save_runtime_state(workspace, state)
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


def _identity_workspace_abspath(state: dict[str, Any], workspace: Path | None = None) -> str:
    if state.get("workspace"):
        return str(Path(str(state["workspace"])).resolve())
    if state.get("team_dir"):
        return str(Path(str(state["team_dir"])).resolve().parent.parent)
    if state.get("spec_path"):
        spec_path = Path(str(state["spec_path"])).resolve()
        return str(spec_path.parent.parent.parent if spec_path.parent.parent.name == ".team" else spec_path.parent)
    return str((workspace or Path(os.environ.get("TEAM_AGENT_WORKSPACE") or os.getcwd())).resolve())


def _identity_os_user() -> str:
    return os.environ.get("USER") or os.environ.get("USERNAME") or ""


def _identity_machine_fingerprint(state: dict[str, Any]) -> str:
    for record in (state.get("team_owner"), state.get("leader_receiver")):
        if isinstance(record, dict) and record.get("machine_fingerprint"):
            return str(record["machine_fingerprint"])
    return os.environ.get("TEAM_AGENT_MACHINE_FINGERPRINT") or ""


def _leader_session_uuid_for_state(state: dict[str, Any], workspace: Path | None = None, team_id: str | None = None) -> str:
    return derive_leader_session_uuid(
        _identity_machine_fingerprint(state),
        _identity_workspace_abspath(state, workspace),
        _identity_os_user(),
        team_id or team_state_key(state),
    )


def _migrate_team_identity(state: dict[str, Any], workspace: Path, team_id: str | None = None) -> bool:
    leader_uuid = _leader_session_uuid_for_state(state, workspace, team_id)
    changed = False
    for key in ("team_owner", "leader_receiver"):
        record = state.get(key)
        if isinstance(record, dict) and not record.get("leader_session_uuid"):
            record["leader_session_uuid"] = leader_uuid
            changed = True
    return changed


def _migrate_state_identity(state: dict[str, Any], workspace: Path) -> bool:
    changed = _migrate_team_identity(state, workspace) if state.get("session_name") else False
    teams = state.get("teams")
    if isinstance(teams, dict):
        for team_id, team_state in teams.items():
            if isinstance(team_state, dict):
                changed = _migrate_team_identity(team_state, workspace, str(team_id)) or changed
    return changed


def _caller_identity_from_env(state: dict[str, Any] | None = None, team_id: str | None = None, workspace: Path | None = None) -> dict[str, str]:
    state = state or {}
    machine_fingerprint = os.environ.get("TEAM_AGENT_MACHINE_FINGERPRINT") or ""
    override = os.environ.get("TEAM_AGENT_LEADER_SESSION_UUID_OVERRIDE") or ""
    env_uuid = os.environ.get("TEAM_AGENT_LEADER_SESSION_UUID") or ""
    leader_uuid = override or env_uuid or derive_leader_session_uuid(
        machine_fingerprint,
        _identity_workspace_abspath(state, workspace),
        _identity_os_user(),
        team_id or os.environ.get("TEAM_AGENT_TEAM_ID") or team_state_key(state),
    )
    return {
        "pane_id": os.environ.get("TEAM_AGENT_LEADER_PANE_ID") or "",
        "provider": os.environ.get("TEAM_AGENT_LEADER_PROVIDER") or "",
        "machine_fingerprint": machine_fingerprint,
        "leader_session_uuid": leader_uuid,
        "leader_session_uuid_source": "explicit-override" if override else ("env" if env_uuid else "derived"),
    }


def check_team_owner(state: dict[str, Any]) -> dict[str, Any] | None:
    owner = state.get("team_owner") or {}
    if not owner:
        return None
    _migrate_team_identity(state, Path(_identity_workspace_abspath(state)), team_state_key(state))
    caller = _caller_identity_from_env(state, team_state_key(state))
    if caller["leader_session_uuid"] == (owner.get("leader_session_uuid") or ""):
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
        _migrate_team_identity(state, Path(_identity_workspace_abspath(state)), team_state_key(state))
        return state["team_owner"]
    caller = _caller_identity_from_env(state, team_state_key(state))
    if not caller["pane_id"]:
        return None
    owner = {
        "pane_id": caller["pane_id"],
        "provider": caller["provider"],
        "machine_fingerprint": caller["machine_fingerprint"],
        "leader_session_uuid": caller["leader_session_uuid"],
        "claimed_at": datetime.now(timezone.utc).isoformat(),
        "claimed_via": source,
    }
    state["team_owner"] = owner
    return owner


def apply_first_time_leader_binding(
    workspace: Path,
    state: dict[str, Any],
    receiver: dict[str, Any],
    pane_info: dict[str, Any],
    identity: dict[str, Any],
    source: str,
) -> dict[str, Any]:
    from team_agent.messaging.leader_panes import _leader_command_looks_usable
    command = pane_info.get("pane_current_command", "")
    provider = str(receiver.get("provider") or "")
    if not _leader_command_looks_usable(command, provider):
        return {"ok": False, "reason": "leader_pane_wrong_command", "error": f"pane command {command!r} is not a leader host", "pane": pane_info}
    current_path = pane_info.get("pane_current_path")
    if not current_path or os.path.realpath(current_path) != os.path.realpath(str(workspace.resolve())):
        return {"ok": False, "reason": "leader_pane_wrong_workspace", "error": f"pane cwd {current_path!r} does not match workspace {str(workspace.resolve())!r}", "pane": pane_info}
    receiver.update({
        "leader_session_uuid": identity["leader_session_uuid"],
        "machine_fingerprint": identity["machine_fingerprint"],
        "owner_epoch": 0,
    })
    state["team_owner"] = {
        "pane_id": receiver["pane_id"],
        "provider": provider,
        "machine_fingerprint": identity["machine_fingerprint"],
        "leader_session_uuid": identity["leader_session_uuid"],
        "owner_epoch": 0,
        "claimed_at": datetime.now(timezone.utc).isoformat(),
        "claimed_via": source,
    }
    state["leader_receiver"] = receiver
    return {"ok": True, "pane": pane_info, "warning": None, "first_time": True}


def leader_env_exports(receiver: dict[str, Any], identity: dict[str, Any]) -> dict[str, str]:
    return {
        "TEAM_AGENT_LEADER_PANE_ID": str(receiver.get("pane_id") or ""),
        "TEAM_AGENT_LEADER_PROVIDER": str(receiver.get("provider") or ""),
        "TEAM_AGENT_LEADER_SESSION_UUID": str(identity.get("leader_session_uuid") or ""),
        "TEAM_AGENT_MACHINE_FINGERPRINT": str(identity.get("machine_fingerprint") or ""),
        "TEAM_AGENT_WORKSPACE": str(identity.get("workspace_abspath") or ""),
        "TEAM_AGENT_TEAM_ID": str(identity.get("team_id") or ""),
    }


def validate_leader_uuid_from_targets(receiver: dict[str, Any], targets: dict[str, Any]) -> dict[str, Any]:
    expected_uuid = str(receiver.get("leader_session_uuid") or "")
    if not expected_uuid or receiver.get("provider") == "fake":
        return {"ok": True}
    if not targets.get("ok"):
        return {"ok": False, "reason": "leader_uuid_lookup_failed", "error": targets.get("error") or "tmux target scan failed"}
    pane_id = receiver.get("pane_id")
    target = next((item for item in targets.get("targets", []) if item.get("pane_id") == pane_id), None)
    env = target.get("leader_env") if isinstance((target or {}).get("leader_env"), dict) else {}
    actual_uuid = str((target or {}).get("leader_session_uuid") or env.get("TEAM_AGENT_LEADER_SESSION_UUID") or "")
    if not actual_uuid:
        return {"ok": False, "reason": "leader_uuid_missing", "error": "bound pane has no TEAM_AGENT_LEADER_SESSION_UUID", "pane": target}
    if actual_uuid != expected_uuid:
        return {"ok": False, "reason": "leader_uuid_mismatch", "error": "bound pane TEAM_AGENT_LEADER_SESSION_UUID does not match stored team owner", "pane": target}
    return {"ok": True}


def save_runtime_state(workspace: Path, state: dict[str, Any]) -> None:
    _migrate_state_identity(state, workspace)
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
