from __future__ import annotations

from datetime import datetime, timezone
from pathlib import Path
from typing import Any

from team_agent.events import EventLog
from team_agent.launch.bootstrap import (
    attach_team_profile_dirs,
    spec_team_dir,
    tmux_session_conflict_error,
)
from team_agent.launch.config import effective_runtime_config, requires_direct_leader_receiver
from team_agent.launch.requirements import ensure_agent_start_requirements
from team_agent.message_store import MessageStore
from team_agent.permissions import resolve_permissions
from team_agent.routing import route_task
from team_agent.spec import load_spec, workspace_from_spec
from team_agent.state import load_runtime_state, merge_workspace_team_state, save_runtime_state, write_team_state


def launch(
    spec_path: Path,
    dry_run: bool = False,
    auto_approve: bool = False,
    skip_profile_smoke: bool = False,
) -> dict[str, Any]:
    from team_agent.runtime import (
        GHOSTTY_DISPLAY_BACKENDS,
        RuntimeError,
        _attach_leader_to_state,
        _capture_agent_session,
        _enable_codex_fast_mode,
        _open_worker_displays,
        _save_team_runtime_snapshot,
        _tmux_session_exists,
        ensure_workspace_dirs,
        get_adapter,
        get_adapter_or_raise,
        run_cmd,
        shell_command_for_agent,
    )

    spec = load_spec(spec_path)
    workspace = workspace_from_spec(spec, spec_path)
    team_dir = spec_team_dir(spec_path, workspace)
    attach_team_profile_dirs(spec, spec_path, workspace, team_dir)
    ensure_workspace_dirs(workspace)
    event_log = EventLog(workspace)
    session_name = spec.get("runtime", {}).get("session_name") or f"team-{spec['team']['name']}"
    state = {
        "spec_path": str(spec_path.resolve()),
        "workspace": str(workspace),
        "team_dir": str(team_dir),
        "session_name": session_name,
        "leader": spec.get("leader"),
        "agents": {},
        "tasks": [dict(task) for task in spec.get("tasks", [])],
        "display_backend": spec.get("runtime", {}).get("display_backend", "none"),
    }
    runtime_cfg = effective_runtime_config(spec.get("runtime", {}))
    dangerous_auto_approve = bool(runtime_cfg.get("dangerous_auto_approve"))
    dangerous_inherited = bool(runtime_cfg.get("dangerous_auto_approve_inherited"))

    routing_decisions: list[dict[str, Any]] = []
    for task in state["tasks"]:
        route = route_task(spec, task)
        task["assignee"] = route["agent_id"]
        decision = {
            "source": "launch",
            "task_id": task.get("id"),
            "selected_agent": route["agent_id"],
            "reason": route["reason"],
            "manual_override": False,
        }
        routing_decisions.append(decision)
        event_log.write("routing.decision", **decision)

    permission_summary = [resolve_permissions(agent) for agent in spec.get("agents", [])]
    event_log.write(
        "launch.permissions_resolved",
        permissions=permission_summary,
        dangerous_auto_approve=dangerous_auto_approve,
        dangerous_auto_approve_source=runtime_cfg.get("dangerous_auto_approve_source"),
    )
    if dry_run:
        return {
            "ok": True,
            "dry_run": True,
            "session_name": session_name,
            "permissions": permission_summary,
            "routes": routing_decisions,
            "safety": {
                "dangerous_auto_approve": dangerous_auto_approve,
                "dangerous_auto_approve_source": runtime_cfg.get("dangerous_auto_approve_source"),
                "dangerous_auto_approve_inherited": dangerous_inherited,
                "requires_explicit_yes": dangerous_auto_approve and not dangerous_inherited,
            },
        }
    if dangerous_auto_approve:
        event_log.write(
            "launch.dangerous_auto_approve_requested",
            reason="provider may bypass approvals or sandbox",
            source=runtime_cfg.get("dangerous_auto_approve_source"),
            inherited=dangerous_inherited,
            inherited_provider=runtime_cfg.get("dangerous_auto_approve_provider"),
            inherited_flag=runtime_cfg.get("dangerous_auto_approve_flag"),
        )
    if dangerous_auto_approve and not dangerous_inherited and not auto_approve:
        raise RuntimeError("dangerous_auto_approve requires explicit --yes after reviewing launch risk")
    if runtime_cfg.get("require_user_approval_before_launch", True) and not auto_approve:
        raise RuntimeError("launch requires approval; rerun with --yes after reviewing resolved permissions")

    tmux = get_adapter_or_raise("tmux")
    _ = tmux
    if _tmux_session_exists(session_name):
        event_log.write(
            "launch.session_conflict",
            session=session_name,
            action="use a different team name or runtime.session_name; do not terminate existing tmux sessions from startup",
        )
        raise RuntimeError(tmux_session_conflict_error(session_name))
    ensure_agent_start_requirements(
        workspace,
        spec.get("agents", []),
        event_log,
        "launch",
        skip_profile_smoke=skip_profile_smoke,
    )

    leader_receiver = None
    leader_provider = state.get("leader", {}).get("provider")
    require_leader_receiver = requires_direct_leader_receiver(spec, runtime_cfg)
    if runtime_cfg.get("auto_attach_leader", True) and leader_provider != "fake":
        try:
            leader_receiver, _ = _attach_leader_to_state(
                workspace,
                state,
                pane=None,
                provider=leader_provider,
                event_log=event_log,
                source="launch",
                require_current=require_leader_receiver,
            )
        except RuntimeError as exc:
            event_log.write(
                "leader_receiver.auto_attach_skipped",
                provider=leader_provider,
                reason=str(exc),
                required=require_leader_receiver,
                suggestion="Start the leader with `team-agent codex` or run quick-start from an existing tmux pane.",
            )
            if require_leader_receiver:
                raise

    first = True
    started: list[dict[str, Any]] = []
    display_jobs: list[tuple[str, dict[str, Any]]] = []
    for agent in spec.get("agents", []):
        if agent.get("paused"):
            state["agents"][agent["id"]] = {"status": "paused", "provider": agent["provider"]}
            continue
        adapter = get_adapter(agent["provider"])
        if not adapter.is_installed():
            event_log.write(
                "launch.provider_missing",
                agent_id=agent["id"],
                provider=agent["provider"],
                command=adapter.command_name,
            )
            raise RuntimeError(
                f"Provider {agent['provider']} command {adapter.command_name!r} not found for agent {agent['id']}"
            )
        mcp_config = adapter.mcp_config(workspace, agent["id"])
        mcp_path = adapter.install_mcp(workspace, agent["id"], mcp_config)
        command_agent = dict(agent)
        command_agent["_runtime"] = runtime_cfg
        command = shell_command_for_agent(command_agent, workspace, mcp_config)
        spawn_time = datetime.now(timezone.utc)
        event_log.write(
            "launch.agent_start",
            agent_id=agent["id"],
            provider=agent["provider"],
            session=session_name,
            window=agent["id"],
            command=command,
            mcp_config=str(mcp_path),
        )
        if first:
            proc = run_cmd(["tmux", "new-session", "-d", "-s", session_name, "-n", agent["id"], "sh", "-lc", command])
            first = False
        else:
            proc = run_cmd(["tmux", "new-window", "-t", session_name, "-n", agent["id"], "sh", "-lc", command])
        if proc.returncode != 0:
            try:
                adapter.cleanup_mcp(workspace, agent["id"], mcp_path)
            except Exception as exc:
                event_log.write(
                    "launch.mcp_cleanup_failed",
                    agent_id=agent["id"],
                    provider=agent["provider"],
                    mcp_config=str(mcp_path),
                    error=str(exc),
                )
            event_log.write(
                "launch.agent_failed",
                agent_id=agent["id"],
                stderr=proc.stderr,
                stdout=proc.stdout,
            )
            raise RuntimeError(f"Failed to start agent {agent['id']}: {proc.stderr.strip()}")
        handled_prompts = adapter.handle_startup_prompts(session_name, agent["id"], checks=1, sleep_s=0.0)
        for prompt_event in handled_prompts:
            event_log.write(
                "launch.startup_prompt_handled",
                agent_id=agent["id"],
                provider=agent["provider"],
                **prompt_event,
            )
        if runtime_cfg.get("fast") and agent.get("provider") == "codex":
            fast_result = _enable_codex_fast_mode(session_name, agent["id"])
            event_log.write("launch.codex_fast_mode", agent_id=agent["id"], **fast_result)
        state["agents"][agent["id"]] = {
            "status": "running",
            "provider": agent["provider"],
            "agent_id": agent["id"],
            "model": agent.get("model"),
            "auth_mode": agent.get("auth_mode"),
            "profile": agent.get("profile"),
            "window": agent["id"],
            "mcp_config": str(mcp_path),
            "permissions": resolve_permissions(agent),
            "session_id": None,
            "rollout_path": None,
            "captured_at": None,
            "captured_via": None,
            "attribution_confidence": None,
            "spawn_cwd": str(workspace),
            "spawned_at": spawn_time.isoformat(),
        }
        profile_launch = command_agent.get("_provider_profile") or {}
        if profile_launch.get("claude_projects_root"):
            state["agents"][agent["id"]]["claude_projects_root"] = profile_launch["claude_projects_root"]
        if command_agent.get("_session_id"):
            state["agents"][agent["id"]]["_pending_session_id"] = command_agent["_session_id"]
        known_session_ids = {
            str(item.get("session_id"))
            for aid, item in state.get("agents", {}).items()
            if aid != agent["id"] and item.get("session_id")
        }
        _capture_agent_session(
            workspace,
            agent["id"],
            state["agents"][agent["id"]],
            event_log,
            timeout_s=1.5,
            exclude_session_ids=known_session_ids,
        )
        if state.get("display_backend") in GHOSTTY_DISPLAY_BACKENDS:
            display_jobs.append((agent["id"], agent))
        started.append({"agent_id": agent["id"], "provider": agent["provider"], "window": agent["id"]})
    for agent_id, display in _open_worker_displays(
        workspace,
        session_name,
        display_jobs,
        event_log,
        state.get("display_backend", "none"),
    ).items():
        if agent_id in state["agents"]:
            state["agents"][agent_id]["display"] = display
    workspace_state = merge_workspace_team_state(load_runtime_state(workspace), state)
    save_runtime_state(workspace, workspace_state)
    _save_team_runtime_snapshot(workspace, state)
    MessageStore(workspace)
    write_team_state(workspace, spec, state)
    event_log.write("launch.complete", session=session_name, started=started)
    return {
        "ok": True,
        "session_name": session_name,
        "agents": started,
        "permissions": permission_summary,
        "routes": routing_decisions,
        "leader_receiver": leader_receiver,
    }
