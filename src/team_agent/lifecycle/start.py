from __future__ import annotations

import copy
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

from team_agent import runtime as _runtime
from team_agent.errors import RuntimeError
from team_agent.events import EventLog
from team_agent.message_store import MessageStore
from team_agent.providers import ResumeUnavailable
from team_agent.spec import load_spec
from team_agent.state import check_team_owner, load_runtime_state, save_runtime_state, write_team_state


_RUNTIME_SYMBOLS = (
    "_attach_profile_resume_root",
    "_attach_team_profile_dirs",
    "_capture_agent_session",
    "_clear_session_capture_fields",
    "_deliver_pending_message",
    "_effective_runtime_config",
    "_enable_codex_fast_mode",
    "_ensure_agent_start_requirements",
    "_find_agent",
    "_handle_startup_prompts_and_verify_window",
    "_open_ghostty_worker_window",
    "_open_ghostty_workspace_agent_display",
    "_prepare_resume_state",
    "_running_agent_state",
    "_runtime_lock",
    "_spec_team_dir",
    "_tmux_start_command_for_agent_window",
    "_tmux_window_exists",
    "ensure_workspace_dirs",
    "get_adapter",
    "run_cmd",
    "shell_command_for_agent",
    "shell_resume_command_for_agent",
    "start_coordinator",
)
for _name in _RUNTIME_SYMBOLS:
    if not hasattr(_runtime, _name):
        raise ImportError(f"team_agent.runtime missing lifecycle start dependency: {_name}")


def _runtime_proxy(name: str):
    def proxy(*args: Any, **kwargs: Any) -> Any:
        return getattr(_runtime, name)(*args, **kwargs)

    return proxy


globals().update({_name: _runtime_proxy(_name) for _name in _RUNTIME_SYMBOLS})


def _resume_rollout_missing(agent: dict[str, Any], previous: dict[str, Any]) -> bool:
    if agent.get("provider") != "codex" or not previous.get("session_id"):
        return False
    rollout_path = previous.get("rollout_path")
    return not rollout_path or not Path(str(rollout_path)).exists()


def start_agent(
    workspace: Path,
    agent_id: str,
    force: bool = False,
    open_display: bool = True,
    allow_fresh: bool = False,
) -> dict[str, Any]:
    with _runtime_lock(workspace, "start-agent"):
        return _start_agent_unlocked(workspace, agent_id, force=force, open_display=open_display, allow_fresh=allow_fresh)


def _start_agent_unlocked(workspace: Path, agent_id: str, force: bool, open_display: bool, allow_fresh: bool) -> dict[str, Any]:
    state = load_runtime_state(workspace)
    gate = check_team_owner(state)
    if gate:
        return gate
    spec_path = Path(state.get("spec_path", workspace / "team.spec.yaml"))
    if not spec_path.exists():
        raise RuntimeError(f"missing spec for start-agent: {spec_path}")
    spec = load_spec(spec_path)
    team_dir = Path(str(state.get("team_dir"))) if state.get("team_dir") else _spec_team_dir(spec_path, workspace)
    _attach_team_profile_dirs(spec, spec_path, workspace, team_dir)
    agent = _find_agent(spec, agent_id)
    if not agent or spec.get("leader", {}).get("id") == agent_id:
        raise RuntimeError(f"unknown worker agent id: {agent_id}")
    if agent.get("paused"):
        return {"ok": False, "status": "paused", "agent_id": agent_id, "reason": "agent_paused"}
    ensure_workspace_dirs(workspace)
    event_log = EventLog(workspace)
    runtime_cfg = _effective_runtime_config(spec.get("runtime", {}))
    session_name = state.get("session_name") or spec.get("runtime", {}).get("session_name") or f"team-{spec['team']['name']}"
    state["session_name"] = session_name
    state.setdefault("workspace", str(workspace))
    state.setdefault("team_dir", str(team_dir))
    state.setdefault("spec_path", str(spec_path.resolve()))
    state.setdefault("leader", spec.get("leader"))
    state.setdefault("tasks", [dict(task) for task in spec.get("tasks", [])])
    state.setdefault("agents", {})
    state["display_backend"] = spec.get("runtime", {}).get("display_backend", state.get("display_backend") or "none")

    previous = state.get("agents", {}).get(agent_id, {})
    target = f"{session_name}:{agent_id}"
    window_present = _tmux_window_exists(session_name, agent_id)
    if window_present and not force:
        agent_state = _running_agent_state(workspace, agent, previous)
        agent_state["session_name"] = session_name
        if open_display and state.get("display_backend") in {"ghostty", "ghostty_window"}:
            display = agent_state.get("display") or {}
            if display.get("status") != "opened":
                agent_state["display"] = _open_ghostty_worker_window(workspace, session_name, agent_id, agent, event_log)
        elif open_display and state.get("display_backend") == "ghostty_workspace":
            display = agent_state.get("display") or {}
            if display.get("status") != "opened":
                agent_state["display"] = _open_ghostty_workspace_agent_display(session_name, agent_id, agent, display, event_log)
        state["agents"][agent_id] = agent_state
        save_runtime_state(workspace, state)
        write_team_state(workspace, spec, state)
        coordinator = start_coordinator(workspace)
        event_log.write("start_agent.noop", agent_id=agent_id, target=target, coordinator=coordinator)
        return {"ok": True, "agent_id": agent_id, "status": "running", "start_mode": "noop", "target": target, "coordinator": coordinator}

    if window_present and force:
        proc = run_cmd(["tmux", "kill-window", "-t", target], timeout=10)
        if proc.returncode != 0:
            raise RuntimeError(f"failed to replace existing agent window {target}: {proc.stderr.strip()}")

    _ensure_agent_start_requirements(workspace, [agent], event_log, "start_agent")
    adapter = get_adapter(agent["provider"])
    if not adapter.is_installed():
        event_log.write("start_agent.provider_missing", agent_id=agent_id, provider=agent["provider"], command=adapter.command_name)
        raise RuntimeError(f"Provider {agent['provider']} command {adapter.command_name!r} not found for agent {agent_id}")
    mcp_config = adapter.mcp_config(workspace, agent_id)
    mcp_path = adapter.install_mcp(workspace, agent_id, mcp_config)
    command_agent = copy.deepcopy(agent)
    command_agent["_runtime"] = runtime_cfg
    previous = _attach_profile_resume_root(workspace, command_agent, previous)
    known_session_ids = {
        str(item.get("session_id"))
        for aid, item in state.get("agents", {}).items()
        if aid != agent_id and item.get("session_id")
    }
    try:
        previous = _prepare_resume_state(
            workspace,
            agent_id,
            previous,
            adapter,
            event_log,
            known_session_ids,
            allow_fresh_on_resume_failure=allow_fresh,
        )
    except ResumeUnavailable as exc:
        try:
            adapter.cleanup_mcp(workspace, agent_id, mcp_path)
        except Exception as cleanup_exc:
            event_log.write(
                "start_agent.mcp_cleanup_failed",
                agent_id=agent_id,
                provider=agent["provider"],
                mcp_config=str(mcp_path),
                error=str(cleanup_exc),
            )
        raise RuntimeError(str(exc)) from exc
    missing_resume_rollout = _resume_rollout_missing(agent, previous)
    start_mode = "resumed" if previous.get("session_id") else "fresh"
    if missing_resume_rollout and allow_fresh:
        event_log.write(
            "start_agent.resume_window_missing_fallback_fresh",
            agent_id=agent_id,
            provider=agent["provider"],
            session_id=previous.get("session_id"),
            reason="rollout_missing",
        )
        start_mode = "fresh_after_missing_rollout"
        previous = dict(previous)
        previous["session_id"] = None
    if start_mode == "resumed":
        try:
            command = shell_resume_command_for_agent(command_agent, previous, workspace, mcp_config)
        except ResumeUnavailable as exc:
            event_log.write("start_agent.resume_unavailable", agent_id=agent_id, error=str(exc))
            if not allow_fresh:
                try:
                    adapter.cleanup_mcp(workspace, agent_id, mcp_path)
                except Exception as cleanup_exc:
                    event_log.write(
                        "start_agent.mcp_cleanup_failed",
                        agent_id=agent_id,
                        provider=agent["provider"],
                        mcp_config=str(mcp_path),
                        error=str(cleanup_exc),
                    )
                raise RuntimeError(
                    f"Cannot resume agent {agent_id}: {exc}. "
                    "Use team-agent start-agent --allow-fresh only if losing that worker context is acceptable."
                ) from exc
            command = shell_command_for_agent(command_agent, workspace, mcp_config)
            start_mode = "fresh"
    else:
        command = shell_command_for_agent(command_agent, workspace, mcp_config)
        event_log.write(
            "start_agent.fresh_spawn",
            agent_id=agent_id,
            provider=agent["provider"],
            reason="rollout_missing" if start_mode == "fresh_after_missing_rollout" else "session_id_missing",
        )

    tmux_cmd, tmux_start_mode = _tmux_start_command_for_agent_window(session_name, agent_id, command)
    event_log.write(
        "start_agent.agent_start",
        agent_id=agent_id,
        provider=agent["provider"],
        start_mode=start_mode,
        session_id=previous.get("session_id"),
        session=session_name,
        window=agent_id,
        tmux_start_mode=tmux_start_mode,
        command=command,
        mcp_config=str(mcp_path),
    )
    proc = run_cmd(tmux_cmd)
    if proc.returncode != 0:
        try:
            adapter.cleanup_mcp(workspace, agent_id, mcp_path)
        except Exception as exc:
            event_log.write("start_agent.mcp_cleanup_failed", agent_id=agent_id, provider=agent["provider"], error=str(exc))
        event_log.write("start_agent.agent_failed", agent_id=agent_id, stderr=proc.stderr, stdout=proc.stdout)
        raise RuntimeError(f"Failed to start agent {agent_id}: {proc.stderr.strip()}")

    if not _handle_startup_prompts_and_verify_window(
        adapter, event_log, "start_agent", agent_id, agent["provider"], session_name, start_mode
    ):
        if start_mode != "resumed":
            try:
                adapter.cleanup_mcp(workspace, agent_id, mcp_path)
            except Exception as exc:
                event_log.write("start_agent.mcp_cleanup_failed", agent_id=agent_id, provider=agent["provider"], error=str(exc))
            raise RuntimeError(f"Failed to start agent {agent_id}: tmux window exited after start")
        if not allow_fresh:
            try:
                adapter.cleanup_mcp(workspace, agent_id, mcp_path)
            except Exception as cleanup_exc:
                event_log.write(
                    "start_agent.mcp_cleanup_failed",
                    agent_id=agent_id,
                    provider=agent["provider"],
                    mcp_config=str(mcp_path),
                    error=str(cleanup_exc),
                )
            raise RuntimeError(
                f"Cannot resume agent {agent_id}: resume window exited or did not become visible. "
                "Use team-agent start-agent --allow-fresh only if losing that worker context is acceptable."
            )
        event_log.write(
            "start_agent.resume_window_missing_fallback_fresh",
            agent_id=agent_id,
            provider=agent["provider"],
            session_id=previous.get("session_id"),
        )
        command = shell_command_for_agent(command_agent, workspace, mcp_config)
        start_mode = "fresh_after_missing_rollout" if missing_resume_rollout else "fresh"
        tmux_cmd, tmux_start_mode = _tmux_start_command_for_agent_window(session_name, agent_id, command)
        event_log.write(
            "start_agent.agent_start",
            agent_id=agent_id,
            provider=agent["provider"],
            start_mode=start_mode,
            session_id=None,
            session=session_name,
            window=agent_id,
            tmux_start_mode=tmux_start_mode,
            command=command,
            mcp_config=str(mcp_path),
        )
        proc = run_cmd(tmux_cmd)
        if proc.returncode != 0:
            try:
                adapter.cleanup_mcp(workspace, agent_id, mcp_path)
            except Exception as exc:
                event_log.write("start_agent.mcp_cleanup_failed", agent_id=agent_id, provider=agent["provider"], error=str(exc))
            event_log.write("start_agent.agent_failed", agent_id=agent_id, stderr=proc.stderr, stdout=proc.stdout)
            raise RuntimeError(f"Failed to start agent {agent_id} fresh after resume exit: {proc.stderr.strip()}")
        if not _handle_startup_prompts_and_verify_window(
            adapter, event_log, "start_agent", agent_id, agent["provider"], session_name, start_mode
        ):
            try:
                adapter.cleanup_mcp(workspace, agent_id, mcp_path)
            except Exception as exc:
                event_log.write("start_agent.mcp_cleanup_failed", agent_id=agent_id, provider=agent["provider"], error=str(exc))
            raise RuntimeError(f"Failed to start agent {agent_id} fresh: tmux window exited after start")
    if runtime_cfg.get("fast") and agent.get("provider") == "codex":
        fast_result = _enable_codex_fast_mode(session_name, agent_id)
        event_log.write("start_agent.codex_fast_mode", agent_id=agent_id, **fast_result)

    spawn_time = datetime.now(timezone.utc)
    agent_state = _running_agent_state(workspace, agent, previous)
    agent_state.update({"mcp_config": str(mcp_path), "session_name": session_name, "spawned_at": spawn_time.isoformat()})
    profile_launch = command_agent.get("_provider_profile") or {}
    if profile_launch.get("claude_projects_root"):
        agent_state["claude_projects_root"] = profile_launch["claude_projects_root"]
    if start_mode in {"fresh", "fresh_after_missing_rollout"}:
        _clear_session_capture_fields(agent_state)
        if command_agent.get("_session_id"):
            agent_state["_pending_session_id"] = command_agent["_session_id"]
        _capture_agent_session(workspace, agent_id, agent_state, event_log, timeout_s=1.5, exclude_session_ids=known_session_ids)
    if open_display and state.get("display_backend") in {"ghostty", "ghostty_window"}:
        agent_state["display"] = _open_ghostty_worker_window(workspace, session_name, agent_id, agent, event_log)
    elif open_display and state.get("display_backend") == "ghostty_workspace":
        agent_state["display"] = _open_ghostty_workspace_agent_display(
            session_name,
            agent_id,
            agent,
            previous.get("display") or {},
            event_log,
        )
    state["agents"][agent_id] = agent_state
    save_runtime_state(workspace, state)
    store = MessageStore(workspace)
    delivered_messages: list[str] = []
    for row in store.messages():
        if row["recipient"] == agent_id and row["status"] in {"pending", "accepted"}:
            delivered = _deliver_pending_message(workspace, state, row["message_id"], wait_visible=True, timeout=30.0)
            if delivered.get("ok"):
                delivered_messages.append(row["message_id"])
                event_log.write("send.pending_delivered", message_id=row["message_id"], agent_id=agent_id, source="start_agent")
    write_team_state(workspace, spec, state)
    coordinator = start_coordinator(workspace)
    event_log.write(
        "start_agent.complete",
        agent_id=agent_id,
        session=session_name,
        start_mode=start_mode,
        delivered_messages=delivered_messages,
        coordinator=coordinator,
    )
    return {
        "ok": True,
        "agent_id": agent_id,
        "status": "running",
        "start_mode": start_mode,
        "session_id": agent_state.get("session_id"),
        "target": target,
        "display_target": agent_state.get("display"),
        "delivered_messages": delivered_messages,
        "coordinator": coordinator,
    }
