from __future__ import annotations

from team_agent.messaging.deps import (
    EventLog,
    MessageStore,
    RuntimeError,
    _capture_missing_sessions,
    _current_task_for_agent,
    _deliver_pending_message,
    _find_agent,
    _find_task,
    _is_leader_sender,
    _is_leader_target,
    _is_runtime_team_agent,
    _leader_id,
    _message_by_id,
    _mirror_peer_message_to_leader,
    _runtime_lock,
    _runtime_team_agent_ids,
    _send_to_leader_receiver,
    load_runtime_state,
    load_spec,
    missing_tools,
    route_task,
    save_runtime_state,
    update_task_status,
)

from pathlib import Path
from typing import Any

def send_message(
    workspace: Path,
    target: str | None,
    content: str,
    task_id: str | None = None,
    sender: str = "leader",
    requires_ack: bool = True,
    confirm_human: bool = False,
    wait_visible: bool = True,
    timeout: float = 30.0,
    lock_timeout: float = 5.0,
    watch_result: bool = False,
    block_until_delivered: bool = True,
) -> dict[str, Any]:
    with _runtime_lock(workspace, "send", timeout=lock_timeout):
        return _send_message_unlocked(
            workspace,
            target,
            content,
            task_id=task_id,
            sender=sender,
            requires_ack=requires_ack,
            confirm_human=confirm_human,
            wait_visible=wait_visible,
            timeout=timeout,
            watch_result=watch_result,
            block_until_delivered=block_until_delivered,
        )


def _send_message_unlocked(
    workspace: Path,
    target: str | None,
    content: str,
    task_id: str | None = None,
    sender: str = "leader",
    requires_ack: bool = True,
    confirm_human: bool = False,
    wait_visible: bool = True,
    timeout: float = 30.0,
    watch_result: bool = False,
    block_until_delivered: bool = True,
) -> dict[str, Any]:
    state = load_runtime_state(workspace)
    spec_path = Path(state.get("spec_path", workspace / "team.spec.yaml"))
    spec = load_spec(spec_path)
    event_log = EventLog(workspace)
    leader_id = _leader_id(state, spec)

    if target == "*":
        if watch_result:
            return {"ok": False, "status": "failed", "reason": "watch_result_not_supported_for_broadcast", "to": target}
        return _broadcast_message_unlocked(
            workspace,
            state,
            spec,
            event_log,
            content,
            task_id=task_id,
            sender=sender,
            requires_ack=requires_ack,
            wait_visible=wait_visible,
            timeout=timeout,
            block_until_delivered=block_until_delivered,
        )

    return _send_single_message_unlocked(
        workspace,
        state,
        spec,
        event_log,
        target,
        content,
        task_id=task_id,
        sender=sender,
        requires_ack=requires_ack,
        confirm_human=confirm_human,
        wait_visible=wait_visible,
        timeout=timeout,
        watch_result=watch_result,
        block_until_delivered=block_until_delivered,
    )


def _send_single_message_unlocked(
    workspace: Path,
    state: dict[str, Any],
    spec: dict[str, Any],
    event_log: EventLog,
    target: str | None,
    content: str,
    *,
    task_id: str | None = None,
    sender: str = "leader",
    requires_ack: bool = True,
    confirm_human: bool = False,
    wait_visible: bool = True,
    timeout: float = 30.0,
    watch_result: bool = False,
    mirror_peer: bool = True,
    route_task_id: bool = True,
    block_until_delivered: bool = True,
) -> dict[str, Any]:
    leader_id = _leader_id(state, spec)

    if _is_leader_target(target, leader_id) and not _is_leader_sender(sender, leader_id):
        return _send_to_leader_receiver(workspace, state, leader_id, content, task_id, sender, requires_ack, event_log)

    if task_id and route_task_id:
        task = _find_task(state.get("tasks", []), task_id)
        if task.get("human_confirmation") and not task.get("human_confirmed"):
            if not confirm_human:
                update_task_status(state["tasks"], task_id, "blocked", "human confirmation required before dispatch")
                save_runtime_state(workspace, state)
                event_log.write(
                    "send.human_confirmation_required",
                    task_id=task_id,
                    requested_target=target,
                )
                return {
                    "ok": False,
                    "status": "blocked",
                    "reason": "human_confirmation_required",
                    "task_id": task_id,
                }
            task["human_confirmed"] = True
            event_log.write("send.human_confirmation_granted", task_id=task_id, confirmed_by=sender)
        route = route_task(spec, task)
        routed_target = route["agent_id"]
        requested_target = target
        target = target or routed_target
        task["assignee"] = target
        event_log.write(
            "routing.decision",
            source="send",
            task_id=task_id,
            route_agent=routed_target,
            selected_agent=target,
            reason=route["reason"],
            manual_override=bool(requested_target and requested_target != routed_target),
        )
        agent = _find_agent(spec, target)
        if agent:
            missing = missing_tools(agent, task)
            if missing:
                update_task_status(state["tasks"], task_id, "blocked", f"missing permissions: {', '.join(missing)}")
                save_runtime_state(workspace, state)
                event_log.write(
                    "send.blocked_missing_permissions",
                    task_id=task_id,
                    agent_id=target,
                    missing_tools=missing,
                )
                return {
                    "ok": False,
                    "status": "blocked",
                    "task_id": task_id,
                    "agent_id": target,
                    "missing_tools": missing,
                }

    if not target:
        raise RuntimeError("send requires target or --task")
    if not _is_leader_target(target, leader_id) and not _is_runtime_team_agent(target, state, spec):
        event_log.write("send.target_rejected", sender=sender, target=target, reason="target_not_in_team")
        return {"ok": False, "status": "refused", "reason": "target_not_in_team", "from": sender, "to": target}
    store = MessageStore(workspace)
    message_id = store.create_message(task_id, sender, target, content, requires_ack=requires_ack)
    if not block_until_delivered:
        watch: dict[str, Any] | None = None
        if watch_result:
            watch_task_id = task_id or _current_task_for_agent(state.get("tasks", []), str(target))
            watcher_id = store.create_result_watcher(watch_task_id, str(target), message_id, leader_id)
            watch = {
                "status": "registered",
                "watcher_id": watcher_id,
                "task_id": watch_task_id,
                "agent_id": target,
                "notice": (
                    "Team Agent will deliver this message when the worker is available, "
                    "then collect the result and notify the leader when this task reports completion."
                ),
            }
            event_log.write(
                "result_watcher.created",
                watcher_id=watcher_id,
                task_id=watch_task_id,
                agent_id=target,
                message_id=message_id,
            )
        _capture_missing_sessions(workspace, state, event_log, timeout_s=0.0, log_miss=False)
        save_runtime_state(workspace, state)
        event_log.write(
            "send.durably_stored",
            message_id=message_id,
            target=target,
            sender=sender,
            task_id=task_id,
        )
        result = {
            "ok": True,
            "message_id": message_id,
            "status": "queued",
            "message_status": "accepted",
            "to": target,
            "queued": True,
            "durably_stored": True,
            "reason": "deferred_to_coordinator",
            "visible": False,
            "submitted": False,
        }
        if watch is not None:
            result["watch_result"] = True
            result["watch"] = watch
        return result
    delivered_result = _deliver_pending_message(workspace, state, message_id, wait_visible=wait_visible, timeout=timeout)
    row = _message_by_id(store, message_id)
    message_status = row["status"] if row else delivered_result.get("message_status", delivered_result.get("status", "accepted"))
    if (
        mirror_peer
        and not _is_leader_sender(sender, leader_id)
        and not _is_leader_target(target, leader_id)
        and delivered_result.get("ok")
        and not delivered_result.get("queued")
    ):
        _mirror_peer_message_to_leader(workspace, state, sender, target, content, task_id, event_log)
    watch: dict[str, Any] | None = None
    if watch_result and delivered_result.get("ok"):
        watch_task_id = task_id or _current_task_for_agent(state.get("tasks", []), str(target))
        watcher_id = store.create_result_watcher(watch_task_id, str(target), message_id, leader_id)
        watch = {
            "status": "registered",
            "watcher_id": watcher_id,
            "task_id": watch_task_id,
            "agent_id": target,
            "notice": (
                "Team Agent will deliver this message when the worker is available, "
                "then collect the result and notify the leader when this task reports completion."
                if delivered_result.get("queued")
                else "Team Agent will collect the result and notify the leader when this task reports completion."
            ),
        }
        event_log.write(
            "result_watcher.created",
            watcher_id=watcher_id,
            task_id=watch_task_id,
            agent_id=target,
            message_id=message_id,
        )
    _capture_missing_sessions(workspace, state, event_log, timeout_s=0.0, log_miss=False)
    save_runtime_state(workspace, state)
    result = {
        "ok": bool(delivered_result.get("ok")),
        "message_id": message_id,
        "status": delivered_result.get("status", message_status),
        "message_status": message_status,
        "to": target,
        "visible": message_status in {"visible", "submitted"},
        "submitted": message_status in {"visible", "submitted", "submitted_unverified", "delivered", "acknowledged"},
        "verification": delivered_result.get("verification"),
        "submit_verification": delivered_result.get("submit_verification"),
        "turn_verification": delivered_result.get("turn_verification"),
    }
    if delivered_result.get("queued"):
        result["queued"] = True
        result["reason"] = delivered_result.get("reason")
    if delivered_result.get("warning"):
        result["warning"] = delivered_result["warning"]
    for key in ("paste_attempts", "submit_attempts"):
        if key in delivered_result:
            result[key] = delivered_result[key]
    if watch is not None:
        result["watch_result"] = True
        result["watch"] = watch
    return result


def _broadcast_message_unlocked(
    workspace: Path,
    state: dict[str, Any],
    spec: dict[str, Any],
    event_log: EventLog,
    content: str,
    *,
    task_id: str | None,
    sender: str,
    requires_ack: bool,
    wait_visible: bool,
    timeout: float,
    block_until_delivered: bool = True,
) -> dict[str, Any]:
    targets = _broadcast_targets(state, spec, sender)
    if not targets:
        event_log.write("send.broadcast_skipped", sender=sender, reason="no_team_recipients")
        return {"ok": False, "status": "failed", "reason": "no_team_recipients", "to": "*", "targets": []}
    event_log.write("send.broadcast_start", sender=sender, targets=targets, task_id=task_id)
    deliveries: list[dict[str, Any]] = []
    for recipient in targets:
        result = _send_single_message_unlocked(
            workspace,
            state,
            spec,
            event_log,
            recipient,
            content,
            task_id=task_id,
            sender=sender,
            requires_ack=requires_ack,
            confirm_human=False,
            wait_visible=wait_visible,
            timeout=timeout,
            watch_result=False,
            mirror_peer=False,
            route_task_id=False,
            block_until_delivered=block_until_delivered,
        )
        deliveries.append(_compact_broadcast_delivery(result))
    failed = [item for item in deliveries if not item.get("ok")]
    status = "broadcast_delivered" if not failed else "broadcast_partial"
    event_log.write(
        "send.broadcast_complete",
        sender=sender,
        targets=targets,
        status=status,
        delivered_count=len(deliveries) - len(failed),
        failed_count=len(failed),
    )
    return {
        "ok": not failed,
        "status": status,
        "to": "*",
        "targets": targets,
        "delivered_count": len(deliveries) - len(failed),
        "failed_count": len(failed),
        "deliveries": deliveries,
    }


def _broadcast_targets(state: dict[str, Any], spec: dict[str, Any], sender: str) -> list[str]:
    leader_id = _leader_id(state, spec)
    targets = [leader_id, *_runtime_team_agent_ids(state, spec)]
    if _is_leader_sender(sender, leader_id):
        excluded = {leader_id}
    else:
        excluded = {sender}
    return [target for target in targets if target not in excluded]


def _compact_broadcast_delivery(result: dict[str, Any]) -> dict[str, Any]:
    keys = ["ok", "status", "message_id", "to", "reason", "channel"]
    return {key: result[key] for key in keys if key in result}
