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

from pathlib import Path
from typing import Any

def _deliver_pending_message(
    workspace: Path,
    state: dict[str, Any],
    message_id: str,
    wait_visible: bool = True,
    timeout: float = 30.0,
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
                EventLog(workspace).write(
                    "leader_panes.trust_auto_answer_retry_needed",
                    pane_id=injection.get("pane_id") or target,
                    workspace=str(workspace),
                    reason="trust_prompt_not_dismissed_after_answer",
                )
                store.mark(message_id, "failed", "trust_prompt_not_dismissed_after_answer")
                return {
                    "ok": False,
                    "status": "retry_needed",
                    "reason": "trust_prompt_not_dismissed_after_answer",
                    "stage": "trust_auto_answer_dismissal_wait",
                    "verification": "trust_prompt_not_dismissed_after_answer",
                    "detected": injection.get("detected"),
                    "pane_id": injection.get("pane_id"),
                    "pane_mode": injection.get("pane_mode"),
                    "pane_capture_tail": injection.get("pane_capture_tail"),
                }
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
    }


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
