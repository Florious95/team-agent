//! 私有内部 helpers (status wire / envelope 校验 / scheduled kind 解析 / activity 信号
//! 解析 / id 生成 / MessageStatusShadow 占位)。跨子模块/测试可见者升 `pub(crate)`。

use std::sync::atomic::{AtomicU64, Ordering};

use rusqlite::{params, OptionalExtension};

use crate::message_store::MessageStore;

use super::{
    ActivityStatus, AgentActivity, DeliveryOutcome, DeliveryRefusal, DeliveryStatus,
    MessagingError, ScheduledKind,
};

/// **PLACEHOLDER** — step 7 `messages.status` 行态 enum (`message_store` lane 的 `MessageStatus`)。
/// step 7 已落地 core 但尚未导出该 enum (当前用裸 `&str`);本 lane 不猜其精确 variant 集,
/// 用本地最小占位让 [`DeliveryOutcome::message_status`] 编得过。leader 集成时换成 step 7 的
/// 权威 `MessageStatus`(`accepted`/`target_resolved`/`injected`/`visible`/`submitted`/
/// `submitted_unverified`/`delivered`/`acknowledged`/`failed`/`queued_*` …)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageStatusShadow(pub String);

static RESULT_COUNTER: AtomicU64 = AtomicU64::new(0);

pub(crate) fn status_wire(status: DeliveryStatus) -> &'static str {
    match status {
        DeliveryStatus::Delivered => "delivered",
        DeliveryStatus::Failed => "failed",
        DeliveryStatus::Queued => "queued",
        DeliveryStatus::Blocked => "blocked",
        DeliveryStatus::Refused => "refused",
        DeliveryStatus::Degraded => "degraded",
        DeliveryStatus::RetryScheduled => "retry_scheduled",
        DeliveryStatus::TrustAutoAnswerExhausted => "trust_auto_answer_exhausted",
        DeliveryStatus::AlreadyDelivered => "already_delivered",
        DeliveryStatus::FallbackLog => "fallback_log",
        DeliveryStatus::BroadcastDelivered => "broadcast_delivered",
        DeliveryStatus::BroadcastPartial => "broadcast_partial",
        DeliveryStatus::FanoutDelivered => "fanout_delivered",
        DeliveryStatus::FanoutPartial => "fanout_partial",
    }
}

pub(crate) fn message_exists(
    store: &MessageStore,
    message_id: &str,
) -> Result<bool, MessagingError> {
    let conn = crate::db::schema::open_db(store.db_path())?;
    let found: Option<String> = conn
        .query_row(
            "select message_id from messages where message_id = ?1",
            params![message_id],
            |row| row.get(0),
        )
        .optional()?;
    Ok(found.is_some())
}

pub(crate) fn next_result_id() -> String {
    let n = RESULT_COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    format!("res_{:012x}", (nanos ^ u128::from(n)) & 0xFFFF_FFFF_FFFF)
}

pub(crate) fn next_run_id() -> String {
    let id = next_result_id();
    id.chars().filter(|c| *c != '_').take(12).collect()
}

pub(crate) fn required_str<'a>(
    value: &'a serde_json::Value,
    key: &str,
) -> Result<&'a str, MessagingError> {
    value
        .get(key)
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| MessagingError::Validation(format!("missing required field: {key}")))
}

pub(crate) fn validate_result_envelope(envelope: &serde_json::Value) -> Result<(), MessagingError> {
    let schema = required_str(envelope, "schema_version")?;
    if schema != "result_envelope_v1" {
        return Err(MessagingError::Validation(format!(
            "unsupported schema_version: {schema}"
        )));
    }
    for key in ["task_id", "agent_id", "status", "summary"] {
        let _ = required_str(envelope, key)?;
    }
    for key in ["changes", "tests", "risks", "artifacts", "next_actions"] {
        if !envelope.get(key).is_some_and(serde_json::Value::is_array) {
            return Err(MessagingError::Validation(format!(
                "missing required array field: {key}"
            )));
        }
    }
    Ok(())
}

pub(crate) fn parse_scheduled_kind(kind: &str) -> Result<ScheduledKind, MessagingError> {
    match kind {
        "send" => Ok(ScheduledKind::Send),
        "health_ping" => Ok(ScheduledKind::HealthPing),
        "trust_retry" => Ok(ScheduledKind::TrustRetry),
        other => Err(MessagingError::Validation(format!(
            "unknown scheduled event kind: {other}"
        ))),
    }
}

pub(crate) fn working_seconds(scrollback: &str) -> Option<u64> {
    let lower = scrollback.to_ascii_lowercase();
    let start = lower.find("working (")?;
    let rest = scrollback.get(start + "Working (".len()..)?;
    let seconds = rest.split_once('s')?.0;
    seconds.parse::<u64>().ok()
}

pub(crate) fn non_provider_command(command: &str) -> Option<&str> {
    let base = command.rsplit('/').next().unwrap_or(command);
    let normalized = base.to_ascii_lowercase();
    match normalized.as_str() {
        "" | "codex" | "claude" | "copilot" | "gemini" | "openai" | "team-agent" => None,
        _ => Some(base),
    }
}

pub(crate) fn latest_prompt_signal(scrollback: &str) -> Option<AgentActivity> {
    let lower = scrollback.to_ascii_lowercase();
    let idle_pos = latest_idle_prompt_pos(scrollback);
    let working_pos = [
        "working", "thinking", "⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏",
    ]
    .iter()
    .filter_map(|needle| lower.rfind(needle))
    .max();
    match (idle_pos, working_pos) {
        (Some(i), Some(w)) if i > w => Some(idle_activity()),
        (Some(_), None) => Some(idle_activity()),
        (_, Some(_)) => Some(AgentActivity {
            status: ActivityStatus::Working,
            confidence: 0.9,
            rationale: "working_indicator".to_string(),
        }),
        (None, None) => None,
    }
}

fn latest_idle_prompt_pos(scrollback: &str) -> Option<usize> {
    scrollback
        .match_indices('❯')
        .map(|(idx, _)| idx)
        .chain(scrollback.match_indices('›').map(|(idx, _)| idx))
        .max()
}

fn idle_activity() -> AgentActivity {
    AgentActivity {
        status: ActivityStatus::Idle,
        confidence: 0.9,
        rationale: "idle_prompt".to_string(),
    }
}

/// `_fail_leader_delivery` (`leader.py:394`) — **diagnostic ONLY** (#230 I-4 退化):
/// 历史上返回 `ok=True/status=FallbackLog/channel="fallback_inbox"` 被上游误读为
/// "已交付的 fallback log"。现新 leader-delivery primitive(`leader_receiver::send_to_leader_receiver`)
/// 不再调本函数。本函数保留只供老代码路径(scheduler / 兼容旧测试)使用,且其
/// outcome **不被视为 success**(I-3 反向断言要求 `leader_receiver.rs` 源文件不得含
/// `DeliveryStatus::FallbackLog` 或 `fallback_inbox` 字面量,所以本函数从 `leader_receiver.rs`
/// 搬到本 helpers 模块,字面量在此被允许)。
pub fn fail_leader_delivery(
    workspace: &std::path::Path,
    payload: &serde_json::Value,
    reason: DeliveryRefusal,
    error: Option<&str>,
) -> Result<DeliveryOutcome, MessagingError> {
    let store = MessageStore::open(workspace)?;
    let sender = payload
        .get("sender")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("system");
    let content = payload
        .get("content")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    let task_id = payload.get("task_id").and_then(serde_json::Value::as_str);
    let message_id = match payload
        .get("message_id")
        .and_then(serde_json::Value::as_str)
    {
        Some(existing) => existing.to_string(),
        None => store.create_message(task_id, sender, "leader", content, None, false, None)?,
    };
    store.mark(&message_id, "failed", error)?;
    crate::event_log::EventLog::new(workspace).write(
        "leader_receiver.delivery_failed",
        serde_json::json!({"message_id": message_id, "reason": serde_json::to_value(reason).ok(), "error": error}),
    )?;
    Ok(DeliveryOutcome {
        ok: true,
        status: DeliveryStatus::FallbackLog,
        message_status: MessageStatusShadow("failed".to_string()),
        message_id: Some(message_id),
        verification: None,
        stage: None,
        reason: Some(reason),
        channel: Some("fallback_inbox".to_string()),
    })
}

pub(crate) fn recent_rfc3339(ts: &str, max_age_seconds: i64) -> bool {
    let Ok(parsed) = chrono::DateTime::parse_from_rfc3339(ts) else {
        return false;
    };
    let age = chrono::Utc::now().signed_duration_since(parsed.with_timezone(&chrono::Utc));
    age.num_seconds() <= max_age_seconds
}

pub(crate) fn stale_rfc3339(ts: &str, min_age_seconds: i64) -> bool {
    let Ok(parsed) = chrono::DateTime::parse_from_rfc3339(ts) else {
        return false;
    };
    let age = chrono::Utc::now().signed_duration_since(parsed.with_timezone(&chrono::Utc));
    age.num_seconds() >= min_age_seconds
}
