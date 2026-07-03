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
    // E47 ROOT-FIX#1 (0.3.24 P0, idle/busy 假阳): the pre-fix `lower.find(...)`
    // scanned the WHOLE scrollback (Tail(40) lines) and matched historical
    // working markers — e.g. an old `• Working (514s · esc to interrupt)`
    // still in the 40-line window after the worker idled. That stale token
    // flipped a truly-idle agent to Stuck whenever the historical seconds
    // exceeded 300. Real fix: only consult the LIVE composer / status line
    // (last non-empty line). If the current bottom line is something like
    // `❯ Run /review`, `Worked for 8m 34s`, or an empty composer prompt,
    // there is no LIVE working indicator and we return None — handing off
    // to the structural latest_prompt_signal / IRON LAW Uncertain path.
    let last_non_empty = scrollback.lines().rev().find(|line| !line.trim().is_empty())?;
    let lower = last_non_empty.to_ascii_lowercase();
    let start = lower.find("working (")?;
    let rest = last_non_empty.get(start + "Working (".len()..)?;
    let seconds = rest.split_once('s')?.0;
    seconds.parse::<u64>().ok()
}

pub(crate) fn non_provider_command(command: &str) -> Option<&str> {
    // Activity command grammar, not provider identity parsing.
    let base = command.rsplit('/').next().unwrap_or(command);
    let normalized = base.to_ascii_lowercase();
    match normalized.as_str() {
        "" | "codex" | "claude" | "copilot" | "gemini" | "openai" | "team-agent" => None,
        _ => Some(base),
    }
}

pub(crate) fn latest_prompt_signal(scrollback: &str) -> Option<AgentActivity> {
    // E47 ROOT-FIX#2 (0.3.24 P0, idle/busy 假阳): limit the scan to the
    // BOTTOM ACTIVE REGION (last 1-5 non-empty lines = composer / status
    // line). The pre-fix `rfind` across the whole Tail(40) buffer let a
    // historical spinner/`Working` token out-position the live `❯`/`›`
    // composer (the rfind-recency family bug — same shape as the #320
    // codex prompt residue). When the composer line is `❯ Run /review`
    // (idle), a scrollback `Working (514s)` 20 lines up should NOT win.
    //
    // IRON LAW (activity.rs:3 / bug-071/077/085): no-signal in the bottom
    // region must be Uncertain (caller treats None here as "no decisive
    // signal" and surfaces Uncertain), NEVER silently flipped to Idle.
    let active_region: Vec<&str> = scrollback
        .lines()
        .rev()
        .filter(|line| !line.trim().is_empty())
        .take(5)
        .collect();
    if active_region.is_empty() {
        return None;
    }
    let mut has_idle_prompt = false;
    let mut has_live_working_indicator = false;
    for line in &active_region {
        let lower = line.to_ascii_lowercase();
        if line.contains('❯') || line.contains('›') {
            has_idle_prompt = true;
        }
        // codex live spinner shapes (provider/adapter.rs:875-876 markers):
        // braille spinner, `• Working (`, `Thinking`; claude `✶`/`✢`/`✻`
        // and Claude Code tool-progress verbs. We look for STRUCTURAL
        // composer/status signals in the active region only.
        if [
            "working (", "thinking", "⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦",
            "⠧", "⠇", "⠏", "✶", "✢", "✻", "analyzing", "reading", "writing",
            "searching", "running", "editing",
        ]
        .iter()
        .any(|needle| lower.contains(needle))
        {
            has_live_working_indicator = true;
        }
    }
    // Working indicator IN the active region wins over a stale `❯`/`›`
    // (composer may show a prompt char before the live spinner refreshes).
    if has_live_working_indicator {
        return Some(AgentActivity {
            status: ActivityStatus::Working,
            confidence: 0.9,
            rationale: "working_indicator".to_string(),
        });
    }
    if has_idle_prompt {
        return Some(idle_activity());
    }
    // No structural signal in the bottom region → caller (activity.rs:184)
    // gets None and treats as no-decisive-signal → Uncertain (IRON LAW).
    None
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
