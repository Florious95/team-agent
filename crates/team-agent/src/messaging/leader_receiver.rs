//! leader.py — leader pane 注入边界 + 恰好一次去重门 (card §21/§72)。

use std::path::Path;

use rusqlite::OptionalExtension;
use serde_json::Value;

use crate::event_log::EventLog;
use crate::message_store::{MessageStore, NotificationClaimParams};
use crate::model::ids::TaskId;
use crate::transport::{
    InjectPayload, InjectReport, Key, PaneId, Target, Transport, TransportError,
};

use super::helpers::MessageStatusShadow;
use super::persist::persist_internal_send;
use super::presentation::PresentationDecision;
use super::{
    DeliveryOutcome, DeliveryRefusal, DeliveryStage, DeliveryStatus, InitialDisposition,
    InternalSendKind, MessagingError, PersistResolution,
};

/// `_send_to_leader_receiver` (`leader.py:69`) — **N31/N32 funnel primitive**:所有 leader-bound
/// caller(send_message(to=leader) / report_result / request_human / idle reminder /
/// broadcast-to-leader / peer-mirror / worker.abnormal_exit)统一经过这里。
///
/// 职责 = create_message + leader_notification_log dedup(result_id 时)+ audit + emit
/// `deliver_to_leader.submit`(funnel 指纹,契约 grep 用)。**不**预 claim:状态留 `accepted`,
/// 让后续 `deliver_pending_messages` 同一管道做物理 inject(MUST-13 单注入点,leader/worker 同路径
/// → #229 step2-gate 的 trust-defer 对 leader pane 自然适用)。**不**调 `fail_leader_delivery`
/// 走 fallback inbox 假绿:无可用 leader pane → 返 `Blocked + channel=rebind_required`(I-4)。
#[allow(clippy::too_many_arguments)]
pub fn send_to_leader_receiver(
    workspace: &Path,
    state: &serde_json::Value,
    leader_id: &str,
    content: &str,
    task_id: Option<&TaskId>,
    sender: &str,
    requires_ack: bool,
    result_id: Option<&str>,
    event_log: &EventLog,
) -> Result<DeliveryOutcome, MessagingError> {
    send_to_leader_receiver_with_message_id(
        workspace,
        state,
        leader_id,
        content,
        task_id,
        sender,
        requires_ack,
        result_id,
        None,
        event_log,
    )
}

/// Same leader funnel as [`send_to_leader_receiver`], but lets the caller supply a
/// stable message id for transport-fallback retries. The public function above
/// keeps the existing call surface unchanged.
#[allow(clippy::too_many_arguments)]
pub fn send_to_leader_receiver_with_message_id(
    workspace: &Path,
    state: &serde_json::Value,
    leader_id: &str,
    content: &str,
    task_id: Option<&TaskId>,
    sender: &str,
    requires_ack: bool,
    result_id: Option<&str>,
    requested_message_id: Option<&str>,
    event_log: &EventLog,
) -> Result<DeliveryOutcome, MessagingError> {
    send_to_leader_receiver_with_presentation(
        workspace,
        state,
        leader_id,
        content,
        task_id,
        sender,
        requires_ack,
        result_id,
        requested_message_id,
        &PresentationDecision::default(),
        event_log,
    )
}

#[allow(clippy::too_many_arguments)]
pub fn send_to_leader_receiver_with_presentation(
    workspace: &Path,
    state: &serde_json::Value,
    leader_id: &str,
    content: &str,
    task_id: Option<&TaskId>,
    sender: &str,
    requires_ack: bool,
    result_id: Option<&str>,
    requested_message_id: Option<&str>,
    presentation: &PresentationDecision,
    event_log: &EventLog,
) -> Result<DeliveryOutcome, MessagingError> {
    let store = MessageStore::open(workspace)?;
    let owner_team = active_team_key(workspace, state);
    if requires_ack {
        event_log.write(
            "leader_receiver.no_ack_forced",
            serde_json::json!({"sender": sender, "leader_id": leader_id, "result_id": result_id}),
        )?;
    }
    let message_id = match persist_internal_send(
        workspace,
        InternalSendKind::LeaderNotification,
        Some(&owner_team),
        task_id.map(TaskId::as_str),
        sender,
        leader_id,
        content,
        None,
        false,
        requested_message_id,
        InitialDisposition::Accepted,
        Some(presentation),
    )? {
        PersistResolution::Duplicate(requested) => {
            return Ok(DeliveryOutcome {
                ok: false,
                status: DeliveryStatus::Refused,
                message_status: MessageStatusShadow("refused".to_string()),
                message_id: Some(requested),
                verification: None,
                stage: None,
                reason: Some(DeliveryRefusal::Duplicate),
                channel: Some("leader_receiver".to_string()),
            });
        }
        PersistResolution::Persisted(persisted) => persisted.message_id,
    };
    // #231 exactly-once across rebind: insert the leader_notification_log PK BEFORE the
    // unbound-pane check. That way, if the result row gets blocked (I-4) and later the
    // leader rebinds + reclaim replays this row, the dedup PK still gates any second
    // report_result attempt with the same result_id (no duplicate notification).
    if let Some(result_id) = result_id {
        let claim = store.claim_leader_notification_delivery(NotificationClaimParams {
            result_id,
            owner_team_id: Some(&owner_team),
            owner_epoch: owner_epoch_i64(state),
            leader_session_uuid: leader_session_uuid(state),
            proposed_message_id: &message_id,
            envelope_hash: "",
            pane_id: leader_pane_id(state),
        })?;
        if claim.status != "claimed_by_you" {
            // Duplicate result_id: keep first winner's notified_message_id, drop this row
            // by marking failed (deliver_pending will then skip it on its claim guard).
            store.mark(&message_id, "failed", Some("already_notified_by"))?;
            event_log.write(
                "deliver_to_leader.submit",
                serde_json::json!({
                    "message_id": claim.notified_message_id,
                    "leader_id": leader_id,
                    "owner_team_id": owner_team,
                    "result_id": result_id,
                    "dedup": "already_notified_by",
                }),
            )?;
            return Ok(DeliveryOutcome {
                ok: true,
                status: DeliveryStatus::AlreadyDelivered,
                message_status: MessageStatusShadow("already_notified".to_string()),
                message_id: Some(claim.notified_message_id),
                verification: None,
                stage: None,
                reason: None,
                channel: Some("leader_receiver".to_string()),
            });
        }
    }
    // #230 funnel fingerprint: deliver_to_leader.submit is emitted on EVERY funnel call,
    // including the I-4 unbound case below — the worker's intent IS to submit to leader,
    // the rebind path will replay this row through deliver_pending once the leader pane
    // is bound. Emitting submit only on bound would skew the funnel audit and miss the
    // exactly-once guarantee for blocked-then-reclaimed deliveries.
    event_log.write(
        "deliver_to_leader.submit",
        serde_json::json!({
            "message_id": message_id,
            "leader_id": leader_id,
            "owner_team_id": owner_team,
            "sender": sender,
            "result_id": result_id,
        }),
    )?;
    event_log.write(
        "leader_receiver.queued",
        serde_json::json!({
            "message_id": message_id,
            "leader_id": leader_id,
            "owner_team_id": owner_team,
            "result_id": result_id,
        }),
    )?;
    Ok(DeliveryOutcome {
        ok: true,
        status: DeliveryStatus::Queued,
        message_status: MessageStatusShadow("accepted".to_string()),
        message_id: Some(message_id),
        verification: None,
        stage: None,
        reason: None,
        channel: Some("leader_receiver".to_string()),
    })
}

/// `_mirror_peer_message_to_leader` (`leader.py:31`):peer→peer 消息镜像到 leader receiver。
pub fn mirror_peer_message_to_leader(
    workspace: &Path,
    state: &serde_json::Value,
    sender: &str,
    recipient: &str,
    content: &str,
    task_id: Option<&TaskId>,
    event_log: &EventLog,
) -> Result<(), MessagingError> {
    let _ = send_to_leader_receiver(
        workspace, state, "leader", content, task_id, sender, false, None, event_log,
    )?;
    let _ = recipient;
    Ok(())
}

/// E23 transport fallback: physically inject the already-built leader payload into
/// the bound leader pane only after the caller's primary path is known not to have
/// succeeded. This primitive is reclaim-neutral: it reads leader_receiver/team_owner
/// fields but never writes binding state.
#[allow(clippy::too_many_arguments)]
pub fn deliver_to_leader_fallback_pane(
    workspace: &Path,
    state: &Value,
    message_id: &str,
    result_id: Option<&str>,
    content: &str,
    primary_ok: bool,
    primary_error: Option<&str>,
    event_log: &EventLog,
) -> Result<DeliveryOutcome, MessagingError> {
    if primary_ok {
        return Ok(DeliveryOutcome {
            ok: true,
            status: DeliveryStatus::AlreadyDelivered,
            message_status: MessageStatusShadow("delivered".to_string()),
            message_id: Some(message_id.to_string()),
            verification: None,
            stage: None,
            reason: None,
            channel: Some("leader_receiver".to_string()),
        });
    }

    let store = MessageStore::open(workspace)?;
    if message_already_delivered(&store, message_id)? {
        return Ok(DeliveryOutcome {
            ok: true,
            status: DeliveryStatus::AlreadyDelivered,
            message_status: MessageStatusShadow("delivered".to_string()),
            message_id: Some(message_id.to_string()),
            verification: None,
            stage: None,
            reason: None,
            channel: Some("leader_receiver".to_string()),
        });
    }

    let owner_team_id = active_team_key(workspace, state);
    let pane_id = leader_pane_id(state).map(str::to_string);
    let primary_error = primary_error
        .filter(|error| !error.trim().is_empty())
        .unwrap_or("primary delivery failed");
    let attempt_payload = serde_json::json!({
        "message_id": message_id,
        "result_id": result_id,
        "owner_team_id": owner_team_id,
        "pane_id": pane_id,
        "primary_error": primary_error,
        "delivered_via": "fallback_pane",
    });
    event_log.write(
        "leader_receiver.fallback_pane_attempt",
        attempt_payload.clone(),
    )?;

    let Some(receiver) = leader_receiver_record(state) else {
        let failed = serde_json::json!({
            "message_id": message_id,
            "result_id": result_id,
            "owner_team_id": owner_team_id,
            "pane_id": null,
            "primary_error": primary_error,
            "delivered_via": "fallback_pane",
            "reason": "no_bound_receiver",
        });
        event_log.write("leader_receiver.fallback_pane_failed", failed)?;
        return Ok(DeliveryOutcome {
            ok: false,
            status: DeliveryStatus::Blocked,
            message_status: MessageStatusShadow("blocked".to_string()),
            message_id: Some(message_id.to_string()),
            verification: Some("run team-agent claim-leader or team-agent takeover".to_string()),
            stage: None,
            reason: Some(DeliveryRefusal::LeaderNotAttached),
            channel: Some("fallback_pane".to_string()),
        });
    };

    let rendered = render_fallback_pane_message(content, message_id, primary_error);
    let payload = InjectPayload::Text(rendered);
    // 0.5.5 gate054 cross-team notify boundary: when the leader receiver
    // has a recorded `tmux_socket` (canonical), that socket is a HARD
    // channel boundary. Pane ids (`%N`) are only unique per tmux server,
    // so falling back from a missing/dead pane on the recorded endpoint
    // to the workspace-canonical or default tmux server can inject team
    // A's leader traffic into team B's leader pane on a different socket
    // (real-machine gate054 acceptance B-arm: private socket %1 absent
    // after leader kill, default server's %1 was the main leader — the
    // prior chain silently landed A's `report_result` there).
    //
    // Loud-not-silent (user tie-break E23 / N32): if the recorded endpoint
    // rejects the inject we return `Blocked` with `channel=rebind_required`
    // and audit `leader_receiver.fallback_pane_failed` — the message row
    // stays pending in the store, coordinator surfaces the pending
    // notification via status/monitor, and no cross-server injection is
    // attempted.
    let socket_bound = leader_tmux_socket(state).is_some();
    let transports: Vec<Box<dyn Transport>> = match leader_tmux_socket(state) {
        Some(socket) => vec![Box::new(crate::transport_factory::tmux_endpoint_transport(
            socket,
        ))],
        None => vec![
            Box::new(crate::transport_factory::tmux_workspace_transport(
                workspace,
            )),
            Box::new(crate::transport_factory::tmux_default_transport()),
        ],
    };
    let mut resolved = None;
    let mut refusal = "leader channel unavailable".to_string();
    for transport in transports {
        match super::leader_channel::resolve_live_leader_channel(
            workspace,
            receiver,
            transport.as_ref(),
        ) {
            super::leader_channel::LeaderChannelResolution::Live(
                super::leader_channel::LiveLeaderChannel::DirectTmux(channel),
            ) => {
                resolved = Some((transport, channel));
                break;
            }
            other => refusal = format!("{other:?}"),
        }
    }
    let Some((transport, channel)) = resolved else {
        event_log.write(
            "leader_receiver.fallback_pane_failed",
            serde_json::json!({
                "message_id": message_id,
                "result_id": result_id,
                "owner_team_id": owner_team_id,
                "pane_id": pane_id,
                "primary_error": primary_error,
                "delivered_via": "fallback_pane",
                "socket_bound": socket_bound,
                "reason": refusal,
            }),
        )?;
        return Ok(DeliveryOutcome {
            ok: false,
            status: DeliveryStatus::Blocked,
            message_status: MessageStatusShadow("blocked".to_string()),
            message_id: Some(message_id.to_string()),
            verification: Some("run team-agent claim-leader or team-agent takeover".to_string()),
            stage: None,
            reason: Some(DeliveryRefusal::LeaderNotAttached),
            channel: Some("rebind_required".to_string()),
        });
    };
    let target = Target::Pane(PaneId::new(channel.pane_id));
    let inject_result: Result<InjectReport, TransportError> =
        transport.inject(&target, &payload, Key::Enter, true);

    match inject_result {
        Ok(report) => {
            // 0.3.30: submit verification is enough for fallback-pane
            // delivery. Readback is retained for diagnostics when submit
            // itself is unverified.
            let submit_ok = super::delivery::inject_submit_verified(&report);
            let readback_ok = super::delivery::pane_readback_verified(&report);
            if submit_ok {
                store.record_delivery_submission(message_id, readback_ok)?;
                if !super::delivery::leader_transcript_has_token(state, message_id) {
                    store.mark(message_id, "submitted_pending_acceptance", None)?;
                    event_log.write(
                        "leader_receiver.acceptance_pending",
                        serde_json::json!({
                            "message_id": message_id,
                            "result_id": result_id,
                            "reason": "provider_receipt_not_observed",
                            "channel": "fallback_pane",
                        }),
                    )?;
                    return Ok(DeliveryOutcome {
                        ok: true,
                        status: DeliveryStatus::RetryScheduled,
                        message_status: MessageStatusShadow(
                            "submitted_pending_acceptance".to_string(),
                        ),
                        message_id: Some(message_id.to_string()),
                        verification: Some("provider_receipt_not_observed".to_string()),
                        stage: Some(DeliveryStage::Submit),
                        reason: None,
                        channel: Some("leader_acceptance_pending".to_string()),
                    });
                }
                store.mark_delivered_with_receipt(message_id)?;
                event_log.write(
                    "leader_receiver.fallback_pane_submitted",
                    serde_json::json!({
                        "message_id": message_id,
                        "result_id": result_id,
                        "owner_team_id": owner_team_id,
                        "pane_id": pane_id,
                        "primary_error": primary_error,
                        "delivered_via": "fallback_pane",
                        "verification": format!("{:?}", report.submit_verification),
                    }),
                )?;
                Ok(DeliveryOutcome {
                    ok: true,
                    status: DeliveryStatus::Delivered,
                    message_status: MessageStatusShadow("delivered".to_string()),
                    message_id: Some(message_id.to_string()),
                    verification: Some("delivered_via=fallback_pane".to_string()),
                    stage: None,
                    reason: None,
                    channel: Some("fallback_pane".to_string()),
                })
            } else {
                let reason = format!(
                    "fallback_pane_unverified:submit={submit_ok},readback={readback_ok},sv={:?}",
                    report.submit_verification
                );
                store.mark(message_id, "submitted_unverified", Some(&reason))?;
                event_log.write(
                    "leader_receiver.fallback_pane_unverified",
                    serde_json::json!({
                        "message_id": message_id,
                        "pane_id": pane_id,
                        "reason": reason,
                        "primary_error": primary_error,
                    }),
                )?;
                Ok(DeliveryOutcome {
                    ok: false,
                    status: DeliveryStatus::Failed,
                    message_status: MessageStatusShadow("submitted_unverified".to_string()),
                    message_id: Some(message_id.to_string()),
                    verification: Some(reason),
                    stage: Some(DeliveryStage::Submit),
                    reason: None,
                    channel: Some("fallback_pane".to_string()),
                })
            }
        }
        Err(error) => {
            // 0.5.5 gate054: when a leader_receiver.tmux_socket is recorded,
            // a fallback inject error is a HARD team-boundary event — we do
            // not cross to workspace/default tmux (see the inject_result
            // branch above). Surface `rebind_required` so callers wire the
            // notification status accordingly; the row stays pending in the
            // store and the audit event carries the same forensic fields as
            // the pre-fix path.
            let socket_bound = leader_tmux_socket(state).is_some();
            event_log.write(
                "leader_receiver.fallback_pane_failed",
                serde_json::json!({
                    "message_id": message_id,
                    "result_id": result_id,
                    "owner_team_id": owner_team_id,
                    "pane_id": pane_id,
                    "primary_error": primary_error,
                    "delivered_via": "fallback_pane",
                    "reason": error.to_string(),
                    "socket_bound": socket_bound,
                }),
            )?;
            // Row status is left untouched so status/monitor surfaces the
            // pending notification until the operator rebinds the leader.
            let (status, channel) = if socket_bound {
                (DeliveryStatus::Blocked, "rebind_required".to_string())
            } else {
                (DeliveryStatus::Failed, "fallback_pane".to_string())
            };
            Ok(DeliveryOutcome {
                ok: false,
                status,
                message_status: MessageStatusShadow("failed".to_string()),
                message_id: Some(message_id.to_string()),
                verification: Some(error.to_string()),
                stage: None,
                reason: Some(DeliveryRefusal::TmuxTargetMissing),
                channel: Some(channel),
            })
        }
    }
}

pub(crate) fn active_team_key(workspace: &Path, state: &Value) -> String {
    state
        .get("active_team_key")
        .and_then(Value::as_str)
        .filter(|team| !team.is_empty())
        .map(ToString::to_string)
        .or_else(|| {
            workspace
                .file_name()
                .map(|name| name.to_string_lossy().to_string())
        })
        .unwrap_or_else(|| "current".to_string())
}

fn owner_epoch(state: &Value) -> Option<u64> {
    receiver_or_owner_field(state, "team_owner", "owner_epoch")
        .and_then(Value::as_u64)
        .or_else(|| {
            receiver_or_owner_field(state, "leader_receiver", "owner_epoch").and_then(Value::as_u64)
        })
}

fn receiver_or_owner_field<'a>(state: &'a Value, record: &str, field: &str) -> Option<&'a Value> {
    state.get(record).and_then(|v| v.get(field)).or_else(|| {
        let active = state.get("active_team_key").and_then(Value::as_str)?;
        state
            .get("teams")
            .and_then(Value::as_object)
            .and_then(|teams| teams.get(active))
            .and_then(|team| team.get(record))
            .and_then(|v| v.get(field))
    })
}

fn leader_record_field<'a>(state: &'a Value, field: &str) -> Option<&'a Value> {
    receiver_or_owner_field(state, "leader_receiver", field)
        .or_else(|| receiver_or_owner_field(state, "team_owner", field))
}

fn leader_receiver_record(state: &Value) -> Option<&Value> {
    state.get("leader_receiver").or_else(|| {
        let active = state.get("active_team_key").and_then(Value::as_str)?;
        state
            .get("teams")
            .and_then(Value::as_object)
            .and_then(|teams| teams.get(active))
            .and_then(|team| team.get("leader_receiver"))
    })
}

fn owner_epoch_i64(state: &Value) -> Option<i64> {
    owner_epoch(state).and_then(|epoch| i64::try_from(epoch).ok())
}

fn leader_session_uuid(state: &Value) -> Option<&str> {
    leader_record_field(state, "leader_session_uuid").and_then(Value::as_str)
}

pub(crate) fn leader_pane_bound_but_not_live(workspace: &Path, state: &Value) -> bool {
    leader_pane_id(state).is_some_and(|pane_id| !leader_pane_is_live(workspace, state, pane_id))
}

fn leader_pane_is_live(workspace: &Path, state: &Value, pane_id: &str) -> bool {
    // 0.5.5 gate054 boundary: when a socket is recorded, liveness is
    // scoped to that endpoint. Never union with workspace/default targets
    // (pane ids are only unique per tmux server; a stray same-numbered
    // pane on another server would falsely report "live" and steer the
    // fallback path into a cross-team inject).
    if let Some(socket) = leader_tmux_socket(state) {
        return crate::transport_factory::tmux_endpoint_transport(socket)
            .list_targets()
            .unwrap_or_default()
            .iter()
            .any(|target| target.pane_id.as_str() == pane_id);
    }
    let mut targets = crate::transport_factory::tmux_workspace_transport(workspace)
        .list_targets()
        .unwrap_or_default();
    targets.extend(
        crate::transport_factory::tmux_default_transport()
            .list_targets()
            .unwrap_or_default(),
    );
    targets
        .iter()
        .any(|target| target.pane_id.as_str() == pane_id)
}

fn leader_pane_id(state: &Value) -> Option<&str> {
    leader_record_field(state, "pane_id").and_then(Value::as_str)
}

fn leader_tmux_socket(state: &Value) -> Option<&str> {
    leader_record_field(state, "tmux_socket")
        .and_then(Value::as_str)
        .filter(|socket| !socket.is_empty())
        .filter(|socket| std::path::Path::new(socket).is_absolute())
}

fn message_already_delivered(
    store: &MessageStore,
    message_id: &str,
) -> Result<bool, MessagingError> {
    let conn = crate::db::schema::open_db(store.db_path())?;
    let status = conn
        .query_row(
            "select status from messages where message_id = ?1",
            [message_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    Ok(matches!(
        status.as_deref(),
        Some("delivered" | "acknowledged" | "submitted" | "submitted_unverified")
    ))
}

fn render_fallback_pane_message(content: &str, message_id: &str, primary_error: &str) -> String {
    format!(
        "Team Agent fallback delivery\n\
         delivered_via=fallback_pane\n\
         primary_error: {primary_error}\n\n\
         {content}\n\n\
         [team-agent-token:{message_id}]"
    )
}

/// E6 (0.5.9 offline-mailbox-toname-design §6.3): enqueue a leader-bound message
/// row in the TARGET workspace `team.db` with status `queued_until_leader_attach`.
///
/// This is a durable "leader mailbox" write, NOT a delivery. Coordinator delivery
/// never claims this status (§3.3), so the row will not churn or be marked
/// `failed` while the leader is unattached. When the target owner runs
/// `attach-leader` / `claim-leader`, the existing `requeue_blocked_leader_messages`
/// helper flips the same row to `accepted` and the standard delivery pipeline
/// injects it exactly once.
///
/// Safety boundary (§5): only the `messages` table is touched. Owner identity
/// (`leader_receiver` / `team_owner` / `owner_epoch`) is never written by this
/// path, and no provider/worker process is spawned.
pub fn enqueue_leader_mailbox_until_attach(
    target_workspace: &Path,
    canonical_team_key: &str,
    content: &str,
    task_id: Option<&TaskId>,
    sender: &str,
    event_log: &EventLog,
) -> Result<DeliveryOutcome, MessagingError> {
    let PersistResolution::Persisted(persisted) = persist_internal_send(
        target_workspace,
        InternalSendKind::OfflineMailbox,
        Some(canonical_team_key),
        task_id.map(TaskId::as_str),
        sender,
        "leader",
        content,
        None,
        false,
        None,
        InitialDisposition::QueuedUntilLeaderAttach,
        None,
    )?
    else {
        unreachable!("offline mailbox does not accept caller-supplied ids")
    };
    let message_status = persisted.row_status.as_str().to_string();
    let message_id = persisted.message_id;
    event_log.write(
        "leader_mailbox.queued_until_attach",
        serde_json::json!({
            "message_id": message_id,
            "owner_team_id": canonical_team_key,
            "sender": sender,
            "target_workspace": target_workspace.display().to_string(),
            "channel": "leader_mailbox",
        }),
    )?;
    Ok(DeliveryOutcome {
        ok: true,
        status: DeliveryStatus::Queued,
        message_status: MessageStatusShadow(message_status),
        message_id: Some(message_id),
        verification: None,
        stage: None,
        reason: None,
        channel: Some("leader_mailbox".to_string()),
    })
}
