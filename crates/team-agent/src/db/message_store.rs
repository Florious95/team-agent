//! Legacy message DTOs and compatibility signatures. Persistent actions are
//! retired and refuse in memory; the C3 inbox owns current message history.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::{params, OptionalExtension};
use thiserror::Error;

static MESSAGE_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Error)]
pub enum MessageStoreError {
    #[error("legacy SQL message authority retired; use station send/inbox")]
    Retired,
    #[error("db: {0}")]
    Db(#[from] crate::db::DbError),
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("event log: {0}")]
    EventLog(#[from] crate::event_log::EventLogError),
    #[error("delivery receipt missing for message: {0}")]
    DeliveryReceiptMissing(String),
    #[error("invalid message transition from {from} to {to}")]
    InvalidTransition { from: String, to: String },
    #[error("submitted recovery refused: {0}")]
    RecoveryRefused(&'static str),
}

/// Outcome of [`MessageStore::claim_leader_notification_delivery`]
/// (`leader_notification_log.py:73-101`). `status` is `"claimed_by_you"` for the
/// winner, `"already_notified_by"` for a deduped loser; `notified_message_id` is
/// always the WINNER's proposed id (a loser sees the first winner's id).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotificationClaim {
    pub status: String,
    pub notified_message_id: String,
}

/// Args for [`MessageStore::claim_leader_notification_delivery`]
/// (`leader_notification_log.py:30-40`).
#[derive(Debug, Clone)]
pub struct NotificationClaimParams<'a> {
    pub result_id: &'a str,
    pub owner_team_id: Option<&'a str>,
    /// `None` → derived from `leader_session_uuid` via [`legacy_epoch_from_uuid`].
    pub owner_epoch: Option<i64>,
    pub leader_session_uuid: Option<&'a str>,
    pub proposed_message_id: &'a str,
    pub envelope_hash: &'a str,
    pub pane_id: Option<&'a str>,
}

/// Legacy #373 parked-row marker, retained only for upgrade recovery.
pub(crate) const MCP_HANDSHAKE_TIMEOUT: &str = "mcp_handshake_timeout";

/// Canonical initial message-row statuses shared by persistence, presentation,
/// claiming and recovery. New durable dispositions must be added here first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageRowStatus {
    Accepted,
    StoredOnly,
    QueuedUntilLeaderAttach,
    QueuedCoordinatorUnavailable,
    QueuedPaneMissing,
    TargetResolved,
    SubmittedAwaitingReceipt,
    InjectedAwaitingReceipt,
    SubmittedUnverified,
    Delivered,
    Acknowledged,
    Consumed,
    Failed,
    BlockedLeaderUnbound,
    BlockedWorkerPaneMissing,
}

impl MessageRowStatus {
    pub const ALL: &[Self] = &[
        Self::Accepted,
        Self::StoredOnly,
        Self::QueuedUntilLeaderAttach,
        Self::QueuedCoordinatorUnavailable,
        Self::QueuedPaneMissing,
        Self::TargetResolved,
        Self::SubmittedAwaitingReceipt,
        Self::InjectedAwaitingReceipt,
        Self::SubmittedUnverified,
        Self::Delivered,
        Self::Acknowledged,
        Self::Consumed,
        Self::Failed,
        Self::BlockedLeaderUnbound,
        Self::BlockedWorkerPaneMissing,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Accepted => "accepted",
            Self::StoredOnly => "stored_only",
            Self::QueuedUntilLeaderAttach => "queued_until_leader_attach",
            Self::QueuedCoordinatorUnavailable => "queued_coordinator_unavailable",
            Self::QueuedPaneMissing => "queued_pane_missing",
            Self::TargetResolved => "target_resolved",
            Self::SubmittedAwaitingReceipt => "submitted_pending_acceptance",
            Self::InjectedAwaitingReceipt => "injected_awaiting_receipt",
            Self::SubmittedUnverified => "submitted_unverified",
            Self::Delivered => "delivered",
            Self::Acknowledged => "acknowledged",
            Self::Consumed => "consumed",
            Self::Failed => "failed",
            Self::BlockedLeaderUnbound => "blocked_leader_unbound",
            Self::BlockedWorkerPaneMissing => "queued_pane_missing",
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BlockedLeaderRequeueCounts {
    pub blocked_leader_unbound: usize,
    pub queued_until_leader_attach: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecoveryBudget {
    pub attempt_budget: u32,
    pub max_age_seconds: u64,
    pub owner_epoch: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneAvailable {
    pub agent_id: String,
    pub pane_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RedisplayKind {
    FrameworkReplay,
    ProviderRerender,
}

impl RedisplayKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::FrameworkReplay => "framework_replay",
            Self::ProviderRerender => "provider_rerender",
        }
    }
}

impl BlockedLeaderRequeueCounts {
    pub const fn total(self) -> usize {
        self.blocked_leader_unbound + self.queued_until_leader_attach
    }
}

/// Fully resolved durable-message insert. Grammar, scope and transport data do
/// not belong here; callers must resolve those before crossing this boundary.
pub struct PersistMessageInput<'a> {
    pub message_id: Option<&'a str>,
    pub owner_team_id: Option<&'a str>,
    pub task_id: Option<&'a str>,
    pub sender: &'a str,
    pub recipient: &'a str,
    pub reply_to: Option<&'a str>,
    pub requires_ack: bool,
    pub status: MessageRowStatus,
    pub content: &'a str,
    pub presentation: &'a str,
    pub error: Option<&'a str>,
}

/// `leader_notification_log._legacy_epoch_from_uuid` (line 145-147):
/// `int(zlib.crc32(str(uuid or "").encode("utf-8")) & 0x7FFFFFFF)`.
fn legacy_epoch_from_uuid(leader_session_uuid: Option<&str>) -> i64 {
    let mut crc = 0xFFFF_FFFFu32;
    for byte in leader_session_uuid.unwrap_or("").as_bytes() {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            let mask = 0u32.wrapping_sub(crc & 1);
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    i64::from((!crc) & 0x7FFF_FFFF)
}

/// SQLite-backed message store (`core.py:MessageStore`). `open` mirrors
/// `__init__`: `runtime_dir(workspace)/team.db`, mkdir parents, init schema.
pub struct MessageStore {
    #[allow(dead_code)]
    path: std::path::PathBuf,
}

impl MessageStore {
    /// `MessageStore.__init__` (`core.py:51-55`): `workspace/.team/runtime/team.db`,
    /// create parents, `initialize_schema`.
    pub fn open(workspace: &Path) -> Result<Self, MessageStoreError> {
        Err(MessageStoreError::Retired)
    }

    /// Absolute path to the backing `team.db` (test/diagnostic accessor).
    pub fn db_path(&self) -> &Path {
        &self.path
    }

    pub fn persist_message(
        &self,
        input: PersistMessageInput<'_>,
    ) -> Result<String, MessageStoreError> {
        Err(MessageStoreError::Retired)
    }

    /// `create_message` (`core.py:71-114`). Returns `msg_<uuid4 hex[:12]>`; inserts
    /// a row with `status='accepted'`, `requires_ack` as 0/1 int, `artifact_refs`
    /// defaulting to `'[]'`, `delivery_attempts=0`, timestamps = now.
    #[allow(clippy::too_many_arguments)]
    pub fn create_message(
        &self,
        task_id: Option<&str>,
        sender: &str,
        recipient: &str,
        content: &str,
        reply_to: Option<&str>,
        requires_ack: bool,
        owner_team_id: Option<&str>,
    ) -> Result<String, MessageStoreError> {
        self.persist_message(PersistMessageInput {
            message_id: None,
            owner_team_id,
            task_id,
            sender,
            recipient,
            reply_to,
            requires_ack,
            status: MessageRowStatus::Accepted,
            content,
            presentation: r#"{"sink":"leader","class":"message"}"#,
            error: None,
        })
    }

    /// Caller-supplied-id variant of [`create_message`] (CR-015/054 — `--message-id`).
    /// Inserts exactly the given `message_id` instead of generating one. The store
    /// PK is `message_id`, so a repeat with the same id is rejected by SQLite; the
    /// caller is expected to gate via [`message_exists`] first to map collision to
    /// a typed `Duplicate` refusal rather than an opaque sqlite error.
    ///
    /// [`message_exists`]: Self::message_exists
    #[allow(clippy::too_many_arguments)]
    pub fn create_message_with_id(
        &self,
        message_id: &str,
        task_id: Option<&str>,
        sender: &str,
        recipient: &str,
        content: &str,
        reply_to: Option<&str>,
        requires_ack: bool,
        owner_team_id: Option<&str>,
    ) -> Result<String, MessageStoreError> {
        self.persist_message(PersistMessageInput {
            message_id: Some(message_id),
            owner_team_id,
            task_id,
            sender,
            recipient,
            reply_to,
            requires_ack,
            status: MessageRowStatus::Accepted,
            content,
            presentation: r#"{"sink":"leader","class":"message"}"#,
            error: None,
        })
    }

    /// `true` iff a `messages` row with this `message_id` already exists. Used by
    /// the send path to map a caller-key collision (CR-015/054) to a `Duplicate`
    /// refusal before attempting an insert that would otherwise fail on the PK.
    pub fn message_exists(&self, message_id: &str) -> Result<bool, MessageStoreError> {
        Err(MessageStoreError::Retired)
    }

    /// `mark` (`core.py:116-138`) — the messages.status state machine (this slice
    /// excludes the `delivery_tokens` side-effect, which is deferred).
    pub fn mark(
        &self,
        message_id: &str,
        status: &str,
        error: Option<&str>,
    ) -> Result<(), MessageStoreError> {
        Err(MessageStoreError::Retired)
    }

    /// Record that the transport submitted a message without claiming that the
    /// provider accepted it. The stable message id is also the receipt token.
    pub fn record_delivery_submission(
        &self,
        message_id: &str,
        visible: bool,
    ) -> Result<(), MessageStoreError> {
        Err(MessageStoreError::Retired)
    }

    /// Persist a verified leader transport submission when no provider receipt
    /// source is available. This state is claim/requeue-ineligible but does not
    /// clear the delivery obligation; `consumed_at` remains unset until a
    /// provider-side receipt reaches `mark_delivered_with_receipt`.
    pub fn mark_injected_awaiting_receipt(
        &self,
        message_id: &str,
    ) -> Result<(), MessageStoreError> {
        Err(MessageStoreError::Retired)
    }

    /// Atomically persist the provider-side receipt and advance the message to
    /// delivered. A transport-only caller cannot use this without first
    /// recording the submission row above.
    pub fn mark_delivered_with_receipt(&self, message_id: &str) -> Result<(), MessageStoreError> {
        Err(MessageStoreError::Retired)
    }

    /// `claim_for_delivery` (`core.py:190-205`): atomic single-winner claim. Flips an
    /// eligible row (status ∈ pending/accepted/queued_until_idle/queued_until_start/
    /// queued_stopped/queued_pane_missing, **or** `target_resolved` with a non-null
    /// error — the retry/error-bypass path) to `target_resolved`, clears `error`,
    /// `delivery_attempts += 1`. Returns `true` iff THIS update matched exactly one
    /// row. Clearing `error` is the CAS: a concurrent retry cannot also match
    /// `error is not null`.
    pub fn claim_for_delivery(&self, message_id: &str) -> Result<bool, MessageStoreError> {
        Err(MessageStoreError::Retired)
    }

    /// Release only an owned claim that has performed no input. Readiness
    /// waiting must remain retryable without consuming the input-attempt budget.
    /// A changed attempt, status, or error means this caller no longer owns it.
    pub(crate) fn release_delivery_before_input(
        &self,
        message_id: &str,
        attempt: u32,
        reason: &str,
    ) -> Result<bool, MessageStoreError> {
        Err(MessageStoreError::Retired)
    }

    /// Read inbox rows through [`Self::inbox`]; recovery transitions below are
    /// separate repository owners and are not claim eligibility.
    pub fn requeue_blocked_leader_messages(
        &self,
        owner_team_id: &str,
    ) -> Result<BlockedLeaderRequeueCounts, MessageStoreError> {
        Err(MessageStoreError::Retired)
    }

    pub fn recover_worker_pane_available(
        &self,
        agent_id: &str,
        owner_team_id: Option<&str>,
    ) -> Result<Vec<String>, MessageStoreError> {
        Err(MessageStoreError::Retired)
    }

    pub fn retry_submitted_explicit(
        &self,
        message_id: &str,
        incident_id: &str,
        operator_reason: &str,
        budget: RecoveryBudget,
    ) -> Result<bool, MessageStoreError> {
        Err(MessageStoreError::Retired)
    }

    /// Read ordinary inbox rows for an agent. Leader stage-result notifications are
    /// delivered through the leader receiver, not exposed as mailbox fallback rows.
    /// This projection intentionally has no owner-team filter when the caller does
    /// not provide one: legacy/CLI inbox must surface NULL-owner messages stored for
    /// the agent.
    pub fn inbox(
        &self,
        agent_id: &str,
        limit: usize,
        owner_team_id: Option<&str>,
    ) -> Result<Vec<serde_json::Value>, MessageStoreError> {
        Err(MessageStoreError::Retired)
    }

    /// `latest_results` (`core.py:458-471`): newest non-invalid result rows, oldest
    /// first (Python fetches `created_at desc limit ?` then reverses).
    pub fn latest_results(
        &self,
        limit: usize,
        owner_team_id: Option<&str>,
    ) -> Result<Vec<serde_json::Value>, MessageStoreError> {
        Err(MessageStoreError::Retired)
    }

    /// `claim_leader_notification_delivery` (`leader_notification_log.py:30-101`):
    /// `INSERT OR IGNORE` on PK `(result_id, owner_team_id, owner_epoch)`. rowcount==1
    /// → `claimed_by_you`; else read the existing winner row → `already_notified_by`.
    /// `owner_team_id` defaults to `""`; `owner_epoch=None` → [`legacy_epoch_from_uuid`].
    pub fn claim_leader_notification_delivery(
        &self,
        params: NotificationClaimParams<'_>,
    ) -> Result<NotificationClaim, MessageStoreError> {
        Err(MessageStoreError::Retired)
    }
}

fn row_to_message_value(row: &rusqlite::Row<'_>) -> rusqlite::Result<serde_json::Value> {
    Ok(serde_json::json!({
        "message_id": row.get::<_, String>(0)?,
        "owner_team_id": row.get::<_, Option<String>>(1)?,
        "task_id": row.get::<_, Option<String>>(2)?,
        "sender": row.get::<_, Option<String>>(3)?,
        "recipient": row.get::<_, Option<String>>(4)?,
        "reply_to": row.get::<_, Option<String>>(5)?,
        "requires_ack": row.get::<_, Option<i64>>(6)?,
        "status": row.get::<_, Option<String>>(7)?,
        "content": row.get::<_, Option<String>>(8)?,
        "artifact_refs": row.get::<_, Option<String>>(9)?,
        "created_at": row.get::<_, Option<String>>(10)?,
        "updated_at": row.get::<_, Option<String>>(11)?,
        "delivered_at": row.get::<_, Option<String>>(12)?,
        "acknowledged_at": row.get::<_, Option<String>>(13)?,
        "error": row.get::<_, Option<String>>(14)?,
        "delivery_attempts": row.get::<_, Option<i64>>(15)?,
    }))
}

/// `result_summary_from_row`(`status/queries.py:92-106`):解析 result 行的 envelope,
/// 产出 status/watch 共用的 result 摘要;envelope 坏/非对象 → `None`。
pub fn result_summary_from_row(row: &serde_json::Value) -> Option<serde_json::Value> {
    let envelope = match row.get("envelope") {
        Some(serde_json::Value::String(text)) => {
            serde_json::from_str::<serde_json::Value>(text).ok()?
        }
        Some(value @ serde_json::Value::Object(_)) => value.clone(),
        _ => return None,
    };
    if !envelope.is_object() {
        return None;
    }
    // Python `envelope.get(k) or row.get(k)` — falsy (null/empty) falls through to the row.
    let pick = |key: &str| {
        envelope
            .get(key)
            .filter(|v| !v.is_null() && v.as_str() != Some(""))
            .or_else(|| row.get(key))
            .cloned()
            .unwrap_or(serde_json::Value::Null)
    };
    Some(serde_json::json!({
        "result_id": row.get("result_id").cloned().unwrap_or(serde_json::Value::Null),
        "task_id": pick("task_id"),
        "agent_id": pick("agent_id"),
        "status": pick("status"),
        "summary": envelope.get("summary").cloned().unwrap_or(serde_json::Value::Null),
        "created_at": row.get("created_at").cloned().unwrap_or(serde_json::Value::Null),
    }))
}

fn now_ts() -> String {
    chrono::Utc::now().to_rfc3339()
}

fn next_message_id() -> String {
    let nanos = match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(duration) => {
            let low = duration.as_nanos() & u128::from(u64::MAX);
            u64::try_from(low).unwrap_or(0)
        }
        Err(_) => 0,
    };
    let counter = MESSAGE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let pid = u64::from(std::process::id());
    let value = nanos ^ counter.rotate_left(17) ^ pid.rotate_left(32);
    format!("msg_{:012x}", value & 0xFFFF_FFFF_FFFF)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use crate::db;
    use rusqlite::Connection;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU32, Ordering};

    static COUNTER: AtomicU32 = AtomicU32::new(0);

    fn temp_workspace() -> PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let ws = std::env::temp_dir().join(format!("ta_rs_msgstore_{}_{}", std::process::id(), n));
        std::fs::create_dir_all(&ws).unwrap();
        ws
    }

    fn store() -> MessageStore {
        MessageStore::open(&temp_workspace()).unwrap()
    }

    /// Fresh read connection onto the store's team.db (asserts DB-STATE parity
    /// independently of the store's own connection).
    fn read(store: &MessageStore) -> Connection {
        db::schema::open_db(store.db_path()).unwrap()
    }

    fn col_str(conn: &Connection, mid: &str, col: &str) -> Option<String> {
        // `col` is a fixed test literal, never user input.
        conn.query_row(
            &format!("select {col} from messages where message_id = ?1"),
            [mid],
            |r| r.get::<_, Option<String>>(0),
        )
        .unwrap()
    }

    fn col_i64(conn: &Connection, mid: &str, col: &str) -> i64 {
        conn.query_row(
            &format!("select {col} from messages where message_id = ?1"),
            [mid],
            |r| r.get(0),
        )
        .unwrap()
    }

    fn status_of(conn: &Connection, mid: &str) -> String {
        col_str(conn, mid, "status").unwrap()
    }

    /// (result_id, owner_team_id, owner_epoch, leader_session_uuid, notified_message_id)
    /// ordered by notified_at — the dedup-relevant projection of leader_notification_log.
    fn notif_rows(conn: &Connection) -> Vec<(String, String, i64, Option<String>, String)> {
        let mut stmt = conn
            .prepare(
                "select result_id, owner_team_id, owner_epoch, leader_session_uuid, notified_message_id \
                 from leader_notification_log order by notified_at",
            )
            .unwrap();
        stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, String>(4)?,
            ))
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
    }

    // ───────────────────────────── create_message ─────────────────────────────



    // ───────────────────────── claim_for_delivery (atomic claim) ─────────────────────────










    // ───────────────────────────── mark state machine ─────────────────────────────





    // ───────────────────── leader_notification_log dedup ─────────────────────

    fn params<'a>(
        result_id: &'a str,
        owner_team_id: Option<&'a str>,
        owner_epoch: Option<i64>,
        uuid: Option<&'a str>,
        proposed: &'a str,
    ) -> NotificationClaimParams<'a> {
        NotificationClaimParams {
            result_id,
            owner_team_id,
            owner_epoch,
            leader_session_uuid: uuid,
            proposed_message_id: proposed,
            envelope_hash: "h1",
            pane_id: Some("%1"),
        }
    }




    #[test]
    fn legacy_epoch_from_uuid_crc32_golden() {
        assert_eq!(legacy_epoch_from_uuid(None), 0);
        assert_eq!(legacy_epoch_from_uuid(Some("")), 0);
        assert_eq!(legacy_epoch_from_uuid(Some("uuid-AAA")), 926068568);
        assert_eq!(legacy_epoch_from_uuid(Some("uuid-BBB")), 122688376);
    }

    // ════════════════════════ FIX-LOOP (wave-1) RED test ════════════════════════
    // B1: mark() error column = coalesce(?, error) — a NEW error overwrites, but a
    // mark WITHOUT an error must PRESERVE the existing error (current impl clobbers
    // it to NULL). Golden /tmp/probe_b1.py vs team-agent-public v0.2.11.

}
