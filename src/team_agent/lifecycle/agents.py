from __future__ import annotations

import copy
from pathlib import Path
from typing import Any

from team_agent.errors import RuntimeError
from team_agent.events import EventLog
from team_agent.message_store import MessageStore
from team_agent.spec import load_spec, validate_spec
from team_agent.state import (
    check_team_owner,
    load_runtime_state,
    resolve_team_scoped_state,
    save_runtime_state,
    save_team_scoped_state,
    write_spec,
    write_team_state,
)


def remove_agent(
    workspace: Path,
    agent_id: str,
    *,
    from_spec: bool = False,
    confirm: bool = False,
    force: bool = False,
    team: str | None = None,
) -> dict[str, Any]:
    import team_agent.runtime as runtime

    workspace = workspace.resolve()
    state, refusal = resolve_team_scoped_state(workspace, team)
    if refusal:
        return refusal
    gate = check_team_owner(state)
    if gate:
        return gate
    spec_path = Path(state.get("spec_path", workspace / "team.spec.yaml"))
    spec = load_spec(spec_path)
    agent = _find_worker(spec, agent_id)
    if not agent:
        raise RuntimeError(f"unknown worker agent id: {agent_id}")

    event_log = EventLog(workspace)
    store = MessageStore(workspace)
    agent_state = state.get("agents", {}).get(agent_id) or {}
    dynamic_role_file = _dynamic_role_file(workspace, agent_id, agent_state)
    dynamic_agent = bool(agent_state.get("dynamic_role_file") or agent.get("forked_from"))
    running = _is_running(runtime, state, agent_id, agent_state)

    if not dynamic_agent and not (from_spec and confirm):
        return {"ok": False, "agent_id": agent_id, "status": "refused", "reason": "from_spec_confirm_required"}
    if running and not force:
        return {"ok": False, "agent_id": agent_id, "status": "refused", "reason": "force_required"}

    rollback = _RemoveRollback(workspace, spec_path, spec, state, dynamic_role_file, store, agent_id, False)
    stopped: dict[str, Any] | None = None
    try:
        if running and force:
            stopped = runtime.stop_agent(workspace, agent_id, team=team)
            rollback.restore_running = True
            state, _refusal_after = resolve_team_scoped_state(workspace, team)
        removed_state = copy.deepcopy(state)
        removed_state.get("agents", {}).pop(agent_id, None)
        save_team_scoped_state(workspace, removed_state)

        removed_spec = copy.deepcopy(spec)
        removed_spec["agents"] = [item for item in removed_spec.get("agents", []) if item.get("id") != agent_id]
        startup_order = removed_spec.get("runtime", {}).get("startup_order")
        if isinstance(startup_order, list):
            removed_spec["runtime"]["startup_order"] = [item for item in startup_order if item != agent_id]
        validate_spec(removed_spec, base_dir=spec_path.parent)
        team_state_path = write_team_state(workspace, removed_spec, removed_state)
        write_spec(spec_path, removed_spec)

        role_file_removed = _remove_dynamic_role_file(dynamic_role_file, bool(agent_state.get("dynamic_role_file")))
        _delete_agent_health(store, agent_id)
    except Exception as exc:
        rollback_result = rollback.restore(runtime, event_log)
        event_log.write("remove_agent.rollback", agent_id=agent_id, ok=rollback_result["ok"], error=str(exc), rollback=rollback_result)
        raise RuntimeError(f"remove-agent failed for {agent_id}: {exc}; rollback_ok={rollback_result['ok']}") from exc

    runtime._save_team_runtime_snapshot(workspace, removed_state)
    warning = None
    try:
        # Storage commit is authoritative; final success event logging is best-effort.
        event_log.write(
            "remove_agent.complete",
            agent_id=agent_id,
            from_spec=from_spec,
            force=force,
            stopped=stopped,
            role_file_removed=role_file_removed,
        )
    except Exception as exc:
        warning = f"remove-agent completed but success event logging failed: {exc}"
    return {
        "ok": True,
        "agent_id": agent_id,
        "status": "removed",
        "from_spec": from_spec,
        "force": force,
        "stopped": stopped,
        "state_file": str(team_state_path),
        "role_file_removed": role_file_removed,
        **({"warning": warning} if warning else {}),
    }


class _RemoveRollback:
    def __init__(
        self,
        workspace: Path,
        spec_path: Path,
        spec: dict[str, Any],
        state: dict[str, Any],
        dynamic_role_file: Path,
        store: MessageStore,
        agent_id: str,
        restore_running: bool,
    ) -> None:
        self.workspace = workspace
        self.spec_path = spec_path
        self.spec_text = spec_path.read_text(encoding="utf-8")
        self.spec = copy.deepcopy(spec)
        self.state = copy.deepcopy(state)
        self.team_state_path = workspace / spec.get("context", {}).get("state_file", "team_state.md")
        self.team_state_text = self.team_state_path.read_text(encoding="utf-8") if self.team_state_path.exists() else None
        self.dynamic_role_file = dynamic_role_file
        self.dynamic_role_bytes = dynamic_role_file.read_bytes() if dynamic_role_file.exists() else None
        self.health = copy.deepcopy(store.agent_health().get(agent_id))
        self.agent_id = agent_id
        self.restore_running = restore_running

    def restore(self, runtime: Any, event_log: EventLog) -> dict[str, Any]:
        errors: list[str] = []
        try:
            self.spec_path.write_text(self.spec_text, encoding="utf-8")
        except Exception as exc:
            errors.append(f"spec:{exc}")
        try:
            save_team_scoped_state(self.workspace, self.state)
        except Exception as exc:
            errors.append(f"workspace_state:{exc}")
        try:
            if self.team_state_text is None:
                self.team_state_path.unlink(missing_ok=True)
            else:
                self.team_state_path.parent.mkdir(parents=True, exist_ok=True)
                self.team_state_path.write_text(self.team_state_text, encoding="utf-8")
        except Exception as exc:
            errors.append(f"team_state:{exc}")
        try:
            if self.dynamic_role_bytes is None:
                self.dynamic_role_file.unlink(missing_ok=True)
            else:
                self.dynamic_role_file.parent.mkdir(parents=True, exist_ok=True)
                self.dynamic_role_file.write_bytes(self.dynamic_role_bytes)
        except Exception as exc:
            errors.append(f"role_file:{exc}")
        try:
            _restore_agent_health(MessageStore(self.workspace), self.agent_id, self.health)
        except Exception as exc:
            errors.append(f"agent_health:{exc}")
        if self.restore_running and not errors:
            try:
                runtime.start_agent(self.workspace, self.agent_id, force=True, allow_fresh=True)
            except Exception as exc:
                errors.append(f"worker_restore:{exc}")
        result = {"ok": not errors, "errors": errors}
        if errors:
            event_log.write("remove_agent.rollback_failed", agent_id=self.agent_id, errors=errors)
        return result


def _find_worker(spec: dict[str, Any], agent_id: str) -> dict[str, Any] | None:
    if spec.get("leader", {}).get("id") == agent_id:
        return None
    for agent in spec.get("agents", []):
        if agent.get("id") == agent_id:
            return agent
    return None


def _dynamic_role_file(workspace: Path, agent_id: str, agent_state: dict[str, Any]) -> Path:
    raw = agent_state.get("dynamic_role_file")
    if raw:
        path = Path(str(raw))
        return path if path.is_absolute() else workspace / path
    return workspace / ".team" / "dynamic-role-files" / f"{agent_id}.md"


def _is_running(runtime: Any, state: dict[str, Any], agent_id: str, agent_state: dict[str, Any]) -> bool:
    if str(agent_state.get("status") or "").lower() in {"running", "busy"}:
        return True
    session_name = state.get("session_name")
    window = agent_state.get("window") or agent_id
    return bool(session_name and runtime._tmux_window_exists(str(session_name), str(window)))


def _remove_dynamic_role_file(path: Path, required: bool) -> bool:
    if path.exists():
        path.unlink()
        return True
    if required:
        raise RuntimeError(f"dynamic role file missing: {path}")
    return False


def _delete_agent_health(store: MessageStore, agent_id: str) -> None:
    store.delete_agent_health(agent_id)


def _restore_agent_health(store: MessageStore, agent_id: str, row: dict[str, Any] | None) -> None:
    if not row:
        _delete_agent_health(store, agent_id)
        return
    store.upsert_agent_health(
        agent_id,
        row.get("status") or "IDLE",
        last_output_at=row.get("last_output_at"),
        context_usage_pct=row.get("context_usage_pct"),
        current_task_id=row.get("current_task_id"),
    )
