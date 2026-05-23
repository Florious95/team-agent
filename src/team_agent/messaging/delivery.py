from __future__ import annotations

from team_agent.messaging.deps import (
    EventLog,
    MessageStore,
    _format_team_agent_message,
    _message_by_id,
    _message_payload,
    _submit_worker_prompt,
    _tmux_paste_ready_timeout,
    _tmux_set_buffer_text,
    _tmux_submit_settle_timeout,
    _tmux_window_exists,
    _wait_for_worker_message_ready,
    run_cmd,
    time,
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
    if agent_state.get("status") == "busy":
        EventLog(workspace).write("send.queued_busy", message_id=message_id, agent_id=row["recipient"])
        return {"ok": False, "status": row["status"], "reason": "agent_busy"}
    session_name = state.get("session_name")
    window = agent_state.get("window", row["recipient"])
    payload = _message_payload(row)
    text = _format_team_agent_message(payload)
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
    EventLog(workspace).write("send.deliver_attempt", message_id=message_id, target=target, payload=payload)
    buffered = _tmux_set_buffer_text("team-agent-message", text)
    if not buffered["ok"]:
        store.mark(message_id, "failed", buffered.get("error"))
        return {"ok": False, "status": "failed", "reason": buffered.get("error"), "stage": buffered["stage"]}
    paste_attempts: list[dict[str, Any]] = []
    max_paste_attempts = 3 if wait_visible else 1
    ready_timeout = _tmux_paste_ready_timeout(text) if wait_visible else 0.1
    submit_settle_timeout = _tmux_submit_settle_timeout(text)
    for paste_attempt in range(1, max_paste_attempts + 1):
        proc = run_cmd(["tmux", "paste-buffer", "-t", target, "-b", "team-agent-message", "-p"], timeout=10)
        if proc.returncode != 0:
            store.mark(message_id, "failed", proc.stderr.strip())
            return {"ok": False, "status": "failed", "reason": proc.stderr.strip()}
        # tmux paste-buffer can return before TUI frameworks finish ingesting
        # bracketed paste. A short delay prevents submitting an empty prompt
        # and leaving the real payload sitting in the input box.
        time.sleep(0.25)
        ready, verification, capture_text = _wait_for_worker_message_ready(target, message_id, ready_timeout, text)
        paste_attempts.append(
            {
                "attempt": paste_attempt,
                "ready": ready,
                "verification": verification,
                "buffer_method": buffered.get("method"),
                "text_bytes": buffered.get("text_bytes"),
                "ready_timeout_sec": ready_timeout,
            }
        )
        if ready or not wait_visible or paste_attempt == max_paste_attempts:
            submit = _submit_worker_prompt(target, capture_text, settle_timeout=submit_settle_timeout)
            if not submit["ok"]:
                if submit.get("stage") == "send-keys":
                    store.mark(message_id, "failed", submit.get("error"))
                    return {"ok": False, "status": "failed", "reason": submit.get("error"), "submit_verification": submit.get("verification")}
                reason = f"{verification}; {submit.get('verification')}"
                store.mark(message_id, "injected_unverified", reason)
                EventLog(workspace).write(
                    "send.unverified",
                    message_id=message_id,
                    target=target,
                    timeout_sec=timeout,
                    verification=verification,
                    submit_verification=submit.get("verification"),
                    paste_attempts=paste_attempts,
                    submit_attempts=submit.get("attempts"),
                )
                return {
                    "ok": False,
                    "status": "injected_unverified",
                    "verification": verification,
                    "submit_verification": submit.get("verification"),
                    "paste_attempts": paste_attempts,
                    "submit_attempts": submit.get("attempts"),
                }
            if ready:
                status = (
                    "submitted"
                    if verification
                    in {
                        "capture_contains_pasted_content_prompt",
                        "capture_contains_new_pasted_content_prompt",
                        "capture_contains_message_fragment",
                    }
                    else "visible"
                )
                store.mark(message_id, status)
                EventLog(workspace).write(
                    "send.submitted",
                    message_id=message_id,
                    target=target,
                    status=status,
                    verification=verification,
                    submit_verification=submit.get("verification"),
                    paste_attempts=paste_attempts,
                    submit_attempts=submit.get("attempts"),
                )
                return {
                    "ok": True,
                    "status": status,
                    "verification": verification,
                    "submit_verification": submit.get("verification"),
                    "paste_attempts": paste_attempts,
                    "submit_attempts": submit.get("attempts"),
                }
            if wait_visible:
                reason = f"visible token or pasted prompt not found after {timeout:g}s"
                warning = "submitted but visible-token capture did not confirm delivery"
                store.mark(message_id, "submitted_unverified", reason)
                EventLog(workspace).write(
                    "send.submitted_unverified",
                    message_id=message_id,
                    target=target,
                    timeout_sec=timeout,
                    verification=verification,
                    submit_verification=submit.get("verification"),
                    paste_attempts=paste_attempts,
                    submit_attempts=submit.get("attempts"),
                    warning=warning,
                )
                return {
                    "ok": True,
                    "status": "submitted_unverified",
                    "verification": verification,
                    "submit_verification": submit.get("verification"),
                    "warning": warning,
                    "paste_attempts": paste_attempts,
                    "submit_attempts": submit.get("attempts"),
                }
            store.mark(message_id, "injected")
            return {
                "ok": True,
                "status": "injected",
                "verification": verification,
                "submit_verification": submit.get("verification"),
                "paste_attempts": paste_attempts,
                "submit_attempts": submit.get("attempts"),
            }
    store.mark(message_id, "injected_unverified", "delivery loop exhausted")
    return {"ok": False, "status": "injected_unverified", "verification": "delivery_loop_exhausted", "paste_attempts": paste_attempts}


def _deliver_pending_messages(workspace: Path, state: dict[str, Any], event_log: EventLog) -> list[str]:
    store = MessageStore(workspace)
    delivered: list[str] = []
    for row in store.messages():
        if row["status"] not in {"pending", "accepted"}:
            continue
        result = _deliver_pending_message(workspace, state, row["message_id"], wait_visible=True, timeout=30.0)
        if result.get("ok"):
            delivered.append(row["message_id"])
            event_log.write("send.pending_delivered", message_id=row["message_id"], agent_id=row["recipient"])
    return delivered
