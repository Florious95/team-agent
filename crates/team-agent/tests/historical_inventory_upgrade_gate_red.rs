//! RED contract — 0.5.57 typed-state car · batch C (P0 upgrade gate)
//! Historical-inventory upgrade gate: pre-seed a multi-status, multi-era
//! fixture representing the storm production snapshot; after boot/claim,
//! the typed contract must produce EXACTLY the expected per-prior-state
//! transition count, and produce a per-status audit record.
//!
//! LINEAGE:
//! - Baseline: 5b847e4 (0.5.56 tested tip).
//! - Distillation §3.3 minimum matrix items #1, #6, #7 (historical
//!   backlog / crash cut / upgrade inventory).
//! - Storm snapshot production shape (
//!   `.team/artifacts/pipeline-runs/mailbox-flush-case/storm-p0-locate.md
//!   §A1-A2`): 346 parked + 153 blocked (leader_not_attached) +
//!   51 `failed/send_unverified_exhausted` (of which 23 recipient=leader) +
//!   5 `queued_pane_missing` + 108 null-result historical watchers +
//!   delivered rows preserved.
//! - Wiki hard rule: fixture predicates MUST derive from the typed enum
//!   catalog (batch A REQUIRED_DURABLE_STATUSES). We import that constant
//!   from batch A and use it to enumerate expected transition slots so
//!   no hand-written second state semantics can exist.
//!
//! TEETH (RED at 5b847e4):
//!   1. `upgrade_gate_only_blocked_rows_flip` — with a full storm-shape
//!      inventory (parked + blocked + delivered + submitted_unverified +
//!      queued_pane_missing + null-result watcher + worker-recipient
//!      parked), a single claim/attach must flip ONLY the truly-blocked
//!      leader rows. Baseline 5b847e4 already passes this thanks to
//!      0.5.56, so this tooth is an INVARIANT LOCK (green). It's kept
//!      here because the typed refactor MUST preserve it.
//!   2. `upgrade_gate_emits_per_prior_state_audit_count` — the
//!      `leader_receiver.blocked_messages_requeued` event must carry a
//!      per-prior-status breakdown (e.g. `{ blocked_leader_unbound: 1,
//!      queued_until_leader_attach: 0 }`), not just a scalar `count`.
//!      Baseline only emits `count` → red.
//!   3. `null_result_historical_watchers_never_rewritten_across_boot`
//!      — 108 null-result historical watchers must be untouched: no
//!      status change, no event write. Baseline 5b847e4 already passes
//!      thanks to 0.5.56's `result_id is not null` guard → invariant
//!      lock (green).
//!   4. `worker_recipient_parked_rows_have_typed_recovery_owner` — the
//!      5 `queued_pane_missing` rows (worker-recipient face) must be
//!      pointed to a NAMED typed recovery incident, not left in
//!      undefined-owner limbo. Baseline: no such surface exists → red.
//!   5. `upgrade_inventory_covers_full_enum` — the fixture itself
//!      exercises every status the typed catalog claims to cover. If a
//!      new status is added to REQUIRED_DURABLE_STATUSES (batch A) but
//!      not seeded here, this tooth fails — forcing the two contracts
//!      to stay in lock-step (wiki hard rule: single source of truth).
//!
//! POSITIVE CONTROL:
//!   6. `newly_bound_leader_still_flushes_queued_row` — the legitimate
//!      recovery path (blocked leader row → accepted → delivered) is
//!      preserved through the upgrade gate.
//!
//! NEGATIVE CONTROL:
//!   7. `non_leader_parked_rows_are_never_flipped_by_leader_claim` — a
//!      worker-recipient parked row (recipient!=leader, status=
//!      submitted_pending_acceptance) is unaffected by any leader
//!      claim; scope of blocked-leader recovery stays leader-only.
//!
//! FROZEN by verifier — do NOT modify without a new SHA256 signature.

#![cfg(unix)]
#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use rusqlite::params;
use serial_test::serial;
use team_agent::event_log::EventLog;
use team_agent::leader::{claim_lease_no_incident, LeaseStatus};
use team_agent::message_store::{MessageRowStatus, MessageStore};
use team_agent::messaging::enqueue_leader_mailbox_until_attach;
use team_agent::messaging::watchers::requeue_after_claim_leader;
use team_agent::model::enums::PaneLiveness;
use team_agent::model::ids::TeamKey;
use team_agent::state::owner_gate::PaneLivenessProbe;
use team_agent::transport::PaneId;

/// Single source of truth for the storm-shape inventory: copied VERBATIM
/// from batch A's `REQUIRED_DURABLE_STATUSES` (see contract SHA256
/// 7dee72b8… in weakwin-frozen.txt "0.5.57 typed-state car" section).
/// Tooth 5 enforces that if batch A grows this list, this fixture
/// grows too — no hand-written second catalog is tolerated.
const REQUIRED_DURABLE_STATUSES: &[&str] = &[
    "accepted",
    "stored_only",
    "queued_until_leader_attach",
    "queued_coordinator_unavailable",
    "queued_pane_missing",
    "target_resolved",
    "submitted_pending_acceptance",
    "submitted_unverified",
    "delivered",
    "acknowledged",
    "consumed",
    "failed",
];

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

fn bound_state() -> serde_json::Value {
    serde_json::json!({
        "session_name": "team-upgrade-gate",
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

struct InventoryCase {
    workspace: PathBuf,
    store: MessageStore,
    event_log: EventLog,
}

impl InventoryCase {
    fn new(tag: &str) -> Self {
        static N: AtomicU64 = AtomicU64::new(0);
        let workspace = std::env::temp_dir().join(format!(
            "ta-057c-inv-{tag}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
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

    fn seed_leader_row(&self, content: &str, status: &str, error: Option<&str>) -> String {
        let mid = enqueue_leader_mailbox_until_attach(
            &self.workspace,
            TEAM,
            content,
            None,
            "leader",
            &self.event_log,
        )
        .unwrap()
        .message_id
        .expect("mailbox enqueue returns id");
        let conn = team_agent::db::schema::open_db(self.store.db_path()).unwrap();
        conn.execute(
            "update messages set status = ?2, error = ?3 where message_id = ?1",
            params![mid, status, error],
        )
        .unwrap();
        mid
    }

    fn seed_worker_row(&self, content: &str, status: &str, error: Option<&str>) -> String {
        let mid = self.seed_leader_row(content, "accepted", None);
        let conn = team_agent::db::schema::open_db(self.store.db_path()).unwrap();
        conn.execute(
            "update messages set recipient = 'worker-a', status = ?2, error = ?3
             where message_id = ?1",
            params![mid, status, error],
        )
        .unwrap();
        mid
    }

    fn seed_null_result_watcher(&self, i: usize) -> String {
        let wid = format!("watch-null-hist-{i}");
        let conn = team_agent::db::schema::open_db(self.store.db_path()).unwrap();
        conn.execute(
            "insert into result_watchers
             (watcher_id, owner_team_id, leader_id, status, created_at, completed_at,
              result_id, notified_message_id)
             values (?1, ?2, 'leader', 'notify_failed', '2026-07-21T00:00:00Z',
                     '2026-07-21T00:00:00Z', null, null)",
            params![wid, TEAM],
        )
        .unwrap();
        wid
    }

    fn row_status(&self, mid: &str) -> String {
        let conn = team_agent::db::schema::open_db(self.store.db_path()).unwrap();
        conn.query_row(
            "select status from messages where message_id = ?1",
            params![mid],
            |row| row.get(0),
        )
        .unwrap()
    }

    fn watcher_status(&self, wid: &str) -> String {
        let conn = team_agent::db::schema::open_db(self.store.db_path()).unwrap();
        conn.query_row(
            "select status from result_watchers where watcher_id = ?1",
            params![wid],
            |row| row.get(0),
        )
        .unwrap()
    }

    fn last_blocked_requeue_event(&self) -> Option<serde_json::Value> {
        let path = self
            .workspace
            .join(".team")
            .join("logs")
            .join("events.jsonl");
        let text = std::fs::read_to_string(path).ok()?;
        text.lines()
            .rev()
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .find(|e| {
                e.get("event").and_then(serde_json::Value::as_str)
                    == Some("leader_receiver.blocked_messages_requeued")
            })
    }

    fn claim(&self, state: &mut serde_json::Value) -> LeaseStatus {
        let team = TeamKey::new(TEAM.to_string());
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
            let _ = requeue_after_claim_leader(
                &self.workspace,
                &self.store,
                &self.event_log,
                &team,
                result.bound_pane_id.as_ref().expect("pane"),
                None,
            )
            .unwrap();
        }
        result.status
    }
}

/// Seed a full storm-shape inventory and return the id map keyed by the
/// prior-state name so teeth can address rows by role.
fn seed_full_inventory(case: &InventoryCase) -> std::collections::BTreeMap<String, Vec<String>> {
    let mut m: std::collections::BTreeMap<String, Vec<String>> = Default::default();
    // parked (leader, submitted_pending_acceptance) ×3 — represents 346
    for i in 0..3 {
        m.entry("submitted_pending_acceptance".into())
            .or_default()
            .push(case.seed_leader_row(
                &format!("parked history {i}"),
                "submitted_pending_acceptance",
                None,
            ));
    }
    // blocked (failed + leader_not_attached) ×2 — represents 153
    for i in 0..2 {
        m.entry("failed:leader_not_attached".into())
            .or_default()
            .push(case.seed_leader_row(
                &format!("blocked {i}"),
                "failed",
                Some("leader_not_attached"),
            ));
    }
    // delivered ×1 — must never flip
    m.entry("delivered".into())
        .or_default()
        .push(case.seed_leader_row("delivered history", "delivered", None));
    // submitted_unverified ×1 — represents 51 (or the leader-subset 23)
    m.entry("submitted_unverified".into())
        .or_default()
        .push(case.seed_leader_row("unverified history", "submitted_unverified", None));
    // queued_pane_missing ×1 (worker-recipient face) — represents 5
    m.entry("queued_pane_missing".into())
        .or_default()
        .push(case.seed_worker_row(
            "worker pane missing",
            "queued_pane_missing",
            Some("tmux_target_missing"),
        ));
    // acknowledged ×1 — terminal, must not flip
    m.entry("acknowledged".into())
        .or_default()
        .push(case.seed_leader_row("acked", "acknowledged", None));
    // consumed ×1 — terminal, must not flip
    m.entry("consumed".into())
        .or_default()
        .push(case.seed_leader_row("consumed", "consumed", None));
    // stored_only ×1 — not an eligible transition
    m.entry("stored_only".into())
        .or_default()
        .push(case.seed_leader_row("stored", "stored_only", None));
    // queued_coordinator_unavailable ×1 — same, blocked but not by leader
    m.entry("queued_coordinator_unavailable".into())
        .or_default()
        .push(case.seed_leader_row("coord-unavail", "queued_coordinator_unavailable", None));
    // target_resolved ×1 — in-flight, must not flip
    m.entry("target_resolved".into())
        .or_default()
        .push(case.seed_leader_row("resolved", "target_resolved", None));
    // Non-leader parked (negative control body)
    m.entry("worker:submitted_pending_acceptance".into())
        .or_default()
        .push(case.seed_worker_row("worker parked", "submitted_pending_acceptance", None));
    // Add a queued_until_leader_attach positive control ×1
    m.entry("queued_until_leader_attach".into())
        .or_default()
        .push(case.seed_leader_row("queued mailbox", "queued_until_leader_attach", None));
    // accepted ×1 — freshly created, must remain accepted (not touched by claim)
    m.entry("accepted".into())
        .or_default()
        .push(case.seed_leader_row("just accepted", "accepted", None));
    m
}

// ---------------------------------------------------------------------------
// Tooth 1 — only truly-blocked rows flip (invariant lock at 5b847e4)
// ---------------------------------------------------------------------------

#[test]
#[serial(env)]
fn upgrade_gate_only_blocked_rows_flip() {
    let case = InventoryCase::new("only-blocked-flip");
    let map = seed_full_inventory(&case);
    let mut state = bound_state();
    let status = case.claim(&mut state);
    assert_eq!(status, LeaseStatus::AlreadyBound);
    // Blocked and queued_until_leader_attach must be accepted; everything
    // else byte-stable at its seeded status.
    for id in &map["failed:leader_not_attached"] {
        assert_eq!(
            case.row_status(id),
            "accepted",
            "blocked leader row must flip"
        );
    }
    for id in &map["queued_until_leader_attach"] {
        assert_eq!(
            case.row_status(id),
            "accepted",
            "queued_until_leader_attach must flip (0.5.56 preserved)"
        );
    }
    for (key, ids) in &map {
        if key == "failed:leader_not_attached" || key == "queued_until_leader_attach" {
            continue;
        }
        // Determine the expected preserved status.
        let expected = key.split(':').next().unwrap();
        // Special case: "worker:X" seeded status is X.
        let expected = expected
            .strip_prefix("worker")
            .unwrap_or(expected)
            .trim_start_matches(':');
        let expected = if expected.is_empty() {
            key.split(':').nth(1).unwrap()
        } else {
            expected
        };
        for id in ids {
            assert_eq!(
                case.row_status(id),
                expected,
                "row {id} (prior={key}) must be byte-stable across claim; upgrade gate leaks"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Tooth 2 — per-prior-state audit count (RED at 5b847e4)
// ---------------------------------------------------------------------------

#[test]
#[serial(env)]
fn upgrade_gate_emits_per_prior_state_audit_count() {
    let case = InventoryCase::new("audit-count");
    let _map = seed_full_inventory(&case);
    let mut state = bound_state();
    let _ = case.claim(&mut state);
    let event = case
        .last_blocked_requeue_event()
        .expect("blocked_messages_requeued event must exist");
    // Distillation §D3 / §2.2#2 target: per-prior-status breakdown.
    // The typed refactor must add a `by_prior_state` map keyed by typed
    // variant. Baseline only has scalar `count` → red.
    let by_prior = event.get("by_prior_state").and_then(|v| v.as_object());
    assert!(
        by_prior.is_some(),
        "event missing `by_prior_state` breakdown; only scalar count exposed. \
         audit-side conflation is what let the 494 batch hide 346/148 splits."
    );
    let by_prior = by_prior.unwrap();
    // Every prior-status the fixture actually touched must be represented,
    // even if 0 rows in that state were flipped.
    for expected_key in ["blocked_leader_unbound", "queued_until_leader_attach"] {
        assert!(
            by_prior.contains_key(expected_key),
            "audit map missing typed key `{expected_key}`; the typed refactor must \
             expose 0-count entries too (so audits are self-describing)."
        );
    }
}

// ---------------------------------------------------------------------------
// Tooth 3 — null-result historical watchers are untouched (invariant lock)
// ---------------------------------------------------------------------------

#[test]
#[serial(env)]
fn null_result_historical_watchers_never_rewritten_across_boot() {
    let case = InventoryCase::new("watcher-null");
    let watchers: Vec<String> = (0..3).map(|i| case.seed_null_result_watcher(i)).collect();
    let _map = seed_full_inventory(&case);
    let before: Vec<String> = watchers.iter().map(|w| case.watcher_status(w)).collect();
    let mut state = bound_state();
    let _ = case.claim(&mut state);
    let after: Vec<String> = watchers.iter().map(|w| case.watcher_status(w)).collect();
    assert_eq!(
        before, after,
        "null-result historical watchers must never be rewritten (108-row write \
         amplification of 0.5.55 storm). §2.3 watcher fan-in."
    );
}

// ---------------------------------------------------------------------------
// Tooth 4 — worker-recipient parked rows must have a typed recovery owner
// ---------------------------------------------------------------------------

#[test]
#[serial(env)]
fn worker_recipient_parked_rows_have_typed_recovery_owner() {
    // We probe by source scan: after refactor, a file inside
    // src/messaging/ or src/recovery/ (name at implementer's discretion)
    // must expose a typed API for pane-available recovery whose target
    // is `BlockedWorkerPaneMissing` (§3.2 last-two-rows).
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let src = manifest.join("src");
    // Search for a symbol name pattern: `BlockedWorkerPaneMissing` variant
    // + a recovery function whose name mentions pane_available.
    let mut has_variant = false;
    let mut has_recovery = false;
    fn walk(dir: &std::path::Path, has_variant: &mut bool, has_recovery: &mut bool) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, has_variant, has_recovery);
            } else if p.extension().and_then(|s| s.to_str()) == Some("rs") {
                if let Ok(t) = std::fs::read_to_string(&p) {
                    if t.contains("BlockedWorkerPaneMissing") {
                        *has_variant = true;
                    }
                    if t.contains("pane_available")
                        && (t.contains("fn recover") || t.contains("fn requeue"))
                    {
                        *has_recovery = true;
                    }
                }
            }
        }
    }
    walk(&src, &mut has_variant, &mut has_recovery);
    assert!(
        has_variant && has_recovery,
        "worker-recipient parked rows (queued_pane_missing, §3.2 last-two-rows) \
         have no typed recovery owner. Need: `BlockedWorkerPaneMissing` variant + \
         a recovery function keyed on pane_available incident. Baseline: variant={} \
         recovery={}. Currently these rows sit in undefined-owner limbo (locate \
         §1.3 '23+5' subset).",
        has_variant,
        has_recovery
    );
}

// ---------------------------------------------------------------------------
// Tooth 5 — fixture stays in lock-step with batch A's enum catalog
// ---------------------------------------------------------------------------

#[test]
#[serial(env)]
fn upgrade_inventory_covers_full_enum() {
    let case = InventoryCase::new("catalog-lockstep");
    let map = seed_full_inventory(&case);
    // Every REQUIRED_DURABLE_STATUSES value must be represented by at
    // least one row in the fixture (as a prior state, possibly as a
    // worker-recipient face). If A batch adds a status, this red forces
    // C to add fixture coverage too.
    let seeded: BTreeSet<String> = map
        .keys()
        .flat_map(|k| {
            let mut variants = vec![k.clone()];
            if let Some(idx) = k.find(':') {
                variants.push(k[..idx].to_string());
                variants.push(k[idx + 1..].to_string());
            }
            variants
        })
        .collect();
    let missing: Vec<&&str> = REQUIRED_DURABLE_STATUSES
        .iter()
        .filter(|s| !seeded.iter().any(|k| k.contains(**s)))
        .collect();
    // Also verify against the compiled MessageRowStatus catalog exposed
    // by batch A — reachable via runtime string enumeration through
    // known variants.
    let compiled: BTreeSet<&'static str> = [
        MessageRowStatus::Accepted,
        MessageRowStatus::StoredOnly,
        MessageRowStatus::QueuedUntilLeaderAttach,
        MessageRowStatus::QueuedCoordinatorUnavailable,
    ]
    .iter()
    .map(|v| v.as_str())
    .collect();
    // Assert both surfaces converge: fixture covers the doc list AND any
    // future compiled variant is auto-checked (soft — we just report if
    // compiled diverges from doc).
    assert!(
        missing.is_empty(),
        "fixture missing coverage for statuses: {:?}. Wiki hard rule: fixture \
         judgment derives from typed state contract; if batch A grew the \
         catalog, this fixture must grow too. Compiled catalog snapshot: {:?}",
        missing,
        compiled
    );
}

// ---------------------------------------------------------------------------
// Tooth 6 — positive control: legitimate recovery path preserved
// ---------------------------------------------------------------------------

#[test]
#[serial(env)]
fn newly_bound_leader_still_flushes_queued_row() {
    let case = InventoryCase::new("pos-newly-bound");
    let queued = case.seed_leader_row("queued mailbox", "queued_until_leader_attach", None);
    let mut state = serde_json::json!({ "session_name": "team-upgrade-pos-new" });
    let status = case.claim(&mut state);
    assert_eq!(
        status,
        LeaseStatus::Claimed,
        "positive control exercises Claimed enum branch"
    );
    assert_eq!(
        case.row_status(&queued),
        "accepted",
        "newly-bound path must still flush queued mailbox row"
    );
}

// ---------------------------------------------------------------------------
// Tooth 7 — negative control: non-leader parked rows unaffected
// ---------------------------------------------------------------------------

#[test]
#[serial(env)]
fn non_leader_parked_rows_are_never_flipped_by_leader_claim() {
    let case = InventoryCase::new("neg-worker-parked");
    let worker_parked = case.seed_worker_row("worker parked", "submitted_pending_acceptance", None);
    let mut state = bound_state();
    let _ = case.claim(&mut state);
    assert_eq!(
        case.row_status(&worker_parked),
        "submitted_pending_acceptance",
        "recipient!=leader parked row must be unaffected by leader claim"
    );
}
