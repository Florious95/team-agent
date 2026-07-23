//! RED contract: a successful explicit leader claim flushes every leader-bound
//! mailbox row, including the `already_bound` success branch.
//!
//! Requirements: N31 (leader-bound delivery cannot remain stuck) and N32 (all
//! leader-bound forms converge through one delivery/requeue funnel).
//! Ground-truth row shape: the 0.5.54 incident exported
//! `recipient=leader,status=submitted_pending_acceptance,delivered_at=NULL,
//! attempts=1,error=NULL` after `leader_mailbox.queued_until_attach`.

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

#[test]
#[serial(env)]
fn already_bound_claim_flushes_submitted_leader_mailbox_row() {
    let case = Case::new("already-bound");
    let message_id = case.seed_incident_row("incident canary");
    let mut state = bound_state();

    let status = case.claim_and_run_success_hook(&mut state);

    assert_eq!(
        status,
        LeaseStatus::AlreadyBound,
        "fixture must exercise the already_bound enum branch"
    );
    assert_eq!(
        case.row(&message_id),
        Row::accepted(1),
        "N31/N32: already_bound is a successful claim and must requeue the same \
         submitted_pending_acceptance leader row; an Ok wrapper without this \
         enum-specific side effect is not delivery convergence"
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

    fn set_recipient(&self, message_id: &str, recipient: &str) {
        let conn = team_agent::db::schema::open_db(self.store.db_path()).unwrap();
        conn.execute(
            "update messages set recipient = ?2 where message_id = ?1",
            params![message_id, recipient],
        )
        .unwrap();
    }

    fn claim_and_run_success_hook(&self, state: &mut serde_json::Value) -> LeaseStatus {
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
        if result.ok {
            requeue_after_claim_leader(
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
        result.status
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
