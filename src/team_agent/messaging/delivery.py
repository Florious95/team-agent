from __future__ import annotations

from team_agent.messaging.deps import (
    EventLog,
    MessageStore,
    _message_by_id,
    _message_payload,
    _tmux_inject_text,
    _tmux_window_exists,
    core_render_message,
)

from datetime import datetime, timedelta, timezone
from pathlib import Path
from typing import Any


# Spark MEDIUM sweep #3 (2026-05-26): retry_needed bounded backoff. Each entry is
# the delay (seconds) BEFORE the attempt with that number runs; attempt 1 was the
# original delivery, attempt 2 fires 5s after retry_needed, attempt 3 fires 15s
# after the previous, attempt 4 fires 30s after the previous. _TRUST_RETRY_MAX_ATTEMPTS
# bounds the total — the 4th retry_needed is terminal and emits
# leader_panes.trust_auto_answer_exhausted.
_TRUST_RETRY_BACKOFF_SECONDS = {2: 5, 3: 15, 4: 30}
_TRUST_RETRY_MAX_ATTEMPTS = 4

def _deliver_pending_message(
    workspace: Path,
    state: dict[str, Any],
    message_id: str,
    wait_visible: bool = True,
    timeout: float = 30.0,
    *,
    _trust_retry_attempt: int = 1,
) -> dict[str, Any]:
    store = MessageStore(workspace)
    row = next((m for m in store.messages() if m["message_id"] == message_id), None)
    if not row:
        return {"ok": False, "status": "failed", "reason": "message_missing"}
    agent_state = state.get("agents", {}).get(row["recipient"])
    if not agent_state:
        store.mark(message_id, "failed", "unknown recipient")
        return {"ok": False, "status": "failed", "reason": "unknown_recipient"}
    session_name = state.get("session_name")
    window = agent_state.get("window", row["recipient"])
    payload = _message_payload(row)
    rendered = core_render_message(payload)
    text = rendered["text"]
    if not session_name or not _tmux_window_exists(session_name, window):
        store.mark(message_id, "failed", "tmux target missing")
        EventLog(workspace).write("send.failed", message_id=message_id, reason="tmux target missing", target=f"{session_name}:{window}")
        return {"ok": False, "status": "failed", "reason": "tmux_target_missing"}
    target = f"{session_name}:{window}"
    if not store.claim_for_delivery(message_id):
        current = _message_by_id(store, message_id)
        status = current["status"] if current else "missing"
        EventLog(workspace).write("send.delivery_claim_skipped", message_id=message_id, target=target, status=status)
        return {
            "ok": status in {"injected", "visible", "submitted", "submitted_unverified", "delivered", "acknowledged"},
            "status": status,
            "reason": "message_already_claimed",
        }
    recipient_status = agent_state.get("status")
    EventLog(workspace).write(
        "send.deliver_attempt",
        message_id=message_id,
        target=target,
        payload=payload,
        recipient_status=recipient_status,
        recipient_busy=recipient_status == "busy",
        visible_token=rendered.get("token"),
    )
    injection = _tmux_inject_text(
        target,
        text,
        "Enter",
        f"team-agent-send-{message_id}",
        attempts=3 if wait_visible else 1,
        provider=agent_state.get("provider", "fake"),
    )
    if not injection.get("ok") and injection.get("detected") == "codex_trust_prompt":
        # Gap 29 (Stage 2): opt-in trust auto-answer. The helper enforces both the
        # opt-in flag and a workspace-dir match before sending '1'+Enter, then we
        # retry the original paste once the prompt has actually been dismissed.
        # Bypassed entirely when opt-out (default) — the existing failed envelope
        # is preserved.
        from team_agent.messaging.leader_panes import attempt_trust_auto_answer
        answer = attempt_trust_auto_answer(
            workspace,
            injection.get("pane_id") or target,
            injection.get("pane_capture_tail") or "",
            EventLog(workspace),
            state=state,
        )
        if answer.get("answered"):
            # Spark MEDIUM #4 (2026-05-26): replace the fixed 0.3s sleep with a
            # bounded poll. Slow terminals can take well over a second to clear
            # the trust prompt; sleeping a fixed amount races dismissal and
            # leaves the retry hitting the same codex_trust_prompt state. We
            # poll for prompt dismissal up to 3s; if still present, return a
            # retry_needed envelope and let the upstream scheduler decide
            # whether to back off and try again later.
            dismissed = _wait_for_trust_prompt_dismissal(
                injection.get("pane_id") or target, timeout=3.0,
            )
            if not dismissed:
                return _handle_trust_retry_needed(
                    workspace, state, store, message_id, target, injection,
                    attempt=_trust_retry_attempt,
                )
            injection = _tmux_inject_text(
                target,
                text,
                "Enter",
                f"team-agent-send-{message_id}-trust-retry",
                attempts=3 if wait_visible else 1,
                provider=agent_state.get("provider", "fake"),
            )
    if injection["ok"]:
        store.mark(message_id, "submitted")
        EventLog(workspace).write(
            "send.submitted",
            message_id=message_id,
            target=target,
            status="submitted",
            verification=injection.get("verification"),
            submit_verification=injection.get("submit_verification"),
            turn_verification=injection.get("turn_verification"),
            paste_attempts=injection.get("attempts"),
            submit_attempts=injection.get("submit_attempts"),
        )
        return {
            "ok": True,
            "status": "delivered",
            "message_status": "submitted",
            "verification": injection.get("verification"),
            "submit_verification": injection.get("submit_verification"),
            "turn_verification": injection.get("turn_verification"),
            "paste_attempts": injection.get("attempts"),
            "submit_attempts": injection.get("submit_attempts"),
        }
    reason = injection.get("error") or injection.get("verification") or "tmux injection failed"
    store.mark(message_id, "failed", reason)
    EventLog(workspace).write(
        "send.failed",
        message_id=message_id,
        reason=reason,
        target=target,
        stage=injection.get("stage"),
        verification=injection.get("verification"),
        submit_verification=injection.get("submit_verification"),
        turn_verification=injection.get("turn_verification"),
        paste_attempts=injection.get("attempts"),
        submit_attempts=injection.get("submit_attempts"),
    )
    return {
        "ok": False,
        "status": "failed",
        "reason": reason,
        "stage": injection.get("stage"),
        "verification": injection.get("verification"),
        "submit_verification": injection.get("submit_verification"),
        "turn_verification": injection.get("turn_verification"),
        "paste_attempts": injection.get("attempts"),
        "submit_attempts": injection.get("submit_attempts"),
        "detected": injection.get("detected"),
        "pane_id": injection.get("pane_id"),
        "pane_mode": injection.get("pane_mode"),
        "pane_capture_tail": injection.get("pane_capture_tail"),
    }


def _handle_trust_retry_needed(
    workspace: Path,
    state: dict[str, Any],
    store: MessageStore,
    message_id: str,
    target: str,
    injection: dict[str, Any],
    *,
    attempt: int,
) -> dict[str, Any]:
    """Spark MEDIUM sweep #3: replace the dead-end failed mark with a real
    bounded-backoff consumer. attempt is the number of the delivery that JUST
    failed (1 = the original delivery; 2..4 = the scheduler-fired retries).

    Behaviour:
      * attempt < _TRUST_RETRY_MAX_ATTEMPTS: schedule a trust_retry
        scheduled_event for the message, holding the message in 'failed' status
        so _deliver_pending_messages does not race the scheduler. Emit
        leader_panes.trust_auto_answer_retry_scheduled. Return status='retry_scheduled'.
      * attempt >= _TRUST_RETRY_MAX_ATTEMPTS: terminal. Mark the message failed
        and emit leader_panes.trust_auto_answer_exhausted. Return
        status='trust_auto_answer_exhausted'.
    """
    event_log = EventLog(workspace)
    next_attempt = attempt + 1
    if next_attempt > _TRUST_RETRY_MAX_ATTEMPTS:
        store.mark(message_id, "failed", "trust_auto_answer_exhausted")
        event_log.write(
            "leader_panes.trust_auto_answer_exhausted",
            message_id=message_id,
            workspace=str(workspace),
            attempts=attempt,
            target=target,
            pane_id=injection.get("pane_id"),
            reason="trust_auto_answer_exhausted",
        )
        return {
            "ok": False,
            "status": "trust_auto_answer_exhausted",
            "reason": "trust_auto_answer_exhausted",
            "attempts": attempt,
            "detected": injection.get("detected"),
            "pane_id": injection.get("pane_id"),
            "pane_mode": injection.get("pane_mode"),
            "pane_capture_tail": injection.get("pane_capture_tail"),
        }
    backoff = _TRUST_RETRY_BACKOFF_SECONDS.get(next_attempt, _TRUST_RETRY_BACKOFF_SECONDS[_TRUST_RETRY_MAX_ATTEMPTS])
    due_at = (datetime.now(timezone.utc) + timedelta(seconds=backoff)).isoformat()
    owner_team_id = _message_owner_team_id(store, message_id)
    event_id = store.add_scheduled_event(
        due_at,
        message_id,
        "trust_retry",
        {
            "message_id": message_id,
            "attempt": next_attempt,
            "max_attempts": _TRUST_RETRY_MAX_ATTEMPTS,
            "first_target": target,
        },
        owner_team_id=owner_team_id,
    )
    # Hold the message in 'failed' so _deliver_pending_messages does not race
    # the scheduled retry. The scheduler consumer resets it to 'accepted' just
    # before re-delivery.
    store.mark(message_id, "failed", "trust_retry_scheduled")
    event_log.write(
        "leader_panes.trust_auto_answer_retry_needed",
        message_id=message_id,
        workspace=str(workspace),
        pane_id=injection.get("pane_id") or target,
        target=target,
        reason="trust_prompt_not_dismissed_after_answer",
        attempt=attempt,
    )
    event_log.write(
        "leader_panes.trust_auto_answer_retry_scheduled",
        message_id=message_id,
        workspace=str(workspace),
        scheduled_event_id=event_id,
        due_at=due_at,
        next_attempt=next_attempt,
        max_attempts=_TRUST_RETRY_MAX_ATTEMPTS,
        backoff_seconds=backoff,
    )
    return {
        "ok": False,
        "status": "retry_scheduled",
        "reason": "trust_prompt_not_dismissed_after_answer",
        "stage": "trust_auto_answer_dismissal_wait",
        "verification": "trust_prompt_not_dismissed_after_answer",
        "scheduled_event_id": event_id,
        "scheduled_retry_at": due_at,
        "next_attempt": next_attempt,
        "max_attempts": _TRUST_RETRY_MAX_ATTEMPTS,
        "detected": injection.get("detected"),
        "pane_id": injection.get("pane_id"),
        "pane_mode": injection.get("pane_mode"),
        "pane_capture_tail": injection.get("pane_capture_tail"),
    }


def _message_owner_team_id(store: MessageStore, message_id: str) -> str | None:
    row = _message_by_id(store, message_id)
    if not row:
        return None
    owner = row.get("owner_team_id")
    return str(owner) if owner else None


def _execute_trust_retry(
    workspace: Path,
    store: MessageStore,
    event_log: EventLog,
    payload: dict[str, Any],
    *,
    owner_team_id: str | None = None,
) -> dict[str, Any]:
    """Scheduler-side consumer for kind='trust_retry'. Resets the message back
    to 'accepted' so claim_for_delivery succeeds, re-runs _deliver_pending_message,
    and either succeeds, escalates to a further retry (via _handle_trust_retry_needed),
    or hits the terminal exhausted branch.
    """
    from team_agent.state import load_runtime_state
    message_id = str(payload.get("message_id") or "")
    if not message_id:
        return {"ok": False, "reason": "trust_retry_missing_message_id"}
    attempt = int(payload.get("attempt") or 1)
    row = _message_by_id(store, message_id)
    if not row:
        event_log.write(
            "leader_panes.trust_auto_answer_retry_skipped",
            message_id=message_id,
            reason="message_missing",
            attempt=attempt,
        )
        return {"ok": False, "reason": "message_missing"}
    # Reset to accepted so claim_for_delivery succeeds. The previous attempt
    # left the row in 'failed' status with reason='trust_retry_scheduled'.
    store.mark(message_id, "accepted", "trust_retry_resuming")
    event_log.write(
        "leader_panes.trust_auto_answer_retry_attempted",
        message_id=message_id,
        workspace=str(workspace),
        attempt=attempt,
        max_attempts=int(payload.get("max_attempts") or _TRUST_RETRY_MAX_ATTEMPTS),
    )
    state = load_runtime_state(workspace)
    if owner_team_id and isinstance(state.get("teams"), dict):
        scoped = state["teams"].get(owner_team_id)
        if isinstance(scoped, dict):
            state = scoped
    delivery_result = _deliver_pending_message(
        workspace, state, message_id,
        wait_visible=True, timeout=30.0,
        _trust_retry_attempt=attempt,
    )
    return delivery_result


def _wait_for_trust_prompt_dismissal(target: str, *, timeout: float = 3.0, poll_interval: float = 0.1) -> bool:
    """Spark MEDIUM #4: bounded poll for trust prompt dismissal. Returns True once
    the pane no longer matches detect_non_input_scrollback, False if the prompt
    is still present after `timeout` seconds. Uses the same detector the inject
    path uses so behaviour stays consistent."""
    import time as _time
    from team_agent.messaging.tmux_prompt import detect_non_input_scrollback
    deadline = _time.monotonic() + max(timeout, 0.0)
    while True:
        capture = _capture_pane_tail(target)
        detected = detect_non_input_scrollback(capture)
        if detected != "codex_trust_prompt":
            return True
        if _time.monotonic() >= deadline:
            return False
        _time.sleep(poll_interval)


def _capture_pane_tail(target: str) -> str:
    from team_agent.messaging.deps import _capture_tmux_pane_text
    capture = _capture_tmux_pane_text(target)
    if not capture.get("ok"):
        return ""
    return str(capture.get("capture") or "")


def _deliver_pending_messages(workspace: Path, state: dict[str, Any], event_log: EventLog) -> list[str]:
    store = MessageStore(workspace)
    delivered: list[str] = []
    for row in store.messages():
        if row["status"] not in {"pending", "accepted"}:
            continue
        agent_state = state.get("agents", {}).get(row["recipient"]) or {}
        if str(agent_state.get("status") or "").lower() == "busy":
            event_log.write(
                "send.deferred_busy",
                message_id=row["message_id"],
                sender=row.get("sender"),
                recipient=row["recipient"],
                reason="recipient_busy",
            )
            continue
        result = _deliver_pending_message(workspace, state, row["message_id"], wait_visible=True, timeout=30.0)
        if result.get("ok"):
            delivered.append(row["message_id"])
            event_log.write("send.pending_delivered", message_id=row["message_id"], agent_id=row["recipient"])
    return delivered
