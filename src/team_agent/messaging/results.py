from __future__ import annotations

from team_agent.messaging.deps import (
    EventLog,
    MessageStore,
    RuntimeError,
    ValidationError,
    _capture_missing_sessions,
    _deliver_pending_messages,
    _find_task,
    _find_task_or_none,
    _handle_provider_runtime_prompts,
    _handle_provider_startup_prompts,
    _is_message_scoped_result,
    _leader_id,
    _leader_receiver_is_direct,
    _notify_leader_of_report_result as _runtime_notify_leader_of_report_result,
    _rediscover_leader_receiver,
    _refresh_agent_runtime_statuses,
    _result_status_to_task_status,
    _validate_leader_receiver,
    copy,
    datetime,
    json,
    load_runtime_state,
    load_spec,
    save_runtime_state,
    send_message,
    start_coordinator,
    team_state_key,
    timezone,
    update_task_status,
    validate_result_envelope,
    write_team_state,
)

from pathlib import Path
from typing import Any

def collect(workspace: Path, result_file: Path | None = None, *, ensure_coordinator: bool = True) -> dict[str, Any]:
    state = load_runtime_state(workspace)
    spec_path = Path(state.get("spec_path", workspace / "team.spec.yaml"))
    spec = load_spec(spec_path)
    store = MessageStore(workspace)
    event_log = EventLog(workspace)
    _refresh_agent_runtime_statuses(workspace, state, event_log)
    _handle_provider_startup_prompts(workspace, state, event_log)
    _handle_provider_runtime_prompts(workspace, state, event_log)
    delivered_messages = _deliver_pending_messages(workspace, state, event_log)
    _capture_missing_sessions(workspace, state, event_log, timeout_s=0.0, log_miss=False)

    invalid_results: list[dict[str, Any]] = []
    if result_file:
        envelope: Any = None
        try:
            envelope = json.loads(result_file.read_text(encoding="utf-8"))
            validate_result_envelope(envelope)
        except (json.JSONDecodeError, ValidationError) as exc:
            invalid_results.append(
                _record_invalid_result(
                    event_log,
                    error=str(exc),
                    result_file=result_file,
                    envelope=envelope,
                )
            )
        else:
            store.add_result(envelope)

    rows = store.results(uncollected_only=True)
    valid_rows: list[tuple[dict[str, Any], dict[str, Any], dict[str, Any] | None]] = []
    for row in rows:
        envelope: Any = None
        try:
            envelope = json.loads(row["envelope"])
            validate_result_envelope(envelope)
            task = _find_task_or_none(state["tasks"], envelope["task_id"])
            if task is None and not _is_message_scoped_result(store, envelope):
                raise RuntimeError(f"unknown task id: {envelope['task_id']}")
        except (json.JSONDecodeError, ValidationError, RuntimeError) as exc:
            invalid_results.append(
                _record_invalid_result(
                    event_log,
                    error=str(exc),
                    result_id=row["result_id"],
                    envelope=envelope,
                )
            )
            store.mark_result_invalid(row["result_id"], str(exc))
        else:
            valid_rows.append((row, envelope, task))

    if invalid_results:
        save_runtime_state(workspace, state)
        state_path = write_team_state(workspace, spec, state, _team_state_result_entries(store, []))
        coordinator = _ensure_coordinator_after_collect(workspace, state, event_log) if ensure_coordinator else {"ok": False, "status": "not_required"}
        return {
            "ok": False,
            "collected": [],
            "collected_results": [],
            "delivered_messages": delivered_messages,
            "invalid_results": invalid_results,
            "results": store.result_counts(),
            "state_file": str(state_path),
            "coordinator": coordinator,
        }

    collected: list[dict[str, Any]] = []
    collected_results: list[dict[str, Any]] = []
    next_state = copy.deepcopy(state)
    for row, envelope, task in valid_rows:
        if task is not None:
            next_task = _find_task(next_state["tasks"], envelope["task_id"])
            task_status = _result_status_to_task_status(next_task, envelope["status"])
            update_task_status(
                next_state["tasks"],
                envelope["task_id"],
                task_status,
                envelope.get("summary"),
                envelope.get("artifacts", []),
            )
            next_task["accepted_result_id"] = row["result_id"]
        else:
            task_status = "message_scoped"
        collected.append(envelope)
        collected_results.append(
            {
                "result_id": row["result_id"],
                "task_id": envelope["task_id"],
                "agent_id": envelope["agent_id"],
                "status": envelope["status"],
                "summary": envelope.get("summary"),
                "tests": envelope.get("tests", []),
                "created_at": row.get("created_at"),
                "scope": "task" if task is not None else "message",
            }
        )
        event_log.write(
            "collect.result",
            result_id=row["result_id"],
            task_id=envelope["task_id"],
            status=envelope["status"],
            task_status=task_status,
            retry_count=task.get("retry_count") if task else None,
            retry_limit=task.get("retry_limit") if task else None,
            scope="task" if task is not None else "message",
        )
    state_path = write_team_state(workspace, spec, next_state, _team_state_result_entries(store, collected))
    save_runtime_state(workspace, next_state)
    for row, _, _ in valid_rows:
        store.mark_result_collected(row["result_id"])
    coordinator = _ensure_coordinator_after_collect(workspace, next_state, event_log) if ensure_coordinator else {"ok": False, "status": "not_required"}
    return {
        "ok": not invalid_results,
        "collected": collected,
        "collected_results": collected_results,
        "delivered_messages": delivered_messages,
        "invalid_results": invalid_results,
        "results": store.result_counts(),
        "state_file": str(state_path),
        "coordinator": coordinator,
    }


def _team_state_result_entries(store: MessageStore, collected: list[dict[str, Any]]) -> list[dict[str, Any]]:
    if collected:
        return [{"envelope": env} for env in collected]
    return [{"envelope": row["envelope"]} for row in store.latest_results(limit=5)]


def _ensure_coordinator_after_collect(workspace: Path, state: dict[str, Any], event_log: EventLog) -> dict[str, Any]:
    if not _coordinator_should_run(state):
        return {"ok": False, "status": "not_required"}
    try:
        coordinator = start_coordinator(workspace)
    except Exception as exc:
        coordinator = {"ok": False, "status": "start_failed", "error": str(exc)}
    event_log.write("collect.coordinator_checked", coordinator=coordinator)
    return coordinator


def _coordinator_should_run(state: dict[str, Any]) -> bool:
    return bool(state.get("session_name") or _leader_receiver_is_direct(state.get("leader_receiver", {})))


def report_result(workspace: Path, envelope: dict[str, Any]) -> dict[str, Any]:
    validate_result_envelope(envelope)
    store = MessageStore(workspace)
    owner_team_id = _owner_team_id_for_report(store, envelope)
    result_id = store.add_result(envelope, owner_team_id=owner_team_id)
    acknowledged = store.acknowledge_task_messages(envelope["task_id"], envelope["agent_id"], owner_team_id=owner_team_id)
    if not acknowledged:
        acknowledged = store.acknowledge_message(envelope["task_id"], envelope["agent_id"], owner_team_id=owner_team_id)
    event_log = EventLog(workspace)
    notification = _runtime_notify_leader_of_report_result(workspace, envelope, result_id, event_log, owner_team_id=owner_team_id)
    leader_notified = bool(notification.get("ok")) and notification.get("status") in {"submitted", "visible", "delivered", "acknowledged"}
    event_log.write(
        "mcp.report_result",
        result_id=result_id,
        task_id=envelope["task_id"],
        agent_id=envelope["agent_id"],
        acknowledged_messages=acknowledged,
        leader_notified=leader_notified,
        notification_message_id=notification.get("message_id"),
        notification_status=notification.get("status"),
        notification_channel=notification.get("channel"),
        notification_event_id=notification.get("event_id"),
        owner_team_id=owner_team_id,
    )
    return {
        "ok": True,
        "result_id": result_id,
        "task_id": envelope["task_id"],
        "agent_id": envelope["agent_id"],
        "acknowledged_messages": acknowledged,
        "leader_notified": leader_notified,
        "notification_message_id": notification.get("message_id"),
        "notification_status": notification.get("status"),
        "notification_channel": notification.get("channel"),
        "notification_event_id": notification.get("event_id"),
    }


def _notify_leader_of_report_result(
    workspace: Path,
    envelope: dict[str, Any],
    result_id: str,
    event_log: EventLog,
    owner_team_id: str | None = None,
) -> dict[str, Any]:
    state = load_runtime_state(workspace)
    if owner_team_id:
        state = _team_state_by_owner_id(state, owner_team_id) or state
    spec_path = Path(state.get("spec_path", workspace / "team.spec.yaml"))
    spec = load_spec(spec_path) if spec_path.exists() else {}
    leader_id = _leader_id(state, spec)
    state = _refresh_leader_receiver_or_flag_rebind(workspace, state, event_log, persist=owner_team_id is None)
    content = _format_report_result_notification(envelope, result_id)
    store = MessageStore(workspace)
    event_owner_team_id = owner_team_id or _state_owner_team_id(state)
    event_id = store.add_scheduled_event(
        datetime.now(timezone.utc).isoformat(),
        leader_id,
        "send",
        {
            "content": content,
            "task_id": envelope["task_id"],
            "sender": envelope["agent_id"],
            "requires_ack": False,
            "wait_visible": True,
            "timeout": 30.0,
            "max_attempts": 3,
        },
        owner_team_id=event_owner_team_id,
    )
    coordinator = {"ok": False, "status": "not_started"}
    if state.get("session_name") or _leader_receiver_is_direct(state.get("leader_receiver", {})):
        try:
            coordinator = start_coordinator(workspace)
        except Exception as exc:
            coordinator = {"ok": False, "status": "start_failed", "error": str(exc)}
    notification = {
        "ok": True,
        "status": "queued",
        "channel": "coordinator",
        "event_id": event_id,
        "coordinator": coordinator,
    }
    event_log.write(
        "mcp.report_result_notify_queued",
        result_id=result_id,
        task_id=envelope["task_id"],
        agent_id=envelope["agent_id"],
        event_id=event_id,
        target=leader_id,
        coordinator=coordinator,
        owner_team_id=event_owner_team_id,
    )
    return notification


def _owner_team_id_for_report(store: MessageStore, envelope: dict[str, Any]) -> str | None:
    for row in reversed(store.messages()):
        if row.get("recipient") != envelope["agent_id"]:
            continue
        if row.get("task_id") not in {envelope["task_id"], None} and row.get("message_id") != envelope["task_id"]:
            continue
        if row.get("owner_team_id"):
            return str(row["owner_team_id"])
    return None


def _team_state_by_owner_id(workspace_state: dict[str, Any], owner_team_id: str) -> dict[str, Any] | None:
    if team_state_key(workspace_state) == owner_team_id:
        return workspace_state
    teams = workspace_state.get("teams")
    if not isinstance(teams, dict):
        return None
    state = teams.get(owner_team_id)
    return state if isinstance(state, dict) else None


def _state_owner_team_id(state: dict[str, Any]) -> str | None:
    if state.get("session_name"):
        return team_state_key(state)
    return None


def _refresh_leader_receiver_or_flag_rebind(
    workspace: Path,
    state: dict[str, Any],
    event_log: EventLog,
    persist: bool = True,
) -> dict[str, Any]:
    receiver = state.get("leader_receiver") or {}
    if receiver.get("mode") != "direct_tmux":
        return state
    validation = _validate_leader_receiver(receiver)
    if validation.get("ok"):
        return state
    rediscovered = _rediscover_leader_receiver(receiver, event_log)
    if rediscovered.get("status") == "updated":
        state["leader_receiver"] = rediscovered["receiver"]
        if persist:
            save_runtime_state(workspace, state)
        event_log.write(
            "leader_receiver.rebind_applied",
            old_pane_id=receiver.get("pane_id"),
            new_pane_id=rediscovered["receiver"].get("pane_id"),
            reason=validation.get("reason"),
            source="report_result_notify",
        )
        return state
    event_log.write(
        "leader_receiver.rebind_required",
        old_pane_id=receiver.get("pane_id"),
        reason=validation.get("reason"),
        validation_error=validation.get("error"),
        rediscovery_status=rediscovered.get("status"),
        provider=receiver.get("provider"),
        source="report_result_notify",
    )
    return state


def _format_report_result_notification(envelope: dict[str, Any], result_id: str) -> str:
    lines = [
        f"Task {envelope['task_id']} reported {envelope['status']} from {envelope['agent_id']}: {envelope.get('summary') or 'completed'}",
        f"Result id: {result_id}",
        "Team Agent stored this result. The coordinator/collect path will update team_state.md; no manual polling loop is needed.",
    ]
    tests = envelope.get("tests") or []
    rendered_tests: list[str] = []
    for test in tests[:3]:
        if isinstance(test, dict):
            command = test.get("command") or "test"
            status = test.get("status") or "unknown"
            rendered_tests.append(f"{command}={status}")
    if rendered_tests:
        lines.insert(1, "Tests: " + "; ".join(rendered_tests))
    return "\n".join(lines)


def _record_invalid_result(
    event_log: EventLog,
    error: str,
    result_file: Path | None = None,
    result_id: str | None = None,
    envelope: Any = None,
) -> dict[str, Any]:
    task_id = envelope.get("task_id") if isinstance(envelope, dict) else None
    agent_id = envelope.get("agent_id") if isinstance(envelope, dict) else None
    event_log.write(
        "collect.invalid_result",
        result_id=result_id,
        result_file=str(result_file) if result_file else None,
        task_id=task_id,
        agent_id=agent_id,
        error=error,
    )
    return {
        "result_id": result_id,
        "path": str(result_file) if result_file else None,
        "task_id": task_id,
        "agent_id": agent_id,
        "error": error,
    }


def _collect_results_and_notify_watchers(workspace: Path, event_log: EventLog) -> dict[str, Any]:
    store = MessageStore(workspace)
    if not store.results(uncollected_only=True):
        return {"ok": True, "collected": 0, "notified": []}
    result = collect(workspace)
    if not result.get("ok"):
        event_log.write("coordinator.result_collect_failed", invalid_results=result.get("invalid_results", []))
        return {"ok": False, "collected": 0, "notified": [], "error": "collect_failed"}
    notified: list[dict[str, Any]] = []
    for item in result.get("collected_results", []):
        notified.extend(_notify_result_watchers(workspace, item, event_log))
    event_log.write(
        "coordinator.result_collect",
        collected=len(result.get("collected_results", [])),
        notified=len(notified),
    )
    return {"ok": True, "collected": len(result.get("collected_results", [])), "notified": notified}


def _notify_result_watchers(workspace: Path, result: dict[str, Any], event_log: EventLog) -> list[dict[str, Any]]:
    store = MessageStore(workspace)
    notified: list[dict[str, Any]] = []
    for watcher in store.pending_result_watchers():
        if not _watcher_matches_result(watcher, result):
            continue
        content = _format_result_watcher_notification(result)
        try:
            delivery = send_message(
                workspace,
                watcher.get("leader_id") or "leader",
                content,
                task_id=result.get("task_id"),
                sender="coordinator",
                requires_ack=False,
                wait_visible=False,
            )
        except Exception as exc:
            store.mark_result_watcher(
                watcher["watcher_id"],
                "notify_failed",
                result_id=result.get("result_id"),
                error=str(exc),
            )
            event_log.write("result_watcher.notify_failed", watcher_id=watcher["watcher_id"], error=str(exc))
            notified.append({"watcher_id": watcher["watcher_id"], "ok": False, "error": str(exc)})
            continue
        status = "notified" if delivery.get("ok") else "notify_failed"
        error = delivery.get("reason") or delivery.get("error")
        store.mark_result_watcher(
            watcher["watcher_id"],
            status,
            result_id=result.get("result_id"),
            notified_message_id=delivery.get("message_id"),
            error=error,
        )
        event_log.write(
            "result_watcher.notified",
            watcher_id=watcher["watcher_id"],
            result_id=result.get("result_id"),
            task_id=result.get("task_id"),
            agent_id=result.get("agent_id"),
            ok=bool(delivery.get("ok")),
            delivery_status=delivery.get("status"),
            message_id=delivery.get("message_id"),
            error=error,
        )
        notified.append(
            {
                "watcher_id": watcher["watcher_id"],
                "result_id": result.get("result_id"),
                "ok": bool(delivery.get("ok")),
                "message_id": delivery.get("message_id"),
            }
        )
    return notified


def _watcher_matches_result(watcher: dict[str, Any], result: dict[str, Any]) -> bool:
    task_id = watcher.get("task_id")
    agent_id = watcher.get("agent_id")
    task_matches = not task_id or task_id == result.get("task_id")
    agent_matches = not agent_id or agent_id == result.get("agent_id")
    return task_matches and agent_matches


def _format_result_watcher_notification(result: dict[str, Any]) -> str:
    task_id = result.get("task_id") or "unknown task"
    agent_id = result.get("agent_id") or "unknown agent"
    status = result.get("status") or "unknown"
    summary = result.get("summary") or "completed"
    lines = [
        f"Task {task_id} reported {status} from {agent_id}: {summary}",
        "Team Agent has collected this result and updated team_state.md. No manual polling is needed.",
    ]
    tests = result.get("tests") or []
    if tests:
        rendered_tests = []
        for test in tests[:3]:
            if isinstance(test, dict):
                command = test.get("command") or "test"
                test_status = test.get("status") or "unknown"
                rendered_tests.append(f"{command}={test_status}")
        if rendered_tests:
            lines.insert(1, "Tests: " + "; ".join(rendered_tests))
    return "\n".join(lines)
