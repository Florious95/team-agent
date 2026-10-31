//! MED A-batch (2/3): semantics & resilience gates — slices A-2 / A-6 / A-8.
//!
//! Triage doc (sole basis): `.team/artifacts/med-triage-fixed-failure-sweep.md`.
//! Python truth source: 0.2.11.
//!
//! A-2 takeover reminder ignores arm_state — leader/takeover.rs:129-133 `let _ = arm_state`
//!     vs Python idle_predicate.py:20-75: an un-armed monitor (no worker turn-open since
//!     ack) must NEVER ping, reason "not_armed_no_worker_turn". The full Python state
//!     machine already exists in RS at provider/classify.rs:66+ (and is unit-locked in
//!     provider/tests/idle.rs); the leader facade simply never consults it. Debounce /
//!     episode-dedup tiers need time inputs the current facade signature lacks — those
//!     stay locked at the classify layer; this contract pins the facade's arm gate.
//!
//! A-6 resilience family:
//!   - watchers: retry_result_deliveries (watchers.rs:300-340) marks watchers `notified`
//!     WITHOUT performing any delivery. Python result_delivery.py:19-35 routes retries
//!     through notify_result_watchers which really delivers and only then records the
//!     notified message id; a failed delivery keeps the watcher retryable.
//!   - tick runtime approval: one agent's failed send_keys aborts the WHOLE tick
//!     (tick.rs:794 `?`). Python approvals/runtime_prompts.py:21-43 handles prompts
//!     per-agent via run_cmd(check=False) loops — one agent's tmux failure never kills
//!     the tick for the rest.
//!   - scheduler payload item from the triage was DOWNGRADED on Python grounding:
//!     Python scheduler.py:44 `json.loads(row["payload_json"] or "{}")` sits OUTSIDE the
//!     try block, so a corrupt payload aborts the Python batch identically (triage rule:
//!     same disease on both sides => not an A contract). Same for a corrupt result
//!     envelope inside the watcher retry (result_delivery.py:366 bare json.loads).
//!
//! A-8 upgrade-compat gate dead — Coordinator::schema_health (tick.rs:994-1001) is a
//!     hardcoded `ok:true` with zero db reads, while start() (tick.rs:942+) documents
//!     "schema 兼容门: 不可静默继续 (card §89)" and depends on it. Python
//!     coordinator/lifecycle.py:197+ message_store_schema_health really inspects team.db
//!     for missing required columns and refuses start.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[path = "support/hermetic.rs"]
mod hermetic_guard;
#[allow(dead_code)]
fn _hermetic_boundary_marker(_: &hermetic_guard::HermeticTestEnv) {}

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use rusqlite::params;
use serde_json::json;
use team_agent::event_log::EventLog;
use team_agent::leader::{evaluate_takeover_reminder, IdleNode, NodeRole};
use team_agent::message_store::MessageStore;
use team_agent::model::enums::Provider;
use team_agent::provider::TurnState;
use team_agent::state::persist::save_runtime_state;

/// A-2: all-idle nodes with an UN-ARMED arm_state must not ping
/// (Python idle_predicate.py:62 reason "not_armed_no_worker_turn").
#[test]
fn a2_all_idle_but_unarmed_must_not_ping() {
    let nodes = vec![idle_node("w1"), idle_node("w2")];
    let result = evaluate_takeover_reminder(&nodes, &json!({})).expect("evaluate should succeed");
    assert!(
        !result.should_ping,
        "A-2: an un-armed monitor must never ping (Python C1: only a worker turn-open \
arms the watch, idle_predicate.py:55-62); got should_ping=true reason={:?}",
        result.reason
    );
    assert_eq!(
        result.reason.as_deref(),
        Some("not_armed_no_worker_turn"),
        "A-2: the no-ping reason must be the Python literal"
    );
}

/// A-2 green lock: a non-idle node still blocks the ping (current behavior, keep it).
#[test]
fn a2_green_lock_blocking_node_still_blocks() {
    let mut blocking = idle_node("w1");
    blocking.state = TurnState::Working;
    let result = evaluate_takeover_reminder(&[blocking, idle_node("w2")], &json!({}))
        .expect("evaluate should succeed");
    assert!(
        !result.should_ping && result.reason.as_deref() == Some("node_working"),
        "A-2 green lock: any non-idle node blocks the ping with reason node_<state>; got {result:?}"
    );
}


// ---------------------------------------------------------------------------
// fixtures
// ---------------------------------------------------------------------------

fn idle_node(id: &str) -> IdleNode {
    IdleNode {
        node_id: id.to_string(),
        role: NodeRole::Worker,
        state: TurnState::Idle,
        turn_id: None,
        annotations: Vec::new(),
        provider: Some(Provider::Codex),
        auth_mode: None,
        rollout_path: None,
    }
}

fn tmp_ws(tag: &str) -> PathBuf {
    static N: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "ta-rs-med-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::canonicalize(dir).unwrap()
}
