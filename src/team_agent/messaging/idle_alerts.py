from __future__ import annotations

from datetime import datetime, timezone
from pathlib import Path
from typing import Any

from team_agent.events import EventLog
from team_agent.message_store import MessageStore
from team_agent.messaging.deps import load_spec, save_runtime_state, team_state_key
from team_agent.messaging.internal_delivery import deliver_stored_message


_UNDELIVERED_MESSAGE_STATUSES = {
    "pending",
    "accepted",
    "queued_until_idle",
    "queued_until_start",
    "queued_stopped",
    "queued_pane_missing",
    "failed",
    "delivery_blocked",
    "injected_unverified",
}


def _team_undelivered_obligations(
    state: dict[str, Any],
    store: MessageStore,
    owner_team_id: str,
    active_task_statuses: set[str],
) -> list[dict[str, Any]]:
    obligations: list[dict[str, Any]] = []
    for message in store.messages(owner_team_id=owner_team_id):
        if message.get("status") in _UNDELIVERED_MESSAGE_STATUSES:
            obligations.append(
                {
                    "kind": "undelivered_message",
                    "message_id": message.get("message_id"),
                    "recipient": message.get("recipient"),
                    "status": message.get("status"),
                }
            )
    for watcher in store.retryable_result_watchers():
        if watcher.get("status") in {"pending", "notify_failed"}:
            obligations.append(
                {
                    "kind": "pending_result_watcher",
                    "watcher_id": watcher.get("watcher_id"),
                    "task_id": watcher.get("task_id"),
                    "agent_id": watcher.get("agent_id"),
                }
            )
    for task in state.get("tasks", []):
        if task.get("status", "pending") in active_task_statuses and task.get("assignee"):
            obligations.append(
                {
                    "kind": "active_task",
                    "task_id": task.get("id"),
                    "assignee": task.get("assignee"),
                    "status": task.get("status"),
                }
            )
    return obligations


def _all_workers_idle(
    state: dict[str, Any],
    store: MessageStore,
    owner_team_id: str,
) -> tuple[bool, list[str]]:
    health = store.agent_health(owner_team_id=owner_team_id)
    worker_ids = list(state.get("agents", {}).keys()) or list(health.keys())
    if not worker_ids:
        return False, []
    idle: list[str] = []
    for agent_id in worker_ids:
        row = health.get(agent_id) or {}
        status = str(row.get("status") or "").lower()
        if status != "idle":
            return False, []
        idle.append(agent_id)
    return True, idle


def _register_unified_alert(
    state: dict[str, Any],
    owner_team_id: str,
    agent_id: str,
    alert_type: str,
    snapshot: dict[str, Any],
    suppressed_by: str,
    now: datetime,
) -> dict[str, Any]:
    coordinator = state.setdefault("coordinator", {})
    suppressed = coordinator.setdefault("suppressed_idle_alerts", {})
    team_suppressions = suppressed.setdefault(owner_team_id, {})
    agent_suppressions = team_suppressions.setdefault(agent_id, {})
    entry = {
        "suppressed_at": now.isoformat(),
        "suppressed_by": suppressed_by,
        "snapshot": snapshot,
    }
    agent_suppressions[alert_type] = entry
    return entry


def detect_idle_fallbacks(
    workspace: Path,
    state: dict[str, Any],
    store: MessageStore,
    event_log: EventLog,
    now: datetime | None = None,
) -> list[dict[str, Any]]:
    from team_agent.messaging.scheduler import (
        _ACTIVE_TASK_STATUSES,
        _active_alert_suppression,
        _agent_alert_snapshot,
    )
    now = now or datetime.now(timezone.utc)
    owner_team_id = team_state_key(state)
    obligations = _team_undelivered_obligations(state, store, owner_team_id, _ACTIVE_TASK_STATUSES)
    if not obligations:
        return []
    all_idle, idle_workers = _all_workers_idle(state, store, owner_team_id)
    if not all_idle:
        return []
    spec_path = Path(state.get("spec_path", workspace / "team.spec.yaml"))
    spec = load_spec(spec_path) if spec_path.exists() else {}
    leader_id = state.get("leader", {}).get("id") or spec.get("leader", {}).get("id") or "leader"
    alerts: list[dict[str, Any]] = []
    for agent_id in idle_workers:
        suppression = _active_alert_suppression(state, store, event_log, agent_id, "idle_fallback")
        if suppression:
            continue
        snapshot = _agent_alert_snapshot(state, store, agent_id, owner_team_id)
        _register_unified_alert(state, owner_team_id, agent_id, "idle_fallback", snapshot, "coordinator", now)
        alerts.append({"agent_id": agent_id, "alert_type": "idle_fallback", "obligations": obligations})
    if not alerts:
        return []
    save_runtime_state(workspace, state)
    content = (
        "There is still unfinished work. Continue coordinating, deliver a result, "
        "or acknowledge that this idle state is intentional via team-agent acknowledge-idle."
    )
    try:
        deliver_stored_message(
            workspace,
            leader_id,
            content,
            sender="coordinator",
            requires_ack=False,
            wait_visible=False,
            team=owner_team_id,
        )
    except Exception as exc:
        event_log.write("coordinator.idle_fallback_push_failed", error=str(exc), team=owner_team_id)
    event_log.write(
        "coordinator.idle_fallback",
        team=owner_team_id,
        idle_workers=idle_workers,
        obligation_count=len(obligations),
        alert_count=len(alerts),
    )
    return alerts


def detect_cross_worker_deadlocks(
    workspace: Path,
    state: dict[str, Any],
    store: MessageStore,
    event_log: EventLog,
    now: datetime | None = None,
) -> list[dict[str, Any]]:
    from team_agent.messaging.scheduler import (
        _active_alert_suppression,
        _agent_alert_snapshot,
    )
    now = now or datetime.now(timezone.utc)
    owner_team_id = team_state_key(state)
    health = store.agent_health(owner_team_id=owner_team_id)
    candidate_recipients: dict[str, list[dict[str, Any]]] = {}
    for message in store.messages(owner_team_id=owner_team_id):
        if message.get("status") not in _UNDELIVERED_MESSAGE_STATUSES:
            continue
        recipient = message.get("recipient")
        if not recipient:
            continue
        candidate_recipients.setdefault(str(recipient), []).append(message)
    alerts: list[dict[str, Any]] = []
    for agent_id, messages in candidate_recipients.items():
        row = health.get(agent_id) or {}
        status = str(row.get("status") or "").lower()
        if status != "idle":
            continue
        suppression = _active_alert_suppression(state, store, event_log, agent_id, "cross_worker_deadlock")
        if suppression:
            continue
        snapshot = _agent_alert_snapshot(state, store, agent_id, owner_team_id)
        snapshot["pending_message_ids"] = sorted(str(m.get("message_id")) for m in messages)
        _register_unified_alert(state, owner_team_id, agent_id, "cross_worker_deadlock", snapshot, "coordinator", now)
        alerts.append(
            {
                "agent_id": agent_id,
                "alert_type": "cross_worker_deadlock",
                "pending_messages": snapshot["pending_message_ids"],
            }
        )
    if not alerts:
        return []
    save_runtime_state(workspace, state)
    event_log.write(
        "coordinator.cross_worker_deadlock",
        team=owner_team_id,
        agent_ids=[alert["agent_id"] for alert in alerts],
        alert_count=len(alerts),
    )
    return alerts
