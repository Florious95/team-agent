from __future__ import annotations

import json
from pathlib import Path
from typing import Any

from team_agent.events import EventLog
from team_agent.message_store import MessageStore
from team_agent.message_store.result_watchers import leader_notified_message_id_for_result
from team_agent.messaging.deps import send_message
from team_agent.messaging.internal_delivery import deliver_stored_message

_RESULT_DELIVERY_MAX_ATTEMPTS = 5
_DELIVERED_RESULT_MESSAGE_STATUSES = {"visible", "submitted", "submitted_unverified", "delivered", "acknowledged"}


def retry_result_deliveries(workspace: Path, event_log: EventLog) -> list[dict[str, Any]]:
    store = MessageStore(workspace)
    notified: list[dict[str, Any]] = []
    for watcher in store.retryable_result_watchers():
        if watcher.get("status") != "notify_failed" or not watcher.get("result_id"):
            continue
        row = store.result_by_id(str(watcher["result_id"]))
        if not row:
            continue
        notified.extend(notify_result_watchers(
            workspace,
            _result_entry_from_row(row),
            event_log,
            watchers=[watcher],
            dedupe_reason="rebind_retry",
        ))
    return notified


def notify_result_watchers(
    workspace: Path,
    result: dict[str, Any],
    event_log: EventLog,
    watchers: list[dict[str, Any]] | None = None,
    dedupe_reason: str | None = None,
) -> list[dict[str, Any]]:
    store = MessageStore(workspace)
    candidates = [
        watcher
        for watcher in (watchers if watchers is not None else store.pending_result_watchers())
        if watcher_matches_result(watcher, result)
    ]
    if not candidates:
        return []
    primary, superseded = _dedupe_watchers_for_result(candidates)
    notified: list[dict[str, Any]] = []
    for stale in superseded:
        store.mark_result_watcher(
            stale["watcher_id"],
            "superseded",
            result_id=result.get("result_id"),
            error="superseded by earlier watcher for same (task_id, agent_id, result_id)",
        )
        event_log.write(
            "result_watcher.superseded",
            watcher_id=stale["watcher_id"],
            result_id=result.get("result_id"),
            task_id=result.get("task_id"),
            agent_id=result.get("agent_id"),
            primary_watcher_id=primary["watcher_id"],
        )
        notified.append(
            {
                "watcher_id": stale["watcher_id"],
                "result_id": result.get("result_id"),
                "ok": False,
                "status": "superseded",
                "primary_watcher_id": primary["watcher_id"],
            }
        )
    attempts = result_delivery_attempts(event_log, primary["watcher_id"], str(result.get("result_id") or ""))
    canonical_message_id = leader_notified_message_id_for_result(
        store, primary.get("owner_team_id"), str(result.get("result_id") or "") or None,
    )
    if canonical_message_id:
        reason = dedupe_reason or _infer_dedupe_reason(primary, store)
        notified.append(_mark_watcher_dedupe_skip(
            store, event_log, primary, result, attempts, canonical_message_id, reason,
        ))
        return notified
    existing = delivered_result_message(
        store,
        str(result.get("result_id") or ""),
        task_id=result.get("task_id"),
        owner_team_id=primary.get("owner_team_id"),
    )
    if existing:
        notified.append(_mark_watcher_already_delivered(store, event_log, primary, result, attempts, existing))
        return notified
    if attempts >= _RESULT_DELIVERY_MAX_ATTEMPTS:
        notified.append(_mark_delivery_exhausted(store, event_log, primary, result, attempts))
    else:
        notified.append(_deliver_result_to_watcher(workspace, store, event_log, primary, result, attempts))
    return notified


def _infer_dedupe_reason(primary: dict[str, Any], store: MessageStore) -> str:
    if primary.get("notified_message_id"):
        return "rebind_retry"
    return "watcher_duplicate"


def _mark_watcher_dedupe_skip(
    store: MessageStore,
    event_log: EventLog,
    watcher: dict[str, Any],
    result: dict[str, Any],
    attempts: int,
    canonical_message_id: str,
    reason: str,
) -> dict[str, Any]:
    original_message_id = watcher.get("notified_message_id")
    store.mark_result_watcher(
        watcher["watcher_id"],
        "notified",
        result_id=result.get("result_id"),
        notified_message_id=canonical_message_id,
    )
    event_log.write(
        "leader_receiver.notification_dedupe_skip",
        result_id=result.get("result_id"),
        original_message_id=original_message_id,
        suppressed_message_id=canonical_message_id,
        reason=reason,
        team_id=watcher.get("owner_team_id"),
        watcher_id=watcher["watcher_id"],
        task_id=result.get("task_id"),
        agent_id=result.get("agent_id"),
        attempt=attempts + 1,
    )
    return {
        "watcher_id": watcher["watcher_id"],
        "result_id": result.get("result_id"),
        "ok": True,
        "message_id": canonical_message_id,
        "deduped": True,
        "dedupe_reason": reason,
    }


def _dedupe_watchers_for_result(
    watchers: list[dict[str, Any]],
) -> tuple[dict[str, Any], list[dict[str, Any]]]:
    ordered = sorted(watchers, key=lambda w: (str(w.get("created_at") or ""), str(w.get("watcher_id") or "")))
    return ordered[0], ordered[1:]


def _deliver_result_to_watcher(
    workspace: Path,
    store: MessageStore,
    event_log: EventLog,
    watcher: dict[str, Any],
    result: dict[str, Any],
    attempts: int,
) -> dict[str, Any]:
    try:
        deliver = deliver_stored_message if watcher.get("owner_team_id") else send_message
        delivery = deliver(
            workspace,
            watcher.get("leader_id") or "leader",
            format_result_watcher_notification(result),
            task_id=result.get("task_id"),
            sender="coordinator",
            requires_ack=False,
            wait_visible=False,
            team=watcher.get("owner_team_id"),
        )
    except Exception as exc:
        return _mark_delivery_failed(store, event_log, watcher, result, attempts, str(exc))
    status = "notified" if delivery.get("ok") else "notify_failed"
    error = delivery.get("reason") or delivery.get("error")
    # Gap 32: only persist notified_message_id when delivery actually succeeded. Setting it on a
    # failed attempt (Phase D pre-hotfix-3 behavior) made downstream dedupe lie about prior
    # visibility, which 78055bc tried to paper over by nulling-on-requeue. Both bugs go away if
    # we never write the id for a failed injection in the first place.
    persisted_message_id = delivery.get("message_id") if delivery.get("ok") else None
    store.mark_result_watcher(
        watcher["watcher_id"],
        status,
        result_id=result.get("result_id"),
        notified_message_id=persisted_message_id,
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


def _mark_watcher_already_delivered(
    store: MessageStore,
    event_log: EventLog,
    watcher: dict[str, Any],
    result: dict[str, Any],
    attempts: int,
    message: dict[str, Any],
) -> dict[str, Any]:
    store.mark_result_watcher(
        watcher["watcher_id"],
        "notified",
        result_id=result.get("result_id"),
        notified_message_id=message.get("message_id"),
    )
    event_log.write(
        "result_watcher.notified",
        watcher_id=watcher["watcher_id"],
        result_id=result.get("result_id"),
        task_id=result.get("task_id"),
        agent_id=result.get("agent_id"),
        ok=True,
        delivery_status="already_delivered",
        message_id=message.get("message_id"),
        deduped=True,
        attempt=attempts,
    )
    return {
        "watcher_id": watcher["watcher_id"],
        "result_id": result.get("result_id"),
        "ok": True,
        "message_id": message.get("message_id"),
        "deduped": True,
    }


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
        if event.get("watcher_id") != watcher_id:
            continue
        if event.get("event") == "result_watcher.requeued":
            attempts = 0
            continue
        if event.get("result_id") != result_id:
            continue
        if event.get("event") in {"result_watcher.notified", "result_watcher.notify_failed"}:
            attempts += 1
    return attempts


def delivered_result_message(
    store: MessageStore,
    result_id: str,
    *,
    task_id: str | None = None,
    owner_team_id: str | None = None,
) -> dict[str, Any] | None:
    if not result_id:
        return None
    for message in reversed(store.messages(owner_team_id=owner_team_id)):
        if message.get("recipient") != "leader":
            continue
        if task_id and message.get("task_id") != task_id:
            continue
        if message.get("status") not in _DELIVERED_RESULT_MESSAGE_STATUSES:
            continue
        if f"Result id: {result_id}" in str(message.get("content") or ""):
            return message
    return None


def result_id_from_text(content: str) -> str | None:
    for line in content.splitlines():
        if line.startswith("Result id: "):
            return line.removeprefix("Result id: ").strip() or None
    return None


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
