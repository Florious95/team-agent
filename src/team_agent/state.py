from __future__ import annotations

import hashlib
import errno
import json
import os
import copy
import subprocess
import time
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
_RUNTIME_STATE_CACHE: dict[str, dict[str, Any]] = {}


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
        cached = _RUNTIME_STATE_CACHE.get(str(path))
        if cached is not None:
            return copy.deepcopy(cached)
        return {"agents": {}, "tasks": [], "session_name": None, "active_team_key": None}
    state = json.loads(path.read_text(encoding="utf-8"))
    normalize_agent_session_state(state)
    changed = _migrate_state_identity(state, workspace)
    if _migrate_active_team_key(state):
        changed = True
    if changed:
        save_runtime_state(workspace, state)
    _RUNTIME_STATE_CACHE[str(path)] = copy.deepcopy(state)
    return state


def _migrate_active_team_key(state: dict[str, Any]) -> bool:
    """0.2.6 Family B (C6): legacy states with a top-level ``session_name``
    but no ``active_team_key`` get the active pointer seeded once. After
    this, ``active_team_key`` is the single explicit source of truth and
    callers mutate it through CLI verbs (claim-leader / takeover /
    shutdown / restart)."""
    if "active_team_key" in state:
        return False
    teams = state.get("teams") if isinstance(state.get("teams"), dict) else {}
    if state.get("session_name"):
        seed = team_state_key(state)
        state["active_team_key"] = seed if seed in teams or not teams else seed
        return True
    if isinstance(teams, dict) and len(teams) == 1:
        state["active_team_key"] = next(iter(teams))
        return True
    state["active_team_key"] = None
    return True


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
    """0.2.6 Family B (C7): the only candidate source is ``state.teams``
    filtered by ``status == "alive"``. Top-level ``session_name`` /
    ``team_dir`` are a derived view of the active team and never count as
    an independent candidate. Shutdown/legacy entries with non-alive
    status are excluded."""
    out: dict[str, dict[str, Any]] = {}
    teams = state.get("teams") if isinstance(state.get("teams"), dict) else {}
    for key, value in teams.items():
        if not isinstance(value, dict):
            continue
        if str(value.get("status") or "alive").lower() != "alive":
            continue
        out[str(key)] = value
    return out


def format_team_candidates(team_states: dict[str, dict[str, Any]]) -> str:
    if not team_states:
        return "No team state was found."
    parts = []
    for key in sorted(team_states):
        st = team_states[key]
        agents = ",".join(sorted(st.get("agents", {}).keys())) or "-"
        parts.append(f"{key} session={st.get('session_name') or '-'} agents={agents}")
    return "Candidates: " + "; ".join(parts)


def _team_entry_from_state(state: dict[str, Any], team_key: str) -> dict[str, Any] | None:
    teams = state.get("teams") if isinstance(state.get("teams"), dict) else {}
    entry = teams.get(team_key)
    if not isinstance(entry, dict):
        return None
    return entry


def _project_top_level_view(state: dict[str, Any], team_key: str) -> dict[str, Any]:
    """0.2.6 Family B (C8): when picking a team for use, the top-level
    keys (``session_name`` / ``team_dir`` / ``agents`` / ``tasks``) are a
    derived view of ``teams[team_key]``. We copy the team entry into a
    flat dict and preserve any auxiliary state (``team_owner`` /
    ``leader_receiver`` / ``coordinator`` already pinned to the team)."""
    entry = _team_entry_from_state(state, team_key) or {}
    projection = copy.deepcopy(entry)
    projection.setdefault("session_name", entry.get("session_name"))
    projection.setdefault("team_dir", entry.get("team_dir"))
    projection["active_team_key"] = team_key
    # Preserve the full teams dict so consumers can introspect siblings.
    projection["teams"] = copy.deepcopy(state.get("teams") or {})
    if "team_owner" in entry:
        projection["team_owner"] = copy.deepcopy(entry["team_owner"])
    elif state.get("team_owner") is not None:
        projection["team_owner"] = copy.deepcopy(state["team_owner"])
    if "leader_receiver" in entry:
        projection["leader_receiver"] = copy.deepcopy(entry["leader_receiver"])
    elif state.get("leader_receiver") is not None:
        projection["leader_receiver"] = copy.deepcopy(state["leader_receiver"])
    if "coordinator" in state:
        projection.setdefault("coordinator", copy.deepcopy(state["coordinator"]))
    return projection


def select_runtime_state(workspace: Path, team: str | None = None) -> dict[str, Any]:
    state = load_runtime_state(workspace)
    alive = team_state_candidates(state)
    if team:
        if not alive and team in {str(state.get("active_team_key") or ""), team_state_key(state)}:
            projection = copy.deepcopy(state)
            projection["active_team_key"] = str(team)
            return projection
        matches = [
            (key, value)
            for key, value in alive.items()
            if team in {key, str(value.get("session_name") or ""), str(value.get("team_dir") or "")}
        ]
        if len(matches) == 1:
            return _project_top_level_view(state, matches[0][0])
        from team_agent.errors import RuntimeError
        if len(matches) > 1:
            raise RuntimeError("team selector is ambiguous. " + format_team_candidates(alive))
        raise RuntimeError(f"team {team!r} not found. " + format_team_candidates(alive))
    active = state.get("active_team_key")
    if active and active in alive:
        return _project_top_level_view(state, str(active))
    if len(alive) == 1:
        return _project_top_level_view(state, next(iter(alive)))
    if not alive:
        return copy.deepcopy(state)
    from team_agent.errors import RuntimeError
    raise RuntimeError(
        "multiple teams found in this workspace; pass --team <team> to choose. "
        + format_team_candidates(alive)
    )


def ambiguous_team_target_result(state: dict[str, Any]) -> dict[str, Any] | None:
    alive = team_state_candidates(state)
    active = state.get("active_team_key")
    if active and active in alive:
        return None
    if len(alive) <= 1:
        return None
    return {
        "ok": False,
        "status": "refused",
        "reason": "team_target_ambiguous",
        "candidates": sorted(alive.keys()),
        "message": "multiple teams found in this workspace; pass --team <team> to choose. "
        + format_team_candidates(alive),
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
        "pane_id": os.environ.get("TEAM_AGENT_LEADER_PANE_ID") or os.environ.get("TMUX_PANE") or "",
        "provider": os.environ.get("TEAM_AGENT_LEADER_PROVIDER") or "",
        "machine_fingerprint": machine_fingerprint,
        "leader_session_uuid": leader_uuid,
        "leader_session_uuid_source": "explicit-override" if override else ("env" if env_uuid else "derived"),
    }


_TMUX_PANE_LIVE = "live"
_TMUX_PANE_DEAD = "dead"
_TMUX_PANE_UNKNOWN = "unknown"


def _tmux_pane_liveness(pane_id: str) -> str:
    if not pane_id:
        return _TMUX_PANE_UNKNOWN
    try:
        from team_agent.runtime import run_cmd
        proc = run_cmd(["tmux", "display-message", "-p", "-t", pane_id, "#{pane_id}"], timeout=3)
    except Exception:
        try:
            proc = subprocess.run(
                ["tmux", "display-message", "-p", "-t", pane_id, "#{pane_id}"],
                text=True,
                capture_output=True,
                timeout=3,
                check=False,
            )
        except Exception:
            return _TMUX_PANE_UNKNOWN
    if proc.returncode == 0:
        return _TMUX_PANE_LIVE
    stderr = str(getattr(proc, "stderr", "") or "").lower()
    if "can't find pane" in stderr or "can't find window" in stderr or "can't find session" in stderr:
        return _TMUX_PANE_DEAD
    return _TMUX_PANE_UNKNOWN


def check_team_owner(state: dict[str, Any]) -> dict[str, Any] | None:
    owner = state.get("team_owner") or {}
    if not owner:
        return None
    _migrate_team_identity(state, Path(_identity_workspace_abspath(state)), team_state_key(state))
    caller = _caller_identity_from_env(state, team_state_key(state))
    owner_uuid = str(owner.get("leader_session_uuid") or "")
    caller_uuid = caller["leader_session_uuid"]
    owner_pane = str(owner.get("pane_id") or "")
    caller_pane = caller.get("pane_id") or ""
    if caller_pane and caller_pane == owner_pane:
        return None
    if (
        caller_pane
        and not os.environ.get("TEAM_AGENT_ID")
        and owner_pane
        and _tmux_pane_liveness(owner_pane) != _TMUX_PANE_LIVE
    ):
        return None
    if caller_uuid == owner_uuid and (not caller_pane or caller_pane == owner_pane):
        return None
    same_uuid = caller_uuid == owner_uuid
    return {
        "ok": False,
        "status": "refused",
        "reason": "team_owner_mismatch",
        "reason_kind": "sticky_bind_collision" if same_uuid else "owner_takeover_required",
        "error": "not_owner",
        "action": "team-agent claim-leader --confirm" if same_uuid else "team-agent takeover --confirm",
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
    # Lease mutation convergence marker: _write_lease_dual_state.
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
    # Lease mutation convergence marker: _write_lease_dual_state.
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
    if receiver.get("provider") == "fake":
        return {"ok": True}
    if not targets.get("ok"):
        return {"ok": False, "reason": "leader_uuid_lookup_failed", "error": targets.get("error") or "tmux target scan failed"}
    pane_id = receiver.get("pane_id")
    target = next((item for item in targets.get("targets", []) if item.get("pane_id") == pane_id), None)
    if not target:
        return {"ok": False, "reason": "leader_pane_missing", "error": "tmux pane does not exist"}
    return {"ok": True, "pane": target}


def save_runtime_state(workspace: Path, state: dict[str, Any]) -> None:
    path = runtime_state_path(workspace)
    cached = _RUNTIME_STATE_CACHE.get(str(path))
    if cached is not None and state == cached:
        return
    _migrate_state_identity(state, workspace)
    cached = _RUNTIME_STATE_CACHE.get(str(path))
    if cached is not None and state == cached:
        return
    if path.exists():
        try:
            existing = json.loads(path.read_text(encoding="utf-8"))
            normalize_agent_session_state(existing)
            _migrate_state_identity(existing, workspace)
            if state == existing:
                _RUNTIME_STATE_CACHE[str(path)] = copy.deepcopy(state)
                return
        except Exception:
            pass
    from team_agent.runtime import _runtime_lock
    with _runtime_lock(workspace, "state-save", timeout=2.0):
        path.parent.mkdir(parents=True, exist_ok=True)
        payload = json.dumps(state, indent=2, ensure_ascii=False)
        delays = [0.05, 0.2, 0.5]
        for attempt in range(len(delays) + 1):
            tmp_path = path.with_name(f"{path.name}.{os.getpid()}.{uuid.uuid4().hex}.tmp")
            try:
                tmp_path.write_text(payload, encoding="utf-8")
                os.replace(tmp_path, path)
                _RUNTIME_STATE_CACHE[str(path)] = copy.deepcopy(state)
                return
            except (PermissionError, OSError) as exc:
                if not _retryable_replace_error(exc) or attempt >= len(delays):
                    if _retryable_replace_error(exc):
                        _self_heal_runtime_state(workspace, path, payload, state, attempt + 1, exc)
                        return
                    raise
                from team_agent.events import EventLog
                EventLog(workspace).write(
                    "runtime.state.save_retry",
                    attempt=attempt + 1,
                    errno=getattr(exc, "errno", None),
                    errno_name=errno.errorcode.get(getattr(exc, "errno", 0), None),
                    error=str(exc),
                )
                time.sleep(delays[attempt])
            finally:
                tmp_path.unlink(missing_ok=True)


def _retryable_replace_error(exc: BaseException) -> bool:
    return isinstance(exc, PermissionError) or (
        isinstance(exc, OSError) and getattr(exc, "errno", None) in {errno.EACCES, errno.EPERM, errno.EBUSY}
    )


def _self_heal_runtime_state(
    workspace: Path,
    path: Path,
    payload: str,
    state: dict[str, Any],
    attempts_used: int,
    original_exc: BaseException,
) -> None:
    from team_agent.events import EventLog
    event_log = EventLog(workspace)
    heal_tmp = path.with_name(f"{path.name}.{os.getpid()}.{uuid.uuid4().hex}.heal.tmp")
    backup = path.with_name(f"{path.name}.bak.{os.getpid()}")
    backup_created = False
    try:
        heal_tmp.write_text(payload, encoding="utf-8")
        try:
            os.replace(path, backup)
            backup_created = True
        except FileNotFoundError:
            backup_created = False
        os.replace(heal_tmp, path)
        _RUNTIME_STATE_CACHE[str(path)] = copy.deepcopy(state)
        event_log.write(
            "runtime.state.self_healed",
            inode_rebuilt=True,
            attempts_used=attempts_used,
            replace_retries=max(0, attempts_used - 1),
        )
    except Exception as exc:
        if backup_created:
            try:
                os.replace(backup, path)
            except Exception as restore_exc:
                event_log.write("runtime.state.self_heal_restore_failed", error=str(restore_exc))
        event_log.write(
            "runtime.state.save_failed",
            phase="save_runtime_state",
            final_errno=getattr(exc, "errno", getattr(original_exc, "errno", None)),
            error=str(exc),
            retries_used=max(0, attempts_used - 1),
        )
        raise
    finally:
        heal_tmp.unlink(missing_ok=True)


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
    incoming_teams = team_state.get("teams") if isinstance(team_state.get("teams"), dict) else None
    if not existing_teams and existing_primary_key == target_key:
        merged = copy.deepcopy(team_state)
        merged.pop("teams", None)
        save_runtime_state(workspace, merged)
        return
    teams = copy.deepcopy(incoming_teams or existing_teams)
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
