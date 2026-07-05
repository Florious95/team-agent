//! leader.py — leader pane 注入边界 + 恰好一次去重门 (card §21/§72)。

use std::path::Path;

use rusqlite::OptionalExtension;
use serde_json::Value;

use crate::event_log::EventLog;
use crate::message_store::{MessageStore, NotificationClaimParams};
use crate::model::ids::{OwnerEpoch, TaskId};
use crate::transport::{InjectPayload, Key, PaneId, Target, Transport};

use super::helpers::MessageStatusShadow;
use super::{DeliveryOutcome, DeliveryRefusal, DeliveryStage, DeliveryStatus, MessagingError};

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
    let store = MessageStore::open(workspace)?;
    let owner_team = active_team_key(workspace, state);
    if requires_ack {
        event_log.write(
            "leader_receiver.no_ack_forced",
            serde_json::json!({"sender": sender, "leader_id": leader_id, "result_id": result_id}),
        )?;
    }
    let message_id = if let Some(requested) = requested_message_id {
        if store.message_exists(requested)? {
            return Ok(DeliveryOutcome {
                ok: false,
                status: DeliveryStatus::Refused,
                message_status: MessageStatusShadow("refused".to_string()),
                message_id: Some(requested.to_string()),
                verification: None,
                stage: None,
                reason: Some(DeliveryRefusal::Duplicate),
                channel: Some("leader_receiver".to_string()),
            });
        }
        store.create_message_with_id(
            requested,
            task_id.map(TaskId::as_str),
            sender,
            leader_id,
            content,
            None,
            false,
            Some(&owner_team),
        )?
    } else {
        store.create_message(
            task_id.map(TaskId::as_str),
            sender,
            leader_id,
            content,
            None,
            false,
            Some(&owner_team),
        )?
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

/// `claim_leader_receiver` (`leader.py:348`):认领/接管 leader pane + `owner_epoch++`。身份判定
/// 借 step 10 原语 (`_target_matches_owner_identity`/`_leader_command_looks_usable`)。
pub fn claim_leader_receiver(
    workspace: &Path,
    state: &mut serde_json::Value,
    candidate: &serde_json::Value,
    event_log: &EventLog,
    confirm: bool,
    expected_epoch: Option<OwnerEpoch>,
) -> Result<serde_json::Value, MessagingError> {
    if !confirm {
        return Ok(serde_json::json!({
            "ok": false,
            "status": "refused",
            "reason": "confirm_required",
            "action": "team-agent claim-leader --confirm",
        }));
    }
    if let Some(expected) = expected_epoch {
        if owner_epoch(state).is_some_and(|current| current != expected.0) {
            let owner_epoch = owner_epoch(state).unwrap_or(0);
            let bound_pane_id = leader_pane_id(state).map(ToString::to_string);
            let value = serde_json::json!({
                "ok": false,
                "status": "refused",
                "reason": "owner_epoch_advanced",
                "owner_epoch": owner_epoch,
                "bound_pane_id": bound_pane_id,
            });
            event_log.write("leader_receiver.claim_refused", value.clone())?;
            return Ok(value);
        }
    }
    let candidate_pane = candidate.get("pane_id").and_then(Value::as_str);
    if candidate_pane.is_some() && candidate_pane == leader_pane_id(state) {
        return Ok(serde_json::json!({
            "ok": true,
            "status": "already_bound",
            "pane_id": candidate_pane,
            "owner_epoch": owner_epoch(state).unwrap_or(0),
        }));
    }
    let next_epoch = owner_epoch(state).unwrap_or(0).saturating_add(1);
    let Some(root) = state.as_object_mut() else {
        return Err(MessagingError::Routing(
            "runtime state root is not an object".to_string(),
        ));
    };
    // Build the updated owner + receiver values in-place (field-level
    // mutation preserves any extra fields the caller carries), then publish
    // through the ownership repository. This keeps `claim_leader_receiver`'s
    // legacy field-merge semantics while sourcing all owner mutations from
    // one API surface (Stage 3a, architect direction 2026-06-23).
    let mut owner_value = root
        .get("team_owner")
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));
    if let Some(owner) = owner_value.as_object_mut() {
        owner.insert("owner_epoch".to_string(), serde_json::json!(next_epoch));
    }
    let mut receiver_value = root
        .get("leader_receiver")
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));
    if let Some(receiver) = receiver_value.as_object_mut() {
        receiver.insert("mode".to_string(), serde_json::json!("direct_tmux"));
        receiver.insert("owner_epoch".to_string(), serde_json::json!(next_epoch));
        copy_candidate_field(receiver, candidate, "pane_id");
        copy_candidate_field(receiver, candidate, "provider");
        copy_candidate_field(receiver, candidate, "leader_session_uuid");
        if let Some(socket) = candidate
            .get("tmux_socket")
            .and_then(Value::as_str)
            .filter(|socket| std::path::Path::new(socket).is_absolute())
            .map(str::to_string)
            .or_else(crate::tmux_backend::socket_name_from_tmux_env)
        {
            receiver.insert("tmux_socket".to_string(), serde_json::json!(socket));
        }
    }
    // The `root` borrow above is released by NLL before this call.
    let team_key = crate::state::projection::team_state_key(state);
    let record = crate::state::ownership::OwnershipWrite::new()
        .with_team_owner(owner_value)
        .with_leader_receiver(receiver_value)
        .with_owner_epoch(next_epoch);
    crate::state::ownership::write_owner(state, &team_key, record);
    crate::state::persist::save_runtime_state_reapplying_after_conflict(
        workspace,
        state,
        |latest| {
            let team_key = crate::state::projection::team_state_key(latest);
            let record = crate::state::ownership::OwnershipWrite::new()
                .with_team_owner(
                    state
                        .get("team_owner")
                        .cloned()
                        .unwrap_or_else(|| serde_json::json!({})),
                )
                .with_leader_receiver(
                    state
                        .get("leader_receiver")
                        .cloned()
                        .unwrap_or_else(|| serde_json::json!({})),
                )
                .with_owner_epoch(next_epoch);
            crate::state::ownership::write_owner(latest, &team_key, record);
        },
    )?;
    event_log.write(
        "leader_receiver.claimed",
        serde_json::json!({"owner_epoch": next_epoch, "candidate": candidate}),
    )?;
    let receiver = state
        .get("leader_receiver")
        .cloned()
        .unwrap_or_else(|| candidate.clone());
    Ok(serde_json::json!({
        "ok": true,
        "status": "claimed",
        "leader_receiver": receiver,
        "owner_epoch": next_epoch,
    }))
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

    let Some(pane_id) = pane_id.filter(|pane| !pane.is_empty() && pane != "__team_agent_unbound__")
    else {
        let failed = serde_json::json!({
            "message_id": message_id,
            "result_id": result_id,
            "owner_team_id": owner_team_id,
            "pane_id": null,
            "primary_error": primary_error,
            "delivered_via": "fallback_pane",
            "reason": "no_bound_pane",
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
    let target = Target::Pane(PaneId::new(&pane_id));
    let payload = InjectPayload::Text(rendered);
    // 0.5.x Phase 1d Batch 4: leader-fallback pane inject uses the
    // factory tmux channel helpers so `grep transport_factory::tmux_`
    // enumerates every tmux-only leader-channel site. Semantics are
    // identical to the previous direct `TmuxBackend::*` calls
    // (helpers are thin wrappers). `transport_kind`-based dispatch
    // is the parallel `feat/appserver-leader-host` work; this batch
    // stays helper-swap only.
    let inject_result = leader_tmux_socket(state)
        .and_then(|socket| {
            let backend = crate::transport_factory::tmux_endpoint_transport(socket);
            backend.inject(&target, &payload, Key::Enter, true).ok()
        })
        .map(Ok)
        .unwrap_or_else(|| {
            let backend = crate::transport_factory::tmux_workspace_transport(workspace);
            backend.inject(&target, &payload, Key::Enter, true)
        })
        .or_else(|_| {
            let backend = crate::transport_factory::tmux_default_transport();
            backend.inject(&target, &payload, Key::Enter, true)
        });

    match inject_result {
        Ok(report) => {
            // 0.3.30: submit verification is enough for fallback-pane
            // delivery. Readback is retained for diagnostics when submit
            // itself is unverified.
            let submit_ok = super::delivery::inject_submit_verified(&report);
            let readback_ok = super::delivery::pane_readback_verified(&report);
            if submit_ok {
                store.mark(message_id, "delivered", None)?;
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
                }),
            )?;
            Ok(DeliveryOutcome {
                ok: false,
                status: DeliveryStatus::Failed,
                message_status: MessageStatusShadow("failed".to_string()),
                message_id: Some(message_id.to_string()),
                verification: Some(error.to_string()),
                stage: None,
                reason: Some(DeliveryRefusal::TmuxTargetMissing),
                channel: Some("fallback_pane".to_string()),
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
    // 0.5.x Phase 1d Batch 4: leader liveness uses factory tmux
    // channel helpers. Semantics unchanged; `transport_kind`-based
    // dispatch is the parallel feat/appserver-leader-host work.
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

fn copy_candidate_field(out: &mut serde_json::Map<String, Value>, candidate: &Value, key: &str) {
    if let Some(value) = candidate.get(key) {
        out.insert(key.to_string(), value.clone());
    }
}
