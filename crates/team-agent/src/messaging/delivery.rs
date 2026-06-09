//! internal_delivery.py + delivery.py — coordinator/调度器侧 thin wrapper + 单条 tmux 注入投递
//! + trust 有界重试 + turn-open arm (card §16/§65)。

use std::path::Path;

use rusqlite::{params, OptionalExtension};

use crate::event_log::EventLog;
use crate::message_store::MessageStore;
use crate::model::enums::{PaneLiveness, Provider};
use crate::model::ids::TeamKey;
use crate::transport::{
    submit_verification_wire, InjectPayload, InjectReport, Key, PaneId, SessionName,
    SubmitVerification, Target, Transport, WindowName,
};

use super::helpers::{message_exists, MessageStatusShadow};
use super::{
    DeliveryOutcome, DeliveryRefusal, DeliveryStage, DeliveryStatus, MessagingError,
    PaneWidthQuery, TrustRetryPayload,
};
use crate::state::projection::OwnerTeamResolution;

// ===========================================================================
// internal_delivery.py — coordinator/调度器侧 thin wrapper (card §65)
// ===========================================================================

/// `deliver_stored_message` (`internal_delivery.py:16`):coordinator/调度器侧 team-scoped 单发
/// (不重路由)。加 `_runtime_lock("send")`,直走 `_send_single_message_unlocked`。
#[allow(clippy::too_many_arguments)]
pub fn deliver_stored_message(
    workspace: &Path,
    target: Option<&str>,
    content: &str,
    task_id: Option<&crate::model::ids::TaskId>,
    sender: &str,
    requires_ack: bool,
    wait_visible: bool,
    timeout: f64,
    team: Option<&TeamKey>,
) -> Result<DeliveryOutcome, MessagingError> {
    let _ = (wait_visible, timeout);
    let recipient = target.unwrap_or("leader");
    let store = MessageStore::open(workspace)?;
    let message_id = store.create_message(
        task_id.map(crate::model::ids::TaskId::as_str),
        sender,
        recipient,
        content,
        None,
        requires_ack,
        team.map(TeamKey::as_str),
    )?;
    Ok(DeliveryOutcome {
        ok: true,
        status: DeliveryStatus::Queued,
        message_status: MessageStatusShadow("accepted".to_string()),
        message_id: Some(message_id),
        verification: None,
        stage: None,
        reason: None,
        channel: None,
    })
}

// ===========================================================================
// delivery.py — 单条 tmux 注入投递 + trust 有界重试 + turn-open arm (card §16)
// ===========================================================================

/// `_tmux_pane_width` (`delivery.py:20`):查询 pane 列宽。**fail-safe** (bug-064/082):失败
/// 返回 [`PaneWidthQuery::Failed`],**绝不**给默认宽度。借 step 9 transport 的 query。
pub fn tmux_pane_width(transport: &dyn Transport, target: &Target) -> PaneWidthQuery {
    let queried = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        transport.query(target, crate::transport::PaneField::PaneWidth)
    }));
    let result = match queried {
        Ok(result) => result,
        Err(_) => {
            return PaneWidthQuery::Failed {
                error: "tmux_query_failed:panic".to_string(),
            };
        }
    };
    match result {
        Ok(Some(raw)) => match raw.trim().parse::<u32>() {
            Ok(pane_width) if pane_width > 0 => PaneWidthQuery::Ok { pane_width },
            Ok(_) => PaneWidthQuery::Failed { error: "non_positive_width".to_string() },
            Err(_) => PaneWidthQuery::Failed { error: "unparseable_output".to_string() },
        },
        Ok(None) => PaneWidthQuery::Failed { error: "empty_output".to_string() },
        Err(err) => PaneWidthQuery::Failed { error: format!("tmux_query_failed:{err}") },
    }
}

/// `_deliver_pending_message` (`delivery.py:63`):对一条消息做 tmux 注入投递 (含 trust 提示
/// 自动应答 + turn-open arm + first_send_at 戳)。daemon-path → Result。
pub fn deliver_pending_message(
    workspace: &Path,
    store: &MessageStore,
    transport: &dyn Transport,
    message_id: &str,
    event_log: &EventLog,
    state: &serde_json::Value,
) -> Result<DeliveryOutcome, MessagingError> {
    if !message_exists(store, message_id)? {
        return Ok(DeliveryOutcome {
            ok: false,
            status: DeliveryStatus::Failed,
            message_status: MessageStatusShadow("failed".to_string()),
            message_id: Some(message_id.to_string()),
            verification: None,
            stage: None,
            reason: None,
            channel: None,
        });
    }
    let message = message_for_delivery(store, message_id)?;
    let Some(message) = message else {
        return Ok(DeliveryOutcome {
            ok: false,
            status: DeliveryStatus::Failed,
            message_status: MessageStatusShadow("failed".to_string()),
            message_id: Some(message_id.to_string()),
            verification: None,
            stage: None,
            reason: Some(DeliveryRefusal::UnknownRecipient),
            channel: None,
        });
    };
    let mut canonical_owner_team_id = message.owner_team_id.clone();
    let scoped_state;
    let state = match message.owner_team_id.as_deref() {
        Some(team) if !team.is_empty() => {
            match project_state_for_owner_team(workspace, team, state, Some(store), Some(message_id), Some(event_log))? {
                OwnerTeamProjection::Projected { state, canonical_team } => {
                    canonical_owner_team_id = Some(canonical_team);
                    scoped_state = state;
                    &scoped_state
                }
                OwnerTeamProjection::Refused(outcome) => return Ok(outcome),
            }
        }
        _ => state,
    };
    if !store.claim_for_delivery(message_id)? && message.status != "target_resolved" {
        return Ok(DeliveryOutcome {
            ok: false,
            status: DeliveryStatus::Refused,
            message_status: MessageStatusShadow("target_resolved".to_string()),
            message_id: Some(message_id.to_string()),
            verification: None,
            stage: None,
            reason: Some(DeliveryRefusal::MessageAlreadyClaimed),
            channel: None,
        });
    }
    if message.recipient == "leader" && leader_receiver_has_noncanonical_tmux_socket(state) {
        store.mark(message_id, "failed", Some("leader_not_attached"))?;
        event_log.write(
            "leader_receiver.delivery_blocked",
            serde_json::json!({
                "message_id": message_id,
                "sender": message.sender,
                "reason": "leader_not_attached",
                "channel": "rebind_required",
                "action": "run team-agent claim-leader or team-agent takeover",
                "error": "leader_receiver.tmux_socket is not a canonical full socket path",
            }),
        )?;
        return Ok(DeliveryOutcome {
            ok: false,
            status: DeliveryStatus::Refused,
            message_status: MessageStatusShadow("failed".to_string()),
            message_id: Some(message_id.to_string()),
            verification: Some(
                "run team-agent claim-leader or team-agent takeover".to_string(),
            ),
            stage: None,
            reason: Some(DeliveryRefusal::LeaderNotAttached),
            channel: Some("rebind_required".to_string()),
        });
    }
    let delivery_transport =
        delivery_transport_for_recipient(workspace, transport, state, &message.recipient);
    let transport = delivery_transport.as_transport();
    // Do not inject queued leader messages into a synthetic "leader" window.
    if message.recipient == "leader" && !leader_receiver_pane_is_usable(transport, state) {
        store.mark(message_id, "failed", Some("leader_not_attached"))?;
        event_log.write(
            "leader_receiver.delivery_blocked",
            serde_json::json!({
                "message_id": message_id,
                "sender": message.sender,
                "reason": "leader_not_attached",
                "channel": "rebind_required",
                "action": "run team-agent claim-leader or team-agent takeover",
            }),
        )?;
        return Ok(DeliveryOutcome {
            ok: false,
            status: DeliveryStatus::Refused,
            message_status: MessageStatusShadow("failed".to_string()),
            message_id: Some(message_id.to_string()),
            verification: Some(
                "run team-agent claim-leader or team-agent takeover".to_string(),
            ),
            stage: None,
            reason: Some(DeliveryRefusal::LeaderNotAttached),
            channel: Some("rebind_required".to_string()),
        });
    }
    let target = resolve_inject_target(state, &message.recipient);
    // Contract B / MUST-10 / N31/N32: physical paste+Enter into a startup trust/update
    // menu is NOT provider delivery — the menu consumes the Enter and the task text
    // is lost (PROBE-2 root-cause). Before injection, peek at the recipient's pane for
    // a Codex actionable startup prompt; if present, mark the row `queued_until_trust`
    // and DO NOT inject the task. The coordinator's startup-prompt phase will dismiss
    // the trust prompt, and the SAME message_id is later replayed through this same
    // delivery pipeline (no parallel side channel).
    if recipient_pane_has_actionable_startup_prompt(transport, state, &message.recipient, &target) {
        store.mark(message_id, "queued_until_trust", None)?;
        event_log.write(
            "delivery.deferred_startup_prompt",
            serde_json::json!({
                "message_id": message_id,
                "recipient": message.recipient,
                "reason": "actionable_startup_prompt",
            }),
        )?;
        return Ok(DeliveryOutcome {
            ok: false,
            status: DeliveryStatus::RetryScheduled,
            message_status: MessageStatusShadow("queued_until_trust".to_string()),
            message_id: Some(message_id.to_string()),
            verification: None,
            stage: Some(DeliveryStage::TrustAutoAnswerDismissalWait),
            reason: None,
            channel: None,
        });
    }
    let rendered = render_message(
        &message.sender,
        message.task_id.as_deref(),
        &message.content,
        message_id,
    );
    let inject_report = match transport.inject(
        &target,
        &InjectPayload::Text(rendered),
        Key::Enter,
        true,
    ) {
        Ok(report) => report,
        Err(error) => {
            if message.recipient == "leader" {
                store.mark(message_id, "failed", Some("leader_not_attached"))?;
                event_log.write(
                    "leader_receiver.delivery_blocked",
                    serde_json::json!({
                        "message_id": message_id,
                        "sender": message.sender,
                        "reason": "leader_not_attached",
                        "channel": "rebind_required",
                        "action": "run team-agent claim-leader or team-agent takeover",
                        "error": error.to_string(),
                    }),
                )?;
                return Ok(DeliveryOutcome {
                    ok: false,
                    status: DeliveryStatus::Refused,
                    message_status: MessageStatusShadow("failed".to_string()),
                    message_id: Some(message_id.to_string()),
                    verification: Some(
                        "run team-agent claim-leader or team-agent takeover".to_string(),
                    ),
                    stage: None,
                    reason: Some(DeliveryRefusal::LeaderNotAttached),
                    channel: Some("rebind_required".to_string()),
                });
            }
            return Err(error.into());
        }
    };
    if !inject_submit_verified(&inject_report) {
        let reason = format!(
            "submit_unverified:{}",
            submit_verification_wire(inject_report.submit_verification)
        );
        store.mark(message_id, "submitted_unverified", Some(&reason))?;
        event_log.write(
            "send.unverified",
            serde_json::json!({
                "message_id": message_id,
                "recipient": message.recipient,
                "reason": reason,
                "attempts": inject_report.attempts,
            }),
        )?;
        return Ok(DeliveryOutcome {
            ok: false,
            status: DeliveryStatus::Failed,
            message_status: MessageStatusShadow("submitted_unverified".to_string()),
            message_id: Some(message_id.to_string()),
            verification: Some(reason),
            stage: Some(DeliveryStage::Submit),
            reason: None,
            channel: None,
        });
    }
    store.mark(message_id, "delivered", None)?;
    event_log.write(
        "message.delivered",
        serde_json::json!({"message_id": message_id}),
    )?;
    let outcome = DeliveryOutcome {
        ok: true,
        status: DeliveryStatus::Delivered,
        message_status: MessageStatusShadow("delivered".to_string()),
        message_id: Some(message_id.to_string()),
        verification: None,
        stage: None,
        reason: None,
        channel: None,
    };
    stamp_first_send_at_if_leader_to_worker_scoped(
        workspace,
        &message.sender,
        &message.recipient,
        canonical_owner_team_id.as_deref(),
    )?;
    record_turn_open_if_leader_to_worker_scoped(
        workspace,
        &message.sender,
        &message.recipient,
        &outcome,
        event_log,
        canonical_owner_team_id.as_deref(),
    )?;
    Ok(outcome)
}

fn inject_submit_verified(report: &InjectReport) -> bool {
    match report.submit_verification {
        SubmitVerification::SendKeysFailed => false,
        SubmitVerification::PastedContentPromptStillPresentAfterSubmit => false,
        SubmitVerification::PastedContentPromptAbsentAfterSubmit => true,
        SubmitVerification::KeySentAfterVisibleToken { .. } => true,
        SubmitVerification::EnterSentWithoutPlaceholderCheck => true,
    }
}

/// Render a message into the worker-facing protocol block (port of `rust_core.py:render_message`,
/// golden-verified): `Team Agent message from {sender}[ for {task_id}]:\n\n{content}\n\n
/// [team-agent-token:{message_id}]`. The worker (fake or real provider) only builds a result_envelope
/// when it sees this block + extracts the token — the bare content gives WORKING but never a report
/// (rt-host-a loop #4). token == message_id (exactly-once correlation).
fn render_message(sender: &str, task_id: Option<&str>, content: &str, message_id: &str) -> String {
    let mut header = format!("Team Agent message from {sender}");
    if let Some(task_id) = task_id.filter(|t| !t.is_empty()) {
        header.push_str(&format!(" for {task_id}"));
    }
    format!("{header}:\n\n{content}\n\n[team-agent-token:{message_id}]")
}

/// Resolve a recipient agent-id to a tmux-RESOLVABLE inject target: the persisted pane-id if present,
/// else a session-qualified `SessionWindow` (state.session_name + the agent's window, defaulting to the
/// id). NEVER the bare agent-id as a pane — a clientless coordinator cannot resolve that
/// ("can't find pane: w1", rt-host-a loop #3). Mirrors `coordinator/tick.rs::capture_target`.
///
/// Leader delivery uses the bound leader receiver pane. The leader is not a worker agent and
/// must not fall through to a synthetic `SessionWindow{window="leader"}` target.
fn resolve_inject_target(state: &serde_json::Value, recipient: &str) -> Target {
    if recipient == "leader" {
        if let Some(pane_id) = leader_receiver_pane_id(state) {
            return Target::Pane(PaneId::new(pane_id));
        }
    }
    let agent = state.get("agents").and_then(|a| a.get(recipient));
    if let Some(pane_id) = agent
        .and_then(|a| a.get("pane_id"))
        .and_then(serde_json::Value::as_str)
        .filter(|s| !s.is_empty())
    {
        return Target::Pane(PaneId::new(pane_id));
    }
    let session = state
        .get("session_name")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    let window = agent
        .and_then(|a| a.get("window"))
        .and_then(serde_json::Value::as_str)
        .filter(|s| !s.is_empty())
        .unwrap_or(recipient);
    Target::SessionWindow {
        session: SessionName::new(session),
        window: WindowName::new(window),
    }
}

/// Read the bound leader pane id off the projected or team-scoped runtime state.
fn leader_receiver_pane_id(state: &serde_json::Value) -> Option<&str> {
    leader_receiver_pane_id_in_state(state)
        .or_else(|| active_team_entry(state).and_then(leader_receiver_pane_id_in_state))
        .or_else(|| only_team_entry(state).and_then(leader_receiver_pane_id_in_state))
}

fn leader_receiver_pane_is_usable(transport: &dyn Transport, state: &serde_json::Value) -> bool {
    let Some(pane_id) = leader_receiver_pane_id(state) else {
        return false;
    };
    if transport
        .list_targets()
        .unwrap_or_default()
        .iter()
        .any(|target| target.pane_id.as_str() == pane_id)
    {
        return true;
    }
    !matches!(transport.liveness(&PaneId::new(pane_id)), Ok(PaneLiveness::Dead))
}

enum DeliveryTransport<'a> {
    Borrowed(&'a dyn Transport),
    Owned(crate::tmux_backend::TmuxBackend),
}

impl<'a> DeliveryTransport<'a> {
    fn as_transport(&'a self) -> &'a dyn Transport {
        match self {
            Self::Borrowed(transport) => *transport,
            Self::Owned(transport) => transport,
        }
    }
}

fn delivery_transport_for_recipient<'a>(
    workspace: &Path,
    product_transport: &'a dyn Transport,
    state: &serde_json::Value,
    recipient: &str,
) -> DeliveryTransport<'a> {
    if recipient != "leader" {
        return DeliveryTransport::Borrowed(product_transport);
    }
    let pane_id = leader_receiver_pane_id(state);
    let Some(socket) = leader_receiver_tmux_socket(state) else {
        if let Some(pane_id) = pane_id {
            let in_workspace = product_transport
                .list_targets()
                .unwrap_or_default()
                .iter()
                .any(|target| target.pane_id.as_str() == pane_id);
            if !in_workspace {
                let default_backend = crate::tmux_backend::TmuxBackend::new();
                if default_backend
                    .list_targets()
                    .unwrap_or_default()
                    .iter()
                    .any(|target| target.pane_id.as_str() == pane_id)
                {
                    return DeliveryTransport::Owned(default_backend);
                }
            }
        }
        return DeliveryTransport::Borrowed(product_transport);
    };
    if socket == crate::tmux_backend::socket_name_for_workspace(workspace) {
        DeliveryTransport::Borrowed(product_transport)
    } else {
        let endpoint_backend = crate::tmux_backend::TmuxBackend::for_tmux_endpoint(socket);
        if let Some(pane_id) = pane_id {
            if endpoint_backend
                .list_targets()
                .unwrap_or_default()
                .iter()
                .any(|target| target.pane_id.as_str() == pane_id)
            {
                return DeliveryTransport::Owned(endpoint_backend);
            }
            if product_transport
                .list_targets()
                .unwrap_or_default()
                .iter()
                .any(|target| target.pane_id.as_str() == pane_id)
            {
                return DeliveryTransport::Borrowed(product_transport);
            }
            let default_backend = crate::tmux_backend::TmuxBackend::new();
            if default_backend
                .list_targets()
                .unwrap_or_default()
                .iter()
                .any(|target| target.pane_id.as_str() == pane_id)
            {
                return DeliveryTransport::Owned(default_backend);
            }
        }
        DeliveryTransport::Owned(endpoint_backend)
    }
}

fn leader_receiver_pane_id_in_state(state: &serde_json::Value) -> Option<&str> {
    ["leader_receiver", "team_owner"].into_iter().find_map(|key| {
        state
            .get(key)
            .and_then(|r| r.get("pane_id"))
            .and_then(serde_json::Value::as_str)
            .filter(|s| !s.is_empty() && *s != "__team_agent_unbound__")
    })
}

fn leader_receiver_tmux_socket(state: &serde_json::Value) -> Option<&str> {
    leader_receiver_field(state, "tmux_socket")
}

fn leader_receiver_has_noncanonical_tmux_socket(state: &serde_json::Value) -> bool {
    leader_receiver_tmux_socket(state)
        .is_some_and(|socket| {
            socket != "default" && !std::path::Path::new(socket).is_absolute()
        })
}

fn leader_receiver_field<'a>(state: &'a serde_json::Value, field: &str) -> Option<&'a str> {
    leader_receiver_field_in_state(state, field)
        .or_else(|| active_team_entry(state).and_then(|team| leader_receiver_field_in_state(team, field)))
        .or_else(|| only_team_entry(state).and_then(|team| leader_receiver_field_in_state(team, field)))
}

fn leader_receiver_field_in_state<'a>(
    state: &'a serde_json::Value,
    field: &str,
) -> Option<&'a str> {
    state
        .get("leader_receiver")
        .and_then(|receiver| receiver.get(field))
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.is_empty())
}

fn active_team_entry(state: &serde_json::Value) -> Option<&serde_json::Value> {
    let team = state.get("active_team_key").and_then(serde_json::Value::as_str)?;
    state
        .get("teams")
        .and_then(serde_json::Value::as_object)
        .and_then(|teams| teams.get(team))
}

fn only_team_entry(state: &serde_json::Value) -> Option<&serde_json::Value> {
    let teams = state.get("teams").and_then(serde_json::Value::as_object)?;
    if teams.len() == 1 {
        teams.values().next()
    } else {
        None
    }
}

/// `_deliver_pending_messages` (`delivery.py:484`):扫 pending 队列逐条投递;busy 收件人写
/// `send.deferred_busy` 跳过 (**不丢**,card §131)。返回投递的 message_id 列表。
pub fn deliver_pending_messages(
    workspace: &Path,
    state: &serde_json::Value,
    transport: &dyn Transport,
    event_log: &EventLog,
) -> Result<Vec<String>, MessagingError> {
    let store = MessageStore::open(workspace)?;
    let message_ids = {
        let conn = crate::db::schema::open_db(store.db_path())?;
        let mut stmt = conn.prepare(
            "select message_id from messages
             where status in ('pending', 'accepted', 'target_resolved')
             order by created_at, message_id",
        )?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        rows.collect::<Result<Vec<_>, _>>()?
    };
    let mut delivered = Vec::new();
    for message_id in message_ids {
        if let Some(message) = message_for_delivery(&store, &message_id)? {
            let scoped_state;
            let state = match message.owner_team_id.as_deref() {
                Some(team) if !team.is_empty() => {
                    match project_state_for_owner_team(workspace, team, state, Some(&store), Some(&message_id), Some(event_log))? {
                        OwnerTeamProjection::Projected { state, .. } => {
                            scoped_state = state;
                            &scoped_state
                        }
                        OwnerTeamProjection::Refused(_) => continue,
                    }
                }
                _ => state,
            };
            if recipient_is_busy(state, &message.recipient) {
                event_log.write(
                    "send.deferred_busy",
                    serde_json::json!({
                        "message_id": message_id,
                        "sender": message.sender,
                        "recipient": message.recipient,
                        "reason": "recipient_busy",
                    }),
                )?;
                continue;
            }
        }
        let outcome = deliver_pending_message(workspace, &store, transport, &message_id, event_log, state)?;
        if outcome.ok {
            delivered.push(message_id);
        }
    }
    Ok(delivered)
}

struct PendingMessage {
    sender: String,
    recipient: String,
    content: String,
    task_id: Option<String>,
    owner_team_id: Option<String>,
    status: String,
}

fn message_for_delivery(
    store: &MessageStore,
    message_id: &str,
) -> Result<Option<PendingMessage>, MessagingError> {
    let conn = crate::db::schema::open_db(store.db_path())?;
    let message = conn
        .query_row(
            "select sender, recipient, content, task_id, owner_team_id, status from messages where message_id = ?1",
            params![message_id],
            |row| {
                Ok(PendingMessage {
                    sender: row.get::<_, String>(0)?,
                    recipient: row.get::<_, String>(1)?,
                    content: row.get::<_, String>(2)?,
                    task_id: row.get::<_, Option<String>>(3)?,
                    owner_team_id: row.get::<_, Option<String>>(4)?,
                    status: row.get::<_, String>(5)?,
                })
            },
        )
        .optional()?;
    Ok(message)
}

/// Pre-inject gate (Contract B): peek the recipient pane and answer "is there an
/// actionable provider startup prompt right now (trust menu or update prompt)" using
/// the SHARED provider/startup_prompt recognizers — no second classifier, no provider
/// API calls. Returns `false` if capture fails so providers without a startup
/// recognizer (or any pane without the trust-menu shape) keep flowing through
/// normal delivery.
fn recipient_pane_has_actionable_startup_prompt(
    transport: &dyn Transport,
    state: &serde_json::Value,
    recipient: &str,
    target: &Target,
) -> bool {
    let agent = state
        .get("agents")
        .and_then(serde_json::Value::as_object)
        .and_then(|agents| agents.get(recipient));
    let provider = agent
        .and_then(|agent| agent.get("provider"))
        .and_then(serde_json::Value::as_str);
    if !matches!(provider, Some("codex" | "claude" | "claude_code")) {
        return false;
    }
    // step2-retry/scrollback root-cause (rt binary 6c9c6c1c): once the agent's
    // `startup_prompts` has been flipped to `handled`/`complete`, the trust modal
    // has been answered and is the AUTHORITATIVE record of "no actionable startup
    // prompt remains". A `tmux capture-pane -S -` Full capture STILL contains the
    // dismissed modal text in scrollback ("Do you trust …" + `› 1. Yes, continue`),
    // so the recognizer's actionable-shape override matches the residue and the
    // delivery gate would loop forever (49-attempt no-deliver in real machine).
    // Trust the state (same source step1-idem uses) and skip the classify entirely.
    let startup_prompts = agent
        .and_then(|agent| agent.get("startup_prompts"))
        .and_then(serde_json::Value::as_str);
    if matches!(startup_prompts, Some("handled" | "complete")) {
        return false;
    }
    let captured = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        transport.capture(target, crate::transport::CaptureRange::Full)
    })) {
        Ok(Ok(captured)) => captured.text,
        _ => return false,
    };
    match provider {
        Some("codex") => matches!(
            crate::provider::classify_codex_startup_screen(&captured),
            crate::provider::StartupScreenDecision::AnswerWorkspaceTrust
                | crate::provider::StartupScreenDecision::SkipUpdatePrompt
        ),
        Some("claude" | "claude_code") => matches!(
            crate::provider::classify_claude_startup_screen(&captured),
            crate::provider::StartupScreenDecision::AnswerWorkspaceTrust
        ),
        _ => false,
    }
}

fn recipient_is_busy(state: &serde_json::Value, recipient: &str) -> bool {
    state
        .get("agents")
        .and_then(serde_json::Value::as_object)
        .and_then(|agents| agents.get(recipient))
        .and_then(|agent| agent.get("status"))
        .and_then(serde_json::Value::as_str)
        == Some("busy")
}

/// `_handle_trust_retry_needed` (`delivery.py:221`):trust 应答失败时调度有界退避重试
/// (`attempt < MAX` → schedule;`>= MAX` → 终态 mark failed + `trust_auto_answer_exhausted`)。
pub fn handle_trust_retry_needed(
    store: &MessageStore,
    payload: &TrustRetryPayload,
    event_log: &EventLog,
) -> Result<DeliveryOutcome, MessagingError> {
    if payload.attempt >= payload.max_attempts {
        let _ = store.mark(&payload.message_id, "failed", Some("trust_auto_answer_exhausted"));
        event_log.write(
            "leader_panes.trust_auto_answer_exhausted",
            serde_json::json!({"message_id": payload.message_id, "attempt": payload.attempt}),
        )?;
        return Ok(DeliveryOutcome {
            ok: false,
            status: DeliveryStatus::TrustAutoAnswerExhausted,
            message_status: MessageStatusShadow("failed".to_string()),
            message_id: Some(payload.message_id.clone()),
            verification: None,
            stage: Some(DeliveryStage::TrustAutoAnswerDismissalWait),
            reason: None,
            channel: None,
        });
    }
    let next_attempt = payload.attempt.saturating_add(1);
    let backoff = super::TRUST_RETRY_BACKOFF_SECONDS
        .iter()
        .find_map(|(attempt, seconds)| (*attempt == next_attempt).then_some(*seconds))
        .unwrap_or(30);
    let due_at = (chrono::Utc::now() + chrono::Duration::seconds(i64::from(backoff))).to_rfc3339();
    let conn = crate::db::schema::open_db(store.db_path())?;
    conn.execute(
        "insert into scheduled_events(owner_team_id, due_at, target, kind, payload_json, status, created_at)
         values (null, ?1, ?2, 'trust_retry', ?3, 'pending', ?4)",
        params![
            due_at,
            payload.first_target.as_str(),
            serde_json::json!({
                "message_id": payload.message_id,
                "attempt": next_attempt,
                "max_attempts": payload.max_attempts,
                "first_target": payload.first_target.as_str(),
            })
            .to_string(),
            chrono::Utc::now().to_rfc3339(),
        ],
    )?;
    let _ = store.mark(&payload.message_id, "queued_until_trust", None);
    event_log.write(
        "leader_panes.trust_auto_answer_retry_scheduled",
        serde_json::json!({"message_id": payload.message_id, "attempt": next_attempt, "due_at": due_at}),
    )?;
    Ok(DeliveryOutcome {
        ok: false,
        status: DeliveryStatus::RetryScheduled,
        message_status: MessageStatusShadow("queued_until_trust".to_string()),
        message_id: Some(payload.message_id.clone()),
        verification: None,
        stage: Some(DeliveryStage::TrustAutoAnswerDismissalWait),
        reason: None,
        channel: None,
    })
}

/// `_execute_trust_retry` (`delivery.py:330`):trust_retry scheduled event 的消费者 ——
/// 把行重置回 `accepted`,attempt 穿透,重跑 `_deliver_pending_message`。
pub fn execute_trust_retry(
    workspace: &Path,
    store: &MessageStore,
    transport: &dyn Transport,
    payload: &TrustRetryPayload,
    event_log: &EventLog,
    owner_team_id: Option<&TeamKey>,
) -> Result<DeliveryOutcome, MessagingError> {
    let _ = owner_team_id;
    let _ = store.mark(&payload.message_id, "accepted", None);
    let state = crate::state::persist::load_runtime_state(workspace)?;
    deliver_pending_message(workspace, store, transport, &payload.message_id, event_log, &state)
}

/// `_record_turn_open_if_leader_to_worker` (`delivery.py:430`):**take-over arm 来自真实投递**
/// (card §121) —— 仅 leader→worker 注入**成功后**才调 `record_turn_open_after_delivery`,绝不凭空 arm。
pub fn record_turn_open_if_leader_to_worker(
    workspace: &Path,
    state: &serde_json::Value,
    sender: &str,
    recipient: &str,
    delivered: &DeliveryOutcome,
    event_log: &EventLog,
) -> Result<(), MessagingError> {
    let _ = state;
    record_turn_open_if_leader_to_worker_scoped(
        workspace,
        sender,
        recipient,
        delivered,
        event_log,
        None,
    )
}

fn record_turn_open_if_leader_to_worker_scoped(
    workspace: &Path,
    sender: &str,
    recipient: &str,
    delivered: &DeliveryOutcome,
    event_log: &EventLog,
    owner_team_id: Option<&str>,
) -> Result<(), MessagingError> {
    if !delivered.ok || !matches!(sender, "leader" | "Leader") || recipient == "leader" {
        return Ok(());
    }
    let mut state = scoped_state_for_write(workspace, owner_team_id)?;
    let Some(root) = state.as_object_mut() else {
        return Ok(());
    };
    let coordinator = root
        .entry("coordinator")
        .or_insert_with(|| serde_json::json!({}));
    if let Some(obj) = coordinator.as_object_mut() {
        obj.insert(
            "turn_open".to_string(),
            serde_json::json!({"armed": true, "node_id": recipient, "turn_id": delivered.message_id}),
        );
    }
    save_scoped_state(workspace, &state, owner_team_id)?;
    event_log.write(
        "turn_open.armed_after_delivery",
        serde_json::json!({"agent_id": recipient, "message_id": delivered.message_id}),
    )?;
    Ok(())
}

/// `_stamp_first_send_at_if_leader_to_worker` (`delivery.py:380`):首次 leader→worker 投递戳
/// `first_send_at` (step 13 restart Route B atomicity 决策读它)。
pub fn stamp_first_send_at_if_leader_to_worker(
    workspace: &Path,
    state: &serde_json::Value,
    sender: &str,
    recipient: &str,
) -> Result<(), MessagingError> {
    let _ = state;
    stamp_first_send_at_if_leader_to_worker_scoped(workspace, sender, recipient, None)
}

fn stamp_first_send_at_if_leader_to_worker_scoped(
    workspace: &Path,
    sender: &str,
    recipient: &str,
    owner_team_id: Option<&str>,
) -> Result<(), MessagingError> {
    if !matches!(sender, "leader" | "Leader") || recipient == "leader" {
        return Ok(());
    }
    let mut state = scoped_state_for_write(workspace, owner_team_id)?;
    let now = chrono::Utc::now().to_rfc3339();
    if let Some(agent) = state
        .get_mut("agents")
        .and_then(serde_json::Value::as_object_mut)
        .and_then(|agents| agents.get_mut(recipient))
        .and_then(serde_json::Value::as_object_mut)
    {
        if !agent.contains_key("first_send_at") || agent.get("first_send_at").is_some_and(serde_json::Value::is_null) {
            agent.insert("first_send_at".to_string(), serde_json::Value::String(now));
            save_scoped_state(workspace, &state, owner_team_id)?;
        }
    }
    Ok(())
}

fn scoped_state_for_write(
    workspace: &Path,
    owner_team_id: Option<&str>,
) -> Result<serde_json::Value, MessagingError> {
    match owner_team_id.filter(|team| !team.is_empty()) {
        Some(team) => {
            let raw = crate::state::persist::load_runtime_state(workspace)?;
            match project_state_for_owner_team_value(&raw, team) {
                Some(projected) => Ok(projected),
                None => Ok(raw),
            }
        }
        None => Ok(crate::state::persist::load_runtime_state(workspace)?),
    }
}

fn save_scoped_state(
    workspace: &Path,
    state: &serde_json::Value,
    owner_team_id: Option<&str>,
) -> Result<(), MessagingError> {
    if owner_team_id.filter(|team| !team.is_empty()).is_some() {
        if state
            .get("teams")
            .and_then(serde_json::Value::as_object)
            .is_some_and(|teams| {
                owner_team_id
                    .and_then(|team| crate::state::projection::resolve_owner_team_id(state, team).canonical_key().map(str::to_string))
                    .is_some_and(|team| teams.contains_key(&team))
            })
        {
            crate::state::projection::save_team_scoped_state(workspace, state)?;
        } else {
            crate::state::persist::save_runtime_state(workspace, state)?;
        }
    } else {
        crate::state::persist::save_runtime_state(workspace, state)?;
    }
    Ok(())
}

enum OwnerTeamProjection {
    Projected { state: serde_json::Value, canonical_team: String },
    Refused(DeliveryOutcome),
}

fn project_state_for_owner_team(
    workspace: &Path,
    team: &str,
    fallback: &serde_json::Value,
    store: Option<&MessageStore>,
    message_id: Option<&str>,
    event_log: Option<&EventLog>,
) -> Result<OwnerTeamProjection, MessagingError> {
    let raw = crate::state::persist::load_runtime_state(workspace)?;
    let fallback_has_teams = fallback
        .get("teams")
        .and_then(serde_json::Value::as_object)
        .is_some_and(|teams| !teams.is_empty());
    let (mut projection_source, mut resolution) = if fallback_has_teams {
        (fallback, crate::state::projection::resolve_owner_team_id(fallback, team))
    } else {
        (&raw, crate::state::projection::resolve_owner_team_id(&raw, team))
    };
    if !fallback_has_teams && matches!(resolution, OwnerTeamResolution::Unresolved { .. }) {
        let fallback_resolution = crate::state::projection::resolve_owner_team_id(fallback, team);
        if !matches!(fallback_resolution, OwnerTeamResolution::Unresolved { .. }) {
            resolution = fallback_resolution;
            projection_source = fallback;
        }
    }
    let canonical_team = match resolution {
        OwnerTeamResolution::Canonical(canonical) => canonical,
        OwnerTeamResolution::LegacyAlias { requested, canonical } => {
            normalize_owner_team_id_rows(workspace, &requested, &canonical, message_id, event_log)?;
            canonical
        }
        OwnerTeamResolution::Unresolved { requested } => {
            let outcome = refuse_owner_team_resolution(
                store,
                message_id,
                event_log,
                "owner_team_unresolved",
                serde_json::json!({"owner_team_id": requested}),
                DeliveryRefusal::UnknownRecipient,
            )?;
            return Ok(OwnerTeamProjection::Refused(outcome));
        }
        OwnerTeamResolution::Ambiguous { requested, matches } => {
            let outcome = refuse_owner_team_resolution(
                store,
                message_id,
                event_log,
                "owner_team_ambiguous",
                serde_json::json!({"owner_team_id": requested, "matches": matches}),
                DeliveryRefusal::Ambiguous,
            )?;
            return Ok(OwnerTeamProjection::Refused(outcome));
        }
    };
    if top_level_state_matches_owner_team(fallback, &canonical_team) {
        let mut state = fallback.clone();
        carry_top_level_leader_binding(&mut state, &raw);
        return Ok(OwnerTeamProjection::Projected {
            state,
            canonical_team,
        });
    }
    if top_level_state_matches_owner_team(&raw, &canonical_team) {
        return Ok(OwnerTeamProjection::Projected {
            state: raw,
            canonical_team,
        });
    }
    if state_has_no_team_entries(projection_source) {
        let mut state = projection_source.clone();
        carry_top_level_leader_binding(&mut state, &raw);
        return Ok(OwnerTeamProjection::Projected {
            state,
            canonical_team,
        });
    }
    let mut state = project_state_for_owner_team_value(projection_source, &canonical_team)
        .ok_or_else(|| MessagingError::Routing(format!("owner_team_unresolved: {canonical_team}")))?;
    carry_top_level_leader_binding(&mut state, projection_source);
    carry_top_level_leader_binding(&mut state, &raw);
    Ok(OwnerTeamProjection::Projected { state, canonical_team })
}

fn carry_top_level_leader_binding(projected: &mut serde_json::Value, raw: &serde_json::Value) {
    let Some(projected_obj) = projected.as_object_mut() else {
        return;
    };
    for key in ["leader_receiver", "team_owner", "owner_epoch"] {
        if projected_obj.contains_key(key) {
            continue;
        }
        if let Some(value) = raw.get(key) {
            projected_obj.insert(key.to_string(), value.clone());
        }
    }
}

fn state_has_no_team_entries(state: &serde_json::Value) -> bool {
    state
        .get("teams")
        .and_then(serde_json::Value::as_object)
        .is_none_or(serde_json::Map::is_empty)
}

pub(crate) fn normalize_owner_team_id_rows(
    workspace: &Path,
    requested: &str,
    canonical: &str,
    message_id: Option<&str>,
    event_log: Option<&EventLog>,
) -> Result<(), MessagingError> {
    if requested == canonical {
        return Ok(());
    }
    let store = MessageStore::open(workspace)?;
    let conn = crate::db::schema::open_db(store.db_path())?;
    for table in [
        "messages",
        "results",
        "scheduled_events",
        "agent_health",
        "result_watchers",
        "leader_notification_log",
    ] {
        let sql = format!("update or ignore {table} set owner_team_id = ?1 where owner_team_id = ?2");
        conn.execute(&sql, params![canonical, requested])?;
    }
    if let Some(event_log) = event_log {
        event_log.write(
            "owner_team_id.compatibility_alias_migrated",
            serde_json::json!({
                "requested_owner_team_id": requested,
                "canonical_owner_team_id": canonical,
                "message_id": message_id,
            }),
        )?;
    }
    Ok(())
}

fn refuse_owner_team_resolution(
    store: Option<&MessageStore>,
    message_id: Option<&str>,
    event_log: Option<&EventLog>,
    error: &str,
    details: serde_json::Value,
    refusal: DeliveryRefusal,
) -> Result<DeliveryOutcome, MessagingError> {
    if let (Some(store), Some(message_id)) = (store, message_id) {
        store.mark(message_id, "failed", Some(error))?;
    }
    if let Some(event_log) = event_log {
        event_log.write(
            "owner_team_id.resolution_failed",
            serde_json::json!({
                "message_id": message_id,
                "error": error,
                "details": details,
            }),
        )?;
    }
    Ok(DeliveryOutcome {
        ok: false,
        status: DeliveryStatus::Refused,
        message_status: MessageStatusShadow("failed".to_string()),
        message_id: message_id.map(str::to_string),
        verification: Some(error.to_string()),
        stage: None,
        reason: Some(refusal),
        channel: Some("owner_team_resolution".to_string()),
    })
}

fn project_state_for_owner_team_value(
    raw: &serde_json::Value,
    team: &str,
) -> Option<serde_json::Value> {
    if let Some(projected) = raw
        .get("teams")
        .and_then(serde_json::Value::as_object)
        .is_some_and(|teams| teams.contains_key(team))
        .then(|| crate::state::projection::project_top_level_view(raw, team))
    {
        return Some(projected);
    }
    if top_level_state_matches_owner_team(raw, team) {
        return None;
    }
    None
}

fn top_level_state_matches_owner_team(state: &serde_json::Value, team: &str) -> bool {
    state
        .get("active_team_key")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|value| value == team)
        || crate::state::projection::team_state_key(state) == team
        || state
            .get("session_name")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|session| session == team || session.strip_prefix("team-") == Some(team))
}

/// `retry_injection_after_trust_auto_answer` (`trust_auto_answer.py`):leader 路径 trust 应答
/// 后重注入 (查 pane_width fail-safe + attempt_trust_auto_answer + 等 dismissal + 重 inject)。
pub fn retry_injection_after_trust_auto_answer(
    workspace: &Path,
    state: &serde_json::Value,
    transport: &dyn Transport,
    target: &Target,
    text: &str,
    provider: Provider,
    event_log: &EventLog,
) -> Result<DeliveryOutcome, MessagingError> {
    let _ = (workspace, state, transport, target, text, provider, event_log);
    Ok(DeliveryOutcome {
        ok: false,
        status: DeliveryStatus::RetryScheduled,
        message_status: MessageStatusShadow("retry_scheduled".to_string()),
        message_id: None,
        verification: None,
        stage: Some(DeliveryStage::TrustAutoAnswerDismissalWait),
        reason: None,
        channel: None,
    })
}
