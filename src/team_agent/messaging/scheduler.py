from __future__ import annotations

from team_agent.messaging.deps import (
    EventLog,
    MessageStore,
    datetime,
    json,
    load_spec,
    send_message,
    timedelta,
    timezone,
)

from pathlib import Path
from typing import Any

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
        event_log.write("coordinator.agent_stuck", agent_id=agent_id, last_output_at=row["last_output_at"])
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
