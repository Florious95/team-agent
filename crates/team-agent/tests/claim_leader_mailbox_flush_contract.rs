//! RED contract (REVISED, inverted): a successful leader claim — including the
//! `already_bound` branch — requeues only rows that never crossed the
//! transport; it must NEVER flush `submitted_pending_acceptance` rows.
//!
//! REVISION LINEAGE (verifier signature, leader ruling msg_2bc80666aef8):
//! - 5027d533…6157 (original freeze at 1b2848a): asserted the OPPOSITE — that
//!   an `already_bound` claim flushes a `submitted_pending_acceptance` row.
//!   That premise was wrong at scale (leader co-signed the same error): those
//!   rows are the DESIGNED PARKED STATE of messages that already crossed the
//!   transport and merely lack a provider receipt. Shipping that semantics
//!   (d3a4966, 0.5.55) caused the P0 replay storm of 2026-07-23: one
//!   attach-triggered claim flipped 346 historical parked rows back to
//!   `accepted` and physically re-injected all of them (attempts 1→2),
//!   O(history) with no time/attempt bound (storm-p0-locate.md).
//! - THIS revision (new SHA): inverts the tooth (`already_bound` never
//!   flushes parked rows), adds the storm tooth (>=3 historical parked rows,
//!   zero flips on claim — the stored-data shape a fresh-environment gate
//!   misses), keeps the positive control (a newly claimed leader still
//!   flushes `queued_until_leader_attach`) and the non-leader negative
//!   control.
//!
//! DOCUMENTED RESIDUAL (explicit, not silent): after the 0.5.56 revert, a row
//! whose physical submit was genuinely LOST (submitted, no receipt, transport
//! evaporated) parks forever again — the original stranding this contract's
//! V1 tried to fix. That recovery belongs to the blocked-before-submit /
//! submitted-awaiting-receipt state-machine separation car (0.5.57+): a typed
//! recovery arm with attempt budget and age/owner-epoch bounds, never a
//! blanket claim-time requeue.
//!
//! Requirements: N31 (leader-bound delivery cannot remain stuck) applies to
//! rows that never crossed the transport; N32 (single delivery/requeue
//! funnel) is unchanged.

#![cfg(unix)]
#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

#[path = "support/hermetic.rs"]
mod hermetic_guard;
#[allow(dead_code)]
fn _hermetic_boundary_marker(_: &hermetic_guard::HermeticTestEnv) {}

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use rusqlite::params;
use serial_test::serial;
use team_agent::event_log::EventLog;
use team_agent::leader::{claim_lease_no_incident, LeaseStatus};
use team_agent::message_store::MessageStore;
use team_agent::messaging::enqueue_leader_mailbox_until_attach;
use team_agent::messaging::watchers::requeue_after_claim_leader;
use team_agent::model::enums::PaneLiveness;
use team_agent::model::ids::TeamKey;
use team_agent::state::owner_gate::PaneLivenessProbe;
use team_agent::transport::PaneId;

const TEAM: &str = "current";
const PANE: &str = "%leader";

struct LiveCaller;

impl PaneLivenessProbe for LiveCaller {
    fn liveness(&self, pane_id: &str) -> PaneLiveness {
        if pane_id == PANE {
            PaneLiveness::Live
        } else {
            PaneLiveness::Dead
        }
    }
}

/// INVERTED RED TOOTH — a `submitted_pending_acceptance` row already crossed
/// the transport; it is the designed parked state awaiting a provider
/// receipt. A claim (any branch, `already_bound` included) must leave it
/// COMPLETELY untouched: status, attempts, error, delivered_at, updated_at
/// and its delivery token row. Baseline 6a4b7a5 red: the claim flips it to
/// `accepted` and the coordinator physically re-injects (the 2026-07-23
/// storm, 346 rows, attempts 1->2).
#[test]
#[serial(env)]
fn already_bound_claim_never_flushes_submitted_pending_acceptance_row() {
    let case = Case::new("parked-untouched");
    let message_id = case.seed_incident_row("incident canary");
    case.seed_delivery_token(&message_id, "TOKEN-PARKED-1");
    let before_row = case.full_row(&message_id);
    let before_token = case.token_row(&message_id);
    let mut state = bound_state();

    let status = case.claim_and_run_success_hook(&mut state);

    assert_eq!(
        status,
        LeaseStatus::AlreadyBound,
        "fixture must exercise the already_bound enum branch"
    );
    assert_eq!(
        case.full_row(&message_id),
        before_row,
        "a parked (submitted_pending_acceptance) row must be byte-stable across a claim: \
         status/attempts/error/delivered_at/updated_at all unchanged. Flipping it to accepted \
         re-injects a message that already crossed the transport (storm-p0-locate.md B2)"
    );
    assert_eq!(
        case.token_row(&message_id),
        before_token,
        "the delivery token row of a parked message must survive a claim unchanged \
         (stable receipt identity)"
    );
}

/// STORM TOOTH — the stored-data face a fresh-environment gate misses:
/// three historical parked rows (created on earlier days) + one truly
/// blocked row + one delivered row. A claim may requeue ONLY the blocked
/// row; parked and delivered rows see zero flips, zero attempt growth, and
/// the requeue audit event reports exactly the blocked count (per-status
/// classification, locate §D3). Baseline red: eligible SQL flips all
/// parked rows too (O(history), no time/attempt bound).
#[test]
#[serial(env)]
fn claim_with_historical_backlog_requeues_only_truly_blocked_rows() {
    let case = Case::new("storm-backlog");
    let parked: Vec<String> = (0..3)
        .map(|i| {
            let id = case.seed_incident_row(&format!("parked history {i}"));
            case.backdate_created_at(&id, &format!("2026-07-2{}T0{}:00:00Z", 1 + i % 2, i));
            id
        })
        .collect();
    let blocked = case.seed_blocked_row("truly blocked canary");
    let delivered = case.seed_delivered_row("delivered history canary");
    let parked_before: Vec<FullRow> = parked.iter().map(|id| case.full_row(id)).collect();
    let delivered_before = case.full_row(&delivered);
    let mut state = bound_state();

    let status = case.claim_and_run_success_hook(&mut state);
    assert_eq!(status, LeaseStatus::AlreadyBound);

    let parked_after: Vec<FullRow> = parked.iter().map(|id| case.full_row(id)).collect();
    assert_eq!(
        parked_after, parked_before,
        "N parked rows must be fully unchanged by a claim — zero flips, zero attempts, \
         zero updated_at churn (the 346-row replay was exactly this set)"
    );
    assert_eq!(
        case.full_row(&delivered).row,
        delivered_before.row,
        "K delivered rows must never re-enter the delivery funnel via a claim"
    );
    assert_eq!(
        case.full_row(&blocked).row,
        Row::accepted(0),
        "M truly blocked rows (failed/leader_not_attached) must still be requeued — \
         the residual N31 face this contract keeps alive"
    );
    assert_eq!(
        case.last_blocked_requeue_count(),
        Some(1),
        "the requeue audit event must report exactly the truly-blocked count (M=1), \
         not parked+blocked — per-original-status classification (locate §D3)"
    );
}

/// WATCHER AMPLIFICATION TOOTH — historical watchers with `result_id=NULL`
/// can never be retried (the retry arm skips them), so rewriting them to
/// `notify_failed` + one audit event each on every claim is pure O(history)
/// write amplification (the 18:22 window: 108 rewrites before the process
/// was killed). A claim must leave null-result watchers untouched and not
/// return them as notices. Baseline red: the claim sweep selects every
/// `notified_message_id is null` watcher unbounded (incident_ts=None).
#[test]
#[serial(env)]
fn claim_does_not_rewrite_null_result_historical_watchers() {
    let case = Case::new("watcher-amp");
    let watchers: Vec<String> = (0..3)
        .map(|i| case.seed_null_result_watcher(i, "2026-07-21T00:00:00Z"))
        .collect();
    let before: Vec<(String, Option<String>)> =
        watchers.iter().map(|id| case.watcher_row(id)).collect();
    let mut state = bound_state();

    let notices = case.claim_and_collect_notices(&mut state);

    let after: Vec<(String, Option<String>)> =
        watchers.iter().map(|id| case.watcher_row(id)).collect();
    assert_eq!(
        after, before,
        "null-result historical watchers must not be rewritten by a claim: they are \
         unretryable (retry skips result_id=NULL), so per-claim rewrites are unbounded \
         write amplification (storm-p0-locate.md §4)"
    );
    assert!(
        notices
            .iter()
            .all(|notice| !watchers.contains(&notice.watcher_id)),
        "null-result watchers must not surface as claim notices; notices={notices:?}"
    );
}

#[test]
#[serial(env)]
fn newly_claimed_path_still_flushes_queued_leader_mailbox_row() {
    let case = Case::new("new-binding");
    let message_id = case.seed_queued_row("positive-control canary");
    let mut state = serde_json::json!({"session_name": "team-mailbox-positive"});

    let status = case.claim_and_run_success_hook(&mut state);

    assert_eq!(
        status,
        LeaseStatus::Claimed,
        "positive control must exercise the newly-bound enum branch"
    );
    assert_eq!(
        case.row(&message_id),
        Row::accepted(0),
        "existing new-binding mailbox flush must remain green"
    );
}

#[test]
#[serial(env)]
fn claim_does_not_flush_non_leader_submitted_row() {
    let case = Case::new("negative-control");
    let message_id = case.seed_incident_row("negative-control canary");
    case.set_recipient(&message_id, "worker-a");
    let mut state = bound_state();

    let status = case.claim_and_run_success_hook(&mut state);

    assert_eq!(status, LeaseStatus::AlreadyBound);
    assert_eq!(
        case.row(&message_id),
        Row::submitted(1),
        "negative control: claim flush is scoped to recipient=leader"
    );
}

struct Case {
    workspace: PathBuf,
    store: MessageStore,
    event_log: EventLog,
}

impl Case {
    fn new(tag: &str) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let workspace = std::env::temp_dir().join(format!(
            "ta-claim-mailbox-flush-{tag}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&workspace).unwrap();
        let store = MessageStore::open(&workspace).unwrap();
        let event_log = EventLog::new(&workspace);
        Self {
            workspace,
            store,
            event_log,
        }
    }

    fn seed_queued_row(&self, content: &str) -> String {
        enqueue_leader_mailbox_until_attach(
            &self.workspace,
            TEAM,
            content,
            None,
            "leader",
            &self.event_log,
        )
        .unwrap()
        .message_id
        .expect("mailbox enqueue returns message id")
    }

    fn seed_incident_row(&self, content: &str) -> String {
        let message_id = self.seed_queued_row(content);
        let conn = team_agent::db::schema::open_db(self.store.db_path()).unwrap();
        conn.execute(
            "update messages
             set status = 'submitted_pending_acceptance',
                 delivered_at = null,
                 delivery_attempts = 1,
                 error = null
             where message_id = ?1",
            params![message_id],
        )
        .unwrap();
        message_id
    }

    fn seed_blocked_row(&self, content: &str) -> String {
        let message_id = self.seed_queued_row(content);
        let conn = team_agent::db::schema::open_db(self.store.db_path()).unwrap();
        conn.execute(
            "update messages
             set status = 'failed', error = 'leader_not_attached', delivery_attempts = 0
             where message_id = ?1",
            params![message_id],
        )
        .unwrap();
        message_id
    }

    fn seed_delivered_row(&self, content: &str) -> String {
        let message_id = self.seed_queued_row(content);
        let conn = team_agent::db::schema::open_db(self.store.db_path()).unwrap();
        conn.execute(
            "update messages
             set status = 'delivered', delivered_at = '2026-07-21T12:00:00Z',
                 delivery_attempts = 1, error = null
             where message_id = ?1",
            params![message_id],
        )
        .unwrap();
        message_id
    }

    fn backdate_created_at(&self, message_id: &str, created_at: &str) {
        let conn = team_agent::db::schema::open_db(self.store.db_path()).unwrap();
        conn.execute(
            "update messages set created_at = ?2, updated_at = ?2 where message_id = ?1",
            params![message_id, created_at],
        )
        .unwrap();
    }

    fn seed_delivery_token(&self, message_id: &str, token: &str) {
        let conn = team_agent::db::schema::open_db(self.store.db_path()).unwrap();
        conn.execute(
            "insert into delivery_tokens (message_id, unique_token, injected_at)
             values (?1, ?2, '2026-07-21T12:00:00Z')",
            params![message_id, token],
        )
        .unwrap();
    }

    fn token_row(&self, message_id: &str) -> Option<(String, String)> {
        let conn = team_agent::db::schema::open_db(self.store.db_path()).unwrap();
        use rusqlite::OptionalExtension;
        conn.query_row(
            "select unique_token, injected_at from delivery_tokens where message_id = ?1",
            params![message_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .unwrap()
    }

    fn seed_null_result_watcher(&self, index: usize, created_at: &str) -> String {
        let watcher_id = format!("watch-null-{index}");
        let conn = team_agent::db::schema::open_db(self.store.db_path()).unwrap();
        conn.execute(
            "insert into result_watchers
             (watcher_id, owner_team_id, leader_id, status, created_at, completed_at,
              result_id, notified_message_id)
             values (?1, ?2, 'leader', 'notify_failed', ?3, ?3, null, null)",
            params![watcher_id, TEAM, created_at],
        )
        .unwrap();
        watcher_id
    }

    fn watcher_row(&self, watcher_id: &str) -> (String, Option<String>) {
        let conn = team_agent::db::schema::open_db(self.store.db_path()).unwrap();
        conn.query_row(
            "select status, completed_at from result_watchers where watcher_id = ?1",
            params![watcher_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap()
    }

    fn last_blocked_requeue_count(&self) -> Option<i64> {
        let path = self
            .workspace
            .join(".team")
            .join("logs")
            .join("events.jsonl");
        let text = std::fs::read_to_string(path).ok()?;
        text.lines()
            .rev()
            .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
            .find(|event| {
                event.get("event").and_then(serde_json::Value::as_str)
                    == Some("leader_receiver.blocked_messages_requeued")
            })
            .and_then(|event| event.get("count").and_then(serde_json::Value::as_i64))
    }

    fn set_recipient(&self, message_id: &str, recipient: &str) {
        let conn = team_agent::db::schema::open_db(self.store.db_path()).unwrap();
        conn.execute(
            "update messages set recipient = ?2 where message_id = ?1",
            params![message_id, recipient],
        )
        .unwrap();
    }

    fn claim_and_run_success_hook(&self, state: &mut serde_json::Value) -> LeaseStatus {
        let (status, _) = self.claim_inner(state);
        status
    }

    fn claim_and_collect_notices(
        &self,
        state: &mut serde_json::Value,
    ) -> Vec<team_agent::messaging::WatcherNotice> {
        let (_, notices) = self.claim_inner(state);
        notices
    }

    fn claim_inner(
        &self,
        state: &mut serde_json::Value,
    ) -> (LeaseStatus, Vec<team_agent::messaging::WatcherNotice>) {
        let team = TeamKey::new(TEAM);
        let pane = PaneId::new(PANE);
        let result = claim_lease_no_incident(
            &self.workspace,
            state,
            None,
            &team,
            &pane,
            true,
            &self.event_log,
            &LiveCaller,
        )
        .unwrap();
        let mut notices = Vec::new();
        if result.ok {
            notices = requeue_after_claim_leader(
                &self.workspace,
                &self.store,
                &self.event_log,
                &team,
                result
                    .bound_pane_id
                    .as_ref()
                    .expect("successful claim has pane"),
                None,
            )
            .unwrap();
        }
        (result.status, notices)
    }

    /// Row plus updated_at — the storm/inverted teeth require byte-stability
    /// of the WHOLE observable row, not just the status.
    fn full_row(&self, message_id: &str) -> FullRow {
        let conn = team_agent::db::schema::open_db(self.store.db_path()).unwrap();
        conn.query_row(
            "select status, delivered_at, delivery_attempts, error, updated_at
             from messages where message_id = ?1",
            params![message_id],
            |row| {
                Ok(FullRow {
                    row: Row {
                        status: row.get(0)?,
                        delivered_at: row.get(1)?,
                        attempts: row.get(2)?,
                        error: row.get(3)?,
                    },
                    updated_at: row.get(4)?,
                })
            },
        )
        .unwrap()
    }

    fn row(&self, message_id: &str) -> Row {
        let conn = team_agent::db::schema::open_db(self.store.db_path()).unwrap();
        conn.query_row(
            "select status, delivered_at, delivery_attempts, error
             from messages where message_id = ?1",
            params![message_id],
            |row| {
                Ok(Row {
                    status: row.get(0)?,
                    delivered_at: row.get(1)?,
                    attempts: row.get(2)?,
                    error: row.get(3)?,
                })
            },
        )
        .unwrap()
    }
}

impl Drop for Case {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.workspace);
    }
}

#[derive(Debug, PartialEq, Eq)]
struct FullRow {
    row: Row,
    updated_at: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
struct Row {
    status: String,
    delivered_at: Option<String>,
    attempts: i64,
    error: Option<String>,
}

impl Row {
    fn accepted(attempts: i64) -> Self {
        Self {
            status: "accepted".to_string(),
            delivered_at: None,
            attempts,
            error: None,
        }
    }

    fn submitted(attempts: i64) -> Self {
        Self {
            status: "submitted_pending_acceptance".to_string(),
            delivered_at: None,
            attempts,
            error: None,
        }
    }
}

fn bound_state() -> serde_json::Value {
    serde_json::json!({
        "session_name": "team-mailbox-already-bound",
        "team_owner": {
            "pane_id": PANE,
            "provider": "codex",
            "owner_epoch": 7,
            "claimed_at": "2026-07-23T05:13:42Z",
            "claimed_via": "claim-leader"
        },
        "leader_receiver": {
            "pane_id": PANE,
            "provider": "codex",
            "owner_epoch": 7,
            "status": "attached"
        }
    })
}
