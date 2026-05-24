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
    GHOSTTY_WORKSPACE_PANES_PER_WINDOW,
    close_ghostty_display as _close_ghostty_display,
    close_ghostty_workspace as _close_ghostty_workspace,
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
    quick_start,
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
    load_runtime_state,
    normalize_agent_session_state,
    runtime_state_path,
    save_runtime_state,
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
from team_agent.task_graph import ready_tasks, update_task_status
from team_agent.task_graph import TASK_STATUSES


def _fire_due_scheduled_events(workspace: Path, store: MessageStore, event_log: EventLog) -> list[int]:
    from team_agent.messaging.scheduler import _fire_due_scheduled_events as impl

    return impl(workspace, store, event_log)


def _schedule_send_retry(store: MessageStore, row: dict[str, Any], payload: dict[str, Any], result: dict[str, Any]) -> dict[str, Any] | None:
    from team_agent.messaging.scheduler import _schedule_send_retry as impl

    return impl(store, row, payload, result)


def _detect_stuck_agents(workspace: Path, state: dict[str, Any], store: MessageStore, event_log: EventLog) -> list[str]:
    from team_agent.messaging.scheduler import _detect_stuck_agents as impl

    return impl(workspace, state, store, event_log)


TMUX_PANE_FORMAT = (
    "#{pane_id}\t#{session_name}\t#{window_index}\t#{window_name}\t"
    "#{pane_index}\t#{pane_tty}\t#{pane_current_command}\t#{pane_active}\t"
    "#{pane_current_path}\t#{session_attached}"
)
HEALTH_STATUSES = {"RUNNING", "IDLE", "AWAITING_APPROVAL", "BLOCKED", "ERROR", "DONE"}
GHOSTTY_DISPLAY_BACKENDS = {"ghostty", "ghostty_window", "ghostty_workspace"}
DELIVERY_CAPTURE_LINES = 40
SUBMITTED_DELIVERY_STATUSES = {"injected", "visible", "submitted", "submitted_unverified", "delivered", "acknowledged"}
STARTUP_PROMPT_RUNTIME_CHECK_LIMIT = 3
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
INTERNAL_MCP_AUTO_APPROVE_TOOLS = {"send_message", "report_result", "get_team_status", "request_human"}
INTERNAL_MCP_APPROVAL_CHOICE = "Allow for this session"
DANGEROUS_LEADER_FLAGS = (
    ("claude", "--dangerously-skip-permissions"),
    ("claude", "--dangerously-skip-permission"),
    ("codex", "--dangerously-bypass-approvals-and-sandbox"),
)


def run_cmd(args: list[str], timeout: int = 20) -> subprocess.CompletedProcess[str]:
    return subprocess.run(args, text=True, capture_output=True, timeout=timeout, check=False)


def ensure_workspace_dirs(workspace: Path) -> None:
    for path in [runtime_dir(workspace), logs_dir(workspace), artifacts_dir(workspace), messages_dir(workspace)]:
        path.mkdir(parents=True, exist_ok=True)


def _effective_runtime_config(runtime_cfg: dict[str, Any]) -> dict[str, Any]:
    effective = dict(runtime_cfg)
    if effective.get("dangerous_auto_approve"):
        effective["dangerous_auto_approve_source"] = "runtime_config"
        effective["dangerous_auto_approve_inherited"] = False
        return effective
    inherited = _detect_inherited_dangerous_permissions()
    if not inherited.get("enabled"):
        effective["dangerous_auto_approve"] = False
        effective["dangerous_auto_approve_source"] = "disabled"
        effective["dangerous_auto_approve_inherited"] = False
        return effective
    effective["dangerous_auto_approve"] = True
    effective["dangerous_auto_approve_source"] = "leader_process"
    effective["dangerous_auto_approve_inherited"] = True
    effective["dangerous_auto_approve_provider"] = inherited.get("provider")
    effective["dangerous_auto_approve_flag"] = inherited.get("flag")
    return effective


def _requires_direct_leader_receiver(spec: dict[str, Any], runtime_cfg: dict[str, Any]) -> bool:
    if runtime_cfg.get("require_leader_receiver") is not None:
        return bool(runtime_cfg.get("require_leader_receiver"))
    return any(agent.get("provider") != "fake" for agent in spec.get("agents", []))


def _detect_inherited_dangerous_permissions() -> dict[str, Any]:
    for proc in _process_ancestry(os.getpid()):
        command = str(proc.get("command") or "")
        for provider, flag in DANGEROUS_LEADER_FLAGS:
            if _command_has_flag(command, flag):
                return {
                    "enabled": True,
                    "provider": provider,
                    "flag": flag,
                    "pid": proc.get("pid"),
                }
    return {"enabled": False}


def _command_has_flag(command: str, flag: str) -> bool:
    return re.search(rf"(?<!\S){re.escape(flag)}(?!\S)", command) is not None


def _process_ancestry(pid: int, max_depth: int = 12) -> list[dict[str, Any]]:
    ancestry: list[dict[str, Any]] = []
    current = pid
    seen: set[int] = set()
    for _ in range(max_depth):
        if current in seen or current <= 0:
            break
        seen.add(current)
        info = _process_info(current)
        if not info:
            break
        ancestry.append(info)
        parent = info.get("ppid")
        if not isinstance(parent, int) or parent <= 1 or parent == current:
            break
        current = parent
    return ancestry


def _process_info(pid: int) -> dict[str, Any] | None:
    try:
        proc = subprocess.run(
            ["ps", "-p", str(pid), "-o", "ppid=", "-o", "command="],
            text=True,
            capture_output=True,
            timeout=2,
            check=False,
        )
    except (OSError, subprocess.TimeoutExpired):
        return None
    if proc.returncode != 0:
        return None
    line = proc.stdout.strip()
    if not line:
        return None
    parts = line.split(None, 1)
    try:
        ppid = int(parts[0])
    except (IndexError, ValueError):
        return None
    return {"pid": pid, "ppid": ppid, "command": parts[1] if len(parts) > 1 else ""}


def init_workspace(workspace: Path, force: bool = False) -> dict[str, Path]:
    ensure_workspace_dirs(workspace)
    team_dir = workspace / ".team" / "current"
    team_dir.mkdir(parents=True, exist_ok=True)
    spec_path = team_dir / "team.spec.yaml"
    state_path = workspace / "team_state.md"
    from team_agent.paths import example_path, template_path

    if spec_path.exists() and not force:
        raise RuntimeError(f"{spec_path} already exists; pass --force to overwrite")
    spec_path.write_text(example_path("team.spec.yaml").read_text(encoding="utf-8"), encoding="utf-8")
    if not state_path.exists() or force:
        state_path.write_text(template_path("team_state.md").read_text(encoding="utf-8"), encoding="utf-8")
    EventLog(workspace).write("init", spec_path=str(spec_path), state_path=str(state_path))
    return {"spec": spec_path, "state": state_path}


def validate_file(spec_path: Path) -> dict[str, Any]:
    if spec_path.is_dir():
        from team_agent.compiler import compile_team

        result = compile_team(spec_path)
        spec = result["spec"]
        return {
            "ok": True,
            "type": "team_dir",
            "workspace": str(Path(spec["team"]["workspace"]).resolve()),
            "team": spec["team"]["name"],
            "agents": [agent["id"] for agent in spec.get("agents", [])],
        }
    spec = load_spec(spec_path)
    workspace = workspace_from_spec(spec, spec_path)
    return {"ok": True, "workspace": str(workspace), "team": spec["team"]["name"]}


def _tmux_session_conflict_error(session_name: str) -> str:
    return (
        f"tmux session already exists: {session_name}. "
        "Startup will not terminate existing tmux sessions because they may belong to active teams. "
        "Use a different team name or runtime.session_name and start again."
    )


def _spec_team_dir(spec_path: Path, workspace: Path) -> Path:
    spec_dir = spec_path.resolve().parent
    if spec_dir.parent.name == ".team":
        return spec_dir
    return workspace.resolve() / ".team" / "current"


def _is_team_doc_dir(team_dir: Path) -> bool:
    return (team_dir / "TEAM.md").exists() and (team_dir / "agents").is_dir()


def _compile_team_dir_spec(team_dir: Path, workspace: Path) -> dict[str, Any]:
    from team_agent.compiler import compile_team

    spec_path = team_dir / "team.spec.yaml"
    compiled = compile_team(team_dir, spec_path)
    if compiled["spec"].get("context", {}).get("state_file") == "team_state.md":
        state_file = str(team_dir.relative_to(workspace) / "team_state.md") if team_dir.is_relative_to(workspace) else "team_state.md"
        compiled["spec"]["context"]["state_file"] = state_file
        spec_path.write_text(dumps(compiled["spec"]), encoding="utf-8")
    return compiled


def _attach_team_profile_dirs(spec: dict[str, Any], spec_path: Path, workspace: Path | None = None, team_dir: Path | None = None) -> None:
    workspace = workspace.resolve() if workspace else workspace_from_spec(spec, spec_path)
    team_dir = team_dir.resolve() if team_dir else _spec_team_dir(spec_path, workspace)
    profiles_dir = team_dir / "profiles"
    for agent in spec.get("agents", []):
        if isinstance(agent, dict) and agent.get("profile"):
            agent["_profile_dir"] = str(profiles_dir)


def launch(
    spec_path: Path,
    dry_run: bool = False,
    auto_approve: bool = False,
    skip_profile_smoke: bool = False,
) -> dict[str, Any]:
    spec = load_spec(spec_path)
    workspace = workspace_from_spec(spec, spec_path)
    team_dir = _spec_team_dir(spec_path, workspace)
    _attach_team_profile_dirs(spec, spec_path, workspace, team_dir)
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
    runtime_cfg = _effective_runtime_config(spec.get("runtime", {}))
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
        raise RuntimeError(_tmux_session_conflict_error(session_name))
    _ensure_agent_start_requirements(
        workspace,
        spec.get("agents", []),
        event_log,
        "launch",
        skip_profile_smoke=skip_profile_smoke,
    )

    leader_receiver = None
    leader_provider = state.get("leader", {}).get("provider")
    require_leader_receiver = _requires_direct_leader_receiver(spec, runtime_cfg)
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
    save_runtime_state(workspace, state)
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


def attach_leader(workspace: Path, pane: str | None = None, provider: str = "codex") -> dict[str, Any]:
    ensure_workspace_dirs(workspace)
    state = load_runtime_state(workspace)
    event_log = EventLog(workspace)
    receiver, validation = _attach_leader_to_state(
        workspace,
        state,
        pane=pane,
        provider=provider,
        event_log=event_log,
        source="manual",
    )
    save_runtime_state(workspace, state)
    return {"ok": True, "leader_receiver": receiver, "validation": validation}


def start_leader(provider: str, provider_args: list[str], workspace: Path) -> None:
    plan = leader_start_plan(provider, provider_args, workspace)
    EventLog(workspace).write(
        "leader.start",
        provider=provider,
        workspace=str(workspace),
        mode=plan["mode"],
        session_name=plan.get("session_name"),
        argv=plan["argv"],
    )
    if plan["mode"] == "exec_provider":
        os.chdir(workspace)
    os.execvp(plan["argv"][0], plan["argv"])


def leader_start_plan(provider: str, provider_args: list[str], workspace: Path) -> dict[str, Any]:
    workspace = workspace.resolve()
    ensure_workspace_dirs(workspace)
    adapter = get_adapter(provider)
    if not adapter.is_installed():
        raise RuntimeError(f"Provider {provider} command {adapter.command_name!r} not found")
    argv = [adapter.command_name, *provider_args]
    if os.environ.get("TMUX"):
        return {"mode": "exec_provider", "provider": provider, "workspace": str(workspace), "argv": argv}
    if not shutil_which("tmux"):
        raise RuntimeError("tmux is not installed; install tmux 3.3+ or start the leader from an existing tmux pane")
    session_name = _leader_session_name(provider, workspace)
    if _tmux_session_exists(session_name):
        return {
            "mode": "attach_existing",
            "provider": provider,
            "workspace": str(workspace),
            "session_name": session_name,
            "argv": ["tmux", "attach-session", "-t", session_name],
        }
    shell = f"cd {shlex.quote(str(workspace))} && exec {shlex.join(argv)}"
    return {
        "mode": "new_tmux_session",
        "provider": provider,
        "workspace": str(workspace),
        "session_name": session_name,
        "argv": ["tmux", "new-session", "-s", session_name, "-n", provider, "-c", str(workspace), "sh", "-lc", shell],
    }


def _leader_session_name(provider: str, workspace: Path) -> str:
    digest = hashlib.sha1(str(workspace.resolve()).encode("utf-8")).hexdigest()[:8]
    folder = re.sub(r"[^A-Za-z0-9_.-]", "_", workspace.name)[:48].strip("._-") or "workspace"
    return f"team-agent-leader-{provider}-{folder}-{digest}"


def _attach_leader_to_state(
    workspace: Path,
    state: dict[str, Any],
    pane: str | None,
    provider: str,
    event_log: EventLog,
    source: str,
    require_current: bool = False,
) -> tuple[dict[str, Any], dict[str, Any]]:
    get_adapter(provider)
    pane_info, discovery = _resolve_leader_pane(pane, provider, workspace=workspace, require_current=require_current)
    inferred_provider = _leader_command_provider(pane_info.get("pane_current_command", ""))
    receiver_provider = inferred_provider or provider
    receiver = {
        "mode": "direct_tmux",
        "status": "attached",
        "provider": receiver_provider,
        "pane_id": pane_info["pane_id"],
        "session_name": pane_info["session_name"],
        "window_index": pane_info["window_index"],
        "window_name": pane_info["window_name"],
        "pane_index": pane_info["pane_index"],
        "pane_tty": pane_info["pane_tty"],
        "pane_current_command": pane_info["pane_current_command"],
        "fingerprint": _target_fingerprint(pane_info),
        "attached_at": datetime.now(timezone.utc).isoformat(),
        "discovery": discovery,
    }
    if receiver_provider != provider:
        receiver["requested_provider"] = provider
    validation = _validate_leader_receiver(receiver)
    if not validation["ok"]:
        event_log.write(
            "leader_receiver.attach_failed",
            target=pane or pane_info.get("pane_id"),
            discovery=discovery,
            provider=provider,
            reason=validation["reason"],
            error=validation.get("error"),
            source=source,
        )
        raise RuntimeError(f"leader pane validation failed: {validation['reason']}")
    if validation.get("warning"):
        receiver["warning"] = validation["warning"]
    state["leader_receiver"] = receiver
    event_log.write(
        "leader_receiver.attached",
        target=receiver["pane_id"],
        session_name=receiver["session_name"],
        window_index=receiver["window_index"],
        window_name=receiver["window_name"],
        pane_index=receiver["pane_index"],
        pane_tty=receiver["pane_tty"],
        pane_current_command=receiver["pane_current_command"],
        provider=receiver_provider,
        requested_provider=provider if receiver_provider != provider else None,
        discovery=discovery,
        source=source,
    )
    return receiver, validation


def send_message(workspace: Path, target: str | None, content: str, task_id: str | None = None, sender: str = "leader", requires_ack: bool = True, confirm_human: bool = False, wait_visible: bool = True, timeout: float = 30.0, lock_timeout: float = 5.0, watch_result: bool = False) -> dict[str, Any]:
    from team_agent.messaging.send import send_message as impl

    return impl(workspace, target, content, task_id, sender, requires_ack, confirm_human, wait_visible, timeout, lock_timeout, watch_result)


def _send_message_unlocked(workspace: Path, target: str | None, content: str, task_id: str | None = None, sender: str = "leader", requires_ack: bool = True, confirm_human: bool = False, wait_visible: bool = True, timeout: float = 30.0, watch_result: bool = False) -> dict[str, Any]:
    from team_agent.messaging.send import _send_message_unlocked as impl

    return impl(workspace, target, content, task_id, sender, requires_ack, confirm_human, wait_visible, timeout, watch_result)


def _send_single_message_unlocked(workspace: Path, state: dict[str, Any], spec: dict[str, Any], event_log: EventLog, target: str | None, content: str, *, task_id: str | None = None, sender: str = "leader", requires_ack: bool = True, confirm_human: bool = False, wait_visible: bool = True, timeout: float = 30.0, watch_result: bool = False, mirror_peer: bool = True, route_task_id: bool = True) -> dict[str, Any]:
    from team_agent.messaging.send import _send_single_message_unlocked as impl

    return impl(workspace, state, spec, event_log, target, content, task_id=task_id, sender=sender, requires_ack=requires_ack, confirm_human=confirm_human, wait_visible=wait_visible, timeout=timeout, watch_result=watch_result, mirror_peer=mirror_peer, route_task_id=route_task_id)


def _broadcast_message_unlocked(workspace: Path, state: dict[str, Any], spec: dict[str, Any], event_log: EventLog, content: str, *, task_id: str | None, sender: str, requires_ack: bool, wait_visible: bool, timeout: float) -> dict[str, Any]:
    from team_agent.messaging.send import _broadcast_message_unlocked as impl

    return impl(workspace, state, spec, event_log, content, task_id=task_id, sender=sender, requires_ack=requires_ack, wait_visible=wait_visible, timeout=timeout)


def collect(workspace: Path, result_file: Path | None = None, *, ensure_coordinator: bool = True) -> dict[str, Any]:
    from team_agent.messaging.results import collect as impl

    return impl(workspace, result_file, ensure_coordinator=ensure_coordinator)


def _team_state_result_entries(store: MessageStore, collected: list[dict[str, Any]]) -> list[dict[str, Any]]:
    from team_agent.messaging.results import _team_state_result_entries as impl

    return impl(store, collected)


def _ensure_coordinator_after_collect(workspace: Path, state: dict[str, Any], event_log: EventLog) -> dict[str, Any]:
    from team_agent.messaging.results import _ensure_coordinator_after_collect as impl

    return impl(workspace, state, event_log)


def _coordinator_should_run(state: dict[str, Any]) -> bool:
    from team_agent.messaging.results import _coordinator_should_run as impl

    return impl(state)


def report_result(workspace: Path, envelope: dict[str, Any]) -> dict[str, Any]:
    from team_agent.messaging.results import report_result as impl

    return impl(workspace, envelope)


def _notify_leader_of_report_result(workspace: Path, envelope: dict[str, Any], result_id: str, event_log: EventLog) -> dict[str, Any]:
    from team_agent.messaging.results import _notify_leader_of_report_result as impl

    return impl(workspace, envelope, result_id, event_log)


def _format_report_result_notification(envelope: dict[str, Any], result_id: str) -> str:
    from team_agent.messaging.results import _format_report_result_notification as impl

    return impl(envelope, result_id)


def _record_invalid_result(event_log: EventLog, error: str, result_file: Path | None = None, result_id: str | None = None, envelope: Any = None) -> dict[str, Any]:
    from team_agent.messaging.results import _record_invalid_result as impl

    return impl(event_log, error, result_file, result_id, envelope)


def shutdown(workspace: Path, keep_logs: bool = True) -> dict[str, Any]:
    state = load_runtime_state(workspace)
    session_name = state.get("session_name")
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
        _close_ghostty_workspace(state, event_log)
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
        _close_ghostty_workspace(state, event_log)
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
    save_runtime_state(workspace, state)
    _save_team_runtime_snapshot(workspace, state)
    return {"ok": True, "session_name": session_name, "logs": captured, "coordinator": coordinator}



def stop_agent(workspace: Path, agent_id: str) -> dict[str, Any]:
    from team_agent.lifecycle.operations import stop_agent as impl

    return impl(workspace, agent_id)


def reset_agent(workspace: Path, agent_id: str, *, discard_session: bool = False, open_display: bool = True) -> dict[str, Any]:
    from team_agent.lifecycle.operations import reset_agent as impl

    return impl(workspace, agent_id, discard_session=discard_session, open_display=open_display)


def add_agent(workspace: Path, agent_id: str, *, role_file_path: str, open_display: bool = True) -> dict[str, Any]:
    from team_agent.lifecycle.operations import add_agent as impl

    return impl(workspace, agent_id, role_file_path=role_file_path, open_display=open_display)


def fork_agent(
    workspace: Path,
    source_agent_id: str,
    *,
    as_agent_id: str,
    label: str | None = None,
    open_display: bool = True,
) -> dict[str, Any]:
    from team_agent.lifecycle.operations import fork_agent as impl

    return impl(workspace, source_agent_id, as_agent_id=as_agent_id, label=label, open_display=open_display)


def start_agent(
    workspace: Path,
    agent_id: str,
    force: bool = False,
    open_display: bool = True,
    allow_fresh: bool = False,
) -> dict[str, Any]:
    from team_agent.lifecycle.start import start_agent as impl

    return impl(workspace, agent_id, force=force, open_display=open_display, allow_fresh=allow_fresh)


def remove_agent(
    workspace: Path,
    agent_id: str,
    *,
    from_spec: bool = False,
    confirm: bool = False,
    force: bool = False,
) -> dict[str, Any]:
    from team_agent.lifecycle.agents import remove_agent as lifecycle_remove_agent

    with _runtime_lock(workspace, "remove-agent"):
        return lifecycle_remove_agent(workspace, agent_id, from_spec=from_spec, confirm=confirm, force=force)


def _start_agent_unlocked(workspace: Path, agent_id: str, force: bool, open_display: bool, allow_fresh: bool) -> dict[str, Any]:
    from team_agent.lifecycle.start import _start_agent_unlocked as impl

    return impl(workspace, agent_id, force, open_display, allow_fresh)


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
    handled_prompts = adapter.handle_startup_prompts(session_name, agent_id, checks=1, sleep_s=0.0)
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




def _collect_results_and_notify_watchers(workspace: Path, event_log: EventLog) -> dict[str, Any]:
    from team_agent.messaging.results import _collect_results_and_notify_watchers as impl

    return impl(workspace, event_log)


def _notify_result_watchers(workspace: Path, result: dict[str, Any], event_log: EventLog) -> list[dict[str, Any]]:
    from team_agent.messaging.results import _notify_result_watchers as impl

    return impl(workspace, result, event_log)


def _watcher_matches_result(watcher: dict[str, Any], result: dict[str, Any]) -> bool:
    from team_agent.messaging.results import _watcher_matches_result as impl

    return impl(watcher, result)


def _format_result_watcher_notification(result: dict[str, Any]) -> str:
    from team_agent.messaging.results import _format_result_watcher_notification as impl

    return impl(result)


def _ensure_agent_start_requirements(
    workspace: Path,
    agents: list[dict[str, Any]],
    event_log: EventLog,
    event_prefix: str,
    skip_profile_smoke: bool = False,
) -> None:
    active_agents = [agent for agent in agents if not agent.get("paused")]
    for agent in active_agents:
        adapter = get_adapter(agent["provider"])
        if not adapter.is_installed():
            event_log.write(
                f"{event_prefix}.provider_missing",
                agent_id=agent["id"],
                provider=agent["provider"],
                command=adapter.command_name,
            )
            raise RuntimeError(
                f"Provider {agent['provider']} command {adapter.command_name!r} not found for agent {agent['id']}"
            )
    profile_checks = _profile_checks_for_agents(workspace, active_agents)
    profile_failures = [item for item in profile_checks if item.get("ok") is False]
    event_log.write(f"{event_prefix}.profile_check", ok=not profile_failures, checks=[compact_profile_check(item) for item in profile_checks])
    if profile_failures:
        raise RuntimeError(_format_profile_check_failures(profile_failures))
    if skip_profile_smoke:
        event_log.write(f"{event_prefix}.profile_smoke_check", ok=True, skipped=True, reason="already_checked")
    else:
        smoke_checks = _profile_smoke_checks_for_agents(workspace, active_agents)
        smoke_failures = [item for item in smoke_checks if item.get("ok") is False]
        event_log.write(f"{event_prefix}.profile_smoke_check", ok=not smoke_failures, checks=[compact_profile_check(item) for item in smoke_checks])
        if smoke_failures:
            raise RuntimeError(_format_profile_smoke_failures(smoke_failures))
    checks = _model_checks_for_agents(active_agents, workspace)
    failures = [item for item in checks if item.get("ok") is False]
    event_log.write(f"{event_prefix}.model_check", ok=not failures, checks=_compact_model_checks(checks))
    if failures:
        raise RuntimeError(_format_model_check_failures(failures))



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


def _broadcast_targets(state: dict[str, Any], spec: dict[str, Any], sender: str) -> list[str]:
    from team_agent.messaging.send import _broadcast_targets as impl

    return impl(state, spec, sender)


def _compact_broadcast_delivery(result: dict[str, Any]) -> dict[str, Any]:
    from team_agent.messaging.send import _compact_broadcast_delivery as impl

    return impl(result)


def allow_peer_talk(workspace: Path, agent_a: str, agent_b: str) -> dict[str, Any]:
    from team_agent.messaging.leader import allow_peer_talk as impl

    return impl(workspace, agent_a, agent_b)


def _mirror_peer_message_to_leader(workspace: Path, state: dict[str, Any], sender: str, target: str, content: str, task_id: str | None, event_log: EventLog) -> None:
    from team_agent.messaging.leader import _mirror_peer_message_to_leader as impl

    return impl(workspace, state, sender, target, content, task_id, event_log)


def _leader_inbox_path(workspace: Path) -> Path:
    from team_agent.messaging.leader import _leader_inbox_path as impl

    return impl(workspace)


def _send_to_leader_receiver(workspace: Path, state: dict[str, Any], leader_id: str, content: str, task_id: str | None, sender: str, requires_ack: bool, event_log: EventLog) -> dict[str, Any]:
    from team_agent.messaging.leader import _send_to_leader_receiver as impl

    return impl(workspace, state, leader_id, content, task_id, sender, requires_ack, event_log)


def _fail_leader_delivery(workspace: Path, state: dict[str, Any], store: MessageStore, message_id: str, payload: dict[str, Any], event_log: EventLog, reason: str, error: str | None = None, stage: str | None = None, message_status: str = "failed", attempts: list[dict[str, Any]] | None = None, submit_attempts: list[dict[str, Any]] | None = None) -> dict[str, Any]:
    from team_agent.messaging.leader import _fail_leader_delivery as impl

    return impl(workspace, state, store, message_id, payload, event_log, reason, error, stage, message_status, attempts, submit_attempts)


def _write_leader_fallback_audit(workspace: Path, payload: dict[str, Any], reason: str, error: str | None) -> Path:
    from team_agent.messaging.leader import _write_leader_fallback_audit as impl

    return impl(workspace, payload, reason, error)


def _leader_receiver_is_direct(receiver: dict[str, Any] | None) -> bool:
    from team_agent.messaging.leader import _leader_receiver_is_direct as impl

    return impl(receiver)


def _message_by_id(store: MessageStore, message_id: str) -> dict[str, Any] | None:
    from team_agent.messaging.leader import _message_by_id as impl

    return impl(store, message_id)


def _message_payload(row: dict[str, Any]) -> dict[str, Any]:
    from team_agent.messaging.leader import _message_payload as impl

    return impl(row)


def _format_team_agent_message(payload: dict[str, Any]) -> str:
    from team_agent.messaging.leader import _format_team_agent_message as impl

    return impl(payload)


def _resolve_leader_pane(pane: str | None, provider: str, workspace: Path | None = None, require_current: bool = False) -> tuple[dict[str, str], str]:
    from team_agent.messaging.leader_panes import _resolve_leader_pane as impl

    return impl(pane, provider, workspace, require_current)


def _tmux_current_client_pane_info() -> dict[str, str] | None:
    from team_agent.messaging.leader_panes import _tmux_current_client_pane_info as impl

    return impl()


def _tmux_list_panes() -> list[dict[str, str]]:
    from team_agent.messaging.leader_panes import _tmux_list_panes as impl

    return impl()


def _infer_active_tmux_pane(provider: str) -> dict[str, str] | None:
    from team_agent.messaging.leader_panes import _infer_active_tmux_pane as impl

    return impl(provider)


def _tmux_pane_info(target: str | None) -> dict[str, str] | None:
    from team_agent.messaging.leader_panes import _tmux_pane_info as impl

    return impl(target)


def _parse_tmux_pane_info(line: str) -> dict[str, str] | None:
    from team_agent.messaging.leader_panes import _parse_tmux_pane_info as impl

    return impl(line)


def _infer_workspace_tmux_pane(provider: str, workspace: Path) -> dict[str, Any]:
    from team_agent.messaging.leader_panes import _infer_workspace_tmux_pane as impl

    return impl(provider, workspace)


def _pane_is_usable_leader(pane: dict[str, str], provider: str, workspace: Path | None) -> bool:
    from team_agent.messaging.leader_panes import _pane_is_usable_leader as impl

    return impl(pane, provider, workspace)


def _pane_path_matches_workspace(pane: dict[str, str], workspace: Path) -> bool:
    from team_agent.messaging.leader_panes import _pane_path_matches_workspace as impl

    return impl(pane, workspace)


def _leader_pane_rank(pane: dict[str, str], provider: str) -> tuple[int, int, int]:
    from team_agent.messaging.leader_panes import _leader_pane_rank as impl

    return impl(pane, provider)


def _tmux_truthy(value: str) -> int:
    from team_agent.messaging.leader_panes import _tmux_truthy as impl

    return impl(value)


def _leader_command_is_exact(command: str, provider: str) -> bool:
    from team_agent.messaging.leader_panes import _leader_command_is_exact as impl

    return impl(command, provider)


def _leader_command_provider(command: str) -> str | None:
    from team_agent.messaging.leader_panes import _leader_command_provider as impl

    return impl(command)


def _format_leader_pane_candidates(candidates: list[dict[str, str]]) -> str:
    from team_agent.messaging.leader_panes import _format_leader_pane_candidates as impl

    return impl(candidates)


def _target_fingerprint(pane_info: dict[str, Any]) -> str:
    from team_agent.messaging.leader_panes import _target_fingerprint as impl

    return impl(pane_info)


def _rediscover_leader_receiver(receiver: dict[str, Any], event_log: EventLog) -> dict[str, Any]:
    from team_agent.messaging.leader_panes import _rediscover_leader_receiver as impl

    return impl(receiver, event_log)


def _validate_leader_receiver(receiver: dict[str, Any]) -> dict[str, Any]:
    from team_agent.messaging.leader_panes import _validate_leader_receiver as impl

    return impl(receiver)


def _leader_command_looks_usable(command: str, provider: str) -> bool:
    from team_agent.messaging.leader_panes import _leader_command_looks_usable as impl

    return impl(command, provider)


def _choose_leader_submit_key(provider: str, capture_text: str) -> tuple[str, str]:
    from team_agent.messaging.leader_panes import _choose_leader_submit_key as impl

    return impl(provider, capture_text)


def _tmux_inject_text(target: str, text: str, submit_key: str, buffer_name: str, attempts: int = 3) -> dict[str, Any]:
    from team_agent.messaging.tmux_io import _tmux_inject_text as impl

    return impl(target, text, submit_key, buffer_name, attempts)


def _leader_submit_verification(submit_verification: str | None, verification: str, submit_key: str) -> str | None:
    from team_agent.messaging.tmux_io import _leader_submit_verification as impl

    return impl(submit_verification, verification, submit_key)


def _tmux_text_size(text: str) -> int:
    from team_agent.messaging.tmux_io import _tmux_text_size as impl

    return impl(text)


def _tmux_paste_ready_timeout(text: str) -> float:
    from team_agent.messaging.tmux_io import _tmux_paste_ready_timeout as impl

    return impl(text)


def _tmux_submit_settle_timeout(text: str) -> float:
    from team_agent.messaging.tmux_io import _tmux_submit_settle_timeout as impl

    return impl(text)


def _tmux_set_buffer_text(buffer_name: str, text: str) -> dict[str, Any]:
    from team_agent.messaging.tmux_io import _tmux_set_buffer_text as impl

    return impl(buffer_name, text)


def _tmux_load_buffer_stdin(buffer_name: str, text: str) -> subprocess.CompletedProcess[str]:
    from team_agent.messaging.tmux_io import _tmux_load_buffer_stdin as impl

    return impl(buffer_name, text)


def _prepare_tmux_pane_for_input(target: str) -> dict[str, Any]:
    from team_agent.messaging.tmux_io import _prepare_tmux_pane_for_input as impl

    return impl(target)


def _enable_codex_fast_mode(session_name: str, window_name: str) -> dict[str, Any]:
    from team_agent.messaging.tmux_prompt import _enable_codex_fast_mode as impl

    return impl(session_name, window_name)


def _wait_for_visible_token(target: str, token: str, timeout: float) -> tuple[bool, str]:
    from team_agent.messaging.tmux_prompt import _wait_for_visible_token as impl

    return impl(target, token, timeout)


def _capture_tmux_pane_text(target: str) -> dict[str, Any]:
    from team_agent.messaging.tmux_prompt import _capture_tmux_pane_text as impl

    return impl(target)


def _wait_for_message_ready(target: str, message_id: str, timeout: float, expected_text: str = "", allow_pasted_prompt: bool = True, baseline_capture: str = "") -> tuple[bool, str, str]:
    from team_agent.messaging.tmux_prompt import _wait_for_message_ready as impl

    return impl(target, message_id, timeout, expected_text, allow_pasted_prompt, baseline_capture)


def _wait_for_worker_message_ready(target: str, message_id: str, timeout: float, expected_text: str = "") -> tuple[bool, str, str]:
    from team_agent.messaging.tmux_prompt import _wait_for_worker_message_ready as impl

    return impl(target, message_id, timeout, expected_text)


def _capture_has_pasted_content_prompt(text: str) -> bool:
    from team_agent.messaging.tmux_prompt import _capture_has_pasted_content_prompt as impl

    return impl(text)


def _capture_contains_message_fragment(capture_text: str, expected_text: str) -> bool:
    from team_agent.messaging.tmux_prompt import _capture_contains_message_fragment as impl

    return impl(capture_text, expected_text)


def _message_fragment_candidates(text: str) -> list[str]:
    from team_agent.messaging.tmux_prompt import _message_fragment_candidates as impl

    return impl(text)


def _message_content_lines(text: str) -> list[str]:
    from team_agent.messaging.tmux_prompt import _message_content_lines as impl

    return impl(text)


def _is_strong_message_fragment(compact: str) -> bool:
    from team_agent.messaging.tmux_prompt import _is_strong_message_fragment as impl

    return impl(compact)


def _compact_visible_text(text: str) -> str:
    from team_agent.messaging.tmux_prompt import _compact_visible_text as impl

    return impl(text)


def _submit_worker_prompt(target: str, before_capture: str, submit_key: str = "Enter", attempts: int = 3, settle_timeout: float = 0.35) -> dict[str, Any]:
    from team_agent.messaging.tmux_prompt import _submit_worker_prompt as impl

    return impl(target, before_capture, submit_key, attempts, settle_timeout)


def _wait_for_pasted_prompt_cleared(target: str, timeout: float) -> tuple[bool, str]:
    from team_agent.messaging.tmux_prompt import _wait_for_pasted_prompt_cleared as impl

    return impl(target, timeout)



def get_adapter_or_raise(name: str) -> str:
    if name == "tmux" and not shutil_which("tmux"):
        raise RuntimeError("tmux is not installed; install tmux 3.3+ before launch")
    return name


def _deliver_pending_message(workspace: Path, state: dict[str, Any], message_id: str, wait_visible: bool = True, timeout: float = 30.0) -> dict[str, Any]:
    from team_agent.messaging.delivery import _deliver_pending_message as impl

    return impl(workspace, state, message_id, wait_visible, timeout)


def _deliver_pending_messages(workspace: Path, state: dict[str, Any], event_log: EventLog) -> list[str]:
    from team_agent.messaging.delivery import _deliver_pending_messages as impl

    return impl(workspace, state, event_log)


def _refresh_agent_runtime_statuses(workspace: Path, state: dict[str, Any], event_log: EventLog) -> None:
    session_name = state.get("session_name")
    tmux_exists = _tmux_session_exists(session_name) if session_name else False
    for agent_id, agent_state in state.get("agents", {}).items():
        if agent_state.get("status") in {"paused", "stopped"}:
            continue
        old_status = agent_state.get("status")
        window = agent_state.get("window", agent_id)
        window_present = _tmux_window_exists(session_name, window) if tmux_exists else False
        agent_state["tmux_window_present"] = window_present
        if not window_present:
            if session_name:
                agent_state["status"] = "missing"
        else:
            detected = _detect_provider_status(agent_state["provider"], session_name, window)
            if detected:
                agent_state["status"] = detected
            else:
                agent_state.setdefault("status", "running")
        if old_status != agent_state.get("status"):
            event_log.write(
                "runtime.status_detected",
                agent_id=agent_id,
                provider=agent_state.get("provider"),
                old_status=old_status,
                status=agent_state.get("status"),
            )


def _sync_agent_health(workspace: Path, state: dict[str, Any], store: MessageStore | None = None) -> None:
    store = store or MessageStore(workspace)
    session_name = state.get("session_name")
    for agent_id, agent_state in state.get("agents", {}).items():
        health_status = _agent_health_status(agent_state)
        last_output_at = agent_state.get("last_output_at")
        window = agent_state.get("window", agent_id)
        if session_name and _tmux_window_exists(session_name, window):
            proc = run_cmd(["tmux", "capture-pane", "-p", "-S", "-40", "-t", f"{session_name}:{window}"], timeout=5)
            if proc.returncode == 0:
                digest = hashlib.sha256(proc.stdout.encode("utf-8", errors="ignore")).hexdigest()
                if digest != agent_state.get("last_output_hash"):
                    last_output_at = datetime.now(timezone.utc).isoformat()
                    agent_state["last_output_hash"] = digest
                    agent_state["last_output_at"] = last_output_at
                if _capture_has_approval_prompt(proc.stdout):
                    health_status = "AWAITING_APPROVAL"
        current_task = _current_task_for_agent(state.get("tasks", []), agent_id)
        store.upsert_agent_health(
            agent_id,
            health_status,
            last_output_at=last_output_at,
            context_usage_pct=agent_state.get("context_usage_pct"),
            current_task_id=current_task,
        )


def _agent_health_status(agent_state: dict[str, Any]) -> str:
    raw = str(agent_state.get("status") or "").lower()
    if raw in {"busy", "running"}:
        return "RUNNING" if raw == "busy" else "IDLE"
    if raw in {"paused", "blocked"}:
        return "BLOCKED"
    if raw in {"error", "missing", "interrupted"}:
        return "ERROR"
    if raw in {"stopped", "done"}:
        return "DONE"
    return "IDLE"


def _current_task_for_agent(tasks: list[dict[str, Any]], agent_id: str) -> str | None:
    active = {"pending", "ready", "running", "blocked", "needs_retry"}
    for task in reversed(tasks):
        if task.get("assignee") == agent_id and task.get("status", "pending") in active:
            return task.get("id")
    return None


def _capture_has_approval_prompt(text: str) -> bool:
    return _extract_approval_prompt("_", text) is not None


def _extract_approval_prompt(agent_id: str, text: str) -> dict[str, Any] | None:
    lines = text.splitlines()
    control_index = _active_approval_control_index(lines)
    if control_index is None:
        return None
    for index in range(control_index, -1, -1):
        line = lines[index]
        if "Allow the team_orchestrator MCP server to run tool" not in line:
            continue
        tool_match = re.search(r'run tool "([^"]+)"', line)
        return {
            "agent_id": agent_id,
            "state": "waiting_approval",
            "kind": "mcp_tool",
            "tool": tool_match.group(1) if tool_match else None,
            "prompt": line.strip(),
            "choices": _extract_approval_choices(lines[index : control_index + 1]),
        }
    for index in range(control_index, -1, -1):
        line = lines[index]
        if _line_is_approval_choice(line):
            continue
        tool_match = re.search(r"\bteam_orchestrator\s*[-.]\s*([A-Za-z_][A-Za-z0-9_]*)\b", line)
        if not tool_match:
            continue
        return {
            "agent_id": agent_id,
            "state": "waiting_approval",
            "kind": "mcp_tool",
            "tool": tool_match.group(1),
            "prompt": f"team_orchestrator - {tool_match.group(1)}",
            "choices": _extract_approval_choices(lines[index : control_index + 1]),
        }
    for index in range(control_index, -1, -1):
        line = lines[index]
        if "Would you like to run the following command" not in line:
            continue
        return {
            "agent_id": agent_id,
            "state": "waiting_approval",
            "kind": "command",
            "command": _extract_command_approval_subject(lines[: control_index + 1], index),
            "prompt": line.strip(),
            "choices": _extract_approval_choices(lines[index : control_index + 1]),
        }
    return {
        "agent_id": agent_id,
        "state": "waiting_approval",
        "kind": "unknown",
        "prompt": "approval prompt detected",
        "choices": _extract_approval_choices(lines[: control_index + 1]),
    }


def _active_approval_control_index(lines: list[str]) -> int | None:
    control_indices = [
        index
        for index, line in enumerate(lines)
        if _is_approval_control_line(line)
    ]
    if not control_indices:
        return None
    control_index = control_indices[-1]
    if any(line.strip() for line in lines[control_index + 1 :]):
        return None
    return control_index


def _is_approval_control_line(line: str) -> bool:
    normalized = line.lower()
    return "enter to submit | esc to cancel" in normalized or ("esc to cancel" in normalized and "tab to amend" in normalized)


def _extract_approval_choices(lines: list[str]) -> list[str]:
    choices: list[str] = []
    for line in lines:
        stripped = line.strip()
        match = _APPROVAL_CHOICE_RE.match(stripped)
        if not match:
            continue
        label = match.group(2).strip()
        if label and label not in choices:
            choices.append(label)
    return choices


_APPROVAL_CHOICE_RE = re.compile(r"(?:[›❯>]\s*)?(\d+)\.\s+(.+?)(?:\s{2,}.+)?$")


def _line_is_approval_choice(line: str) -> bool:
    return _APPROVAL_CHOICE_RE.match(line.strip()) is not None


def _extract_command_approval_subject(lines: list[str], prompt_index: int) -> str | None:
    for line in reversed(lines[:prompt_index]):
        stripped = line.strip()
        if stripped.startswith("Bash(") or stripped.startswith("Shell("):
            return stripped[:200]
    for line in lines[prompt_index + 1 : prompt_index + 8]:
        stripped = line.strip()
        if stripped.startswith("Bash(") or stripped.startswith("Shell("):
            return stripped[:200]
    return None


def _age_text(iso_text: str | None) -> str:
    if not iso_text:
        return "-"
    try:
        dt = datetime.fromisoformat(iso_text)
        if dt.tzinfo is None:
            dt = dt.replace(tzinfo=timezone.utc)
        seconds = max(0, int((datetime.now(timezone.utc) - dt).total_seconds()))
    except ValueError:
        return "-"
    if seconds < 60:
        return f"{seconds}s ago"
    minutes = seconds // 60
    if minutes < 60:
        return f"{minutes}m ago"
    return f"{minutes // 60}h ago"


def _detect_provider_status(provider: str, session_name: str, window: str) -> str | None:
    proc = run_cmd(["tmux", "capture-pane", "-p", "-t", f"{session_name}:{window}"], timeout=5)
    if proc.returncode != 0:
        return None
    patterns = get_adapter(provider).status_patterns()
    positions: dict[str, int] = {}
    for status_name, pattern in patterns.items():
        if not pattern:
            continue
        try:
            matches = list(re.finditer(pattern, proc.stdout, re.MULTILINE))
        except re.error:
            continue
        if matches:
            positions[status_name] = matches[-1].start()
    if not positions:
        return None
    latest = max(positions, key=positions.get)
    return {"idle": "running", "processing": "busy", "error": "error"}.get(latest)


def _handle_provider_runtime_prompts(workspace: Path, state: dict[str, Any], event_log: EventLog) -> None:
    session_name = state.get("session_name")
    if not session_name or not _tmux_session_exists(session_name):
        return
    for agent_id, agent_state in state.get("agents", {}).items():
        if agent_state.get("status") in {"paused", "stopped", "missing"}:
            continue
        window = agent_state.get("window", agent_id)
        if not _tmux_window_exists(session_name, window):
            continue
        internal_mcp = _handle_internal_mcp_approval_prompt(agent_id, session_name, window, event_log)
        if internal_mcp is not None:
            continue
        adapter = get_adapter(agent_state["provider"])
        for prompt_event in adapter.handle_runtime_prompts(session_name, window):
            event_log.write(
                "runtime.prompt_handled",
                agent_id=agent_id,
                provider=agent_state["provider"],
                **prompt_event,
            )


def _handle_provider_startup_prompts(workspace: Path, state: dict[str, Any], event_log: EventLog) -> None:
    session_name = state.get("session_name")
    if not session_name or not _tmux_session_exists(session_name):
        return
    for agent_id, agent_state in state.get("agents", {}).items():
        if agent_state.get("status") in {"paused", "stopped", "missing"}:
            continue
        window = agent_state.get("window", agent_id)
        if not _tmux_window_exists(session_name, window):
            continue
        spawned_at = str(agent_state.get("spawned_at") or "")
        if agent_state.get("startup_prompt_check_spawned_at") != spawned_at:
            agent_state["startup_prompt_check_spawned_at"] = spawned_at
            agent_state["startup_prompt_check_count"] = 0
        check_count = int(agent_state.get("startup_prompt_check_count") or 0)
        if check_count >= STARTUP_PROMPT_RUNTIME_CHECK_LIMIT:
            continue
        agent_state["startup_prompt_check_count"] = check_count + 1
        adapter = get_adapter(agent_state["provider"])
        for prompt_event in adapter.handle_startup_prompts(session_name, window, checks=1, sleep_s=0.0):
            event_log.write(
                "runtime.startup_prompt_handled",
                agent_id=agent_id,
                provider=agent_state["provider"],
                **prompt_event,
            )


def _handle_internal_mcp_approval_prompt(
    agent_id: str,
    session_name: str,
    window: str,
    event_log: EventLog,
) -> dict[str, Any] | None:
    target = f"{session_name}:{window}"
    proc = run_cmd(["tmux", "capture-pane", "-p", "-S", f"-{APPROVAL_SCAN_LINES}", "-t", target], timeout=5)
    if proc.returncode != 0:
        return None
    prompt = _extract_approval_prompt(agent_id, proc.stdout)
    if not prompt or prompt.get("kind") != "mcp_tool":
        return None
    tool = str(prompt.get("tool") or "")
    fingerprint = _approval_prompt_fingerprint(prompt)
    if tool not in INTERNAL_MCP_AUTO_APPROVE_TOOLS:
        result = {
            "ok": False,
            "action": "skipped",
            "reason": "tool_not_allowlisted",
            "tool": tool,
            "fingerprint": fingerprint,
        }
        event_log.write("runtime.internal_mcp_approval.skipped", agent_id=agent_id, **result)
        return result
    result = _submit_internal_mcp_approval(agent_id, target, tool, prompt, proc.stdout)
    event_log.write("runtime.internal_mcp_approval.auto", agent_id=agent_id, **result)
    return result


def _submit_internal_mcp_approval(
    agent_id: str,
    target: str,
    tool: str,
    prompt: dict[str, Any],
    capture_text: str,
    attempts: int = 3,
) -> dict[str, Any]:
    choice = _choose_internal_mcp_approval_choice(prompt)
    fingerprint = _approval_prompt_fingerprint(prompt)
    attempt_log: list[dict[str, Any]] = []
    current_prompt = prompt
    current_capture = capture_text
    for attempt in range(1, attempts + 1):
        keys = _approval_choice_keys(current_prompt, current_capture, choice)
        proc = run_cmd(["tmux", "send-keys", "-t", target, *keys], timeout=10)
        if proc.returncode != 0:
            return {
                "ok": False,
                "action": "auto_approve",
                "tool": tool,
                "choice": choice,
                "fingerprint": fingerprint,
                "attempts": attempt_log + [{"attempt": attempt, "submitted": False, "error": proc.stderr.strip()}],
                "verification": "send_keys_failed",
            }
        time.sleep(0.35)
        verify = run_cmd(["tmux", "capture-pane", "-p", "-S", f"-{APPROVAL_SCAN_LINES}", "-t", target], timeout=5)
        if verify.returncode != 0:
            attempt_log.append({"attempt": attempt, "submitted": True, "keys": keys, "verification": "capture_failed"})
            continue
        after_prompt = _extract_approval_prompt(agent_id, verify.stdout)
        if not after_prompt:
            return {
                "ok": True,
                "action": "auto_approved",
                "tool": tool,
                "choice": choice,
                "fingerprint": fingerprint,
                "attempts": attempt_log + [{"attempt": attempt, "submitted": True, "keys": keys, "verification": "prompt_absent"}],
                "verification": "prompt_absent_after_submit",
            }
        if after_prompt.get("kind") != "mcp_tool" or after_prompt.get("tool") != tool:
            return {
                "ok": True,
                "action": "auto_approved",
                "tool": tool,
                "choice": choice,
                "fingerprint": fingerprint,
                "attempts": attempt_log + [{"attempt": attempt, "submitted": True, "keys": keys, "verification": "different_prompt_present"}],
                "verification": "original_prompt_replaced",
            }
        attempt_log.append({"attempt": attempt, "submitted": True, "keys": keys, "verification": "prompt_still_present"})
        current_prompt = after_prompt
        current_capture = verify.stdout
    return {
        "ok": False,
        "action": "auto_approve",
        "tool": tool,
        "choice": choice,
        "fingerprint": fingerprint,
        "attempts": attempt_log,
        "verification": "prompt_still_present_after_retries",
    }


def _choose_internal_mcp_approval_choice(prompt: dict[str, Any]) -> str:
    choices = prompt.get("choices") or []
    if INTERNAL_MCP_APPROVAL_CHOICE in choices:
        return INTERNAL_MCP_APPROVAL_CHOICE
    for choice in choices:
        if str(choice).startswith("Yes, and don't ask again"):
            return str(choice)
    if "Allow" in choices:
        return "Allow"
    if "Yes" in choices:
        return "Yes"
    return INTERNAL_MCP_APPROVAL_CHOICE


def _approval_choice_keys(prompt: dict[str, Any], capture_text: str, choice: str) -> list[str]:
    choices = prompt.get("choices") or []
    try:
        target_index = choices.index(choice)
    except ValueError:
        return ["Down", "Enter"]
    active_index = _active_approval_choice_index(capture_text)
    if active_index is None:
        return [str(target_index + 1), "Enter"]
    delta = target_index - active_index
    if delta > 0:
        return ["Down"] * delta + ["Enter"]
    if delta < 0:
        return ["Up"] * abs(delta) + ["Enter"]
    return ["Enter"]


def _active_approval_choice_index(text: str) -> int | None:
    for line in text.splitlines():
        stripped = line.strip()
        if not (stripped.startswith("›") or stripped.startswith("❯") or stripped.startswith(">")):
            continue
        match = re.match(r"[›❯>]\s*(\d+)\.", stripped)
        if match:
            return int(match.group(1)) - 1
    return None


def _capture_has_team_orchestrator_mcp_prompt(text: str) -> bool:
    return (
        "Allow the team_orchestrator MCP server to run tool" in text
        or re.search(r"\bteam_orchestrator\s*[-.]\s*[A-Za-z_][A-Za-z0-9_]*\b", text) is not None
    )


def _approval_prompt_fingerprint(prompt: dict[str, Any]) -> str:
    data = {
        "kind": prompt.get("kind"),
        "tool": prompt.get("tool"),
        "prompt": prompt.get("prompt"),
        "choices": prompt.get("choices") or [],
    }
    return hashlib.sha256(json.dumps(data, sort_keys=True, ensure_ascii=False).encode("utf-8")).hexdigest()[:16]


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
