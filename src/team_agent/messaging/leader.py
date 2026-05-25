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
    os,
    runtime_dir,
    save_runtime_state,
    team_state_key,
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

    owner_identity = state.get("team_owner") or None
    side_pane_refusal = _side_pane_owner_refusal(state, owner_identity)
    if side_pane_refusal:
        event_log.write("leader_receiver.side_pane_refused", **side_pane_refusal)
        return {
            "ok": False,
            "message_id": message_id,
            "status": "refused",
            "to": leader_id,
            "channel": "direct_tmux",
            **side_pane_refusal,
        }
    receiver_for_validation = dict(receiver)
    if owner_identity and owner_identity.get("leader_session_uuid") and not receiver_for_validation.get("leader_session_uuid"):
        receiver_for_validation["leader_session_uuid"] = owner_identity["leader_session_uuid"]
    validation = _validate_leader_receiver(receiver_for_validation)
    if not validation["ok"]:
        rediscovery = _rediscover_leader_receiver(
            receiver_for_validation,
            event_log,
            owner_identity,
            invalidation_reason=validation.get("reason"),
            team_id=team_state_key(state),
        )
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
                error="multiple possible leader panes found; run team-agent claim-leader --confirm from the intended pane",
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


def _side_pane_owner_refusal(state: dict[str, Any], owner_identity: dict[str, Any] | None) -> dict[str, Any] | None:
    owner_uuid = str((owner_identity or {}).get("leader_session_uuid") or "")
    caller_uuid = os.environ.get("TEAM_AGENT_LEADER_SESSION_UUID") or os.environ.get("TEAM_AGENT_LEADER_SESSION_UUID_OVERRIDE") or ""
    if not owner_uuid or not caller_uuid or caller_uuid == owner_uuid:
        return None
    bound_pane = (state.get("leader_receiver") or {}).get("pane_id") or (owner_identity or {}).get("pane_id")
    team_id = team_state_key(state)
    return {
        "reason": "team_owner_mismatch",
        "error": (
            f"This workspace's team `{team_id}` is already bound to pane `{bound_pane}`. "
            "To work in this window either start a new team with a different team_id, operate through the bound pane, "
            "or run `team-agent claim-leader --confirm` only if you intend to forcibly take over."
        ),
        "bound_pane_id": bound_pane,
        "caller_uuid_prefix": caller_uuid[:8],
        "uuid_prefix": owner_uuid[:8],
        "action": "team-agent claim-leader --confirm",
    }


def claim_leader_receiver(
    workspace: Path,
    state: dict[str, Any],
    candidate: dict[str, Any],
    event_log: EventLog,
    *,
    confirm: bool,
    expected_epoch: int | None = None,
) -> dict[str, Any]:
    from team_agent.messaging.leader_panes import _leader_command_looks_usable, _receiver_from_target, _target_matches_owner_identity, _uuid_prefix
    if not confirm:
        return {"ok": False, "status": "refused", "reason": "confirm_required", "action": "team-agent claim-leader --confirm"}
    owner = state.setdefault("team_owner", {})
    receiver = state.get("leader_receiver") or {}
    current_epoch = int(owner.get("owner_epoch") or receiver.get("owner_epoch") or 0)
    if expected_epoch is not None and current_epoch != expected_epoch:
        event_log.write("leader_receiver.claim_refused", reason="owner_epoch_advanced", owner_epoch=current_epoch, bound_pane_id=receiver.get("pane_id"))
        return {"ok": False, "status": "refused", "reason": "owner_epoch_advanced", "owner_epoch": current_epoch, "bound_pane_id": receiver.get("pane_id")}
    if receiver.get("pane_id") == candidate.get("pane_id"):
        return {"ok": True, "status": "already_bound", "leader_receiver": receiver, "owner_epoch": current_epoch}
    if not _target_matches_owner_identity(candidate, owner):
        event_log.write("leader_receiver.claim_refused", reason="uuid_mismatch", candidate_pane_id=candidate.get("pane_id"))
        return {"ok": False, "status": "refused", "reason": "uuid_mismatch"}
    provider = str(candidate.get("provider") or receiver.get("provider") or "codex")
    if not _leader_command_looks_usable(str(candidate.get("pane_current_command", "")), provider):
        return {"ok": False, "status": "refused", "reason": "wrong_command", "candidate_pane_id": candidate.get("pane_id")}
    next_epoch = current_epoch + 1
    new_receiver = _receiver_from_target(candidate, provider, owner.get("leader_session_uuid"), next_epoch)
    owner["owner_epoch"] = next_epoch
    state["leader_receiver"] = new_receiver
    from team_agent.runtime import _runtime_lock, save_runtime_state
    with _runtime_lock(workspace, "leader_receiver"):
        save_runtime_state(workspace, state)
    event_log.write("leader_receiver.claimed", pane_id=new_receiver["pane_id"], owner_epoch=next_epoch, uuid_prefix=_uuid_prefix(owner))
    return {"ok": True, "status": "claimed", "leader_receiver": new_receiver, "owner_epoch": next_epoch}


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


































