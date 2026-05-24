from __future__ import annotations

import json
from pathlib import Path
from typing import Any

from team_agent.events import EventLog
from team_agent.message_store import MessageStore
from team_agent.messaging.deps import send_message

_RESULT_DELIVERY_MAX_ATTEMPTS = 5


def retry_result_deliveries(workspace: Path, event_log: EventLog) -> list[dict[str, Any]]:
    store = MessageStore(workspace)
    notified: list[dict[str, Any]] = []
    for watcher in store.retryable_result_watchers():
        if watcher.get("status") != "notify_failed" or not watcher.get("result_id"):
            continue
        row = store.result_by_id(str(watcher["result_id"]))
        if not row:
            continue
        notified.extend(notify_result_watchers(workspace, _result_entry_from_row(row), event_log, watchers=[watcher]))
    return notified


def notify_result_watchers(
    workspace: Path,
    result: dict[str, Any],
    event_log: EventLog,
    watchers: list[dict[str, Any]] | None = None,
) -> list[dict[str, Any]]:
    store = MessageStore(workspace)
    notified: list[dict[str, Any]] = []
    for watcher in watchers if watchers is not None else store.pending_result_watchers():
        if not watcher_matches_result(watcher, result):
            continue
        attempts = result_delivery_attempts(event_log, watcher["watcher_id"], str(result.get("result_id") or ""))
        if attempts >= _RESULT_DELIVERY_MAX_ATTEMPTS:
            notified.append(_mark_delivery_exhausted(store, event_log, watcher, result, attempts))
            continue
        notified.append(_deliver_result_to_watcher(workspace, store, event_log, watcher, result, attempts))
    return notified


def _deliver_result_to_watcher(
    workspace: Path,
    store: MessageStore,
    event_log: EventLog,
    watcher: dict[str, Any],
    result: dict[str, Any],
    attempts: int,
) -> dict[str, Any]:
    try:
        delivery = send_message(
            workspace,
            watcher.get("leader_id") or "leader",
            format_result_watcher_notification(result),
            task_id=result.get("task_id"),
            sender="coordinator",
            requires_ack=False,
            wait_visible=False,
        )
    except Exception as exc:
        return _mark_delivery_failed(store, event_log, watcher, result, attempts, str(exc))
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
        attempt=attempts + 1,
    )
    return {
        "watcher_id": watcher["watcher_id"],
        "result_id": result.get("result_id"),
        "ok": bool(delivery.get("ok")),
        "message_id": delivery.get("message_id"),
    }


def _mark_delivery_failed(
    store: MessageStore,
    event_log: EventLog,
    watcher: dict[str, Any],
    result: dict[str, Any],
    attempts: int,
    error: str,
) -> dict[str, Any]:
    store.mark_result_watcher(watcher["watcher_id"], "notify_failed", result_id=result.get("result_id"), error=error)
    event_log.write(
        "result_watcher.notify_failed",
        watcher_id=watcher["watcher_id"],
        result_id=result.get("result_id"),
        attempt=attempts + 1,
        error=error,
    )
    return {"watcher_id": watcher["watcher_id"], "result_id": result.get("result_id"), "ok": False, "error": error}


def _mark_delivery_exhausted(
    store: MessageStore,
    event_log: EventLog,
    watcher: dict[str, Any],
    result: dict[str, Any],
    attempts: int,
) -> dict[str, Any]:
    error = "result delivery retry budget exhausted"
    store.mark_result_watcher(watcher["watcher_id"], "delivery_exhausted", result_id=result.get("result_id"), error=error)
    event_log.write(
        "result_delivery_exhausted",
        watcher_id=watcher["watcher_id"],
        result_id=result.get("result_id"),
        task_id=result.get("task_id"),
        agent_id=result.get("agent_id"),
        attempts=attempts,
        last_error=watcher.get("error"),
    )
    return {"watcher_id": watcher["watcher_id"], "result_id": result.get("result_id"), "ok": False, "error": error}


def _result_entry_from_row(row: dict[str, Any]) -> dict[str, Any]:
    envelope = json.loads(row["envelope"])
    return {
        "result_id": row["result_id"],
        "task_id": envelope.get("task_id"),
        "agent_id": envelope.get("agent_id"),
        "status": envelope.get("status"),
        "summary": envelope.get("summary"),
        "tests": envelope.get("tests", []),
        "created_at": row.get("created_at"),
        "scope": "task",
    }


def result_delivery_attempts(event_log: EventLog, watcher_id: str, result_id: str) -> int:
    attempts = 0
    for event in event_log.tail(500):
        if event.get("watcher_id") == watcher_id and event.get("result_id") == result_id:
            if event.get("event") in {"result_watcher.notified", "result_watcher.notify_failed"}:
                attempts += 1
    return attempts


def watcher_matches_result(watcher: dict[str, Any], result: dict[str, Any]) -> bool:
    task_id = watcher.get("task_id")
    agent_id = watcher.get("agent_id")
    return (not task_id or task_id == result.get("task_id")) and (not agent_id or agent_id == result.get("agent_id"))


def format_result_watcher_notification(result: dict[str, Any]) -> str:
    task_id = result.get("task_id") or "unknown task"
    agent_id = result.get("agent_id") or "unknown agent"
    status = result.get("status") or "unknown"
    summary = result.get("summary") or "completed"
    lines = [
        f"Task {task_id} reported {status} from {agent_id}: {summary}",
        "Team Agent has collected this result and updated team_state.md. No manual polling is needed.",
    ]
    if result.get("result_id"):
        lines.insert(1, f"Result id: {result['result_id']}")
    rendered_tests = [
        f"{test.get('command') or 'test'}={test.get('status') or 'unknown'}"
        for test in (result.get("tests") or [])[:3]
        if isinstance(test, dict)
    ]
    if rendered_tests:
        lines.insert(1, "Tests: " + "; ".join(rendered_tests))
    return "\n".join(lines)
