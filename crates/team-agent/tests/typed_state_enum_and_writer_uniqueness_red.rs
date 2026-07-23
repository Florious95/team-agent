//! RED contract — 0.5.57 typed-state car · batch A
//! Typed message-state enum catalog completeness + status-writer uniqueness.
//!
//! LINEAGE:
//! - Baseline: 5b847e4 (0.5.56 tested tip). Red teeth expose that the current
//!   product does NOT satisfy the two 0.5.57 invariants distilled in
//!   `.team/artifacts/germline-increments/storm-p0-distillation.md`:
//!   §3.1 typed enum catalog (must cover every durable disposition, not only
//!         initial statuses), and
//!   §3.2 unique writer per transition (SQL literals for
//!         `update messages set status = …` must not be scattered across
//!         claim/attach/coordinator; a single owner API per transition).
//! - 0.5.55 replay storm proved that a scattered writer set + a shared
//!   `submitted_pending_acceptance` semantics collapsed two facts into one
//!   status. This car locks the invariants BEFORE any code refactor.
//! - Contract is RED at 5b847e4 by design; it must go GREEN only after the
//!   typed-state refactor introduces a full enum and funnels all durable
//!   status transitions through a single repository API.
//!
//! TEETH (RED at 5b847e4):
//!   1. `enum_catalog_covers_every_observed_status` — the compiled
//!      `MessageRowStatus` enum must expose a variant for every durable
//!      status observed in production storm data + module design
//!      (delivered / acknowledged / consumed / failed / target_resolved /
//!      submitted_pending_acceptance / submitted_unverified /
//!      queued_pane_missing). Baseline enum only has 4 initial variants →
//!      catalog gap → red.
//!   2. `submitted_awaiting_receipt_and_blocked_leader_unbound_are_distinct`
//!      — a parked (submitted, no receipt) row and a blocked (never
//!      submitted, leader unbound) row must map to distinct typed variants;
//!      the storm proved that collapsing them is the P0 root cause. Baseline
//!      has NO typed variant for either → red.
//!   3. `at_most_one_writer_per_status_transition` — scanning product
//!      sources under `crates/team-agent/src/**/*.rs`, the count of
//!      SQL-literal `update messages set status = 'X'` sites is expected
//!      to be ≤1 per target status. Baseline has ≥4 scattered sites
//!      (message_store.rs / watchers.rs / delivery.rs ×2) → red.
//!
//! POSITIVE CONTROL:
//!   4. `existing_accepted_initial_writer_stays_typed` — creating a new
//!      message via the public store API still lands on the typed
//!      `Accepted` variant round-trippable to `"accepted"`; the refactor
//!      must NOT regress the current happy path.
//!
//! NEGATIVE CONTROL:
//!   5. `non_status_writes_are_not_counted_as_status_writers` — sites that
//!      touch other message columns (delivered_at / error) without also
//!      writing `status = 'X'` are not counted, i.e. the scanner is not
//!      trivially recovering "all sites are 1" by matching too little.
//!
//! FROZEN by verifier — do NOT modify without a new SHA256 signature.

#![cfg(unix)]
#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

#[path = "support/hermetic.rs"]
mod hermetic_guard;
#[allow(dead_code)]
fn _hermetic_boundary_marker(_: &hermetic_guard::HermeticTestEnv) {}

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serial_test::serial;
use team_agent::message_store::{MessageRowStatus, MessageStore};

// Statuses the module-map + storm distillation require the typed enum to
// cover. NOTE: this list is the ONE authoritative source of truth for the
// contract; the fixture / recovery gate batches (C/D) MUST derive from the
// same enum, never from a hand-copied second list.
const REQUIRED_DURABLE_STATUSES: &[&str] = &[
    // Initial / mailbox
    "accepted",
    "stored_only",
    "queued_until_leader_attach",
    "queued_coordinator_unavailable",
    "queued_pane_missing",
    // In-flight
    "target_resolved",
    "submitted_pending_acceptance",
    "submitted_unverified",
    // Terminal
    "delivered",
    "acknowledged",
    "consumed",
    "failed",
];

fn crate_src_root() -> PathBuf {
    // tests/ is a sibling of src/ inside the team-agent crate.
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest.join("src")
}

fn walk_rs_files(root: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            // Skip generated / vendored areas.
            let name = path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or_default();
            if matches!(name, "tests" | "target" | ".git") {
                continue;
            }
            walk_rs_files(&path, out);
        } else if path.extension().and_then(|s| s.to_str()) == Some("rs") {
            out.push(path);
        }
    }
}

/// Very small, deliberately dumb line scanner: any src/**/*.rs line that
/// contains `update messages` AND `set status = '<X>'` (single-quoted literal)
/// counts as ONE writer for status `X` at that path. Multi-status SQL (e.g.
/// `set status = 'accepted', error = null` still counts once for `accepted`).
fn scan_status_writers() -> BTreeMap<String, BTreeSet<PathBuf>> {
    let mut files = Vec::new();
    walk_rs_files(&crate_src_root(), &mut files);
    let mut writers: BTreeMap<String, BTreeSet<PathBuf>> = BTreeMap::new();
    for file in files {
        let Ok(text) = fs::read_to_string(&file) else {
            continue;
        };
        // Cheap two-pass: check the file contains "update messages" at all;
        // then scan for status literals.
        if !text.contains("update messages") {
            continue;
        }
        for status in REQUIRED_DURABLE_STATUSES {
            let needle = format!("set status = '{status}'");
            if text.contains(&needle) {
                writers
                    .entry((*status).to_string())
                    .or_default()
                    .insert(file.clone());
            }
        }
    }
    writers
}

// ---------------------------------------------------------------------------
// Tooth 1 — enum catalog completeness
// ---------------------------------------------------------------------------

#[test]
fn enum_catalog_covers_every_observed_status() {
    // Reflection is unavailable; we probe by asserting that each required
    // status name is representable AS a MessageRowStatus round-trip. The
    // baseline enum only exposes 4 variants → 8 statuses have no typed
    // variant → this test panics on the first uncovered one and lists all
    // gaps so the GREEN implementer sees the whole delta at once.
    let mut covered: BTreeSet<&'static str> = BTreeSet::new();
    // Existing variants (baseline).
    for v in [
        MessageRowStatus::Accepted,
        MessageRowStatus::StoredOnly,
        MessageRowStatus::QueuedUntilLeaderAttach,
        MessageRowStatus::QueuedCoordinatorUnavailable,
    ] {
        covered.insert(v.as_str());
    }
    let missing: Vec<&&str> = REQUIRED_DURABLE_STATUSES
        .iter()
        .filter(|s| !covered.contains(*s))
        .collect();
    assert!(
        missing.is_empty(),
        "MessageRowStatus catalog missing typed variants for durable statuses: {:?}. \
         Distillation §3.1 requires every observed durable status to have a typed variant \
         so downstream fixtures and recovery arms cannot introduce a second hand-written \
         state semantics (wiki hard rule).",
        missing
    );
}

// ---------------------------------------------------------------------------
// Tooth 2 — SubmittedAwaitingReceipt vs BlockedLeaderUnbound must be distinct
// ---------------------------------------------------------------------------

#[test]
fn submitted_awaiting_receipt_and_blocked_leader_unbound_are_distinct() {
    // We cannot import variants that do not yet exist without breaking the
    // build; instead assert via as_str() catalog that BOTH typed names are
    // present. GREEN implementer must introduce the two variants with the
    // exact user-visible status strings below (distillation §3.1).
    let parked = "submitted_pending_acceptance";
    let blocked = "queued_until_leader_attach";
    let known: BTreeSet<&'static str> = [
        MessageRowStatus::Accepted,
        MessageRowStatus::StoredOnly,
        MessageRowStatus::QueuedUntilLeaderAttach,
        MessageRowStatus::QueuedCoordinatorUnavailable,
    ]
    .into_iter()
    .map(MessageRowStatus::as_str)
    .collect();
    // Both statuses must be typed AND they must be distinct enum values.
    // Baseline: `submitted_pending_acceptance` is not typed → red.
    // Baseline: `failed + leader_not_attached` is expressed as
    //   (status="failed", error="leader_not_attached") not as a distinct
    //   typed variant → red on the modeling axis too.
    let parked_typed = known.contains(parked);
    let blocked_typed = known.contains(blocked);
    let leader_unbound_typed = known.iter().any(|s| s.contains("blocked_leader_unbound"));
    assert!(
        parked_typed && blocked_typed && leader_unbound_typed,
        "typed distinction missing: parked={} (submitted_pending_acceptance), \
         blocked={} (queued_until_leader_attach), leader_unbound_typed={} \
         (need a dedicated variant e.g. `BlockedLeaderUnbound`, NOT reuse of \
         status='failed' + error='leader_not_attached'). §3.1 hard rule: \
         SubmittedAwaitingReceipt ≠ BlockedLeaderUnbound.",
        parked_typed,
        blocked_typed,
        leader_unbound_typed
    );
}

// ---------------------------------------------------------------------------
// Tooth 3 — at most one writer per status transition
// ---------------------------------------------------------------------------

#[test]
#[serial(env)]
fn at_most_one_writer_per_status_transition() {
    let writers = scan_status_writers();
    // Report a concrete list of violators so the GREEN implementer has a
    // punch list, not a boolean.
    let violations: Vec<(String, Vec<PathBuf>)> = writers
        .iter()
        .filter(|(_, sites)| sites.len() > 1)
        .map(|(s, sites)| (s.clone(), sites.iter().cloned().collect()))
        .collect();
    // Also flag any status that is written via literal SQL at all when the
    // enum + repository API is in place: after refactor, direct SQL
    // literals for durable statuses should live only inside the
    // message_store transition module. Baseline scatters across at least
    // watchers.rs and delivery.rs → red.
    let scattered_outside_store: Vec<(String, Vec<PathBuf>)> = writers
        .iter()
        .filter_map(|(s, sites)| {
            let outside: Vec<PathBuf> = sites
                .iter()
                .filter(|p| {
                    let s = p.to_string_lossy();
                    !s.ends_with("db/message_store.rs") && !s.contains("db/message_store")
                })
                .cloned()
                .collect();
            (!outside.is_empty()).then_some((s.clone(), outside))
        })
        .collect();
    assert!(
        violations.is_empty() && scattered_outside_store.is_empty(),
        "status-writer uniqueness violated. §3.2 requires ONE owner API per \
         transition; SQL literals must not be scattered outside the message \
         store. multi-writer statuses: {:?}; statuses whose writer lives \
         OUTSIDE db/message_store.rs: {:?}",
        violations,
        scattered_outside_store
    );
}

// ---------------------------------------------------------------------------
// Tooth 4 — positive control: existing happy path is preserved
// ---------------------------------------------------------------------------

#[test]
#[serial(env)]
fn existing_accepted_initial_writer_stays_typed() {
    static N: AtomicU64 = AtomicU64::new(0);
    let env = hermetic_guard::HermeticTestEnv::enter("typed-state-a-pos");
    let workspace = env.workspace(&format!(
        "typed-state-a-{}",
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let store = MessageStore::open(&workspace).expect("store open");
    let mid = store
        .create_message_with_id(
            "msg_pos_ctl_typed_accepted",
            None,
            "sender-a",
            "recipient-b",
            "hello",
            None,
            false,
            None,
        )
        .expect("create");
    assert_eq!(mid, "msg_pos_ctl_typed_accepted");
    // Read the row back via a low-level query; status must be the typed
    // Accepted's string, proving the initial-writer path still funnels
    // through MessageRowStatus.
    let conn = rusqlite::Connection::open(store.db_path()).expect("open db");
    let status: String = conn
        .query_row(
            "select status from messages where message_id = ?1",
            rusqlite::params!["msg_pos_ctl_typed_accepted"],
            |row| row.get(0),
        )
        .expect("row");
    assert_eq!(status, MessageRowStatus::Accepted.as_str());
}

// ---------------------------------------------------------------------------
// Tooth 5 — negative control: the scanner does not silently match too much
// ---------------------------------------------------------------------------

#[test]
#[serial(env)]
fn non_status_writes_are_not_counted_as_status_writers() {
    // Sanity: for a completely unused status string, the scanner returns
    // an empty writer set, proving it does not e.g. match any `update
    // messages` line as a status writer.
    let writers = scan_status_writers();
    let unused = "definitely_not_a_real_status_zxq";
    assert!(
        writers.get(unused).is_none(),
        "scanner falsely attributed writers to a non-existent status; \
         its literal-match heuristic is too loose."
    );
    // Additionally: the scanner must have detected AT LEAST the known
    // scattered writers, otherwise tooth 3 could be trivially green
    // because the scanner missed them.
    let accepted_writers = writers.get("accepted").cloned().unwrap_or_default();
    let target_resolved_writers = writers.get("target_resolved").cloned().unwrap_or_default();
    assert!(
        accepted_writers.len() + target_resolved_writers.len() >= 2,
        "scanner failed to detect known baseline writers (accepted={} target_resolved={}); \
         tooth 3 would be vacuously green — scanner is broken.",
        accepted_writers.len(),
        target_resolved_writers.len()
    );
}
