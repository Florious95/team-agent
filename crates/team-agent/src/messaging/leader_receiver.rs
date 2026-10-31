//!
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
/// caller(send_message(to=leader) / report_result / idle reminder /
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
pub(crate) fn send_to_leader_receiver_with_message_id(
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
    Err(MessagingError::Validation("legacy SQL messaging retired; use station send and C3 inbox".into()))
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
    Err(MessagingError::Validation("legacy SQL messaging retired; use station send and C3 inbox".into()))
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
    Err(MessagingError::Validation("legacy SQL messaging retired; use station send and C3 inbox".into()))
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
        ack_forced_off: false,
        turn_verification: None,
    })
}
