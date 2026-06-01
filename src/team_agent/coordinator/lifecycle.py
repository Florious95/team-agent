from __future__ import annotations

import os
import signal
import subprocess
import sys
from pathlib import Path
from typing import Any

from team_agent.coordinator.metadata import (
    COORDINATOR_PROTOCOL_VERSION,
    coordinator_metadata_ok,
    pid_is_running,
    read_coordinator_metadata,
    write_coordinator_metadata,
)
from team_agent.coordinator.paths import (
    coordinator_log_path,
    coordinator_meta_path,
    coordinator_pid_path,
)
from team_agent.events import EventLog
from team_agent.message_store import MessageStore


def coordinator_health(workspace: Path) -> dict[str, Any]:
    schema = message_store_schema_health(workspace)
    pid_path = coordinator_pid_path(workspace)
    if not pid_path.exists():
        return {"ok": False, "status": "missing", "pid": None, "metadata": None, "metadata_ok": False, **schema}
    try:
        pid = int(pid_path.read_text(encoding="utf-8").strip())
    except ValueError:
        return {"ok": False, "status": "invalid_pid", "pid": None, "metadata": None, "metadata_ok": False, **schema}
    running = pid_is_running(pid)
    metadata = read_coordinator_metadata(workspace)
    metadata_ok = coordinator_metadata_ok(metadata, pid)
    ok = running and metadata_ok and bool(schema.get("schema_ok"))
    return {
        "ok": ok,
        "status": "running" if running else "stale",
        "pid": pid,
        "metadata": metadata,
        "metadata_ok": metadata_ok,
        **schema,
    }


def start_coordinator(workspace: Path) -> dict[str, Any]:
    from team_agent.runtime import ensure_workspace_dirs
    ensure_workspace_dirs(workspace)
    health = coordinator_health(workspace)
    if health["ok"]:
        return {"ok": True, "pid": health["pid"], "status": "already_running", "log": str(coordinator_log_path(workspace))}
    if health["status"] == "running" and not health.get("metadata_ok"):
        EventLog(workspace).write(
            "coordinator.restart_incompatible",
            pid=health.get("pid"),
            metadata=health.get("metadata"),
            expected_protocol=COORDINATOR_PROTOCOL_VERSION,
            expected_schema=MessageStore.SCHEMA_VERSION,
        )
        stopped = stop_coordinator(workspace)
        if not stopped.get("ok"):
            EventLog(workspace).write(
                "coordinator.restart_incompatible_stop_failed",
                pid=health.get("pid"),
                stop_result=stopped,
            )
            return {
                "ok": False,
                "pid": health.get("pid"),
                "status": "restart_incompatible_stop_failed",
                "error": stopped.get("error") or stopped.get("status"),
                "stop_result": stopped,
            }
    if not health.get("schema_ok", False):
        EventLog(workspace).write(
            "coordinator.schema_incompatible",
            error=health.get("schema_error"),
            schema=health.get("schema"),
            reason=health.get("reason"),
            table=health.get("table"),
            missing_columns=health.get("missing_columns"),
        )
        return {
            "ok": False,
            "pid": None,
            "status": "schema_incompatible",
            "error": health.get("schema_error"),
            "schema": health.get("schema"),
            "action": health.get("action", _SCHEMA_ACTION_HINT),
            "reason": health.get("reason"),
            "table": health.get("table"),
            "expected_columns": health.get("expected_columns"),
            "actual_columns": health.get("actual_columns"),
            "missing_columns": health.get("missing_columns"),
        }
    if health["status"] in {"stale", "invalid_pid"}:
        coordinator_pid_path(workspace).unlink(missing_ok=True)
        coordinator_meta_path(workspace).unlink(missing_ok=True)
    log_path = coordinator_log_path(workspace)
    log_path.parent.mkdir(parents=True, exist_ok=True)
    env = dict(os.environ)
    repo_src = str(Path(__file__).resolve().parents[2])
    env["PYTHONPATH"] = repo_src + (os.pathsep + env["PYTHONPATH"] if env.get("PYTHONPATH") else "")
    log = log_path.open("a", encoding="utf-8")
    proc = subprocess.Popen(
        [sys.executable, "-m", "team_agent.coordinator", "--workspace", str(workspace)],
        cwd=str(workspace),
        stdin=subprocess.DEVNULL,
        stdout=log,
        stderr=log,
        env=env,
        start_new_session=True,
    )
    log.close()
    coordinator_pid_path(workspace).write_text(str(proc.pid), encoding="utf-8")
    write_coordinator_metadata(workspace, proc.pid, source="start")
    EventLog(workspace).write("coordinator.started", pid=proc.pid, log=str(log_path))
    return {"ok": True, "pid": proc.pid, "status": "started", "log": str(log_path)}


_SCHEMA_EXPECTED_COLUMNS: dict[str, set[str]] = {}
_SCHEMA_MIGRATABLE_COLUMNS: dict[str, set[str]] = {
    "messages": {"delivery_attempts", "owner_team_id"},
    "scheduled_events": {"owner_team_id"},
    "agent_health": {"owner_team_id"},
    "result_watchers": {"owner_team_id"},
}
_SCHEMA_ACTION_HINT = (
    "use team-agent advanced repair-state --schema to re-run column migrations; "
    "if that fails, back up .team/runtime/team.db, then delete it and rerun team-agent launch "
    "(in-flight messages will be lost)"
)


def _load_expected_schema_columns() -> dict[str, set[str]]:
    if _SCHEMA_EXPECTED_COLUMNS:
        return _SCHEMA_EXPECTED_COLUMNS
    from team_agent.message_store.schema import (
        AGENT_HEALTH_COLUMNS,
        DELIVERY_TOKEN_COLUMNS,
        MESSAGE_COLUMNS,
        PEER_ALLOWLIST_COLUMNS,
        RESULT_COLUMNS,
        RESULT_WATCHER_COLUMNS,
        SCHEDULED_EVENT_COLUMNS,
    )
    _SCHEMA_EXPECTED_COLUMNS.update(
        {
            "messages": set(MESSAGE_COLUMNS),
            "results": set(RESULT_COLUMNS),
            "scheduled_events": set(SCHEDULED_EVENT_COLUMNS),
            "delivery_tokens": set(DELIVERY_TOKEN_COLUMNS),
            "agent_health": set(AGENT_HEALTH_COLUMNS),
            "peer_allowlist": set(PEER_ALLOWLIST_COLUMNS),
            "result_watchers": set(RESULT_WATCHER_COLUMNS),
        }
    )
    return _SCHEMA_EXPECTED_COLUMNS


def _diagnose_schema_mismatch(workspace: Path, *, ignore_migratable: bool = False) -> dict[str, Any] | None:
    import sqlite3
    from team_agent.paths import runtime_dir
    db_path = runtime_dir(workspace) / "team.db"
    if not db_path.exists():
        return None
    conn = sqlite3.connect(db_path)
    try:
        for table, expected in _load_expected_schema_columns().items():
            present = conn.execute(
                "select name from sqlite_master where type='table' and name=?",
                (table,),
            ).fetchone()
            if present is None:
                continue
            actual = {row[1] for row in conn.execute(f"pragma table_info({table})").fetchall()}
            missing = expected - actual
            if ignore_migratable:
                migratable = _SCHEMA_MIGRATABLE_COLUMNS.get(table, set())
                missing = missing - migratable
            if missing:
                return {
                    "reason": "schema_mismatch",
                    "table": table,
                    "expected_columns": sorted(expected),
                    "actual_columns": sorted(actual),
                    "missing_columns": sorted(missing),
                }
    finally:
        conn.close()
    return None


def message_store_schema_health(workspace: Path) -> dict[str, Any]:
    schema_version = {"message_store_schema_version": MessageStore.SCHEMA_VERSION}
    pre_mismatch = _diagnose_schema_mismatch(workspace, ignore_migratable=True)
    if pre_mismatch is not None:
        return {
            "schema_ok": False,
            "schema_error": (
                f"team.db table {pre_mismatch['table']} is missing required column(s): "
                + ", ".join(pre_mismatch["missing_columns"])
            ),
            "schema": schema_version,
            "action": _SCHEMA_ACTION_HINT,
            **pre_mismatch,
        }
    try:
        MessageStore(workspace)
    except Exception as exc:
        post_init_mismatch = _diagnose_schema_mismatch(workspace) or {}
        return {
            "schema_ok": False,
            "schema_error": str(exc),
            "schema": schema_version,
            "action": _SCHEMA_ACTION_HINT,
            **post_init_mismatch,
        }
    return {
        "schema_ok": True,
        "schema_error": None,
        "schema": schema_version,
    }


def stop_coordinator(workspace: Path) -> dict[str, Any]:
    pid_path = coordinator_pid_path(workspace)
    if not pid_path.exists():
        return {"ok": True, "status": "missing"}
    try:
        pid = int(pid_path.read_text(encoding="utf-8").strip())
    except ValueError:
        pid_path.unlink(missing_ok=True)
        coordinator_meta_path(workspace).unlink(missing_ok=True)
        return {"ok": True, "status": "invalid_pid_removed"}
    if pid_is_running(pid):
        try:
            os.kill(pid, signal.SIGTERM)
        except OSError as exc:
            return {"ok": False, "status": "kill_failed", "pid": pid, "error": str(exc)}
    pid_path.unlink(missing_ok=True)
    coordinator_meta_path(workspace).unlink(missing_ok=True)
    EventLog(workspace).write("coordinator.stopped", pid=pid)
    return {"ok": True, "status": "stopped", "pid": pid}


def coordinator_tick(workspace: Path) -> dict[str, Any]:
    from team_agent.runtime import (
        _capture_missing_sessions,
        _deliver_pending_messages,
        _detect_stuck_agents,
        _fire_due_scheduled_events,
        _handle_provider_runtime_prompts,
        _handle_provider_startup_prompts,
        _refresh_agent_runtime_statuses,
        _sync_agent_health,
        _tmux_session_exists,
        _collect_results_and_notify_watchers,
    )
    from team_agent.messaging.idle_alerts import (
        detect_cross_worker_deadlocks,
    )
    from team_agent.idle_predicate import evaluate_takeover_reminder
    from team_agent.idle_takeover_wiring import build_idle_nodes, push_idle_reminder, IDLE_DEBOUNCE_SECONDS
    import time as _time
    from team_agent.messaging.activity_detector import detect_compaction_degradation
    from team_agent.messaging.leader_api_errors import detect_leader_api_errors
    from team_agent.messaging.session_drift import detect_session_drift
    from team_agent.state import load_runtime_state, save_runtime_state
    state = load_runtime_state(workspace)
    event_log = EventLog(workspace)
    store = MessageStore(workspace)
    session_name = state.get("session_name")
    if session_name and not _tmux_session_exists(session_name):
        event_log.write("coordinator.session_missing", session=session_name)
        return {"ok": False, "stop": True, "reason": "tmux_session_missing"}
    _capture_missing_sessions(workspace, state, event_log, timeout_s=0.0, log_miss=False)
    _refresh_agent_runtime_statuses(workspace, state, event_log)
    _handle_provider_startup_prompts(workspace, state, event_log)
    _handle_provider_runtime_prompts(workspace, state, event_log)
    captures = _sync_agent_health(workspace, state, store) or {}
    delivered = _deliver_pending_messages(workspace, state, event_log)
    fired = _fire_due_scheduled_events(workspace, store, event_log)
    stuck = _detect_stuck_agents(workspace, state, store, event_log)
    # Gap 32: the take-over reminder is driven by file-fact turn-state via the
    # idle_takeover predicate (the legacy screen-scrape obligation path is retired).
    _coord_meta = state.setdefault("coordinator", {})
    idle_nodes = build_idle_nodes(state)
    _record_unknown_idle_nodes(state, idle_nodes, event_log)
    idle_eval = evaluate_takeover_reminder(
        idle_nodes,
        monitor_state=_coord_meta.get("idle_takeover_monitor"),
        now_monotonic=_time.monotonic(),
        debounce_seconds=IDLE_DEBOUNCE_SECONDS,
        event_sink=lambda name, fields: event_log.write(name, **fields),
    )
    _coord_meta["idle_takeover_monitor"] = idle_eval.get("monitor_state")
    if idle_eval.get("should_ping"):
        push_idle_reminder(workspace, state, event_log, idle_eval)
    idle_alerts = (
        [{"alert_type": "idle_takeover", "message": idle_eval.get("message"),
          "reason": idle_eval.get("reason"), "interrupted": idle_eval.get("interrupted_nodes")}]
        if idle_eval.get("should_ping")
        else []
    )
    deadlock_alerts = detect_cross_worker_deadlocks(workspace, state, store, event_log)
    compaction_results: list[dict[str, Any]] = []
    for agent_id, agent_state in state.get("agents", {}).items():
        provider = str(agent_state.get("provider") or "")
        if provider != "codex":
            continue
        cap = captures.get(agent_id) or {}
        scrollback = str(cap.get("scrollback") or "")
        if not scrollback:
            continue
        stuck_loop = agent_id in (stuck or [])
        result = detect_compaction_degradation(
            workspace,
            state,
            event_log,
            agent_id=agent_id,
            provider=provider,
            scrollback=scrollback,
            stuck_loop=stuck_loop,
        )
        if result.get("event") and result.get("event") != "compaction_threshold_crossed.none":
            compaction_results.append(result)
    drift_results: list[dict[str, Any]] = []
    for agent_id, agent_state in state.get("agents", {}).items():
        if str(agent_state.get("provider") or "") != "codex":
            continue
        scrollback = str((captures.get(agent_id) or {}).get("scrollback") or "")
        if not scrollback:
            continue
        drift = detect_session_drift(
            workspace, state, event_log,
            agent_id=agent_id, agent_state=agent_state, scrollback=scrollback,
        )
        if drift:
            drift_results.append(drift)
    api_errors = detect_leader_api_errors(workspace, state, store, event_log)
    try:
        save_runtime_state(workspace, state)
    except Exception as exc:
        event_log.write("runtime.state.save_failed", phase="tick_end", error=str(exc), exc_type=type(exc).__name__)
        return {
            "ok": False,
            "stop": False,
            "reason": "persistence_degraded",
            "persisted": False,
            "error": str(exc),
            "delivered": delivered,
            "scheduled": fired,
            "stuck": stuck,
            "idle_alerts": idle_alerts,
            "deadlock_alerts": deadlock_alerts,
            "compaction": compaction_results,
            "session_drift": drift_results,
            "api_errors": api_errors,
        }
    results = _collect_results_and_notify_watchers(workspace, event_log)
    # Stage 12: prune the dedupe log every tick — cheap O(n) delete bounded by 24h window.
    from team_agent.message_store.leader_notification_log import prune_leader_notification_log
    try:
        pruned = prune_leader_notification_log(store, max_age_hours=24)
        if pruned:
            event_log.write("leader_notification.log_pruned", removed=pruned)
    except Exception as exc:
        event_log.write("leader_notification.prune_failed", error=str(exc))
    return {
        "ok": True,
        "stop": False,
        "delivered": delivered,
        "scheduled": fired,
        "stuck": stuck,
        "idle_alerts": idle_alerts,
        "deadlock_alerts": deadlock_alerts,
        "compaction": compaction_results,
        "session_drift": drift_results,
        "api_errors": api_errors,
        "results": results,
    }


def _record_unknown_idle_nodes(state: dict[str, Any], nodes: list[dict[str, Any]], event_log: EventLog) -> None:
    coordinator = state.setdefault("coordinator", {})
    unknown_ticks = coordinator.setdefault("unknown_ticks", {})
    current_unknown: set[str] = set()
    for node in nodes:
        node_id = str(node.get("node_id") or "")
        if not node_id:
            continue
        if node.get("state") == "unknown":
            current_unknown.add(node_id)
            count = int(unknown_ticks.get(node_id) or 0) + 1
            unknown_ticks[node_id] = count
            if count >= 60 and count % 12 == 0:
                event_log.write(
                    "idle_takeover.unknown_persistent",
                    node_id=node_id,
                    provider=node.get("provider"),
                    auth_mode=node.get("auth_mode"),
                    consecutive_ticks=count,
                    rollout_path=node.get("rollout_path"),
                )
    for node_id in list(unknown_ticks):
        if node_id not in current_unknown:
            unknown_ticks.pop(node_id, None)
