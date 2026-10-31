//!
//! Scheduled-event dispatch and coordinator stuck detection (card §18/§68/§73).

use std::path::Path;

use rusqlite::params;

use crate::event_log::EventLog;
use crate::message_store::MessageStore;
use crate::transport::{PaneId, Transport};

use super::delivery::{deliver_stored_message, handle_trust_retry_needed};
use super::helpers::{parse_scheduled_kind, status_wire};
use super::{MessagingError, ScheduledKind, TrustRetryPayload, TRUST_RETRY_MAX_ATTEMPTS};

/// `_fire_due_scheduled_events` (`scheduler.py:41`):coordinator tick 的调度器心脏 —— 分派到期
/// `send`/`health_ping`/`trust_retry` ([`ScheduledKind`] 穷尽 match),send 失败有界重试。
/// 返回 fired 的 scheduled_event id 列表。**daemon-path** (step 12 调) → Result。
pub fn fire_due_scheduled_events(
    workspace: &Path,
    store: &MessageStore,
    transport: &dyn Transport,
    event_log: &EventLog,
) -> Result<Vec<i64>, MessagingError> {
    Err(MessagingError::Validation("legacy SQL messaging retired; use station send and C3 inbox".into()))
}

/// `_detect_stuck_agents` (`scheduler.py:146`):stuck 检测。**§84 守门**:`_agent_has_stuck_relevant_work`
/// 仅在有 active task / inbound message 时推送,无 pending obligation 时绝不注入探索性 prompt。
pub fn detect_stuck_agents(
    workspace: &Path,
    state: &serde_json::Value,
    store: &MessageStore,
    event_log: &EventLog,
) -> Result<Vec<String>, MessagingError> {
    Err(MessagingError::Validation("legacy SQL messaging retired; use station send and C3 inbox".into()))
}

fn output_is_stale(last_output_at: &str, timeout_seconds: i64) -> bool {
    let Ok(ts) = chrono::DateTime::parse_from_rfc3339(last_output_at) else {
        return false;
    };
    chrono::Utc::now()
        .signed_duration_since(ts.with_timezone(&chrono::Utc))
        .num_seconds()
        >= timeout_seconds
}
