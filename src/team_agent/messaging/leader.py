from __future__ import annotations

from team_agent.messaging.deps import (
    EventLog,
    MessageStore,
    _choose_leader_submit_key,
    _leader_id,
    _rediscover_leader_receiver,
    _tmux_inject_text,
    _validate_leader_receiver,
    core_render_message,
    json,
    runtime_dir,
    save_runtime_state,
    time,
)

from pathlib import Path
from typing import Any

def allow_peer_talk(workspace: Path, agent_a: str, agent_b: str) -> dict[str, Any]:
    MessageStore(workspace).allow_peer(agent_a, agent_b)
    EventLog(workspace).write("communication.peer_allowed", a=agent_a, b=agent_b)
    return {"ok": True, "a": agent_a, "b": agent_b, "status": "compat_noop", "reason": "team_scoped_peer_messages_enabled"}


def _mirror_peer_message_to_leader(
    workspace: Path,
    state: dict[str, Any],
    sender: str,
    target: str,
    content: str,
    task_id: str | None,
    event_log: EventLog,
) -> None:
    leader_id = _leader_id(state, {})
    mirror = f"Team Agent peer message from {sender} to {target}"
    if task_id:
        mirror += f" for {task_id}"
    mirror += f":\n\n{content}"
    try:
        result = _send_to_leader_receiver(workspace, state, leader_id, mirror, task_id, sender, False, event_log)
        event_log.write("communication.peer_mirrored", sender=sender, target=target, ok=result.get("ok"))
    except Exception as exc:
        event_log.write("communication.peer_mirror_failed", sender=sender, target=target, error=str(exc))


def _leader_inbox_path(workspace: Path) -> Path:
    return runtime_dir(workspace) / "leader-inbox.log"


def _send_to_leader_receiver(
    workspace: Path,
    state: dict[str, Any],
    leader_id: str,
    content: str,
    task_id: str | None,
    sender: str,
    requires_ack: bool,
    event_log: EventLog,
) -> dict[str, Any]:
    store = MessageStore(workspace)
    message_id = store.create_message(task_id, sender, leader_id, content, requires_ack=False)
    if requires_ack:
        event_log.write("leader_receiver.no_ack_forced", message_id=message_id, requested_requires_ack=True)
    row = _message_by_id(store, message_id)
    if not row:
        return {"ok": False, "message_id": message_id, "status": "failed", "to": leader_id, "reason": "message_missing"}
    if not store.claim_for_delivery(message_id):
        current = _message_by_id(store, message_id)
        status = current["status"] if current else "missing"
        event_log.write("leader_receiver.delivery_claim_skipped", message_id=message_id, status=status)
        return {
            "ok": status in {"submitted", "visible", "delivered", "acknowledged"},
            "message_id": message_id,
            "status": status,
            "to": leader_id,
            "channel": "direct_tmux",
            "reason": "message_already_claimed",
        }
    payload = _message_payload(row)
    rendered = core_render_message(payload)
    text = rendered["text"]
    receiver = state.get("leader_receiver", {})
    if not _leader_receiver_is_direct(receiver):
        return _fail_leader_delivery(
            workspace,
            state,
            store,
            message_id,
            payload,
            event_log,
            reason="leader_not_attached",
            error="No direct leader tmux pane is attached. Run team-agent attach-leader.",
        )

    validation = _validate_leader_receiver(receiver)
    if not validation["ok"]:
        rediscovery = _rediscover_leader_receiver(receiver, event_log)
        if rediscovery.get("status") == "updated":
            state["leader_receiver"].update(rediscovery["receiver"])
            receiver = state["leader_receiver"]
            validation = _validate_leader_receiver(receiver)
        elif rediscovery.get("status") == "ambiguous":
            return _fail_leader_delivery(
                workspace,
                state,
                store,
                message_id,
                payload,
                event_log,
                reason="ambiguous",
                error="multiple possible leader panes found; rerun team-agent attach-leader --pane <pane_id>",
                message_status="ambiguous",
            )
    if not validation["ok"]:
        return _fail_leader_delivery(
            workspace,
            state,
            store,
            message_id,
            payload,
            event_log,
            reason=validation["reason"],
            error=validation.get("error"),
        )
    state["leader_receiver"].update(validation["pane"])
    submit_key, submit_reason = _choose_leader_submit_key(receiver.get("provider", "codex"), validation.get("capture", ""))
    target = receiver["pane_id"]
    event_log.write(
        "leader_receiver.deliver_attempt",
        message_id=message_id,
        target=target,
        provider=receiver.get("provider"),
        submit_key=submit_key,
        submit_reason=submit_reason,
        render_engine=rendered.get("engine"),
        visible_token=rendered.get("token"),
        payload=payload,
        warning=validation.get("warning"),
    )
    injection = _tmux_inject_text(
        target,
        text,
        submit_key,
        f"team-agent-leader-receiver-{message_id}",
        provider=receiver.get("provider", "codex"),
    )
    if injection["ok"]:
        store.mark(message_id, "submitted")
        event_log.write(
            "leader_receiver.submitted",
            message_id=message_id,
            sender=sender,
            task_id=task_id,
            target=target,
            provider=receiver.get("provider"),
            submit_key=submit_key,
            submit_reason=submit_reason,
            visible=True,
            submitted=True,
            visible_token=rendered.get("token"),
            verification=injection.get("verification"),
            submit_verification=injection.get("submit_verification"),
            turn_verification=injection.get("turn_verification"),
            attempts=injection.get("attempts"),
            submit_attempts=injection.get("submit_attempts"),
        )
        save_runtime_state(workspace, state)
        return {
            "ok": True,
            "message_id": message_id,
            "status": "submitted",
            "to": leader_id,
            "channel": "direct_tmux",
            "leader_receiver": state["leader_receiver"],
            "submit_key": submit_key,
            "visible": True,
            "submitted": True,
            "visible_token": rendered.get("token"),
            "verification": injection.get("verification"),
            "submit_verification": injection.get("submit_verification"),
            "turn_verification": injection.get("turn_verification"),
            "attempts": injection.get("attempts"),
            "submit_attempts": injection.get("submit_attempts"),
            "warning": "leader messages are no-ack; requires_ack was forced false" if requires_ack else None,
        }
    return _fail_leader_delivery(
        workspace,
        state,
        store,
        message_id,
        payload,
        event_log,
        reason="tmux_injection_failed",
        error=injection.get("error"),
        stage=injection.get("stage"),
        attempts=injection.get("attempts"),
        submit_attempts=injection.get("submit_attempts"),
    )


def _fail_leader_delivery(
    workspace: Path,
    state: dict[str, Any],
    store: MessageStore,
    message_id: str,
    payload: dict[str, Any],
    event_log: EventLog,
    reason: str,
    error: str | None = None,
    stage: str | None = None,
    message_status: str = "failed",
    attempts: list[dict[str, Any]] | None = None,
    submit_attempts: list[dict[str, Any]] | None = None,
) -> dict[str, Any]:
    store.mark(message_id, message_status, error or reason)
    fallback_path = _write_leader_fallback_audit(workspace, payload, reason, error)
    event_log.write(
        "leader_receiver.delivery_failed",
        message_id=message_id,
        target=state.get("leader_receiver", {}).get("pane_id"),
        reason=reason,
        error=error,
        stage=stage,
        attempts=attempts,
        submit_attempts=submit_attempts,
        fallback_path=str(fallback_path),
        suggestion="Run team-agent attach-leader --workspace . --provider codex, or pass --pane <pane_id>.",
    )
    save_runtime_state(workspace, state)
    return {
        "ok": False,
        "message_id": message_id,
        "status": "fallback",
        "message_status": message_status,
        "to": payload["to"],
        "channel": "fallback_inbox",
        "reason": reason,
        "error": error,
        "attempts": attempts,
        "submit_attempts": submit_attempts,
        "fallback_path": str(fallback_path),
        "suggestion": "Run team-agent attach-leader --workspace . --provider codex, or pass --pane <pane_id>.",
    }


def _write_leader_fallback_audit(workspace: Path, payload: dict[str, Any], reason: str, error: str | None) -> Path:
    inbox_path = _leader_inbox_path(workspace)
    inbox_path.parent.mkdir(parents=True, exist_ok=True)
    stamp = time.strftime("%Y-%m-%d %H:%M:%S")
    text = core_render_message(payload)["text"]
    with inbox_path.open("a", encoding="utf-8") as inbox:
        inbox.write(f"\n[{stamp}] fallback reason={reason} error={error or '-'}\n{text}\n")
    return inbox_path


def _leader_receiver_is_direct(receiver: dict[str, Any] | None) -> bool:
    return bool(receiver and receiver.get("mode") == "direct_tmux" and receiver.get("pane_id"))


def _message_by_id(store: MessageStore, message_id: str) -> dict[str, Any] | None:
    return next((m for m in store.messages() if m["message_id"] == message_id), None)


def _message_payload(row: dict[str, Any]) -> dict[str, Any]:
    return {
        "message_id": row["message_id"],
        "task_id": row["task_id"],
        "from": row["sender"],
        "to": row["recipient"],
        "reply_to": row["reply_to"],
        "requires_ack": bool(row["requires_ack"]),
        "artifact_refs": json.loads(row["artifact_refs"] or "[]"),
        "content": row["content"],
    }


def _format_team_agent_message(payload: dict[str, Any]) -> str:
    return core_render_message(payload)["text"]





































