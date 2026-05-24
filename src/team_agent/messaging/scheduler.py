from __future__ import annotations

from team_agent.messaging.deps import (
    EventLog,
    MessageStore,
    check_team_owner,
    datetime,
    json,
    load_runtime_state,
    load_spec,
    save_runtime_state,
    send_message,
    timedelta,
    timezone,
)

from pathlib import Path
from typing import Any

_ACTIVE_TASK_STATUSES = {"pending", "assigned", "in_progress", "ready", "running", "needs_retry"}
_INBOUND_WORK_STATUSES = {"pending", "accepted", "target_resolved", "injected"}
_DELIVERED_MESSAGE_STATUSES = {"visible", "submitted", "delivered", "acknowledged"}
_PROGRESS_EVENTS = {
    "mcp.report_result",
    "report_result.accepted",
    "send.deliver_attempt",
    "send.submitted",
    "leader_receiver.deliver_attempt",
    "leader_receiver.submitted",
    "communication.peer_mirrored",
}
_RESTART_RESET_EVENTS = {"restart.agent_start", "restart.complete", "reset_agent.complete", "start_agent.complete"}
_ALERT_TYPES = {"stuck", "idle_fallback"}

def _fire_due_scheduled_events(workspace: Path, store: MessageStore, event_log: EventLog) -> list[int]:
    fired: list[int] = []
    for row in store.due_scheduled_events():
        payload = json.loads(row["payload_json"] or "{}")
        try:
            if row["kind"] == "send":
                result = send_message(
                    workspace,
                    row["target"],
                    str(payload.get("content") or ""),
                    task_id=payload.get("task_id"),
                    sender=payload.get("sender", "coordinator"),
                    requires_ack=bool(payload.get("requires_ack", True)),
                    wait_visible=bool(payload.get("wait_visible", True)),
                    timeout=float(payload.get("timeout", 30)),
                )
            elif row["kind"] == "health_ping":
                result = {"ok": True, "status": "logged"}
                event_log.write("coordinator.health_ping", target=row["target"], payload=payload)
            else:
                result = {"ok": False, "error": f"unknown scheduled event kind: {row['kind']}"}
            if not result.get("ok") and row["kind"] == "send":
                retry = _schedule_send_retry(store, row, payload, result)
                if retry:
                    result = {**result, **retry}
                    store.mark_scheduled_event(int(row["id"]), "retry_scheduled", result)
                    event_log.write(
                        "coordinator.scheduled_retry",
                        id=row["id"],
                        retry_event_id=retry["retry_event_id"],
                        target=row["target"],
                        attempt=retry["next_attempt"],
                    )
                    fired.append(int(row["id"]))
                    continue
            store.mark_scheduled_event(int(row["id"]), "done" if result.get("ok") else "failed", result)
            fired.append(int(row["id"]))
        except Exception as exc:
            result = {"ok": False, "error": str(exc)}
            store.mark_scheduled_event(int(row["id"]), "failed", result)
            event_log.write("coordinator.scheduled_failed", id=row["id"], error=str(exc))
    return fired


def _schedule_send_retry(
    store: MessageStore,
    row: dict[str, Any],
    payload: dict[str, Any],
    result: dict[str, Any],
) -> dict[str, Any] | None:
    attempt = int(payload.get("attempt") or 1)
    max_attempts = int(payload.get("max_attempts") or 1)
    if attempt >= max_attempts:
        return None
    retry_payload = dict(payload)
    retry_payload["attempt"] = attempt + 1
    due_at = datetime.now(timezone.utc) + timedelta(seconds=min(2 * attempt, 5))
    retry_id = store.add_scheduled_event(due_at.isoformat(), row["target"], row["kind"], retry_payload)
    return {
        "retry_event_id": retry_id,
        "next_attempt": attempt + 1,
        "max_attempts": max_attempts,
        "retry_reason": result.get("reason") or result.get("error"),
    }


def _detect_stuck_agents(
    workspace: Path,
    state: dict[str, Any],
    store: MessageStore,
    event_log: EventLog,
) -> list[str]:
    spec_path = Path(state.get("spec_path", workspace / "team.spec.yaml"))
    spec = load_spec(spec_path) if spec_path.exists() else {}
    runtime_cfg = spec.get("runtime", {})
    stuck_timeout = int(runtime_cfg.get("stuck_timeout_sec", 300))
    push_min_interval = int(runtime_cfg.get("push_min_interval_sec", 60))
    health = store.agent_health()
    stuck: list[str] = []
    now = datetime.now(timezone.utc)
    for agent_id, row in health.items():
        if row.get("status") not in {"RUNNING"} or not row.get("last_output_at"):
            continue
        try:
            last = datetime.fromisoformat(row["last_output_at"])
        except ValueError:
            continue
        if last.tzinfo is None:
            last = last.replace(tzinfo=timezone.utc)
        if (now - last).total_seconds() < stuck_timeout:
            continue
        suppression = _active_alert_suppression(state, store, event_log, agent_id, "stuck")
        has_work, work_reason = _agent_has_stuck_relevant_work(state, store, agent_id)
        if not has_work:
            event_log.write("coordinator.agent_stuck_suppressed", agent_id=agent_id, reason="idle_no_work", last_output_at=row["last_output_at"])
            continue
        if suppression:
            continue
        progress_event = _recent_agent_progress_event(event_log, agent_id, last)
        if progress_event:
            event_log.write(
                "coordinator.agent_stuck_suppressed",
                agent_id=agent_id,
                reason="recent_progress_event",
                progress_event=progress_event.get("event"),
                progress_ts=progress_event.get("ts"),
                last_output_at=row["last_output_at"],
                work_reason=work_reason,
            )
            continue
        stuck.append(agent_id)
        state.setdefault("coordinator", {})
        push_key = f"last_stuck_push_at:{agent_id}"
        last_push_raw = state["coordinator"].get(push_key)
        should_push = True
        if last_push_raw:
            try:
                last_push = datetime.fromisoformat(last_push_raw)
                if last_push.tzinfo is None:
                    last_push = last_push.replace(tzinfo=timezone.utc)
                should_push = (now - last_push).total_seconds() >= push_min_interval
            except ValueError:
                should_push = True
        event_log.write("coordinator.agent_stuck", agent_id=agent_id, last_output_at=row["last_output_at"], work_reason=work_reason)
        if should_push:
            state["coordinator"][push_key] = now.isoformat()
            try:
                send_message(
                    workspace,
                    "leader",
                    f"agent {agent_id} appears stuck: no output for {stuck_timeout}s",
                    sender="coordinator",
                    requires_ack=False,
                    wait_visible=False,
                )
            except Exception as exc:
                event_log.write("coordinator.stuck_push_failed", agent_id=agent_id, error=str(exc))
    return stuck


def stuck_list(workspace: Path) -> dict[str, Any]:
    state = load_runtime_state(workspace)
    suppressed = state.get("coordinator", {}).get("suppressed_idle_alerts", {})
    return {"ok": True, "suppressed_idle_alerts": suppressed}


def stuck_cancel(
    workspace: Path,
    agent_id: str,
    alert_type: str = "stuck",
    suppressed_by: str = "leader",
) -> dict[str, Any]:
    if alert_type == "all":
        alert_types = sorted(_ALERT_TYPES)
    elif alert_type in _ALERT_TYPES:
        alert_types = [alert_type]
    else:
        return {"ok": False, "status": "refused", "reason": "invalid_alert_type", "alert_type": alert_type}
    state = load_runtime_state(workspace)
    gate = check_team_owner(state)
    if gate:
        return gate
    store = MessageStore(workspace)
    coordinator = state.setdefault("coordinator", {})
    suppressed = coordinator.setdefault("suppressed_idle_alerts", {})
    agent_suppressions = suppressed.setdefault(agent_id, {})
    now = datetime.now(timezone.utc).isoformat()
    snapshot = _agent_alert_snapshot(state, store, agent_id)
    for item in alert_types:
        agent_suppressions[item] = {
            "suppressed_at": now,
            "suppressed_by": suppressed_by,
            "snapshot": snapshot,
        }
    save_runtime_state(workspace, state)
    EventLog(workspace).write("coordinator.idle_alert_suppressed", agent_id=agent_id, alert_types=alert_types, suppressed_by=suppressed_by)
    return {"ok": True, "agent_id": agent_id, "alert_types": alert_types, "suppressed": agent_suppressions}


def _active_alert_suppression(
    state: dict[str, Any],
    store: MessageStore,
    event_log: EventLog,
    agent_id: str,
    alert_type: str,
) -> dict[str, Any] | None:
    entry = state.get("coordinator", {}).get("suppressed_idle_alerts", {}).get(agent_id, {}).get(alert_type)
    if not isinstance(entry, dict):
        return None
    cleared = _suppression_clear_reason(state, store, event_log, agent_id, entry)
    if cleared:
        _clear_alert_suppression(state, agent_id, alert_type)
        event_log.write("coordinator.idle_alert_suppression_cleared", agent_id=agent_id, alert_type=alert_type, reason=cleared)
        return None
    return entry


def _suppression_clear_reason(
    state: dict[str, Any],
    store: MessageStore,
    event_log: EventLog,
    agent_id: str,
    entry: dict[str, Any],
) -> str | None:
    previous = entry.get("snapshot") if isinstance(entry.get("snapshot"), dict) else {}
    current = _agent_alert_snapshot(state, store, agent_id)
    if current.get("assigned_task_ids") != previous.get("assigned_task_ids"):
        return "task_assignment_changed"
    if current.get("delivered_message_ids") != previous.get("delivered_message_ids"):
        return "inbound_delivery_changed"
    try:
        suppressed_at = datetime.fromisoformat(str(entry.get("suppressed_at")))
    except ValueError:
        return "invalid_suppression_timestamp"
    if suppressed_at.tzinfo is None:
        suppressed_at = suppressed_at.replace(tzinfo=timezone.utc)
    if _recent_agent_progress_event(event_log, agent_id, suppressed_at):
        return "progress_event"
    if _recent_restart_or_reset_event(event_log, agent_id, suppressed_at):
        return "restart_or_reset"
    return None


def _clear_alert_suppression(state: dict[str, Any], agent_id: str, alert_type: str) -> None:
    suppressed = state.get("coordinator", {}).get("suppressed_idle_alerts", {})
    agent_suppressions = suppressed.get(agent_id, {})
    agent_suppressions.pop(alert_type, None)
    if not agent_suppressions:
        suppressed.pop(agent_id, None)


def _agent_alert_snapshot(state: dict[str, Any], store: MessageStore, agent_id: str) -> dict[str, Any]:
    assigned_task_ids = sorted(str(task.get("id")) for task in state.get("tasks", []) if task.get("assignee") == agent_id)
    delivered_message_ids = sorted(
        str(message.get("message_id"))
        for message in store.messages()
        if message.get("recipient") == agent_id and message.get("status") in _DELIVERED_MESSAGE_STATUSES
    )
    return {"assigned_task_ids": assigned_task_ids, "delivered_message_ids": delivered_message_ids}


def _agent_has_stuck_relevant_work(state: dict[str, Any], store: MessageStore, agent_id: str) -> tuple[bool, str]:
    for task in state.get("tasks", []):
        if task.get("assignee") == agent_id and task.get("status", "pending") in _ACTIVE_TASK_STATUSES:
            return True, "active_task"
    for message in store.messages():
        if message.get("recipient") == agent_id and message.get("status") in _INBOUND_WORK_STATUSES:
            return True, "inbound_message"
    return False, "idle_no_work"


def _recent_agent_progress_event(event_log: EventLog, agent_id: str, since: datetime) -> dict[str, Any] | None:
    for event in reversed(event_log.tail(200)):
        if event.get("event") not in _PROGRESS_EVENTS:
            continue
        if not _event_mentions_agent(event, agent_id):
            continue
        try:
            ts = datetime.fromisoformat(str(event.get("ts")))
        except ValueError:
            continue
        if ts.tzinfo is None:
            ts = ts.replace(tzinfo=timezone.utc)
        if ts >= since:
            return event
    return None


def _event_mentions_agent(event: dict[str, Any], agent_id: str) -> bool:
    if event.get("agent_id") == agent_id or event.get("sender") == agent_id or event.get("target") == agent_id:
        return True
    payload = event.get("payload")
    return isinstance(payload, dict) and (payload.get("from") == agent_id or payload.get("to") == agent_id)


def _recent_restart_or_reset_event(event_log: EventLog, agent_id: str, since: datetime) -> dict[str, Any] | None:
    for event in reversed(event_log.tail(200)):
        if event.get("event") not in _RESTART_RESET_EVENTS:
            continue
        if event.get("agent_id") != agent_id and agent_id not in set(event.get("agents") or []):
            continue
        try:
            ts = datetime.fromisoformat(str(event.get("ts")))
        except ValueError:
            continue
        if ts.tzinfo is None:
            ts = ts.replace(tzinfo=timezone.utc)
        if ts >= since:
            return event
    return None
