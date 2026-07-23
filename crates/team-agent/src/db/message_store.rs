// unit-10 (Stage 4): moved from crate::message_store to crate::db::message_store.
// The historical body uses unwrap/expect/panic for SQL-row failures; the parent
// db/ module denies these lints, so re-allow them here so the move is a pure
// physical relocation (zero behavior change).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! step 7 · message_store — core message lifecycle over `team.db`.
//!
//! Truth source (READ-ONLY): `team-agent-public` @ v0.2.11,
//! `team_agent/message_store/core.py` + `leader_notification_log.py`.
//! Builds on step 3 [`crate::db::schema`] (DDL/migration already create all 8
//! tables incl. `messages` + `leader_notification_log`).
//!
//! SCOPE (this slice) = the semantic-tricky core lifecycle:
//!   1. [`MessageStore::create_message`] — insert a fresh `msg_<hex12>` row,
//!      status `accepted` (`core.py:71-114`).
//!   2. [`MessageStore::claim_for_delivery`] — atomic single-winner claim; flips
//!      an eligible row to `target_resolved`, bumps `delivery_attempts`, returns
//!      whether THIS caller won (`rowcount == 1`) (`core.py:190-205`).
//!   3. [`MessageStore::mark`] — status state machine; the only guard is that
//!      `acknowledged` is STICKY against delivery statuses (injected/visible/
//!      submitted/submitted_unverified/delivered) but NOT against others
//!      (e.g. `failed` overwrites) (`core.py:116-138`, the SQL CASE).
//!   4. [`MessageStore::claim_leader_notification_delivery`] — exactly-once dedup
//!      at the leader-injection boundary. Dedup key = `(result_id, owner_team_id,
//!      owner_epoch)` via the PK + `INSERT OR IGNORE`; **`leader_session_uuid` is
//!      NOT part of the key** (nullable audit metadata only). When `owner_epoch`
//!      is `None` it is derived from the uuid via [`legacy_epoch_from_uuid`]
//!      (`leader_notification_log.py:30-101,145-147`).
//!
//! DEFERRED (note, don't build) — follow-on RED slices: scheduled events, token
//! accounting (incl. the `delivery_tokens` side-effect of `mark`), agent health,
//! result watchers, results store, `artifact_refs` payloads, busy-retry timing.
//!
//! §10: pure-ish lib over SQLite — no panic on malformed input; every path
//! returns `Result<_, MessageStoreError>`.

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::{params, OptionalExtension};
use thiserror::Error;

static MESSAGE_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Error)]
pub enum MessageStoreError {
    #[error("db: {0}")]
    Db(#[from] crate::db::DbError),
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("delivery receipt missing for message: {0}")]
    DeliveryReceiptMissing(String),
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
    SubmittedUnverified,
    Delivered,
    Acknowledged,
    Consumed,
    Failed,
    BlockedLeaderUnbound,
    BlockedWorkerPaneMissing,
}

impl MessageRowStatus {
    pub const ALL: [Self; 14] = [
        Self::Accepted,
        Self::StoredOnly,
        Self::QueuedUntilLeaderAttach,
        Self::QueuedCoordinatorUnavailable,
        Self::QueuedPaneMissing,
        Self::TargetResolved,
        Self::SubmittedAwaitingReceipt,
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
pub fn legacy_epoch_from_uuid(leader_session_uuid: Option<&str>) -> i64 {
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
        let runtime_dir = workspace.join(".team").join("runtime");
        std::fs::create_dir_all(&runtime_dir)?;
        let path = runtime_dir.join("team.db");
        let existed = path.exists();
        let conn = crate::db::schema::open_db(&path)?;
        if existed {
            conn.busy_timeout(Duration::from_millis(5))?;
            let version = conn.query_row("pragma user_version", [], |row| row.get::<_, i64>(0));
            conn.busy_timeout(Duration::from_millis(30_000))?;
            match version {
                Ok(version) if version == crate::db::schema::SCHEMA_VERSION => {}
                Ok(_) => crate::db::schema::initialize_schema(&conn, Some(&path))?,
                Err(rusqlite::Error::SqliteFailure(err, _))
                    if matches!(
                        err.code,
                        rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked
                    ) => {}
                Err(error) => return Err(error.into()),
            }
        } else {
            crate::db::schema::initialize_schema(&conn, Some(&path))?;
        }
        Ok(Self { path })
    }

    /// Absolute path to the backing `team.db` (test/diagnostic accessor).
    pub fn db_path(&self) -> &Path {
        &self.path
    }

    pub fn persist_message(
        &self,
        input: PersistMessageInput<'_>,
    ) -> Result<String, MessageStoreError> {
        let conn = crate::db::schema::open_db(&self.path)?;
        let message_id = input
            .message_id
            .map(ToOwned::to_owned)
            .unwrap_or_else(next_message_id);
        let now = now_ts();
        conn.execute(
            "insert into messages(
                message_id, owner_team_id, task_id, sender, recipient, reply_to, requires_ack,
                status, content, presentation, artifact_refs, created_at, updated_at, delivered_at,
                acknowledged_at, error, delivery_attempts
            ) values (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, '[]', ?11, ?11, null, null, ?12, 0)",
            params![
                message_id,
                input.owner_team_id,
                input.task_id,
                input.sender,
                input.recipient,
                input.reply_to,
                if input.requires_ack { 1 } else { 0 },
                input.status.as_str(),
                input.content,
                input.presentation,
                now,
                input.error,
            ],
        )?;
        Ok(message_id)
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
        let conn = crate::db::schema::open_db(&self.path)?;
        let row: Option<i64> = conn
            .query_row(
                "select 1 from messages where message_id = ?1",
                params![message_id],
                |row| row.get(0),
            )
            .optional()?;
        Ok(row.is_some())
    }

    /// `mark` (`core.py:116-138`) — the messages.status state machine (this slice
    /// excludes the `delivery_tokens` side-effect, which is deferred).
    pub fn mark(
        &self,
        message_id: &str,
        status: &str,
        error: Option<&str>,
    ) -> Result<(), MessageStoreError> {
        let conn = crate::db::schema::open_db(&self.path)?;
        let now = now_ts();
        conn.execute(
            "update messages
             set status = case
                 when status = 'acknowledged'
                      and ?2 in ('injected', 'visible', 'submitted', 'submitted_unverified', 'delivered')
                 then status
                 else ?2
             end,
             updated_at = ?3,
             delivered_at = case
                 when ?2 in ('injected', 'visible', 'submitted', 'delivered')
                 then ?3
                 else delivered_at
             end,
             acknowledged_at = case when ?2 = 'acknowledged' then ?3 else acknowledged_at end,
             error = case when ?2 = 'delivered' then null else coalesce(?4, error) end
             where message_id = ?1",
            params![message_id, status, now, error],
        )?;
        Ok(())
    }

    /// Record that the transport submitted a message without claiming that the
    /// provider accepted it. The stable message id is also the receipt token.
    pub fn record_delivery_submission(
        &self,
        message_id: &str,
        visible: bool,
    ) -> Result<(), MessageStoreError> {
        let conn = crate::db::schema::open_db(&self.path)?;
        let now = now_ts();
        conn.execute(
            "insert into delivery_tokens(
                 message_id, unique_token, injected_at, visible_at,
                 consumed_at, failed_at, failure_reason
             ) values (?1, ?1, ?2, ?3, null, null, null)
             on conflict(message_id) do update set
                 visible_at = coalesce(delivery_tokens.visible_at, excluded.visible_at),
                 failed_at = null,
                 failure_reason = null",
            params![message_id, now, visible.then_some(now_ts())],
        )?;
        Ok(())
    }

    /// Atomically persist the provider-side receipt and advance the message to
    /// delivered. A transport-only caller cannot use this without first
    /// recording the submission row above.
    pub fn mark_delivered_with_receipt(&self, message_id: &str) -> Result<(), MessageStoreError> {
        let mut conn = crate::db::schema::open_db(&self.path)?;
        let tx = conn.transaction()?;
        let now = now_ts();
        let receipts = tx.execute(
            "update delivery_tokens
             set consumed_at = coalesce(consumed_at, ?2),
                 failed_at = null,
                 failure_reason = null
             where message_id = ?1",
            params![message_id, now],
        )?;
        if receipts != 1 {
            return Err(MessageStoreError::DeliveryReceiptMissing(
                message_id.to_string(),
            ));
        }
        tx.execute(
            "update messages
             set status = case when status = 'acknowledged' then status else 'delivered' end,
                 updated_at = ?2,
                 delivered_at = ?2,
                 error = null
             where message_id = ?1",
            params![message_id, now_ts()],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// `claim_for_delivery` (`core.py:190-205`): atomic single-winner claim. Flips an
    /// eligible row (status ∈ pending/accepted/queued_until_idle/queued_until_start/
    /// queued_stopped/queued_pane_missing) to `target_resolved`, `delivery_attempts +=
    /// 1`. Returns `true` iff THIS update matched exactly one row.
    pub fn claim_for_delivery(&self, message_id: &str) -> Result<bool, MessageStoreError> {
        let conn = crate::db::schema::open_db(&self.path)?;
        let rows = conn.execute(
            "update messages
             set status = 'target_resolved',
                 delivery_attempts = delivery_attempts + 1,
                 updated_at = ?2
             where message_id = ?1
               and status in (
                   'pending', 'accepted', 'queued_until_idle', 'queued_until_start',
                   'queued_stopped', 'queued_pane_missing', ?3
               )",
            params![
                message_id,
                now_ts(),
                MessageRowStatus::QueuedCoordinatorUnavailable.as_str()
            ],
        )?;
        Ok(rows == 1)
    }

    pub fn requeue_blocked_leader_messages(
        &self,
        owner_team_id: &str,
    ) -> Result<BlockedLeaderRequeueCounts, MessageStoreError> {
        let mut conn = crate::db::schema::open_db(&self.path)?;
        let tx = conn.transaction()?;
        let now = now_ts();
        let blocked_leader_unbound = tx.execute(
            "update messages
             set status = ?1, error = null, updated_at = ?2
             where recipient = 'leader'
               and owner_team_id = ?3
               and status = ?4
               and error = 'leader_not_attached'",
            params![
                MessageRowStatus::Accepted.as_str(),
                now,
                owner_team_id,
                MessageRowStatus::Failed.as_str()
            ],
        )?;
        let queued_until_leader_attach = tx.execute(
            "update messages
             set status = ?1, error = null, updated_at = ?2
             where recipient = 'leader'
               and owner_team_id = ?3
               and status = ?4",
            params![
                MessageRowStatus::Accepted.as_str(),
                now,
                owner_team_id,
                MessageRowStatus::QueuedUntilLeaderAttach.as_str()
            ],
        )?;
        tx.commit()?;
        Ok(BlockedLeaderRequeueCounts {
            blocked_leader_unbound,
            queued_until_leader_attach,
        })
    }

    pub fn recover_worker_pane_available(
        &self,
        agent_id: &str,
        owner_team_id: Option<&str>,
    ) -> Result<Vec<String>, MessageStoreError> {
        let mut conn = crate::db::schema::open_db(&self.path)?;
        let tx = conn.transaction()?;
        let ids = {
            let mut stmt = tx.prepare(
                "select message_id from messages
                 where recipient = ?1
                   and status = ?2
                   and error = 'tmux_target_missing'
                   and (
                       (?3 is null and owner_team_id is null)
                       or owner_team_id = ?3
                   )
                 order by created_at, message_id",
            )?;
            let rows = stmt
                .query_map(
                    params![
                        agent_id,
                        MessageRowStatus::QueuedPaneMissing.as_str(),
                        owner_team_id
                    ],
                    |row| row.get::<_, String>(0),
                )?
                .collect::<Result<Vec<_>, _>>()?;
            rows
        };
        let now = now_ts();
        for message_id in &ids {
            tx.execute(
                "update messages
                 set status = ?1, error = null, updated_at = ?2
                 where message_id = ?3",
                params![MessageRowStatus::Accepted.as_str(), now, message_id],
            )?;
        }
        tx.commit()?;
        Ok(ids)
    }

    /// Read inbox rows for an agent. This projection intentionally has no owner-team
    /// filter when the caller does not provide one: legacy/CLI inbox must surface
    /// NULL-owner messages stored for the agent.
    pub fn inbox(
        &self,
        agent_id: &str,
        limit: usize,
        owner_team_id: Option<&str>,
    ) -> Result<Vec<serde_json::Value>, MessageStoreError> {
        let conn = crate::db::schema::open_db(&self.path)?;
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let sql = match owner_team_id {
            Some(_) => {
                "select message_id, owner_team_id, task_id, sender, recipient, reply_to, requires_ack,
                        status, content, artifact_refs, created_at, updated_at, delivered_at,
                        acknowledged_at, error, delivery_attempts
                 from messages
                 where (sender = ?1 or recipient = ?1) and owner_team_id = ?3
                 order by created_at desc
                 limit ?2"
            }
            None => {
                "select message_id, owner_team_id, task_id, sender, recipient, reply_to, requires_ack,
                        status, content, artifact_refs, created_at, updated_at, delivered_at,
                        acknowledged_at, error, delivery_attempts
                 from messages
                 where sender = ?1 or recipient = ?1
                 order by created_at desc
                 limit ?2"
            }
        };
        let rows = match owner_team_id {
            Some(team) => {
                let mut stmt = conn.prepare(sql)?;
                let values = stmt
                    .query_map(params![agent_id, limit, team], row_to_message_value)?
                    .collect::<Result<Vec<_>, _>>()?;
                values
            }
            None => {
                let mut stmt = conn.prepare(sql)?;
                let values = stmt
                    .query_map(params![agent_id, limit], row_to_message_value)?
                    .collect::<Result<Vec<_>, _>>()?;
                values
            }
        };
        Ok(rows.into_iter().rev().collect())
    }

    /// `latest_results` (`core.py:458-471`): newest non-invalid result rows, oldest
    /// first (Python fetches `created_at desc limit ?` then reverses).
    pub fn latest_results(
        &self,
        limit: usize,
        owner_team_id: Option<&str>,
    ) -> Result<Vec<serde_json::Value>, MessageStoreError> {
        let conn = crate::db::schema::open_db(&self.path)?;
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let sql = match owner_team_id {
            Some(_) => {
                "select owner_team_id, result_id, task_id, agent_id, envelope, status, created_at
                 from results
                 where status != 'invalid' and owner_team_id = ?2
                 order by created_at desc
                 limit ?1"
            }
            None => {
                "select owner_team_id, result_id, task_id, agent_id, envelope, status, created_at
                 from results
                 where status != 'invalid'
                 order by created_at desc
                 limit ?1"
            }
        };
        let mut stmt = conn.prepare(sql)?;
        let map_row = |row: &rusqlite::Row<'_>| {
            Ok(serde_json::json!({
                "owner_team_id": row.get::<_, Option<String>>(0)?,
                "result_id": row.get::<_, String>(1)?,
                "task_id": row.get::<_, Option<String>>(2)?,
                "agent_id": row.get::<_, Option<String>>(3)?,
                "envelope": row.get::<_, Option<String>>(4)?,
                "status": row.get::<_, Option<String>>(5)?,
                "created_at": row.get::<_, Option<String>>(6)?,
            }))
        };
        let rows = match owner_team_id {
            Some(team) => stmt
                .query_map(params![limit, team], map_row)?
                .collect::<Result<Vec<_>, _>>()?,
            None => stmt
                .query_map(params![limit], map_row)?
                .collect::<Result<Vec<_>, _>>()?,
        };
        Ok(rows.into_iter().rev().collect())
    }

    /// Allow direct peer messages in both directions. Golden stores `(a,b)` and
    /// `(b,a)` so either sender/recipient lookup can use a single ordered key.
    pub fn allow_peer(&self, a: &str, b: &str) -> Result<(), MessageStoreError> {
        let conn = crate::db::schema::open_db(&self.path)?;
        let now = now_ts();
        conn.execute(
            "insert or ignore into peer_allowlist(a, b, created_at) values (?1, ?2, ?3)",
            params![a, b, now.as_str()],
        )?;
        conn.execute(
            "insert or ignore into peer_allowlist(a, b, created_at) values (?1, ?2, ?3)",
            params![b, a, now.as_str()],
        )?;
        Ok(())
    }

    /// `claim_leader_notification_delivery` (`leader_notification_log.py:30-101`):
    /// `INSERT OR IGNORE` on PK `(result_id, owner_team_id, owner_epoch)`. rowcount==1
    /// → `claimed_by_you`; else read the existing winner row → `already_notified_by`.
    /// `owner_team_id` defaults to `""`; `owner_epoch=None` → [`legacy_epoch_from_uuid`].
    pub fn claim_leader_notification_delivery(
        &self,
        params: NotificationClaimParams<'_>,
    ) -> Result<NotificationClaim, MessageStoreError> {
        let conn = crate::db::schema::open_db(&self.path)?;
        let owner_team_id = params.owner_team_id.unwrap_or("");
        let owner_epoch = match params.owner_epoch {
            Some(epoch) => epoch,
            None => legacy_epoch_from_uuid(params.leader_session_uuid),
        };
        let rows = conn.execute(
            "insert or ignore into leader_notification_log(
                result_id, owner_team_id, owner_epoch, leader_session_uuid, notified_message_id,
                notified_at, leader_pane_id_at_notify, envelope_content_hash
             ) values (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                params.result_id,
                owner_team_id,
                owner_epoch,
                params.leader_session_uuid,
                params.proposed_message_id,
                now_ts(),
                params.pane_id,
                params.envelope_hash,
            ],
        )?;
        if rows == 1 {
            return Ok(NotificationClaim {
                status: "claimed_by_you".to_string(),
                notified_message_id: params.proposed_message_id.to_string(),
            });
        }

        let notified_message_id = conn
            .query_row(
                "select notified_message_id from leader_notification_log
                 where result_id = ?1 and owner_team_id = ?2 and owner_epoch = ?3",
                params![params.result_id, owner_team_id, owner_epoch],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .ok_or(rusqlite::Error::QueryReturnedNoRows)?;
        Ok(NotificationClaim {
            status: "already_notified_by".to_string(),
            notified_message_id,
        })
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

    #[test]
    fn create_message_inserts_accepted_row() {
        let s = store();
        let mid = s
            .create_message(
                Some("task_1"),
                "leader",
                "alice",
                "hello",
                None,
                true,
                Some("team_A"),
            )
            .unwrap();
        assert!(mid.starts_with("msg_"), "id: {mid}");
        assert_eq!(mid.len(), 16, "msg_ + 12 hex");

        let c = read(&s);
        assert_eq!(status_of(&c, &mid), "accepted");
        assert_eq!(col_str(&c, &mid, "task_id").as_deref(), Some("task_1"));
        assert_eq!(col_str(&c, &mid, "sender").as_deref(), Some("leader"));
        assert_eq!(col_str(&c, &mid, "recipient").as_deref(), Some("alice"));
        assert_eq!(
            col_str(&c, &mid, "owner_team_id").as_deref(),
            Some("team_A")
        );
        assert_eq!(col_str(&c, &mid, "content").as_deref(), Some("hello"));
        assert_eq!(col_str(&c, &mid, "artifact_refs").as_deref(), Some("[]"));
        assert_eq!(col_str(&c, &mid, "reply_to"), None);
        assert_eq!(col_i64(&c, &mid, "requires_ack"), 1);
        assert_eq!(col_i64(&c, &mid, "delivery_attempts"), 0);
        assert_eq!(col_str(&c, &mid, "delivered_at"), None);
        assert_eq!(col_str(&c, &mid, "acknowledged_at"), None);
        assert_eq!(col_str(&c, &mid, "error"), None);
    }

    #[test]
    fn create_message_no_task_no_owner_ack_false() {
        let s = store();
        let mid = s
            .create_message(None, "leader", "bob", "hi", None, false, None)
            .unwrap();
        let c = read(&s);
        assert_eq!(col_str(&c, &mid, "task_id"), None);
        assert_eq!(col_str(&c, &mid, "owner_team_id"), None);
        assert_eq!(col_i64(&c, &mid, "requires_ack"), 0);
        assert_eq!(status_of(&c, &mid), "accepted");
    }

    // ───────────────────────── claim_for_delivery (atomic claim) ─────────────────────────

    #[test]
    fn claim_for_delivery_first_caller_wins() {
        let s = store();
        let mid = s
            .create_message(Some("t"), "s", "r", "c", None, true, None)
            .unwrap();

        assert!(
            s.claim_for_delivery(&mid).unwrap(),
            "accepted is eligible → claim wins"
        );
        let c = read(&s);
        assert_eq!(status_of(&c, &mid), "target_resolved");
        assert_eq!(col_i64(&c, &mid, "delivery_attempts"), 1);
    }

    #[test]
    fn claim_for_delivery_second_caller_loses_and_state_unchanged() {
        // Atomic single-winner: once target_resolved, a re-claim returns false and
        // must NOT bump delivery_attempts again.
        let s = store();
        let mid = s
            .create_message(Some("t"), "s", "r", "c", None, true, None)
            .unwrap();
        assert!(s.claim_for_delivery(&mid).unwrap());
        assert!(
            !s.claim_for_delivery(&mid).unwrap(),
            "already target_resolved → no second winner"
        );

        let c = read(&s);
        assert_eq!(status_of(&c, &mid), "target_resolved");
        assert_eq!(col_i64(&c, &mid, "delivery_attempts"), 1);
    }

    #[test]
    fn claim_for_delivery_nonexistent_is_false() {
        let s = store();
        assert!(!s.claim_for_delivery("msg_doesnotexist").unwrap());
    }

    #[test]
    fn claim_for_delivery_ineligible_status_is_false() {
        // 'failed' is not in the eligible set → claim returns false, status unchanged.
        let s = store();
        let mid = s
            .create_message(Some("t"), "s", "r", "c", None, true, None)
            .unwrap();
        s.mark(&mid, "failed", Some("boom")).unwrap();
        assert!(!s.claim_for_delivery(&mid).unwrap());
        assert_eq!(status_of(&read(&s), &mid), "failed");
    }

    #[test]
    fn claim_for_delivery_never_claims_stored_only_presentation() {
        let s = store();
        let mid = s
            .persist_message(PersistMessageInput {
                message_id: None,
                owner_team_id: Some("team-a"),
                task_id: None,
                sender: "worker",
                recipient: "leader",
                reply_to: None,
                requires_ack: false,
                status: MessageRowStatus::StoredOnly,
                content: "casefile evidence",
                presentation: r#"{"sink":"casefile","class":"stage_result"}"#,
                error: None,
            })
            .unwrap();
        assert!(!s.claim_for_delivery(&mid).unwrap());
        assert_eq!(status_of(&read(&s), &mid), "stored_only");
        assert_eq!(col_i64(&read(&s), &mid, "delivery_attempts"), 0);
    }

    // ───────────────────────────── mark state machine ─────────────────────────────

    #[test]
    fn mark_injected_sets_status_and_delivered_at() {
        let s = store();
        let mid = s
            .create_message(Some("t"), "s", "r", "c", None, true, None)
            .unwrap();
        s.mark(&mid, "injected", None).unwrap();
        let c = read(&s);
        assert_eq!(status_of(&c, &mid), "injected");
        assert!(
            col_str(&c, &mid, "delivered_at").is_some(),
            "delivered_at set for injected"
        );
        assert_eq!(col_str(&c, &mid, "acknowledged_at"), None);
    }

    #[test]
    fn mark_acknowledged_is_sticky_against_delivery_statuses() {
        // CASE guard: once acknowledged, marks to injected/visible/... are ignored.
        let s = store();
        let mid = s
            .create_message(Some("t"), "s", "r", "c", None, true, None)
            .unwrap();
        s.mark(&mid, "acknowledged", None).unwrap();
        assert_eq!(status_of(&read(&s), &mid), "acknowledged");
        assert!(col_str(&read(&s), &mid, "acknowledged_at").is_some());

        s.mark(&mid, "injected", None).unwrap();
        assert_eq!(
            status_of(&read(&s), &mid),
            "acknowledged",
            "injected ignored after ack"
        );
        s.mark(&mid, "visible", None).unwrap();
        assert_eq!(
            status_of(&read(&s), &mid),
            "acknowledged",
            "visible ignored after ack"
        );
    }

    #[test]
    fn mark_acknowledged_then_failed_overwrites() {
        // 'failed' is NOT in the guarded delivery set → it overwrites acknowledged.
        let s = store();
        let mid = s
            .create_message(Some("t"), "s", "r", "c", None, true, None)
            .unwrap();
        s.mark(&mid, "acknowledged", None).unwrap();
        s.mark(&mid, "failed", Some("x")).unwrap();
        assert_eq!(status_of(&read(&s), &mid), "failed");
    }

    #[test]
    fn mark_overwrites_for_non_acknowledged() {
        let s = store();
        let mid = s
            .create_message(Some("t"), "s", "r", "c", None, true, None)
            .unwrap();
        s.mark(&mid, "injected", None).unwrap();
        s.mark(&mid, "visible", None).unwrap();
        assert_eq!(status_of(&read(&s), &mid), "visible");
    }

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
    fn leader_notification_dedup_key_excludes_session_uuid() {
        // SAME (result_id, owner_team_id, owner_epoch) but DIFFERENT leader_session_uuid
        // and proposed id → second is deduped to the first winner.
        let s = store();
        let r1 = s
            .claim_leader_notification_delivery(params(
                "res_1",
                Some("team_A"),
                Some(7),
                Some("uuid-AAA"),
                "msg_notif_1",
            ))
            .unwrap();
        assert_eq!(r1.status, "claimed_by_you");
        assert_eq!(r1.notified_message_id, "msg_notif_1");

        let r2 = s
            .claim_leader_notification_delivery(params(
                "res_1",
                Some("team_A"),
                Some(7),
                Some("uuid-BBB"),
                "msg_notif_2",
            ))
            .unwrap();
        assert_eq!(r2.status, "already_notified_by");
        assert_eq!(
            r2.notified_message_id, "msg_notif_1",
            "loser sees first winner's id, not its own"
        );

        // Exactly ONE row — carrying the FIRST caller's uuid (uuid not in the key).
        assert_eq!(
            notif_rows(&read(&s)),
            vec![(
                "res_1".to_string(),
                "team_A".to_string(),
                7,
                Some("uuid-AAA".to_string()),
                "msg_notif_1".to_string()
            )]
        );
    }

    #[test]
    fn leader_notification_different_epoch_is_a_new_claim() {
        let s = store();
        s.claim_leader_notification_delivery(params(
            "res_1",
            Some("team_A"),
            Some(7),
            Some("uuid-AAA"),
            "msg_notif_1",
        ))
        .unwrap();
        let r3 = s
            .claim_leader_notification_delivery(params(
                "res_1",
                Some("team_A"),
                Some(8),
                Some("uuid-AAA"),
                "msg_notif_3",
            ))
            .unwrap();
        assert_eq!(
            r3.status, "claimed_by_you",
            "different owner_epoch → different PK → new claim"
        );

        let rows = notif_rows(&read(&s));
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].2, 7);
        assert_eq!(rows[1].2, 8);
        assert_eq!(rows[1].4, "msg_notif_3");
    }

    #[test]
    fn leader_notification_none_epoch_derives_from_uuid() {
        // owner_epoch=None → epoch derived from uuid. Same uuid dedups; different uuid
        // yields a different derived epoch → a new claim.
        let s = store();
        let a1 = s
            .claim_leader_notification_delivery(params(
                "res_2",
                Some("team_B"),
                None,
                Some("uuid-AAA"),
                "m1",
            ))
            .unwrap();
        assert_eq!(a1.status, "claimed_by_you");

        let a2 = s
            .claim_leader_notification_delivery(params(
                "res_2",
                Some("team_B"),
                None,
                Some("uuid-AAA"),
                "m2",
            ))
            .unwrap();
        assert_eq!(a2.status, "already_notified_by");
        assert_eq!(a2.notified_message_id, "m1");

        let a3 = s
            .claim_leader_notification_delivery(params(
                "res_2",
                Some("team_B"),
                None,
                Some("uuid-BBB"),
                "m3",
            ))
            .unwrap();
        assert_eq!(
            a3.status, "claimed_by_you",
            "different uuid → different derived epoch → new claim"
        );

        // Stored epochs equal the crc32 derivation of each uuid.
        let rows = notif_rows(&read(&s));
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].2, 926068568); // crc32('uuid-AAA') & 0x7FFFFFFF
        assert_eq!(rows[1].2, 122688376); // crc32('uuid-BBB') & 0x7FFFFFFF
    }

    #[test]
    fn legacy_epoch_from_uuid_crc32_golden() {
        assert_eq!(legacy_epoch_from_uuid(None), 0);
        assert_eq!(legacy_epoch_from_uuid(Some("")), 0);
        assert_eq!(legacy_epoch_from_uuid(Some("uuid-AAA")), 926068568);
        assert_eq!(legacy_epoch_from_uuid(Some("uuid-BBB")), 122688376);
    }

    #[test]
    fn allow_peer_inserts_bidirectional_rows_idempotently() {
        let s = store();
        s.allow_peer("alice", "bob").unwrap();
        s.allow_peer("alice", "bob").unwrap();

        let c = read(&s);
        let mut rows = c
            .prepare("select a, b from peer_allowlist order by a, b")
            .unwrap()
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        rows.sort();
        assert_eq!(
            rows,
            vec![
                ("alice".to_string(), "bob".to_string()),
                ("bob".to_string(), "alice".to_string()),
            ]
        );
    }

    // ════════════════════════ FIX-LOOP (wave-1) RED test ════════════════════════
    // B1: mark() error column = coalesce(?, error) — a NEW error overwrites, but a
    // mark WITHOUT an error must PRESERVE the existing error (current impl clobbers
    // it to NULL). Golden /tmp/probe_b1.py vs team-agent-public v0.2.11.

    #[test]
    fn fix_b1_mark_preserves_existing_error_when_none_given() {
        let s = store();
        let mid = s
            .create_message(Some("t"), "a", "b", "c", None, true, None)
            .unwrap();

        s.mark(&mid, "failed", Some("boom")).unwrap();
        assert_eq!(col_str(&read(&s), &mid, "error").as_deref(), Some("boom"));

        // Non-error mark to a delivery status: status/delivered_at advance, but error
        // must remain 'boom' (coalesce(NULL, error)), NOT be clobbered to NULL.
        s.mark(&mid, "injected", None).unwrap();
        let c = read(&s);
        assert_eq!(status_of(&c, &mid), "injected");
        assert_eq!(
            col_str(&c, &mid, "error").as_deref(),
            Some("boom"),
            "existing error must survive a no-error mark"
        );

        // A NEW error overwrites.
        s.mark(&mid, "failed", Some("second")).unwrap();
        assert_eq!(col_str(&read(&s), &mid, "error").as_deref(), Some("second"));
    }
}
