from __future__ import annotations

import json
import hashlib
import os
import re
import signal
import shlex
import shutil
import subprocess
import sys
import time
import copy
import fcntl
from concurrent.futures import ThreadPoolExecutor, as_completed
from contextlib import contextmanager
from datetime import datetime, timedelta, timezone
from pathlib import Path
from typing import Any

from team_agent.errors import RuntimeError, ValidationError
from team_agent.events import EventLog
from team_agent.message_store import MessageStore
from team_agent.paths import artifacts_dir, logs_dir, messages_dir, runtime_dir, team_workspace
from team_agent.permissions import missing_tools, resolve_permissions
from team_agent.profiles import (
    compact_profile_check,
    effective_model,
    prepare_agent_profile_launch,
    smoke_check_agent_profile,
    validate_agent_profile,
)
from team_agent.rust_core import core_binary, list_targets as core_list_targets, redact_text, render_message as core_render_message
from team_agent.providers import (
    ResumeUnavailable,
    get_adapter,
    shell_command_for_agent,
    shell_fork_command_for_agent,
    shell_resume_command_for_agent,
)
from team_agent.display import (
    GHOSTTY_DISPLAY_BACKENDS,
    GHOSTTY_WORKSPACE_PANES_PER_WINDOW,
    close_ghostty_display as _close_ghostty_display,
    close_ghostty_workspace as _close_ghostty_workspace,
    close_ghostty_workspace_slot as _close_ghostty_workspace_slot,
    close_team_display_backends as _close_team_display_backends,
    ghostty_app_exists as _ghostty_app_exists,
    ghostty_attach_args as _ghostty_attach_args,
    ghostty_command as _ghostty_command,
    ghostty_display_session_name as _ghostty_display_session_name,
    ghostty_pids_by_title as _ghostty_pids_by_title,
    ghostty_workspace_aggregator_name as _ghostty_workspace_aggregator_name,
    ghostty_workspace_blocked as _ghostty_workspace_blocked,
    ghostty_workspace_pane_command as _ghostty_workspace_pane_command,
    ghostty_workspace_pane_title as _ghostty_workspace_pane_title,
    ghostty_workspace_partial_update_display as _ghostty_workspace_partial_update_display,
    ghostty_workspace_window_name as _ghostty_workspace_window_name,
    kill_ghostty_workspace_linked_sessions as _kill_ghostty_workspace_linked_sessions,
    open_ghostty_worker_window as _open_ghostty_worker_window,
    open_ghostty_workspace as _open_ghostty_workspace,
    open_ghostty_workspace_agent_display as _open_ghostty_workspace_agent_display,
    open_worker_displays as _open_worker_displays,
    prepare_ghostty_display_session as _prepare_ghostty_display_session,
    prepare_ghostty_workspace_aggregator as _prepare_ghostty_workspace_aggregator,
    prepare_ghostty_workspace_linked_sessions as _prepare_ghostty_workspace_linked_sessions,
    set_ghostty_workspace_pane_title as _set_ghostty_workspace_pane_title,
)
from team_agent.leader import (
    LEADER_OWNERSHIP_LOCK,
    attach_leader,
    attach_leader_to_state as _attach_leader_to_state,
    claim_leader as _legacy_claim_leader,
    leader_identity,
    leader_session_name as _leader_session_name,
    leader_start_plan,
    start_leader,
)
from team_agent.launch import (
    DANGEROUS_LEADER_FLAGS,
    attach_team_profile_dirs as _attach_team_profile_dirs,
    command_has_flag as _command_has_flag,
    compile_team_dir_spec as _compile_team_dir_spec,
    detect_inherited_dangerous_permissions as _detect_inherited_dangerous_permissions,
    effective_runtime_config as _effective_runtime_config,
    ensure_agent_start_requirements as _ensure_agent_start_requirements,
    init_workspace,
    is_team_doc_dir as _is_team_doc_dir,
    launch,
    process_ancestry as _process_ancestry,
    process_info as _process_info,
    requires_direct_leader_receiver as _requires_direct_leader_receiver,
    spec_team_dir as _spec_team_dir,
    tmux_session_conflict_error as _tmux_session_conflict_error,
    validate_file,
)
from team_agent.approvals import (
    APPROVAL_CHOICE_RE as _APPROVAL_CHOICE_RE,
    INTERNAL_MCP_APPROVAL_CHOICE,
    INTERNAL_MCP_AUTO_APPROVE_TOOLS,
    STARTUP_PROMPT_RUNTIME_CHECK_LIMIT,
    active_approval_choice_index as _active_approval_choice_index,
    active_approval_control_index as _active_approval_control_index,
    age_text as _age_text,
    agent_health_status as _agent_health_status,
    approval_choice_keys as _approval_choice_keys,
    approval_prompt_fingerprint as _approval_prompt_fingerprint,
    capture_has_approval_prompt as _capture_has_approval_prompt,
    capture_has_team_orchestrator_mcp_prompt as _capture_has_team_orchestrator_mcp_prompt,
    choose_internal_mcp_approval_choice as _choose_internal_mcp_approval_choice,
    current_task_for_agent as _current_task_for_agent,
    detect_provider_status as _detect_provider_status,
    extract_approval_choices as _extract_approval_choices,
    extract_approval_prompt as _extract_approval_prompt,
    extract_command_approval_subject as _extract_command_approval_subject,
    handle_internal_mcp_approval_prompt as _handle_internal_mcp_approval_prompt,
    handle_provider_runtime_prompts as _handle_provider_runtime_prompts,
    handle_provider_startup_prompts as _handle_provider_startup_prompts,
    is_approval_control_line as _is_approval_control_line,
    line_is_approval_choice as _line_is_approval_choice,
    refresh_agent_runtime_statuses as _refresh_agent_runtime_statuses,
    submit_internal_mcp_approval as _submit_internal_mcp_approval,
    sync_agent_health as _sync_agent_health,
)
from team_agent.diagnose import (
    compact_model_checks as _compact_model_checks,
    diagnose,
    doctor,
    ensure_profiles_for_roles as _ensure_profiles_for_roles,
    format_model_check_failures as _format_model_check_failures,
    format_profile_check_failures as _format_profile_check_failures,
    format_profile_smoke_failures as _format_profile_smoke_failures,
    model_checks_for_agents as _model_checks_for_agents,
    prepare_quick_start_team as _prepare_quick_start_team,
    preflight,
    preflight_blockers as _preflight_blockers,
    preflight_next_actions as _preflight_next_actions,
    profile_checks_for_agents as _profile_checks_for_agents,
    profile_smoke_checks_for_agents as _profile_smoke_checks_for_agents,
    quick_start as _legacy_quick_start,
    repair_state,
    settle,
    start,
    wait_ready,
)
from team_agent.coordinator import (
    COORDINATOR_PROTOCOL_VERSION,
    coordinator_health,
    coordinator_log_path,
    coordinator_meta_path,
    coordinator_metadata_ok as _coordinator_metadata_ok,
    coordinator_pid_path,
    coordinator_tick,
    message_store_schema_health as _message_store_schema_health,
    pid_is_running as _pid_is_running,
    read_coordinator_metadata as _read_coordinator_metadata,
    start_coordinator,
    stop_coordinator,
    write_coordinator_metadata,
)
from team_agent.restart import (
    format_restart_candidates as _format_restart_candidates,
    quick_start_existing_context as _quick_start_existing_context,
    restart,
    restart_candidate_from_state as _restart_candidate_from_state,
    restart_candidates as _restart_candidates,
    rollback_restart_session as _rollback_restart_session,
    safe_snapshot_name as _safe_snapshot_name,
    save_team_runtime_snapshot as _save_team_runtime_snapshot,
    select_restart_state as _select_restart_state,
    state_has_restart_context as _state_has_restart_context,
    state_team_name as _state_team_name,
    team_runtime_snapshot_dir as _team_runtime_snapshot_dir,
    load_snapshot_state as _load_snapshot_state,
)
from team_agent.routing import route_task
from team_agent.sessions import (
    attach_profile_resume_root as _attach_profile_resume_root,
    capture_agent_session as _capture_agent_session,
    capture_missing_sessions as _capture_missing_sessions,
    clear_session_capture_fields as _clear_session_capture_fields,
    copy_session_metadata as _copy_session_metadata,
    prepare_resume_state as _prepare_resume_state,
    recover_resume_session_from_events as _recover_resume_session_from_events,
    sessions_overview as sessions,
)
from team_agent.status import (
    APPROVAL_SCAN_LINES,
    PEEK_MAX_LINES,
    PEEK_MAX_MATCHES,
    PEEK_SEARCH_SCAN_LINES,
    PENDING_DELIVERY_STATUSES,
    STATUS_EVENT_LIMIT,
    STATUS_TEXT_LIMIT,
    approvals,
    compact_agent_state as _compact_agent_state,
    compact_event as _compact_event,
    compact_mapping as _compact_mapping,
    compact_status as _compact_status,
    compact_task as _compact_task,
    compact_value as _compact_value,
    format_approvals,
    format_inbox,
    format_search_matches as _format_search_matches,
    format_status,
    inbox,
    latest_result_summaries as _latest_result_summaries,
    peek,
    queued_message_statuses as _queued_message_statuses,
    result_summary_from_row as _result_summary_from_row,
    search_lines as _search_lines,
    status,
    validate_line_count as _validate_line_count,
)
from team_agent.simple_yaml import dumps
from team_agent.spec import load_spec, validate_result_envelope, validate_spec, workspace_from_spec
from team_agent.state import (
    SESSION_CAPTURE_FIELDS,
    SESSION_STATE_FIELDS,
    check_team_owner,
    load_runtime_state,
    normalize_agent_session_state,
    populate_team_owner_from_env,
    runtime_state_path,
    save_runtime_state,
    save_team_scoped_state,
    select_runtime_state,
    team_state_key,
    write_spec,
    write_team_state,
)

# Import-time assertions: lifecycle/start.py + messaging/deps.py keep an
# existence check on these runtime symbols. The aliases above pin them so
# accidental rename in team_agent.sessions trips a loud ImportError here.
assert callable(_attach_profile_resume_root)
assert callable(_capture_agent_session)
assert callable(_capture_missing_sessions)
assert callable(_clear_session_capture_fields)
assert callable(_copy_session_metadata)
assert callable(_prepare_resume_state)
assert callable(_recover_resume_session_from_events)
assert callable(sessions)

# Display lane re-exports: lifecycle/start.py + lifecycle/operations.py +
# existing tests assume team_agent.runtime exposes these names. Each alias
# fails at import if team_agent.display drops the symbol, preventing the
# 0a36ad9-style "imports in HEAD, body missing on disk" hazard.
assert callable(_close_ghostty_display)
assert callable(_close_ghostty_workspace)
assert callable(_close_ghostty_workspace_slot)
assert callable(_ghostty_app_exists)
assert callable(_ghostty_attach_args)
assert callable(_ghostty_command)
assert callable(_ghostty_display_session_name)
assert callable(_ghostty_pids_by_title)
assert callable(_ghostty_workspace_aggregator_name)
assert callable(_ghostty_workspace_blocked)
assert callable(_ghostty_workspace_pane_command)
assert callable(_ghostty_workspace_pane_title)
assert callable(_ghostty_workspace_partial_update_display)
assert callable(_ghostty_workspace_window_name)
assert callable(_kill_ghostty_workspace_linked_sessions)
assert callable(_open_ghostty_worker_window)
assert callable(_open_ghostty_workspace)
assert callable(_open_ghostty_workspace_agent_display)
assert callable(_open_worker_displays)
assert callable(_prepare_ghostty_display_session)
assert callable(_prepare_ghostty_workspace_aggregator)
assert callable(_prepare_ghostty_workspace_linked_sessions)
assert callable(_set_ghostty_workspace_pane_title)
assert isinstance(GHOSTTY_WORKSPACE_PANES_PER_WINDOW, int)

# Status lane re-exports: the runtime.* alias for each status helper keeps
# CLI handlers and existing tests stable; constants travel through runtime
# so callers that read runtime.APPROVAL_SCAN_LINES (or the others) still
# resolve. Drift in team_agent.status fails loudly here.
assert callable(approvals)
assert callable(format_approvals)
assert callable(format_inbox)
assert callable(format_status)
assert callable(inbox)
assert callable(peek)
assert callable(status)
assert callable(_compact_agent_state)
assert callable(_compact_event)
assert callable(_compact_mapping)
assert callable(_compact_status)
assert callable(_compact_task)
assert callable(_compact_value)
assert callable(_format_search_matches)
assert callable(_latest_result_summaries)
assert callable(_queued_message_statuses)
assert callable(_result_summary_from_row)
assert callable(_search_lines)
assert callable(_validate_line_count)
assert isinstance(APPROVAL_SCAN_LINES, int)
assert isinstance(PEEK_MAX_LINES, int)
assert isinstance(PEEK_MAX_MATCHES, int)
assert isinstance(PEEK_SEARCH_SCAN_LINES, int)
assert isinstance(STATUS_EVENT_LIMIT, int)
assert isinstance(STATUS_TEXT_LIMIT, int)
assert isinstance(PENDING_DELIVERY_STATUSES, set)

# Restart lane re-exports: lifecycle/agents.py + lifecycle/operations.py
# call runtime._save_team_runtime_snapshot, existing tests target
# runtime.restart, runtime._select_restart_state, runtime._rollback_restart_session,
# runtime._safe_snapshot_name, etc. The aliases above plus these assertions
# catch any rename or removal in team_agent.restart at import time.
assert callable(restart)
assert callable(_rollback_restart_session)
assert callable(_save_team_runtime_snapshot)
assert callable(_restart_candidates)
assert callable(_restart_candidate_from_state)
assert callable(_state_has_restart_context)
assert callable(_select_restart_state)
assert callable(_format_restart_candidates)
assert callable(_quick_start_existing_context)
assert callable(_safe_snapshot_name)
assert callable(_state_team_name)
assert callable(_team_runtime_snapshot_dir)
assert callable(_load_snapshot_state)

# Coordinator lane re-exports keep runtime.coordinator_health,
# runtime.start_coordinator, runtime.coordinator_pid_path, etc. resolving
# for the daemon entry (team_agent.coordinator.__main__) and the existing
# tests. One representative identity smoke + one lightweight loop catch
# any rename or drop in team_agent.coordinator at import time without the
# over-coupled per-symbol assertIs sweep. Per-helper behavior is verified
# by tests/test_coordinator_boundary.py and the broader suite.
import team_agent.coordinator as _coordinator_pkg
assert start_coordinator is _coordinator_pkg.start_coordinator
for _name in (
    "COORDINATOR_PROTOCOL_VERSION",
    "coordinator_health",
    "coordinator_log_path",
    "coordinator_meta_path",
    "coordinator_metadata_ok",
    "coordinator_pid_path",
    "coordinator_tick",
    "message_store_schema_health",
    "pid_is_running",
    "read_coordinator_metadata",
    "start_coordinator",
    "stop_coordinator",
    "write_coordinator_metadata",
):
    assert hasattr(_coordinator_pkg, _name), f"team_agent.coordinator missing {_name}"
del _coordinator_pkg, _name

# Diagnose lane re-exports keep runtime.diagnose, runtime.doctor,
# runtime.preflight, runtime.start, runtime.quick_start, runtime.wait_ready,
# runtime.settle, runtime.repair_state plus the private helper aliases
# resolving for CLI handlers and existing tests. Same identity-smoke +
# lightweight-loop convention as coordinator.
import team_agent.diagnose as _diagnose_pkg
assert diagnose is _diagnose_pkg.diagnose
for _name in (
    "compact_model_checks",
    "diagnose",
    "doctor",
    "ensure_profiles_for_roles",
    "format_model_check_failures",
    "format_profile_check_failures",
    "format_profile_smoke_failures",
    "model_checks_for_agents",
    "prepare_quick_start_team",
    "preflight",
    "preflight_blockers",
    "preflight_next_actions",
    "profile_checks_for_agents",
    "profile_smoke_checks_for_agents",
    "quick_start",
    "repair_state",
    "settle",
    "start",
    "wait_ready",
):
    assert hasattr(_diagnose_pkg, _name), f"team_agent.diagnose missing {_name}"
del _diagnose_pkg, _name

# Approvals lane re-exports keep runtime.<symbol> resolving for the
# coordinator-tick + status-refresh + prompt-detection helpers that
# messaging/lifecycle/tests look up via the runtime alias surface. Same
# calibrated convention as coordinator and diagnose: one identity smoke
# + one lightweight loop over the full public name set.
import team_agent.approvals as _approvals_pkg
assert _refresh_agent_runtime_statuses is _approvals_pkg.refresh_agent_runtime_statuses
for _name in (
    "APPROVAL_CHOICE_RE",
    "INTERNAL_MCP_APPROVAL_CHOICE",
    "INTERNAL_MCP_AUTO_APPROVE_TOOLS",
    "STARTUP_PROMPT_RUNTIME_CHECK_LIMIT",
    "active_approval_choice_index",
    "active_approval_control_index",
    "age_text",
    "agent_health_status",
    "approval_choice_keys",
    "approval_prompt_fingerprint",
    "capture_has_approval_prompt",
    "capture_has_team_orchestrator_mcp_prompt",
    "choose_internal_mcp_approval_choice",
    "current_task_for_agent",
    "detect_provider_status",
    "extract_approval_choices",
    "extract_approval_prompt",
    "extract_command_approval_subject",
    "handle_internal_mcp_approval_prompt",
    "handle_provider_runtime_prompts",
    "handle_provider_startup_prompts",
    "is_approval_control_line",
    "line_is_approval_choice",
    "refresh_agent_runtime_statuses",
    "submit_internal_mcp_approval",
    "sync_agent_health",
):
    assert hasattr(_approvals_pkg, _name), f"team_agent.approvals missing {_name}"
del _approvals_pkg, _name

# Launch lane re-exports keep runtime.launch, runtime.init_workspace,
# runtime.validate_file plus the private bootstrap/config helpers
# resolving for CLI handlers and tests. Same calibrated convention.
import team_agent.launch as _launch_pkg
assert launch is _launch_pkg.launch
for _name in (
    "DANGEROUS_LEADER_FLAGS",
    "attach_team_profile_dirs",
    "command_has_flag",
    "compile_team_dir_spec",
    "detect_inherited_dangerous_permissions",
    "effective_runtime_config",
    "ensure_agent_start_requirements",
    "init_workspace",
    "is_team_doc_dir",
    "launch",
    "process_ancestry",
    "process_info",
    "requires_direct_leader_receiver",
    "spec_team_dir",
    "tmux_session_conflict_error",
    "validate_file",
):
    assert hasattr(_launch_pkg, _name), f"team_agent.launch missing {_name}"
del _launch_pkg, _name

# Leader lane re-exports keep runtime leader helpers resolving for CLI handlers and tests.
import team_agent.leader as _leader_pkg
assert attach_leader is _leader_pkg.attach_leader
for _name in ("attach_leader", "attach_leader_to_state", "claim_leader", "leader_identity", "leader_session_name", "leader_start_plan", "start_leader"):
    assert hasattr(_leader_pkg, _name), f"team_agent.leader missing {_name}"
del _leader_pkg, _name
from team_agent.task_graph import ready_tasks, update_task_status
from team_agent.task_graph import TASK_STATUSES


TMUX_PANE_FORMAT = (
    "#{pane_id}\t#{session_name}\t#{window_index}\t#{window_name}\t"
    "#{pane_index}\t#{pane_tty}\t#{pane_current_command}\t#{pane_active}\t"
    "#{pane_current_path}\t#{session_attached}\t#{pane_in_mode}"
)
HEALTH_STATUSES = {"RUNNING", "IDLE", "AWAITING_APPROVAL", "BLOCKED", "ERROR", "DONE"}
DELIVERY_CAPTURE_LINES = 40
SUBMITTED_DELIVERY_STATUSES = {"injected", "visible", "submitted", "submitted_unverified", "delivered", "acknowledged"}
TMUX_STDIN_BUFFER_THRESHOLD = 16 * 1024
TMUX_PASTE_MIN_READY_TIMEOUT = 1.5
TMUX_PASTE_MAX_READY_TIMEOUT = 30.0
TMUX_PASTE_BYTES_PER_SECOND = 25_000
TMUX_SUBMIT_MIN_SETTLE_TIMEOUT = 0.35
TMUX_SUBMIT_MAX_SETTLE_TIMEOUT = 15.0
TMUX_SUBMIT_BYTES_PER_SECOND = 50_000
PASTED_CONTENT_PROMPT_RE = re.compile(
    r"\[\s*Pasted\s+(?:Content\s+\d+\s+chars?|text\s+#\d+\s+\+\s*\d+\s+lines?)\s*\]",
    re.IGNORECASE,
)

def run_cmd(args: list[str], timeout: int = 20) -> subprocess.CompletedProcess[str]:
    return subprocess.run(args, text=True, capture_output=True, timeout=timeout, check=False)


def ensure_workspace_dirs(workspace: Path) -> None:
    for path in [runtime_dir(workspace), logs_dir(workspace), artifacts_dir(workspace), messages_dir(workspace)]:
        path.mkdir(parents=True, exist_ok=True)


def shutdown(workspace: Path, keep_logs: bool = True, team: str | None = None) -> dict[str, Any]:
    from team_agent.state import resolve_team_scoped_state
    state, refusal = resolve_team_scoped_state(workspace, team)
    if refusal:
        return refusal
    gate = check_team_owner(state)
    if gate:
        return gate
    session_name = state.get("session_name")
    resolved_team_id = (
        team
        or state.get("active_team_key")
        or (team_state_key(state) if state.get("session_name") else None)
    )
    event_log = EventLog(workspace)
    captured: list[str] = []
    closed_displays: set[str] = set()
    missing_before = [agent_id for agent_id, agent_state in state.get("agents", {}).items() if not agent_state.get("session_id")]
    fallback_captured = _capture_missing_sessions(workspace, state, event_log, timeout_s=2.0, log_miss=False)
    event_log.write("shutdown.session_capture_checked", missing_before=missing_before, captured=fallback_captured)
    for agent_id, agent_state in state.get("agents", {}).items():
        if not agent_state.get("session_id"):
            event_log.write(
                "shutdown.session_capture_missed",
                agent_id=agent_id,
                provider=agent_state.get("provider"),
                spawn_cwd=agent_state.get("spawn_cwd"),
            )
    coordinator = stop_coordinator(workspace)
    if session_name and _tmux_session_exists(session_name):
        leader_receiver = state.get("leader_receiver", {})
        leader_window = leader_receiver.get("window") if leader_receiver.get("mode") != "direct_tmux" else None
        if leader_window and _tmux_window_exists(session_name, leader_window):
            log_path = logs_dir(workspace) / f"{leader_window}.scrollback"
            proc = run_cmd(["tmux", "capture-pane", "-p", "-S", "-", "-t", f"{session_name}:{leader_window}"], timeout=10)
            if proc.returncode == 0:
                log_path.write_text(proc.stdout, encoding="utf-8")
                captured.append(str(log_path))
        for agent_id, agent_state in state.get("agents", {}).items():
            window = agent_state.get("window", agent_id)
            log_path = logs_dir(workspace) / f"{agent_id}.scrollback"
            proc = run_cmd(["tmux", "capture-pane", "-p", "-S", "-", "-t", f"{session_name}:{window}"], timeout=10)
            if proc.returncode == 0:
                log_path.write_text(proc.stdout, encoding="utf-8")
                captured.append(str(log_path))
        display_cleanup = _close_team_display_backends(state, event_log)
        for agent_id, agent_state in state.get("agents", {}).items():
            _close_ghostty_display(agent_id, agent_state, event_log)
            closed_displays.add(agent_id)
        proc = run_cmd(["tmux", "kill-session", "-t", session_name], timeout=10)
        if proc.returncode != 0:
            if "can't find session" in proc.stderr:
                event_log.write("shutdown.idempotent", session=session_name, reason="session disappeared before kill")
            else:
                raise RuntimeError(f"tmux kill-session failed: {proc.stderr.strip()}")
        else:
            event_log.write("shutdown.kill_session", session=session_name, keep_logs=keep_logs, captured=captured)
    else:
        event_log.write("shutdown.idempotent", session=session_name, reason="session missing")
        display_cleanup = _close_team_display_backends(state, event_log)
    for agent_id, agent_state in state.get("agents", {}).items():
        if agent_id not in closed_displays:
            _close_ghostty_display(agent_id, agent_state, event_log)
        mcp_path = Path(agent_state["mcp_config"]) if agent_state.get("mcp_config") else None
        try:
            get_adapter(agent_state["provider"]).cleanup_mcp(workspace, agent_id, mcp_path)
            event_log.write(
                "shutdown.mcp_cleanup",
                agent_id=agent_id,
                provider=agent_state.get("provider"),
                mcp_config=str(mcp_path) if mcp_path else None,
            )
        except Exception as exc:
            event_log.write(
                "shutdown.mcp_cleanup_failed",
                agent_id=agent_id,
                provider=agent_state.get("provider"),
                mcp_config=str(mcp_path) if mcp_path else None,
                error=str(exc),
            )
        if agent_state.get("status") != "paused":
            agent_state["status"] = "stopped"
    save_team_scoped_state(workspace, state)
    _save_team_runtime_snapshot(workspace, state)
    # 0.2.6 Family B (C10/C11/C12): atomically unregister the team and
    # archive its runtime snapshot directory. Both branches of --keep-logs
    # still drop the team from state.teams; logs survive inside the
    # archived directory.
    archive_path, teams_remaining, new_active = _commit_shutdown_cleanup(
        workspace, str(resolved_team_id or ""), session_name, event_log
    )
    result = {
        "ok": True,
        "session_name": session_name,
        "team": resolved_team_id,
        "logs": captured,
        "coordinator": coordinator,
        "archive_path": archive_path,
        "teams_remaining": teams_remaining,
        "new_active_team_key": new_active,
        "cleanup_mode": "synchronous_committed",
    }
    orphans = (display_cleanup or {}).get("orphans_detected") or []
    if orphans:
        result["cleanup_mode"] = "synchronous_with_orphans"
        result["orphans_detected"] = orphans
        result["warning"] = "Adaptive display tmux objects remain after shutdown cleanup."
        event_log.write(
            "shutdown.orphans_detected",
            warning=result["warning"],
            message=result["warning"],
            orphans_detected=orphans,
            adaptive_display_sessions=orphans.get("adaptive_display_sessions", []),
            adaptive_overview_windows=orphans.get("adaptive_overview_windows", []),
        )
    return result


def _commit_shutdown_cleanup(
    workspace: Path,
    team_key: str,
    session_name: str | None,
    event_log: EventLog,
) -> tuple[str | None, list[str], str | None]:
    import shutil as _shutil
    from datetime import datetime as _dt, timezone as _tz
    workspace_state = load_runtime_state(workspace)
    teams = workspace_state.get("teams") if isinstance(workspace_state.get("teams"), dict) else {}
    if team_key and team_key in teams:
        teams.pop(team_key, None)
    if workspace_state.get("active_team_key") == team_key:
        workspace_state["active_team_key"] = None
    workspace_state["teams"] = teams
    archive_dest: Path | None = None
    if session_name:
        runtime_teams_dir = runtime_dir(workspace) / "teams"
        from team_agent.restart.snapshot import safe_snapshot_name as _safe
        snapshot_name = _safe(str(session_name))
        snapshot_dir = runtime_teams_dir / snapshot_name
        if snapshot_dir.exists():
            ts = _dt.now(_tz.utc).strftime("%Y%m%dT%H%M%SZ")
            archive_dest = runtime_teams_dir / f".archived-{snapshot_name}-{ts}"
            try:
                _shutil.move(str(snapshot_dir), str(archive_dest))
            except OSError as exc:
                event_log.write(
                    "team.shutdown_blocked",
                    reason="archive_move_failed",
                    team_key=team_key,
                    error=str(exc),
                    hint="check filesystem permissions on .team/runtime/teams and rerun shutdown",
                )
                return None, sorted(teams.keys()), workspace_state.get("active_team_key")
    save_runtime_state(workspace, workspace_state)
    archive_path_str = str(archive_dest) if archive_dest is not None else None
    new_active = workspace_state.get("active_team_key")
    event_log.write(
        "team.shutdown_completed",
        team_key=team_key,
        archive_path=archive_path_str,
        teams_remaining=sorted(teams.keys()),
        new_active_team_key=new_active,
    )
    return archive_path_str, sorted(teams.keys()), new_active



def remove_agent(
    workspace: Path,
    agent_id: str,
    *,
    from_spec: bool = False,
    confirm: bool = False,
    force: bool = False,
    team: str | None = None,
) -> dict[str, Any]:
    from team_agent.lifecycle.agents import remove_agent as lifecycle_remove_agent

    with _runtime_lock(workspace, "remove-agent"):
        return lifecycle_remove_agent(workspace, agent_id, from_spec=from_spec, confirm=confirm, force=force, team=team)


def acknowledge_idle(workspace: Path, agent_id: str | None = None, *, team: str | None = None) -> dict[str, Any]:
    with _runtime_lock(workspace, "acknowledge-idle"):
        try:
            state = select_runtime_state(workspace, team)
        except Exception as exc:
            return {"ok": False, "status": "refused", "reason": "team_target_unresolved", "team": team, "error": str(exc)}
        gate = check_team_owner(state)
        if gate:
            return gate
        now_dt = datetime.now(timezone.utc); now = now_dt.isoformat()
        ttl_seconds = 1800
        expires_at = (now_dt + timedelta(seconds=ttl_seconds)).isoformat()
        owner_team_id = team_state_key(state); coordinator = state.setdefault("coordinator", {})
        coordinator.setdefault("idle_acknowledged", {})[owner_team_id] = {"acknowledged_at": now, "expires_at": expires_at, "ttl_seconds": ttl_seconds}
        team_suppressions = coordinator.setdefault("suppressed_idle_alerts", {}).setdefault(owner_team_id, {})
        entry = {"suppressed_at": now, "suppressed_by": "manual_acknowledge", "manual_acknowledge": True, "expires_at": expires_at, "ttl_seconds": ttl_seconds}
        for worker_id in state.get("agents", {}):
            team_suppressions.setdefault(worker_id, {})["idle_fallback"] = dict(entry)
        save_team_scoped_state(workspace, state)
        EventLog(workspace).write("coordinator.idle_acknowledged", agent_id=agent_id, team=owner_team_id, acknowledged_at=now, expires_at=expires_at, ttl_seconds=ttl_seconds)
        return {"ok": True, "team": owner_team_id, "agent_id": agent_id, "acknowledged_at": now, "expires_at": expires_at, "ttl_seconds": ttl_seconds}

_OWNER_IDENTITY_FIELDS = (
    "pane_id",
    "leader_session_uuid",
    "machine_fingerprint",
    "provider",
    "os_user",
)


def _owner_identity_matches(existing: dict[str, Any], candidate: dict[str, Any]) -> bool:
    for field in _OWNER_IDENTITY_FIELDS:
        if str(existing.get(field) or "") != str(candidate.get(field) or ""):
            return False
    return True


def _resolve_owner_team_id(state: dict[str, Any], team: str | None) -> str | None:
    if team:
        return str(team)
    active = state.get("active_team_key")
    if active:
        return str(active)
    teams = state.get("teams") or {}
    if isinstance(teams, dict) and len(teams) == 1:
        return next(iter(teams))
    return None


def takeover(workspace: Path, team: str | None = None, confirm: bool = False) -> dict[str, Any]:
    """0.2.6 Family A: positive-source ownership rebind.

    Identity is sourced exclusively from ``bind_owner_from_caller_pane``
    (``$TMUX_PANE`` + one targeted ``tmux display-message``). The new
    owner record force-writes every identity field into
    ``state.teams[<team_id>].team_owner``; old fields are not merged,
    migrated, or setdefaulted. Idempotent: re-running with the same
    caller identity returns success without mutating state.
    """
    if not confirm:
        return {
            "ok": False,
            "status": "refused",
            "reason": "confirm_required",
            "action": "rerun with --confirm to claim ownership of this team",
        }
    from team_agent.leader_binding import (
        bind_owner_from_caller_pane,
        emit_owner_bound_event,
    )
    with _runtime_lock(workspace, LEADER_OWNERSHIP_LOCK):
        state = load_runtime_state(workspace)
        team_id = _resolve_owner_team_id(state, team)
        if not team_id:
            return {
                "ok": False,
                "status": "refused",
                "reason": "team_target_unresolved",
                "team": team,
                "hint": "pass --team <name> or run quick-start first to register an active team.",
            }
        bind = bind_owner_from_caller_pane(workspace, team_id)
        if not bind.get("ok"):
            return {"ok": False, "status": "refused", **bind}
        new_owner = bind["owner"]
        teams = state.setdefault("teams", {})
        team_entry = teams.get(team_id) or {}
        existing_owner = team_entry.get("team_owner") if isinstance(team_entry.get("team_owner"), dict) else {}
        if existing_owner and _owner_identity_matches(existing_owner, new_owner):
            return {
                "ok": True,
                "status": "claimed",
                "team": team_id,
                "team_owner": existing_owner,
                "idempotent": True,
            }
        team_entry["team_owner"] = new_owner
        teams[team_id] = team_entry
        if team_state_key(state) == team_id:
            state["team_owner"] = new_owner
        from team_agent.leader import _write_lease_dual_state
        _write_lease_dual_state(workspace, state)
        emit_owner_bound_event(
            workspace,
            caller_pane_id=bind.get("caller_pane_id", ""),
            caller_current_command=bind.get("caller_current_command", ""),
            derived_leader_session_uuid=new_owner["leader_session_uuid"],
            team_id=team_id,
            old_leader_session_uuid=str(existing_owner.get("leader_session_uuid") or ""),
        )
        return {
            "ok": True,
            "status": "claimed",
            "team": team_id,
            "team_owner": new_owner,
            "previous_owner": existing_owner or None,
        }


def claim_leader(workspace: Path, team: str | None = None, confirm: bool = False) -> dict[str, Any]:
    """0.2.6 Family A: positive-source claim-leader.

    Calls :func:`bind_owner_from_caller_pane` to confirm the caller is in
    a leader-shaped tmux pane, then delegates to the legacy multi-
    candidate lease arbiter for residual handling. The bind step is the
    only source of caller identity; the legacy lease path no longer
    re-derives it.
    """
    from team_agent.leader_binding import bind_owner_from_caller_pane
    state = load_runtime_state(workspace)
    team_id = _resolve_owner_team_id(state, team) or team_state_key(state)
    bind = bind_owner_from_caller_pane(workspace, team_id)
    if not bind.get("ok"):
        return {"ok": False, "status": "refused", **bind}
    return _legacy_claim_leader(workspace, team=team, confirm=confirm)


def quick_start(
    agents_dir: Path,
    name: str | None = None,
    yes: bool = False,
    fresh: bool = False,
    team_id: str | None = None,
) -> dict[str, Any]:
    """0.2.6 Family A: positive-source quick-start.

    The caller-pane shape gate is owned by
    :func:`bind_owner_from_caller_pane`. Quick-start binds the caller
    pane BEFORE any team setup runs; ``$TMUX_PANE`` missing or the
    caller pane not running a leader host short-circuits to a refusal
    (no fallback to legacy reverse-scan). On success, the legacy
    bootstrap brings up the workspace and the bind-derived
    ``team_owner`` is force-written into
    ``state.teams[team_id].team_owner`` so the runtime owner identity
    matches the caller pane verbatim.
    """
    from team_agent.leader_binding import (
        bind_owner_from_caller_pane,
        emit_owner_bound_event,
    )
    from team_agent.diagnose.quick_start import prepare_quick_start_team

    # Pre-resolve team_dir + workspace so the caller-pane bind can write
    # its audit event before any worker is spawned. ``prepare_quick_start_team``
    # is idempotent (mkdir + shutil.copy2 of role docs) and used inside
    # ``_legacy_quick_start`` as the very first step anyway.
    team_dir = prepare_quick_start_team(
        Path(agents_dir).resolve(), Path.cwd().resolve(), name, team_id=team_id
    )
    workspace = team_workspace(team_dir)
    ensure_workspace_dirs(workspace)
    # Spark MED 1 (b1b17b1 review): the on-disk team_dir already passed
    # through ``_safe_snapshot_name``; reusing ``team_dir.name`` here
    # keeps the on-disk path and the state.teams key aligned. Using the
    # raw ``team_id`` would have split the two writes whenever the
    # caller-supplied id contained spaces or shell-unsafe characters.
    resolved_team_id = team_dir.name or "current"
    bind = bind_owner_from_caller_pane(workspace, resolved_team_id)
    if not bind.get("ok"):
        return {"ok": False, "status": "refused", **bind}
    new_owner = bind["owner"]
    result = _legacy_quick_start(
        Path(agents_dir).resolve(), name=name, yes=yes, fresh=fresh, team_id=team_id
    )
    # Spark MED 2 (b1b17b1 review): only commit the owner force-write
    # and emit ``owner.bound_from_caller_pane`` when the legacy bootstrap
    # actually succeeded. Otherwise pass the refusal envelope back
    # verbatim — ``existing_runtime_state`` / ``preflight`` failures
    # must not leave a "team owner claimed" side effect behind.
    if not result.get("ok"):
        return result
    state = load_runtime_state(workspace)
    teams = state.setdefault("teams", {})
    team_entry = teams.get(resolved_team_id) or {}
    existing_owner = (
        team_entry.get("team_owner")
        if isinstance(team_entry.get("team_owner"), dict)
        else {}
    )
    if not (existing_owner and _owner_identity_matches(existing_owner, new_owner)):
        team_entry["team_owner"] = new_owner
        teams[resolved_team_id] = team_entry
        if not state.get("active_team_key"):
            state["active_team_key"] = resolved_team_id
        from team_agent.leader import _write_lease_dual_state
        _write_lease_dual_state(workspace, state)
        emit_owner_bound_event(
            workspace,
            caller_pane_id=bind.get("caller_pane_id", ""),
            caller_current_command=bind.get("caller_current_command", ""),
            derived_leader_session_uuid=new_owner["leader_session_uuid"],
            team_id=resolved_team_id,
            old_leader_session_uuid=str(existing_owner.get("leader_session_uuid") or ""),
        )
    return result


def _running_agent_state(workspace: Path, agent: dict[str, Any], previous: dict[str, Any]) -> dict[str, Any]:
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
            "permissions": resolve_permissions(agent),
            "spawn_cwd": str(workspace),
        }
    )
    return agent_state


def _handle_startup_prompts_and_verify_window(
    adapter: Any,
    event_log: EventLog,
    event_prefix: str,
    agent_id: str,
    provider: str,
    session_name: str,
    start_mode: str,
) -> bool:
    handled_prompts = adapter.handle_startup_prompts(session_name, agent_id, checks=20, sleep_s=0.5)
    for prompt_event in handled_prompts:
        event_log.write(f"{event_prefix}.startup_prompt_handled", agent_id=agent_id, provider=provider, **prompt_event)
    deadline = time.monotonic() + 1.0
    saw_window = False
    while True:
        if _tmux_window_exists(session_name, agent_id):
            saw_window = True
            if time.monotonic() >= deadline:
                return True
        elif saw_window or time.monotonic() >= deadline:
            break
        time.sleep(0.2)
    event_log.write(
        f"{event_prefix}.window_missing_after_start",
        agent_id=agent_id,
        provider=provider,
        start_mode=start_mode,
        target=f"{session_name}:{agent_id}",
        saw_window=saw_window,
    )
    return False




def shutil_which(command: str) -> str | None:
    from shutil import which

    return which(command)


@contextmanager
def _runtime_lock(workspace: Path, name: str, timeout: float = 5.0):
    lock_path = runtime_dir(workspace) / f"{name}.lock"
    lock_path.parent.mkdir(parents=True, exist_ok=True)
    event_log = EventLog(workspace)
    start = time.monotonic()
    with lock_path.open("w", encoding="utf-8") as lock_file:
        while True:
            try:
                fcntl.flock(lock_file.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
                waited = time.monotonic() - start
                event_log.write("runtime.lock_acquired", lock=name, waited_sec=round(waited, 3))
                break
            except BlockingIOError:
                if time.monotonic() - start >= timeout:
                    event_log.write("runtime.lock_busy", lock=name, timeout_sec=timeout)
                    raise RuntimeError(
                        f"{name} is locked by another team-agent process; serialize team-agent {name} calls and retry"
                    )
                time.sleep(0.05)
        try:
            yield
        finally:
            fcntl.flock(lock_file.fileno(), fcntl.LOCK_UN)
            event_log.write("runtime.lock_released", lock=name)


def _leader_id(state: dict[str, Any], spec: dict[str, Any]) -> str:
    return state.get("leader", {}).get("id") or spec.get("leader", {}).get("id") or "leader"


def _is_leader_sender(sender: str, leader_id: str) -> bool:
    return sender in {leader_id, "leader", "Leader"}


def _is_leader_target(target: str | None, leader_id: str) -> bool:
    return target in {leader_id, "leader", "Leader"}


def _spec_agent_ids(spec: dict[str, Any]) -> list[str]:
    return [str(agent["id"]) for agent in spec.get("agents", [])]


def _runtime_team_agent_ids(state: dict[str, Any], spec: dict[str, Any]) -> list[str]:
    runtime_agents = state.get("agents", {})
    return [agent_id for agent_id in _spec_agent_ids(spec) if agent_id in runtime_agents]


def _is_runtime_team_agent(agent_id: str, state: dict[str, Any], spec: dict[str, Any]) -> bool:
    return agent_id in set(_runtime_team_agent_ids(state, spec))


def get_adapter_or_raise(name: str) -> str:
    if name == "tmux" and not shutil_which("tmux"):
        raise RuntimeError("tmux is not installed; install tmux 3.3+ before launch")
    return name


def _tmux_session_exists(session_name: str | None) -> bool:
    if not session_name:
        return False
    proc = run_cmd(["tmux", "has-session", "-t", session_name], timeout=5)
    return proc.returncode == 0


def _tmux_start_command_for_agent_window(session_name: str, window_name: str, command: str) -> tuple[list[str], str]:
    if _tmux_session_exists(session_name):
        return ["tmux", "new-window", "-t", session_name, "-n", window_name, "sh", "-lc", command], "new-window"
    return ["tmux", "new-session", "-d", "-s", session_name, "-n", window_name, "sh", "-lc", command], "new-session"


def _tmux_window_exists(session_name: str | None, window: str | None) -> bool:
    if not session_name or not window:
        return False
    proc = run_cmd(["tmux", "list-windows", "-t", session_name, "-F", "#{window_name}"], timeout=5)
    if proc.returncode != 0:
        return False
    return window in proc.stdout.splitlines()


def _find_task(tasks: list[dict[str, Any]], task_id: str) -> dict[str, Any]:
    for task in tasks:
        if task.get("id") == task_id:
            return task
    raise RuntimeError(f"unknown task id: {task_id}")


def _find_task_or_none(tasks: list[dict[str, Any]], task_id: str) -> dict[str, Any] | None:
    for task in tasks:
        if task.get("id") == task_id:
            return task
    return None


def _is_message_scoped_result(store: MessageStore, envelope: dict[str, Any]) -> bool:
    task_id = str(envelope.get("task_id") or "")
    agent_id = str(envelope.get("agent_id") or "")
    if not task_id.startswith("msg_"):
        return False
    message = _message_by_id(store, task_id)
    return bool(message and message.get("recipient") == agent_id)


def _find_agent(spec: dict[str, Any], agent_id: str | None) -> dict[str, Any] | None:
    if not agent_id:
        return None
    for agent in spec.get("agents", []):
        if agent.get("id") == agent_id:
            return agent
    if spec.get("leader", {}).get("id") == agent_id:
        return spec["leader"]
    return None


def _result_status_to_task_status(task: dict[str, Any], result_status: str) -> str:
    if result_status == "success":
        return "done"
    if result_status == "blocked":
        return "blocked"
    if result_status in {"partial", "failed"}:
        return _retry_or_failed(task)
    raise KeyError(result_status)


def _retry_or_failed(task: dict[str, Any]) -> str:
    retry_count = int(task.get("retry_count") or 0)
    retry_limit = int(task.get("retry_limit") or 0)
    if retry_count < retry_limit:
        task["retry_count"] = retry_count + 1
        return "needs_retry"
    task["retry_count"] = retry_count
    return "failed"


def _deliver_pending_message(workspace: Path, state: dict[str, Any], message_id: str, wait_visible: bool = True, timeout: float = 30.0, *, _trust_retry_attempt: int = 1) -> dict[str, Any]:
    from team_agent.messaging.delivery import _deliver_pending_message as impl

    return impl(workspace, state, message_id, wait_visible, timeout, _trust_retry_attempt=_trust_retry_attempt)

def _enable_codex_fast_mode(session_name: str, window_name: str) -> dict[str, Any]:
    from team_agent.messaging.tmux_prompt import _enable_codex_fast_mode as impl

    return impl(session_name, window_name)

def _leader_command_provider(command: str) -> str | None:
    from team_agent.messaging.leader_panes import _leader_command_provider as impl

    return impl(command)

def _message_by_id(store: MessageStore, message_id: str) -> dict[str, Any] | None:
    from team_agent.messaging.leader import _message_by_id as impl

    return impl(store, message_id)

def _resolve_leader_pane(pane: str | None, provider: str, workspace: Path | None = None, require_current: bool = False) -> tuple[dict[str, str], str]:
    from team_agent.messaging.leader_panes import _resolve_leader_pane as impl

    return impl(pane, provider, workspace, require_current)

def _target_fingerprint(pane_info: dict[str, Any]) -> str:
    from team_agent.messaging.leader_panes import _target_fingerprint as impl

    return impl(pane_info)

def _validate_leader_receiver(receiver: dict[str, Any]) -> dict[str, Any]:
    from team_agent.messaging.leader_panes import _validate_leader_receiver as impl

    return impl(receiver)

def collect(workspace: Path, result_file: Path | None = None, *, ensure_coordinator: bool = True) -> dict[str, Any]:
    from team_agent.messaging.results import collect as impl

    return impl(workspace, result_file, ensure_coordinator=ensure_coordinator)

def send_message(workspace: Path, target: str | list[str] | None, content: str, task_id: str | None = None, sender: str = "leader", requires_ack: bool = True, confirm_human: bool = False, wait_visible: bool = True, timeout: float = 30.0, lock_timeout: float = 5.0, watch_result: bool = False, block_until_delivered: bool = True, team: str | None = None) -> dict[str, Any]:
    from team_agent.messaging.send import send_message as impl

    return impl(workspace, target, content, task_id, sender, requires_ack, confirm_human, wait_visible, timeout, lock_timeout, watch_result, block_until_delivered, team)


# Lazy-resolved delegation surface. 77 wrappers that used to live inline in
# runtime.py collapse into this single map plus a PEP 562 __getattr__. Tests
# that patch team_agent.runtime.<name> still work because mock.patch's
# setattr shadows the attribute via runtime.__dict__ until the patch exits;
# attribute access outside a patch resolves via __getattr__ which imports
# the target module lazily (no top-level cycle into messaging/lifecycle/etc.).
_DELEGATE_MAP: dict[str, str] = {
    '_broadcast_message_unlocked': 'team_agent.messaging.send._broadcast_message_unlocked',
    '_broadcast_targets': 'team_agent.messaging.send._broadcast_targets',
    '_capture_contains_message_fragment': 'team_agent.messaging.tmux_prompt._capture_contains_message_fragment',
    '_capture_has_pasted_content_prompt': 'team_agent.messaging.tmux_prompt._capture_has_pasted_content_prompt',
    '_capture_tmux_pane_text': 'team_agent.messaging.tmux_prompt._capture_tmux_pane_text',
    '_choose_leader_submit_key': 'team_agent.messaging.leader_panes._choose_leader_submit_key',
    '_collect_results_and_notify_watchers': 'team_agent.messaging.results._collect_results_and_notify_watchers',
    '_compact_broadcast_delivery': 'team_agent.messaging.send._compact_broadcast_delivery',
    '_compact_visible_text': 'team_agent.messaging.tmux_prompt._compact_visible_text',
    '_coordinator_should_run': 'team_agent.messaging.results._coordinator_should_run',
    '_deliver_pending_messages': 'team_agent.messaging.delivery._deliver_pending_messages',
    '_detect_stuck_agents': 'team_agent.messaging.scheduler._detect_stuck_agents',
    '_ensure_coordinator_after_collect': 'team_agent.messaging.results._ensure_coordinator_after_collect',
    '_fail_leader_delivery': 'team_agent.messaging.leader._fail_leader_delivery',
    '_fire_due_scheduled_events': 'team_agent.messaging.scheduler._fire_due_scheduled_events',
    '_format_leader_pane_candidates': 'team_agent.messaging.leader_panes._format_leader_pane_candidates',
    '_format_report_result_notification': 'team_agent.messaging.results._format_report_result_notification',
    '_format_result_watcher_notification': 'team_agent.messaging.results._format_result_watcher_notification',
    '_format_team_agent_message': 'team_agent.messaging.leader._format_team_agent_message',
    '_infer_active_tmux_pane': 'team_agent.messaging.leader_panes._infer_active_tmux_pane',
    '_infer_workspace_tmux_pane': 'team_agent.messaging.leader_panes._infer_workspace_tmux_pane',
    '_is_strong_message_fragment': 'team_agent.messaging.tmux_prompt._is_strong_message_fragment',
    '_leader_command_is_exact': 'team_agent.messaging.leader_panes._leader_command_is_exact',
    '_leader_command_looks_usable': 'team_agent.messaging.leader_panes._leader_command_looks_usable',
    '_leader_inbox_path': 'team_agent.messaging.leader._leader_inbox_path',
    '_leader_pane_rank': 'team_agent.messaging.leader_panes._leader_pane_rank',
    '_leader_receiver_is_direct': 'team_agent.messaging.leader._leader_receiver_is_direct',
    '_leader_submit_verification': 'team_agent.messaging.tmux_io._leader_submit_verification',
    '_message_content_lines': 'team_agent.messaging.tmux_prompt._message_content_lines',
    '_message_fragment_candidates': 'team_agent.messaging.tmux_prompt._message_fragment_candidates',
    '_message_payload': 'team_agent.messaging.leader._message_payload',
    '_mirror_peer_message_to_leader': 'team_agent.messaging.leader._mirror_peer_message_to_leader',
    '_notify_leader_of_report_result': 'team_agent.messaging.results._notify_leader_of_report_result',
    '_notify_result_watchers': 'team_agent.messaging.results._notify_result_watchers',
    '_pane_is_usable_leader': 'team_agent.messaging.leader_panes._pane_is_usable_leader',
    '_pane_path_matches_workspace': 'team_agent.messaging.leader_panes._pane_path_matches_workspace',
    '_parse_tmux_pane_info': 'team_agent.messaging.leader_panes._parse_tmux_pane_info',
    '_prepare_tmux_pane_for_input': 'team_agent.messaging.tmux_io._prepare_tmux_pane_for_input',
    '_record_invalid_result': 'team_agent.messaging.results._record_invalid_result',
    '_rediscover_leader_receiver': 'team_agent.messaging.leader_panes._rediscover_leader_receiver',
    '_schedule_send_retry': 'team_agent.messaging.scheduler._schedule_send_retry',
    '_send_message_unlocked': 'team_agent.messaging.send._send_message_unlocked',
    '_send_single_message_unlocked': 'team_agent.messaging.send._send_single_message_unlocked',
    '_send_to_leader_receiver': 'team_agent.messaging.leader._send_to_leader_receiver',
    '_start_agent_unlocked': 'team_agent.lifecycle.start._start_agent_unlocked',
    '_submit_worker_prompt': 'team_agent.messaging.tmux_prompt._submit_worker_prompt',
    '_team_state_result_entries': 'team_agent.messaging.results._team_state_result_entries',
    '_tmux_current_client_pane_info': 'team_agent.messaging.leader_panes._tmux_current_client_pane_info',
    '_tmux_inject_text': 'team_agent.messaging.tmux_io._tmux_inject_text',
    '_tmux_list_panes': 'team_agent.messaging.leader_panes._tmux_list_panes',
    '_tmux_load_buffer_stdin': 'team_agent.messaging.tmux_io._tmux_load_buffer_stdin',
    '_tmux_pane_info': 'team_agent.messaging.leader_panes._tmux_pane_info',
    '_tmux_paste_ready_timeout': 'team_agent.messaging.tmux_io._tmux_paste_ready_timeout',
    '_tmux_set_buffer_text': 'team_agent.messaging.tmux_io._tmux_set_buffer_text',
    '_tmux_submit_settle_timeout': 'team_agent.messaging.tmux_io._tmux_submit_settle_timeout',
    '_tmux_text_size': 'team_agent.messaging.tmux_io._tmux_text_size',
    '_tmux_truthy': 'team_agent.messaging.leader_panes._tmux_truthy',
    '_wait_for_message_ready': 'team_agent.messaging.tmux_prompt._wait_for_message_ready',
    '_wait_for_pasted_prompt_cleared': 'team_agent.messaging.tmux_prompt._wait_for_pasted_prompt_cleared',
    '_wait_for_visible_token': 'team_agent.messaging.tmux_prompt._wait_for_visible_token',
    '_wait_for_worker_message_ready': 'team_agent.messaging.tmux_prompt._wait_for_worker_message_ready',
    '_watcher_matches_result': 'team_agent.messaging.results._watcher_matches_result',
    '_write_leader_fallback_audit': 'team_agent.messaging.leader._write_leader_fallback_audit',
    'add_agent': 'team_agent.lifecycle.operations.add_agent',
    'allow_peer_talk': 'team_agent.messaging.leader.allow_peer_talk',
    'fork_agent': 'team_agent.lifecycle.operations.fork_agent',
    'report_result': 'team_agent.messaging.results.report_result',
    'reset_agent': 'team_agent.lifecycle.operations.reset_agent',
    'start_agent': 'team_agent.lifecycle.start.start_agent',
    'stop_agent': 'team_agent.lifecycle.operations.stop_agent',
    'stuck_cancel': 'team_agent.messaging.scheduler.stuck_cancel',
    'stuck_list': 'team_agent.messaging.scheduler.stuck_list',
}


def __getattr__(name: str) -> Any:
    target = _DELEGATE_MAP.get(name)
    if target is None:
        raise AttributeError(f"module 'team_agent.runtime' has no attribute {name!r}")
    module_path, _, attr = target.rpartition('.')
    import importlib
    try:
        # Eager import + cache so subsequent runtime.<name> lookups return
        # the same object (stable identity), preserve the real callable's
        # signature/docstring/qualname for inspect.signature and
        # functools.wraps, and skip __getattr__ entirely on the hot path.
        real = getattr(importlib.import_module(module_path), attr)
    except Exception:
        # Partial-load fallback: messaging/deps.py runs a top-level
        # hasattr(_runtime, _name) sweep while the messaging package is
        # still loading, so eager import of a messaging.* target would
        # cycle. Return a deferred proxy that retries at call time and
        # self-installs the real callable on first successful call so
        # subsequent calls hit the cache.
        def _proxy(*args: Any, **kwargs: Any) -> Any:
            real_callable = getattr(importlib.import_module(module_path), attr)
            globals()[name] = real_callable
            return real_callable(*args, **kwargs)

        _proxy.__name__ = name
        _proxy.__qualname__ = name
        _proxy.__module__ = "team_agent.runtime"
        return _proxy
    globals()[name] = real
    return real
