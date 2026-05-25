from __future__ import annotations

import json
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

from team_agent.events import EventLog
from team_agent.message_store import MessageStore
from team_agent.message_store.result_watchers import (
    claim_leader_notification,
    leader_notified_message_id_for_result,
    promote_leader_notification_id,
    release_leader_notification_claim,
)
from team_agent.messaging.deps import send_message
from team_agent.messaging.internal_delivery import deliver_stored_message
import uuid as _uuid

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
    # Stage 11.12 Gap 32 atomic UPSERT: collapse the check-then-write race that recurred
    # in edac6b3 / 9f52048. claim_leader_notification serializes concurrent notify calls
    # for the same (team, result_id) via a single SQLite BEGIN IMMEDIATE transaction.
    # Only the caller that receives 'claimed_by_you' fires the actual deliver_attempt;
    # every other concurrent caller receives 'already_notified_by' and dedupes silently.
    result_id_str = str(result.get("result_id") or "") or None
    if result_id_str:
        proposed_token = f"claim:{_uuid.uuid4().hex}"
        claim = claim_leader_notification(
            store,
            primary.get("owner_team_id"),
            result_id_str,
            primary["watcher_id"],
            proposed_token,
        )
        if claim["status"] == "already_notified_by":
            canonical = claim["canonical_message_id"]
            reason = dedupe_reason or _infer_dedupe_reason(primary, store)
            notified.append(_mark_watcher_dedupe_skip(
                store, event_log, primary, result, attempts, canonical, reason,
            ))
            return notified
        if claim["status"] == "claimed_by_you":
            existing = delivered_result_message(
                store, result_id_str,
                task_id=result.get("task_id"),
                owner_team_id=primary.get("owner_team_id"),
            )
            if existing and existing.get("message_id") != proposed_token:
                # A prior leader-pane delivery already landed for this result via the
                # legacy delivered_result_message scan (e.g. older watcher in another row).
                # Promote our sentinel to that canonical id and treat as dedupe.
                promote_leader_notification_id(store, primary["watcher_id"], proposed_token, existing["message_id"])
                notified.append(_mark_watcher_already_delivered(store, event_log, primary, result, attempts, existing))
                return notified
            if attempts >= _RESULT_DELIVERY_MAX_ATTEMPTS:
                release_leader_notification_claim(store, primary["watcher_id"], proposed_token)
                notified.append(_mark_delivery_exhausted(store, event_log, primary, result, attempts))
            else:
                notified.append(_deliver_result_to_watcher(
                    workspace, store, event_log, primary, result, attempts,
                    claim_token=proposed_token,
                ))
            return notified
    # Fallback path when result has no result_id (legacy contract). Keep the existing
    # delivered_result_message scan to avoid regressions on that branch.
    existing = delivered_result_message(
        store, str(result.get("result_id") or ""),
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
    # Stage 11.12: do NOT write notified_message_id to the loser's watcher row. The Gap 32
    # canonical-id invariant requires at most one watcher row to carry the canonical for a
    # given (team, result_id). Writing the canonical (which may be a sentinel during the
    # winner's in-flight delivery) into N loser rows pollutes the future
    # leader_notified_message_id_for_result lookup with stale sentinels. Status moves to
    # 'notified' so retry scans skip this watcher; the canonical lives only on the winner.
    store.mark_result_watcher(
        watcher["watcher_id"],
        "notified",
        result_id=result.get("result_id"),
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
    *,
    claim_token: str | None = None,
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
        if claim_token:
            release_leader_notification_claim(store, watcher["watcher_id"], claim_token)
        return _mark_delivery_failed(store, event_log, watcher, result, attempts, str(exc))
    status = "notified" if delivery.get("ok") else "notify_failed"
    error = delivery.get("reason") or delivery.get("error")
    # Stage 11.12 Gap 32: if the caller pre-claimed via claim_leader_notification, the
    # watcher's notified_message_id already holds our sentinel claim_token. On delivery
    # success we promote the sentinel to the real message_id (atomic compare-and-update).
    # On failure we RELEASE the sentinel so the next retry path can re-claim — clearing
    # back to NULL only when our own sentinel is still present (no clobbering of a
    # concurrent caller's claim).
    if claim_token:
        if delivery.get("ok") and delivery.get("message_id"):
            promote_leader_notification_id(store, watcher["watcher_id"], claim_token, delivery["message_id"])
        else:
            release_leader_notification_claim(store, watcher["watcher_id"], claim_token)
        store.mark_result_watcher(
            watcher["watcher_id"], status,
            result_id=result.get("result_id"),
            error=error,
        )
    else:
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


def requeue_after_claim_leader(
    workspace: Path,
    store: MessageStore,
    event_log: EventLog,
    owner_team_id: str,
    claimed_pane_id: str,
    *,
    incident_ts: str | None = None,
) -> list[dict[str, Any]]:
    """Post-claim hook (Gap 26 / Mac mini Stage 11 Scenarios 3, 11.10): re-route every
    not-yet-delivered leader-bound notification to the newly claimed pane. Returns the
    list of requeued watcher records (may be empty).

    Stage 11.10 semantic reframe: claim-leader means "all not-yet-delivered leader-bound
    notifications for this team_id reroute to the claimed pane". Watcher status is
    irrelevant — `notified_message_id` is the only dedupe gate. Gap 32 exactly-once
    contract still holds: notified_message_id non-null blocks redelivery.

    Selection rules:
      - watcher is scoped to this team (owner_team_id match)
      - watcher has no notified_message_id (Gap 32 once-only)
      - watcher's latest activity timestamp (completed_at fallback created_at) is
        at-or-after incident_ts when provided; without an incident_ts every
        un-notified watcher is requeued.
      - watcher status is otherwise ignored (pending / delivery_blocked /
        delivery_exhausted / notify_failed all become candidates).

    Atomicity vs coordinator's own scheduled retry: just before flipping a watcher's
    status, re-fetch the row from the store. If notified_message_id became non-null
    in the gap (the scheduled retry beat us), emit a benign
    leader_receiver.claim_requeue_already_in_flight event and skip. If the race
    leaks past this check, Gap 32 dedupe inside notify_result_watchers still
    guarantees exactly-once injection.
    """
    # Stage 11.12: CAS re-fetch + claim_requeue_already_in_flight event retired. The atomic
    # UPSERT in notify_result_watchers (claim_leader_notification) is now the single race
    # gate. We mark eligible watchers to notify_failed and let retry_result_deliveries route
    # through the UPSERT — concurrent claim/scheduled-retry paths both pass through the
    # same atomic claim and only one fires deliver_attempt.
    incident_dt = _parse_iso(incident_ts)
    requeued: list[dict[str, Any]] = []
    for watcher in store.result_watchers(owner_team_id=owner_team_id):
        if watcher.get("notified_message_id"):
            continue
        latest_ts = _parse_iso(watcher.get("completed_at")) or _parse_iso(watcher.get("created_at"))
        if incident_dt and latest_ts and latest_ts < incident_dt:
            continue
        watcher_id = watcher["watcher_id"]
        prior_state = str(watcher.get("status") or "")
        store.mark_result_watcher(
            watcher_id, "notify_failed",
            result_id=watcher.get("result_id"),
        )
        event_log.write(
            "leader_receiver.claim_requeue",
            result_id=watcher.get("result_id"),
            watcher_id=watcher_id,
            prior_state=prior_state,
            requeued_at=datetime.now(timezone.utc).isoformat(),
            claimed_pane_id=claimed_pane_id,
            team_id=owner_team_id,
        )
        requeued.append({
            "watcher_id": watcher_id,
            "result_id": watcher.get("result_id"),
            "prior_state": prior_state,
        })
    if requeued:
        try:
            retry_result_deliveries(workspace, event_log)
        except Exception as exc:
            event_log.write(
                "leader_receiver.claim_requeue_delivery_failed",
                error=str(exc),
                watcher_ids=[r["watcher_id"] for r in requeued],
                team_id=owner_team_id,
                claimed_pane_id=claimed_pane_id,
            )
    return requeued


def _parse_iso(text: Any) -> datetime | None:
    if not isinstance(text, str) or not text:
        return None
    try:
        dt = datetime.fromisoformat(text.replace("Z", "+00:00"))
    except ValueError:
        return None
    if dt.tzinfo is None:
        dt = dt.replace(tzinfo=timezone.utc)
    return dt


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
