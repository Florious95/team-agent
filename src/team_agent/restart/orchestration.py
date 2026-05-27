from __future__ import annotations

import copy
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

from team_agent.events import EventLog
from team_agent.message_store import MessageStore
from team_agent.permissions import resolve_permissions
from team_agent.display.backend import display_backend_has_worker_views, display_backend_opens_before_leader_rebind, resolve_restart_display_backend
from team_agent.display.close import close_team_display_backends
from team_agent.display.rebuild import rebuild_restart_display_after_rebind
from team_agent.restart.selection import select_restart_state
from team_agent.restart.snapshot import save_team_runtime_snapshot
from team_agent.spec import load_spec
from team_agent.state import (
    check_team_owner,
    populate_team_owner_from_env,
    save_runtime_state,
    write_team_state,
)

def restart(workspace: Path, allow_fresh: bool = False, team: str | None = None) -> dict[str, Any]:
    # Lazy-import everything from team_agent.runtime so existing tests that
    # patch runtime.shell_resume_command_for_agent / runtime.run_cmd /
    # runtime.start_coordinator / runtime.get_adapter continue to take effect
    # at call time. Runtime re-exports the provider helpers, so this also
    # routes through the providers module without binding it directly.
    from team_agent.runtime import (
        ResumeUnavailable,
        RuntimeError,
        _attach_profile_resume_root,
        _attach_team_profile_dirs,
        _capture_agent_session,
        _clear_session_capture_fields,
        _close_ghostty_display,
        _compile_team_dir_spec,
        _effective_runtime_config,
        _ensure_agent_start_requirements,
        _handle_startup_prompts_and_verify_window,
        _is_team_doc_dir,
        _open_worker_displays,
        _prepare_resume_state,
        _spec_team_dir,
        _tmux_session_conflict_error,
        _tmux_session_exists,
        _tmux_start_command_for_agent_window,
        _tmux_window_exists,
        ensure_workspace_dirs,
        get_adapter,
        run_cmd,
        shell_command_for_agent,
        shell_resume_command_for_agent,
        start_coordinator,
    )
    state = select_restart_state(workspace, team)
    gate = check_team_owner(state)
    if gate:
        return gate
    spec_path = Path(state.get("spec_path", workspace / "team.spec.yaml"))
    team_dir = Path(str(state.get("team_dir"))) if state.get("team_dir") else _spec_team_dir(spec_path, workspace)
    if _is_team_doc_dir(team_dir):
        compiled = _compile_team_dir_spec(team_dir, workspace)
        spec = compiled["spec"]
        spec_path = team_dir / "team.spec.yaml"
        state["spec_path"] = str(spec_path)
    else:
        if not spec_path.exists():
            raise RuntimeError(f"missing spec for restart: {spec_path}")
        spec = load_spec(spec_path)
    _attach_team_profile_dirs(spec, spec_path, workspace, team_dir)
    ensure_workspace_dirs(workspace)
    event_log = EventLog(workspace)
    session_name = state.get("session_name") or spec.get("runtime", {}).get("session_name") or f"team-{spec['team']['name']}"
    state.setdefault("team_dir", str(team_dir))
    if _tmux_session_exists(session_name):
        event_log.write(
            "restart.session_conflict",
            session=session_name,
            action="use a different team name or runtime.session_name; do not terminate existing tmux sessions from restart",
        )
        raise RuntimeError(_tmux_session_conflict_error(session_name))
    runtime_cfg = _effective_runtime_config(spec.get("runtime", {}))
    display_backend = resolve_restart_display_backend(spec, state, event_log)
    # Stage 7 S5 — Slice 6 lifecycle atomicity contract: compute restart_agents
    # early so we can pre-validate resumability BEFORE any destructive teardown
    # (ghostty close, tmux session creation). Without --allow-fresh, every
    # non-paused worker MUST be resumable; if any is not, refuse the operation
    # atomically with a structured result and a restart.atomic_refusal event.
    # No rollback path is needed because nothing has been created yet.
    restart_agents = [
        agent
        for agent in spec.get("agents", [])
        if state.get("agents", {}).get(agent["id"], {}).get("status") != "paused" and not agent.get("paused")
    ]
    # cr strict-typing (2026-05-27): refuse the operation deterministically
    # before any decision logic if any persisted first_send_at is corrupt
    # (empty string, 0, False, literal "null", any non-ISO garbage). This
    # avoids silent misclassification through Python truthiness and gives the
    # operator a clear audit signal that state.json is damaged.
    invalid_first_send_at = _collect_corrupt_first_send_at(restart_agents, state)
    if invalid_first_send_at:
        for entry in invalid_first_send_at:
            event_log.write(
                "restart.first_send_at_invalid",
                worker_id=entry["worker_id"],
                raw_first_send_at=entry["raw_first_send_at"],
                raw_first_send_at_type=entry["raw_first_send_at_type"],
            )
        invalid_names = [entry["worker_id"] for entry in invalid_first_send_at]
        return {
            "ok": False,
            "status": "refused",
            "reason": "invalid_first_send_at",
            "invalid_first_send_at": invalid_first_send_at,
            "allow_fresh": bool(allow_fresh),
            "error": (
                f"Cannot restart: workers {invalid_names} have a corrupt "
                "first_send_at in state.json (only null/missing or a valid "
                "ISO-8601 UTC timestamp string is accepted). Inspect the "
                "restart.first_send_at_invalid audit events for raw values "
                "and repair state.json before retrying."
            ),
        }
    # cr C2: emit one restart.resume_decision event per non-paused worker so
    # every restart attempt produces an auditable per-worker classification.
    # The function returns only refused workers — populated when
    # allow_fresh=False AND at least one interacted worker cannot be repaired.
    refused = _emit_resume_decisions(
        workspace, restart_agents, state, get_adapter, event_log, allow_fresh,
    )
    if refused:
        event_log.write(
            "restart.atomic_refusal",
            unresumable=refused,
            allow_fresh=bool(allow_fresh),
            reason="resume_atomicity",
        )
        return {
            "ok": False,
            "status": "refused",
            "reason": "resume_atomicity",
            "unresumable": refused,
            "allow_fresh": bool(allow_fresh),
            "error": _format_atomic_refusal_error(refused),
        }
    close_team_display_backends(state, event_log)
    for agent_id, agent_state in state.get("agents", {}).items():
        _close_ghostty_display(agent_id, agent_state, event_log)
    state["display_backend"] = display_backend
    _ensure_agent_start_requirements(workspace, restart_agents, event_log, "restart")
    first = True
    restarted: list[dict[str, Any]] = []
    new_agents: dict[str, Any] = {}
    display_jobs: list[tuple[str, dict[str, Any]]] = []
    for agent in spec.get("agents", []):
        previous = state.get("agents", {}).get(agent["id"], {})
        if previous.get("status") == "paused" or agent.get("paused"):
            new_agents[agent["id"]] = dict(previous or {"status": "paused", "provider": agent["provider"]})
            new_agents[agent["id"]]["status"] = "paused"
            continue
        adapter = get_adapter(agent["provider"])
        if not adapter.is_installed():
            event_log.write(
                "restart.provider_missing",
                agent_id=agent["id"],
                provider=agent["provider"],
                command=adapter.command_name,
            )
            raise RuntimeError(
                f"Provider {agent['provider']} command {adapter.command_name!r} not found for agent {agent['id']}"
            )
        mcp_config = adapter.mcp_config(workspace, agent["id"])
        mcp_path = adapter.install_mcp(workspace, agent["id"], mcp_config)
        command_agent = copy.deepcopy(agent)
        command_agent["_runtime"] = runtime_cfg
        previous = _attach_profile_resume_root(workspace, command_agent, previous)
        known_session_ids = {
            str(item.get("session_id"))
            for aid, item in {**state.get("agents", {}), **new_agents}.items()
            if aid != agent["id"] and item.get("session_id")
        }
        try:
            previous = _prepare_resume_state(
                workspace,
                agent["id"],
                previous,
                adapter,
                event_log,
                known_session_ids,
                allow_fresh_on_resume_failure=allow_fresh,
            )
        except ResumeUnavailable as exc:
            try:
                adapter.cleanup_mcp(workspace, agent["id"], mcp_path)
            except Exception as cleanup_exc:
                event_log.write(
                    "restart.mcp_cleanup_failed",
                    agent_id=agent["id"],
                    provider=agent["provider"],
                    mcp_config=str(mcp_path),
                    error=str(cleanup_exc),
                )
            raise RuntimeError(str(exc)) from exc
        restart_mode = "resumed" if previous.get("session_id") else "fresh"
        if restart_mode == "resumed":
            try:
                command = shell_resume_command_for_agent(command_agent, previous, workspace, mcp_config)
            except ResumeUnavailable as exc:
                event_log.write("restart.resume_unavailable", agent_id=agent["id"], error=str(exc))
                if not allow_fresh:
                    try:
                        adapter.cleanup_mcp(workspace, agent["id"], mcp_path)
                    except Exception as cleanup_exc:
                        event_log.write(
                            "restart.mcp_cleanup_failed",
                            agent_id=agent["id"],
                            provider=agent["provider"],
                            mcp_config=str(mcp_path),
                            error=str(cleanup_exc),
                        )
                    raise RuntimeError(
                        f"Cannot resume agent {agent['id']}: {exc}. "
                        "Use team-agent restart --allow-fresh only if losing that worker context is acceptable."
                    ) from exc
                command = shell_command_for_agent(command_agent, workspace, mcp_config)
                restart_mode = "fresh"
        else:
            command = shell_command_for_agent(command_agent, workspace, mcp_config)
            event_log.write("restart.fresh_spawn", agent_id=agent["id"], provider=agent["provider"], reason="session_id_missing")
        event_log.write(
            "restart.agent_start",
            agent_id=agent["id"],
            provider=agent["provider"],
            restart_mode=restart_mode,
            session_id=previous.get("session_id"),
            session=session_name,
            window=agent["id"],
            tmux_start_mode="new-session" if first else "new-window",
            command=command,
            mcp_config=str(mcp_path),
        )
        if first:
            proc = run_cmd(["tmux", "new-session", "-d", "-s", session_name, "-n", agent["id"], "sh", "-lc", command])
            first = False
        else:
            proc = run_cmd(["tmux", "new-window", "-t", session_name, "-n", agent["id"], "sh", "-lc", command])
        if proc.returncode != 0:
            raise RuntimeError(f"Failed to restart agent {agent['id']}: {proc.stderr.strip()}")
        if not _handle_startup_prompts_and_verify_window(
            adapter, event_log, "restart", agent["id"], agent["provider"], session_name, restart_mode
        ):
            if restart_mode != "resumed":
                raise RuntimeError(f"Failed to restart agent {agent['id']}: tmux window exited after start")
            if not allow_fresh:
                try:
                    adapter.cleanup_mcp(workspace, agent["id"], mcp_path)
                except Exception as cleanup_exc:
                    event_log.write(
                        "restart.mcp_cleanup_failed",
                        agent_id=agent["id"],
                        provider=agent["provider"],
                        mcp_config=str(mcp_path),
                        error=str(cleanup_exc),
                    )
                raise RuntimeError(
                    f"Cannot resume agent {agent['id']}: resume window exited or did not become visible. "
                    "Use team-agent restart --allow-fresh only if losing that worker context is acceptable."
                )
            event_log.write(
                "restart.resume_window_missing_fallback_fresh",
                agent_id=agent["id"],
                provider=agent["provider"],
                session_id=previous.get("session_id"),
            )
            command = shell_command_for_agent(command_agent, workspace, mcp_config)
            restart_mode = "fresh"
            tmux_cmd, tmux_start_mode = _tmux_start_command_for_agent_window(session_name, agent["id"], command)
            event_log.write(
                "restart.agent_start",
                agent_id=agent["id"],
                provider=agent["provider"],
                restart_mode=restart_mode,
                session_id=None,
                session=session_name,
                window=agent["id"],
                tmux_start_mode=tmux_start_mode,
                command=command,
                mcp_config=str(mcp_path),
            )
            proc = run_cmd(tmux_cmd)
            if proc.returncode != 0:
                raise RuntimeError(f"Failed to restart agent {agent['id']} fresh after resume exit: {proc.stderr.strip()}")
            if not _handle_startup_prompts_and_verify_window(
                adapter, event_log, "restart", agent["id"], agent["provider"], session_name, restart_mode
            ):
                raise RuntimeError(f"Failed to restart agent {agent['id']} fresh: tmux window exited after start")
        spawn_time = datetime.now(timezone.utc)
        agent_state = dict(previous)
        agent_state.update(
            {
                "status": "running",
                "provider": agent["provider"],
                "agent_id": agent["id"],
                "model": agent.get("model"),
                "auth_mode": agent.get("auth_mode"),
                "profile": agent.get("profile"),
                "window": agent["id"],
                "mcp_config": str(mcp_path),
                "permissions": resolve_permissions(agent),
                "spawn_cwd": str(workspace),
                "spawned_at": spawn_time.isoformat(),
            }
        )
        profile_launch = command_agent.get("_provider_profile") or {}
        if profile_launch.get("claude_projects_root"):
            agent_state["claude_projects_root"] = profile_launch["claude_projects_root"]
        if restart_mode == "fresh":
            _clear_session_capture_fields(agent_state)
            if command_agent.get("_session_id"):
                agent_state["_pending_session_id"] = command_agent["_session_id"]
            _capture_agent_session(
                workspace,
                agent["id"],
                agent_state,
                event_log,
                timeout_s=1.5,
                exclude_session_ids=known_session_ids,
                raise_on_missed=False,
            )
        if display_backend_has_worker_views(display_backend):
            display_jobs.append((agent["id"], agent))
        new_agents[agent["id"]] = agent_state
        restarted.append(
            {
                "agent_id": agent["id"],
                "restart_mode": restart_mode,
                "session_id": agent_state.get("session_id"),
                "display_target": None,
            }
        )
    display_results = _open_worker_displays(workspace, session_name, display_jobs, event_log, display_backend) if display_backend_opens_before_leader_rebind(display_backend) else {}
    for agent_id, display in display_results.items():
        if agent_id in new_agents:
            new_agents[agent_id]["display"] = display
    for item in restarted:
        agent_id = item["agent_id"]
        if agent_id in display_results:
            item["display_target"] = display_results[agent_id]
    missing_after_start = [item["agent_id"] for item in restarted if not _tmux_window_exists(session_name, item["agent_id"])]
    if missing_after_start:
        for agent_id in missing_after_start:
            event_log.write("restart.agent_missing_after_start", agent_id=agent_id, target=f"{session_name}:{agent_id}")
        rollback = rollback_restart_session(session_name, event_log)
        raise RuntimeError(
            f"Failed to restart agent {missing_after_start[0]}: tmux window exited after start; "
            f"rollback_session_ok={rollback.get('ok')}"
        )
    state["session_name"] = session_name
    state["agents"] = new_agents
    populate_team_owner_from_env(state, source="restart")
    save_runtime_state(workspace, state)
    save_team_runtime_snapshot(workspace, state)
    MessageStore(workspace)
    write_team_state(workspace, spec, state)
    from team_agent.leader import autobind_leader_receiver_from_env
    leader_provider = str(spec.get("leader", {}).get("provider") or "codex")
    rebound_receiver = autobind_leader_receiver_from_env(workspace, leader_provider, source="restart")
    rebuild_restart_display_after_rebind(display_backend, workspace, session_name, spec, event_log, restarted, receiver=rebound_receiver)
    coordinator = start_coordinator(workspace)
    event_log.write("restart.complete", session=session_name, agents=restarted, coordinator=coordinator)
    return {"ok": True, "session_name": session_name, "agents": restarted, "coordinator": coordinator}


_FIRST_SEND_AT_ABSENT = "absent"
_FIRST_SEND_AT_VALID = "valid"
_FIRST_SEND_AT_CORRUPT = "corrupt"


def _classify_first_send_at(value: Any) -> str:
    """Strict first_send_at typing (cr verdict, 2026-05-27).

    Returns one of:
      "absent"  — None or missing field (worker never-interacted).
      "valid"   — non-empty ISO-8601 UTC string parseable by datetime.fromisoformat.
      "corrupt" — anything else: empty string, 0, False, literal "null", garbage.

    The contract requires that corrupt values be detected deterministically
    before any restart decision so we never silent-misclassify a worker's
    interaction state via Python truthiness.
    """
    if value is None:
        return _FIRST_SEND_AT_ABSENT
    if not isinstance(value, str):
        return _FIRST_SEND_AT_CORRUPT
    if not value:
        return _FIRST_SEND_AT_CORRUPT
    try:
        datetime.fromisoformat(value)
    except (ValueError, TypeError):
        return _FIRST_SEND_AT_CORRUPT
    return _FIRST_SEND_AT_VALID


def _collect_corrupt_first_send_at(
    restart_agents: list[dict[str, Any]],
    state: dict[str, Any],
) -> list[dict[str, Any]]:
    """Walk every non-paused worker and flag any whose persisted first_send_at
    is corrupt. Returns the list of invalid records ready for the
    `restart.first_send_at_invalid` event and the refusal envelope."""
    invalid: list[dict[str, Any]] = []
    for agent in restart_agents:
        agent_id = agent["id"]
        previous = state.get("agents", {}).get(agent_id, {})
        raw = previous.get("first_send_at") if isinstance(previous, dict) else None
        if _classify_first_send_at(raw) != _FIRST_SEND_AT_CORRUPT:
            continue
        invalid.append({
            "worker_id": agent_id,
            "raw_first_send_at": raw,
            "raw_first_send_at_type": type(raw).__name__,
        })
    return invalid


def _emit_resume_decisions(
    workspace: Path,
    restart_agents: list[dict[str, Any]],
    state: dict[str, Any],
    get_adapter_fn: Any,
    event_log: EventLog,
    allow_fresh: bool,
) -> list[dict[str, Any]]:
    """Route B audit-events contract (cr C2, 2026-05-27). For every non-paused
    worker considered by restart, derive the resume decision per the Route B
    matrix and emit ONE `restart.resume_decision` event:

      resumable AND ...                     -> decision = "resume"
      not resumable AND not interacted      -> decision = "fresh_start"
      not resumable AND interacted AND fresh -> decision = "fresh_start"
      not resumable AND interacted AND not fresh -> decision = "refuse"

    Resumability mirrors sessions.resume.prepare_resume_state's repair chain
    so workers the runtime would legitimately repair are NOT flagged. Returns
    the subset of refused workers — populated only when allow_fresh=False AND
    some interacted worker cannot be repaired — for use by atomic_refusal.
    """
    from team_agent.sessions.resume import recover_resume_session_from_events
    refused: list[dict[str, Any]] = []
    for agent in restart_agents:
        agent_id = agent["id"]
        previous = state.get("agents", {}).get(agent_id, {})
        session_id = previous.get("session_id")
        first_send_at = previous.get("first_send_at")
        has_first_send_at = _classify_first_send_at(first_send_at) == _FIRST_SEND_AT_VALID
        has_session_id = bool(session_id)
        adapter = get_adapter_fn(agent["provider"])
        resumable = bool(session_id) and adapter.session_is_resumable(previous, workspace)
        if not resumable:
            known_session_ids = {
                str(item.get("session_id"))
                for aid, item in state.get("agents", {}).items()
                if aid != agent_id and item.get("session_id")
            }
            repaired = recover_resume_session_from_events(
                workspace, agent_id, previous, adapter, known_session_ids,
            )
            if not repaired:
                repaired = adapter.recover_session_id(
                    agent_id, previous, workspace, known_session_ids,
                )
            resumable = bool(repaired)
        if resumable:
            decision = "resume"
        elif not has_first_send_at:
            decision = "fresh_start"
        elif allow_fresh:
            decision = "fresh_start"
        else:
            decision = "refuse"
        event_log.write(
            "restart.resume_decision",
            worker_id=agent_id,
            has_first_send_at=has_first_send_at,
            has_session_id=has_session_id,
            allow_fresh=bool(allow_fresh),
            decision=decision,
            first_send_at=first_send_at if has_first_send_at else None,
            session_id=session_id,
        )
        if decision == "refuse":
            refused.append({
                "agent_id": agent_id,
                "reason": "no_persisted_session_id" if not session_id else "session_unresumable",
                "session_id": session_id,
                "first_send_at": first_send_at,
            })
    return refused


def _format_atomic_refusal_error(refused: list[dict[str, Any]]) -> str:
    """C4 (cr verdict, 2026-05-27): the human-readable refusal error must
    name every refused worker AND its first_send_at timestamp so an operator
    can decide whether to pass --allow-fresh and accept losing that
    interaction history."""
    names = [item["agent_id"] for item in refused]
    details = ". ".join(
        f"{item['agent_id']} was first interacted with at {item.get('first_send_at')}; "
        "its persisted session is missing"
        for item in refused
    )
    return (
        f"Cannot restart: workers {names} have no resumable session despite "
        f"previous interaction. {details}. "
        "Pass --allow-fresh if you accept losing that interaction history."
    )


def rollback_restart_session(session_name: str, event_log: EventLog) -> dict[str, Any]:
    from team_agent.runtime import run_cmd
    proc = run_cmd(["tmux", "kill-session", "-t", session_name], timeout=10)
    result = {
        "ok": proc.returncode == 0,
        "session": session_name,
        "stdout": proc.stdout.strip(),
        "stderr": proc.stderr.strip(),
    }
    event_log.write("restart.rollback_session", **result)
    return result
