//! leader.py — leader pane 注入边界 + 恰好一次去重门 (card §21/§72)。

use std::path::Path;

use serde_json::Value;

use crate::event_log::EventLog;
use crate::message_store::{MessageStore, NotificationClaimParams};
use crate::model::ids::{OwnerEpoch, TaskId};
use crate::transport::Transport;

use super::helpers::MessageStatusShadow;
use super::{DeliveryOutcome, DeliveryStatus, MessagingError};

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
    let store = MessageStore::open(workspace)?;
    let owner_team = active_team_key(workspace, state);
    if requires_ack {
        event_log.write(
            "leader_receiver.no_ack_forced",
            serde_json::json!({"sender": sender, "leader_id": leader_id, "result_id": result_id}),
        )?;
    }
    let message_id = store.create_message(
        task_id.map(TaskId::as_str),
        sender,
        leader_id,
        content,
        None,
        false,
        Some(&owner_team),
    )?;
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
        return Err(MessagingError::Routing("runtime state root is not an object".to_string()));
    };
    let owner = root
        .entry("team_owner")
        .or_insert_with(|| serde_json::json!({}));
    if let Some(owner) = owner.as_object_mut() {
        owner.insert("owner_epoch".to_string(), serde_json::json!(next_epoch));
    }
    let receiver = root
        .entry("leader_receiver")
        .or_insert_with(|| serde_json::json!({}));
    if let Some(receiver) = receiver.as_object_mut() {
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
    crate::state::persist::save_runtime_state(workspace, state)?;
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
        workspace,
        state,
        "leader",
        content,
        task_id,
        sender,
        false,
        None,
        event_log,
    )?;
    let _ = recipient;
    Ok(())
}

pub(crate) fn active_team_key(workspace: &Path, state: &Value) -> String {
    state
        .get("active_team_key")
        .and_then(Value::as_str)
        .filter(|team| !team.is_empty())
        .map(ToString::to_string)
        .or_else(|| workspace.file_name().map(|name| name.to_string_lossy().to_string()))
        .unwrap_or_else(|| "current".to_string())
}

fn owner_epoch(state: &Value) -> Option<u64> {
    receiver_or_owner_field(state, "team_owner", "owner_epoch")
        .and_then(Value::as_u64)
        .or_else(|| receiver_or_owner_field(state, "leader_receiver", "owner_epoch").and_then(Value::as_u64))
}

fn receiver_or_owner_field<'a>(state: &'a Value, record: &str, field: &str) -> Option<&'a Value> {
    state
        .get(record)
        .and_then(|v| v.get(field))
        .or_else(|| {
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
    leader_pane_id(state)
        .is_some_and(|pane_id| !leader_pane_is_live(workspace, state, pane_id))
}

fn leader_pane_is_live(workspace: &Path, state: &Value, pane_id: &str) -> bool {
    if let Some(socket) = leader_tmux_socket(state) {
        return crate::tmux_backend::TmuxBackend::for_tmux_endpoint(socket)
            .list_targets()
            .unwrap_or_default()
            .iter()
            .any(|target| target.pane_id.as_str() == pane_id);
    }
    let mut targets = crate::tmux_backend::TmuxBackend::for_workspace(workspace)
        .list_targets()
        .unwrap_or_default();
    targets.extend(
        crate::tmux_backend::TmuxBackend::new()
            .list_targets()
            .unwrap_or_default(),
    );
    targets.iter().any(|target| target.pane_id.as_str() == pane_id)
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

fn copy_candidate_field(
    out: &mut serde_json::Map<String, Value>,
    candidate: &Value,
    key: &str,
) {
    if let Some(value) = candidate.get(key) {
        out.insert(key.to_string(), value.clone());
    }
}
