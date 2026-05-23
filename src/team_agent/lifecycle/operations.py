from __future__ import annotations

import copy
import hashlib
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

from team_agent import runtime as _runtime
from team_agent.errors import RuntimeError
from team_agent.events import EventLog
from team_agent.spec import load_spec, validate_spec
from team_agent.state import (
    SESSION_CAPTURE_FIELDS,
    load_runtime_state,
    save_runtime_state,
    write_spec,
    write_team_state,
)


_RUNTIME_SYMBOLS = (
    "_capture_agent_session",
    "_close_ghostty_display",
    "_effective_runtime_config",
    "_find_agent",
    "_handle_startup_prompts_and_verify_window",
    "_open_ghostty_worker_window",
    "_open_ghostty_workspace_agent_display",
    "_running_agent_state",
    "_runtime_lock",
    "_save_team_runtime_snapshot",
    "_spec_team_dir",
    "_tmux_start_command_for_agent_window",
    "_tmux_window_exists",
    "ensure_workspace_dirs",
    "get_adapter",
    "run_cmd",
    "shell_fork_command_for_agent",
    "start_agent",
    "start_coordinator",
)
for _name in _RUNTIME_SYMBOLS:
    if not hasattr(_runtime, _name):
        raise ImportError(f"team_agent.runtime missing lifecycle operation dependency: {_name}")


def _runtime_proxy(name: str):
    def proxy(*args: Any, **kwargs: Any) -> Any:
        return getattr(_runtime, name)(*args, **kwargs)

    return proxy


globals().update({_name: _runtime_proxy(_name) for _name in _RUNTIME_SYMBOLS})


def stop_agent(workspace: Path, agent_id: str) -> dict[str, Any]:
    with _runtime_lock(workspace, "stop-agent"):
        state = load_runtime_state(workspace)
        spec_path = Path(state.get("spec_path", workspace / "team.spec.yaml"))
        spec = load_spec(spec_path)
        agent = _find_agent(spec, agent_id)
        if not agent or spec.get("leader", {}).get("id") == agent_id:
            raise RuntimeError(f"unknown worker agent id: {agent_id}")
        ensure_workspace_dirs(workspace)
        event_log = EventLog(workspace)
        session_name = state.get("session_name") or spec.get("runtime", {}).get("session_name") or f"team-{spec['team']['name']}"
        agent_state = dict(state.get("agents", {}).get(agent_id) or {"provider": agent["provider"], "agent_id": agent_id})
        window = str(agent_state.get("window") or agent_id)
        target = f"{session_name}:{window}"
        stopped = False
        if _tmux_window_exists(session_name, window):
            proc = run_cmd(["tmux", "kill-window", "-t", target], timeout=10)
            if proc.returncode != 0:
                event_log.write("stop_agent.window_stop_failed", agent_id=agent_id, target=target, stderr=proc.stderr.strip())
                raise RuntimeError(f"failed to stop agent {agent_id}: {proc.stderr.strip()}")
            stopped = True
        _close_ghostty_display(agent_id, agent_state, event_log)
        agent_state.update({"status": "stopped", "provider": agent["provider"], "agent_id": agent_id, "window": window})
        state.setdefault("agents", {})[agent_id] = agent_state
        save_runtime_state(workspace, state)
        _save_team_runtime_snapshot(workspace, state)
        state_path = write_team_state(workspace, spec, state)
        event_log.write("stop_agent.complete", agent_id=agent_id, target=target, stopped=stopped)
        return {"ok": True, "agent_id": agent_id, "status": "stopped", "target": target, "stopped": stopped, "state_file": str(state_path)}


def reset_agent(workspace: Path, agent_id: str, *, discard_session: bool = False, open_display: bool = True) -> dict[str, Any]:
    if not discard_session:
        return {"ok": False, "agent_id": agent_id, "status": "refused", "reason": "discard_session_required"}
    stopped = stop_agent(workspace, agent_id)
    state = load_runtime_state(workspace)
    spec_path = Path(state.get("spec_path", workspace / "team.spec.yaml"))
    spec = load_spec(spec_path)
    agent_state = dict(state.get("agents", {}).get(agent_id) or {})
    for key in [*SESSION_CAPTURE_FIELDS, "_pending_session_id"]:
        agent_state.pop(key, None)
    agent_state["status"] = "stopped"
    state.setdefault("agents", {})[agent_id] = agent_state
    save_runtime_state(workspace, state)
    write_team_state(workspace, spec, state)
    started = start_agent(workspace, agent_id, force=True, open_display=open_display, allow_fresh=True)
    EventLog(workspace).write("reset_agent.complete", agent_id=agent_id, stopped=stopped, started=started)
    return {"ok": True, "agent_id": agent_id, "status": "running", "stopped": stopped, "started": started}


def add_agent(workspace: Path, agent_id: str, *, role_file_path: str, open_display: bool = True) -> dict[str, Any]:
    from team_agent.compiler import compile_role_doc_agent

    state = load_runtime_state(workspace)
    spec_path = Path(state.get("spec_path", workspace / "team.spec.yaml"))
    spec = load_spec(spec_path)
    if _find_agent(spec, agent_id):
        raise RuntimeError(f"agent id already exists: {agent_id}")
    team_dir = Path(str(state.get("team_dir"))) if state.get("team_dir") else _spec_team_dir(spec_path, workspace)
    role_file = Path(role_file_path)
    if not role_file.is_absolute():
        role_file = workspace / role_file
    if not role_file.is_file():
        raise RuntimeError(f"role file not found: {role_file}")
    role_bytes = role_file.read_bytes()
    role_sha = hashlib.sha256(role_bytes).hexdigest()
    dynamic_dir = workspace / ".team" / "dynamic-role-files"
    dynamic_path = dynamic_dir / f"{agent_id}.md"
    old_spec_text = spec_path.read_text(encoding="utf-8")
    old_state = copy.deepcopy(state)
    old_dynamic = dynamic_path.read_bytes() if dynamic_path.exists() else None
    event_log = EventLog(workspace)
    try:
        dynamic_dir.mkdir(parents=True, exist_ok=True)
        dynamic_path.write_bytes(role_bytes)
        agent = compile_role_doc_agent(dynamic_path, team_dir, agent_id)
        spec.setdefault("agents", []).append(agent)
        spec.setdefault("runtime", {}).setdefault("startup_order", []).append(agent_id)
        validate_spec(spec, base_dir=spec_path.parent)
        write_spec(spec_path, spec)
        write_team_state(workspace, spec, state)
        started = start_agent(workspace, agent_id, open_display=open_display, allow_fresh=True)
        state = load_runtime_state(workspace)
        state["agents"][agent_id]["dynamic_role_file"] = str(dynamic_path.relative_to(workspace))
        state["agents"][agent_id]["role_file_sha"] = role_sha
        save_runtime_state(workspace, state)
        state_path = write_team_state(workspace, spec, state)
    except Exception:
        spec_path.write_text(old_spec_text, encoding="utf-8")
        save_runtime_state(workspace, old_state)
        if old_dynamic is None:
            dynamic_path.unlink(missing_ok=True)
        else:
            dynamic_path.parent.mkdir(parents=True, exist_ok=True)
            dynamic_path.write_bytes(old_dynamic)
        raise
    event_log.write("add_agent.complete", agent_id=agent_id, role_file=str(dynamic_path), role_file_sha=role_sha, started=started)
    return {
        "ok": True,
        "agent_id": agent_id,
        "new_agent_id": agent_id,
        "status": "running",
        "role_file": str(dynamic_path),
        "role_file_sha": role_sha,
        "started": started,
        "state_file": str(state_path),
    }


def fork_agent(
    workspace: Path,
    source_agent_id: str,
    *,
    as_agent_id: str,
    label: str | None = None,
    open_display: bool = True,
) -> dict[str, Any]:
    state = load_runtime_state(workspace)
    spec_path = Path(state.get("spec_path", workspace / "team.spec.yaml"))
    spec = load_spec(spec_path)
    if _find_agent(spec, as_agent_id):
        raise RuntimeError(f"agent id already exists: {as_agent_id}")
    source_agent = _find_agent(spec, source_agent_id)
    if not source_agent or spec.get("leader", {}).get("id") == source_agent_id:
        raise RuntimeError(f"unknown worker agent id: {source_agent_id}")
    source_state = state.get("agents", {}).get(source_agent_id) or {}
    source_session_id = str(source_state.get("session_id") or "")
    if not source_session_id:
        raise RuntimeError(f"cannot fork {source_agent_id}: source session_id is missing")
    session_name = state.get("session_name") or spec.get("runtime", {}).get("session_name") or f"team-{spec['team']['name']}"
    if _tmux_window_exists(session_name, as_agent_id):
        raise RuntimeError(f"tmux window already exists for fork target: {session_name}:{as_agent_id}")
    new_agent = copy.deepcopy(source_agent)
    new_agent["id"] = as_agent_id
    new_agent["role"] = str(label or new_agent.get("role") or as_agent_id)
    new_agent["forked_from"] = source_agent_id
    new_agent["preferred_for"] = [as_agent_id, new_agent["role"]]
    old_spec_text = spec_path.read_text(encoding="utf-8")
    old_state = copy.deepcopy(state)
    event_log = EventLog(workspace)
    mcp_path: Path | None = None
    try:
        spec.setdefault("agents", []).append(new_agent)
        spec.setdefault("runtime", {}).setdefault("startup_order", []).append(as_agent_id)
        validate_spec(spec, base_dir=spec_path.parent)
        write_spec(spec_path, spec)
        runtime_cfg = _effective_runtime_config(spec.get("runtime", {}))
        adapter = get_adapter(new_agent["provider"])
        if not adapter.supports_session_fork(new_agent):
            raise RuntimeError(f"{new_agent['provider']} does not support native session fork")
        mcp_config = adapter.mcp_config(workspace, as_agent_id)
        mcp_path = adapter.install_mcp(workspace, as_agent_id, mcp_config)
        command_agent = copy.deepcopy(new_agent)
        command_agent["_runtime"] = runtime_cfg
        command = shell_fork_command_for_agent(command_agent, source_session_id, workspace, mcp_config)
        tmux_cmd, tmux_start_mode = _tmux_start_command_for_agent_window(session_name, as_agent_id, command)
        event_log.write(
            "fork_agent.agent_start",
            source_agent_id=source_agent_id,
            new_agent_id=as_agent_id,
            provider=new_agent["provider"],
            source_session_id=source_session_id,
            tmux_start_mode=tmux_start_mode,
            command=command,
            mcp_config=str(mcp_path),
        )
        proc = run_cmd(tmux_cmd)
        if proc.returncode != 0:
            raise RuntimeError(f"failed to fork agent {source_agent_id}: {proc.stderr.strip()}")
        if not _handle_startup_prompts_and_verify_window(
            adapter, event_log, "fork_agent", as_agent_id, new_agent["provider"], session_name, "forked"
        ):
            raise RuntimeError(f"Failed to fork agent {as_agent_id}: tmux window exited after start")
        spawn_time = datetime.now(timezone.utc)
        agent_state = _running_agent_state(workspace, new_agent, {})
        agent_state.update(
            {
                "mcp_config": str(mcp_path),
                "session_name": session_name,
                "spawned_at": spawn_time.isoformat(),
                "forked_from": source_agent_id,
            }
        )
        if command_agent.get("_session_id"):
            agent_state["_pending_session_id"] = command_agent["_session_id"]
        _capture_agent_session(
            workspace,
            as_agent_id,
            agent_state,
            event_log,
            timeout_s=1.5,
            exclude_session_ids={source_session_id},
        )
        if open_display and state.get("display_backend") in {"ghostty", "ghostty_window"}:
            agent_state["display"] = _open_ghostty_worker_window(workspace, session_name, as_agent_id, new_agent, event_log)
        elif open_display and state.get("display_backend") == "ghostty_workspace":
            agent_state["display"] = _open_ghostty_workspace_agent_display(session_name, as_agent_id, new_agent, {}, event_log)
        state.setdefault("agents", {})[as_agent_id] = agent_state
        save_runtime_state(workspace, state)
        _save_team_runtime_snapshot(workspace, state)
        state_path = write_team_state(workspace, spec, state)
        coordinator = start_coordinator(workspace)
    except Exception:
        if _tmux_window_exists(session_name, as_agent_id):
            run_cmd(["tmux", "kill-window", "-t", f"{session_name}:{as_agent_id}"], timeout=10)
        if mcp_path is not None:
            try:
                get_adapter(new_agent["provider"]).cleanup_mcp(workspace, as_agent_id, mcp_path)
            except Exception as exc:
                event_log.write("fork_agent.mcp_cleanup_failed", new_agent_id=as_agent_id, error=str(exc))
        spec_path.write_text(old_spec_text, encoding="utf-8")
        save_runtime_state(workspace, old_state)
        raise
    event_log.write(
        "fork_agent.complete",
        source_agent_id=source_agent_id,
        new_agent_id=as_agent_id,
        session_id=state["agents"][as_agent_id].get("session_id"),
        coordinator=coordinator,
    )
    return {
        "ok": True,
        "source_agent_id": source_agent_id,
        "new_agent_id": as_agent_id,
        "agent_id": as_agent_id,
        "status": "running",
        "session_id": state["agents"][as_agent_id].get("session_id"),
        "state_file": str(state_path),
        "coordinator": coordinator,
    }
