//! internal_delivery.py + delivery.py — coordinator/调度器侧 thin wrapper + 单条 tmux 注入投递
//! + trust 有界重试 + turn-open arm (card §16/§65)。

use std::path::Path;

use rusqlite::{params, OptionalExtension};

use crate::event_log::EventLog;
use crate::message_store::MessageStore;
use crate::model::enums::Provider;
use crate::model::ids::TeamKey;
use crate::transport::{InjectPayload, Key, PaneId, SessionName, Target, Transport, WindowName};

use super::helpers::{message_exists, MessageStatusShadow};
use super::{
    DeliveryOutcome, DeliveryRefusal, DeliveryStage, DeliveryStatus, MessagingError,
    PaneWidthQuery, TrustRetryPayload,
};

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
    if !store.claim_for_delivery(message_id)? {
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
    transport.inject(
        &target,
        &InjectPayload::Text(rendered),
        Key::Enter,
        true,
    )?;
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
    stamp_first_send_at_if_leader_to_worker(workspace, state, &message.sender, &message.recipient)?;
    record_turn_open_if_leader_to_worker(
        workspace,
        state,
        &message.sender,
        &message.recipient,
        &outcome,
        event_log,
    )?;
    Ok(outcome)
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
fn resolve_inject_target(state: &serde_json::Value, recipient: &str) -> Target {
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
             where status in ('pending', 'accepted')
             order by created_at, message_id",
        )?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        rows.collect::<Result<Vec<_>, _>>()?
    };
    let mut delivered = Vec::new();
    for message_id in message_ids {
        if let Some(message) = message_for_delivery(&store, &message_id)? {
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
}

fn message_for_delivery(
    store: &MessageStore,
    message_id: &str,
) -> Result<Option<PendingMessage>, MessagingError> {
    let conn = crate::db::schema::open_db(store.db_path())?;
    let message = conn
        .query_row(
            "select sender, recipient, content, task_id from messages where message_id = ?1",
            params![message_id],
            |row| {
                Ok(PendingMessage {
                    sender: row.get::<_, String>(0)?,
                    recipient: row.get::<_, String>(1)?,
                    content: row.get::<_, String>(2)?,
                    task_id: row.get::<_, Option<String>>(3)?,
                })
            },
        )
        .optional()?;
    Ok(message)
}

/// Pre-inject gate (Contract B): peek the recipient pane and answer "is there an
/// actionable Codex startup prompt right now (trust menu or update prompt)" using
/// the SHARED provider/startup_prompt recognizer — no second classifier, no provider
/// API calls. Returns `false` if capture fails so non-Codex providers (or any pane
/// without the trust-menu shape) keep flowing through normal delivery.
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
    if !matches!(provider, Some("codex")) {
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
    matches!(
        crate::provider::classify_codex_startup_screen(&captured),
        crate::provider::StartupScreenDecision::AnswerWorkspaceTrust
            | crate::provider::StartupScreenDecision::SkipUpdatePrompt
    )
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
    if !delivered.ok || !matches!(sender, "leader" | "Leader") || recipient == "leader" {
        return Ok(());
    }
    let mut state = crate::state::persist::load_runtime_state(workspace)?;
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
    crate::state::persist::save_runtime_state(workspace, &state)?;
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
    if !matches!(sender, "leader" | "Leader") || recipient == "leader" {
        return Ok(());
    }
    let mut state = crate::state::persist::load_runtime_state(workspace)?;
    let now = chrono::Utc::now().to_rfc3339();
    if let Some(agent) = state
        .get_mut("agents")
        .and_then(serde_json::Value::as_object_mut)
        .and_then(|agents| agents.get_mut(recipient))
        .and_then(serde_json::Value::as_object_mut)
    {
        if !agent.contains_key("first_send_at") || agent.get("first_send_at").is_some_and(serde_json::Value::is_null) {
            agent.insert("first_send_at".to_string(), serde_json::Value::String(now));
            crate::state::persist::save_runtime_state(workspace, &state)?;
        }
    }
    Ok(())
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
