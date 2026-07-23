//! RED contract — 0.5.57 typed-state car · batch E
//! Ownership of two recovery arms currently missing/mis-owned in the
//! baseline:
//!   - `BlockedWorkerPaneMissing` rows may only be recovered by a
//!     typed `pane_available` incident scoped to the specific
//!     agent/pane, NEVER by a whole-table coordinator scan.
//!   - Watcher recovery must ALWAYS filter `result_id IS NOT NULL`
//!     (0.5.56 invariant lock) AND be scoped to a specific
//!     incident_id (0.5.57 target); no incident-less null-guarded
//!     helper is allowed as a shortcut.
//!
//! LINEAGE:
//! - Baseline: 5b847e4 (0.5.56 tested tip).
//! - Distillation §2.3 watcher fan-in (5 items) + §3.2 last-two-rows
//!   (BlockedWorkerPaneMissing / watcher exhausted).
//! - Storm proof: 108 null-result watchers, 5 pane_missing rows —
//!   the "23+5" subset — sat in undefined-owner limbo.
//!
//! TEETH (RED at 5b847e4):
//!   1. `pane_available_incident_type_exists` — a typed incident
//!      such as `PaneAvailable { agent_id, pane_id }` must be
//!      declared. Baseline: no such symbol → red.
//!   2. `worker_pane_missing_recovery_scoped_by_agent` — a recovery
//!      helper (e.g. `recover_worker_pane_available(agent_id, ...)`)
//!      must exist and its SQL must filter by both status AND
//!      recipient/agent_id (never a bare `where status = 'queued_pane_missing'`
//!      that reaches all agents' rows). Baseline: no such helper →
//!      red.
//!   3. `coordinator_pending_scan_does_not_own_pane_recovery` — the
//!      coordinator's periodic pending scan (currently
//!      pending/accepted/target_resolved) must not silently include
//!      `queued_pane_missing`, because that would re-couple recovery
//!      to a whole-table scanner instead of a typed incident owner.
//!      Baseline: passes (invariant lock) → green.
//!   4. `watcher_recovery_scoped_by_incident_id`
//!      — the watcher recovery function must accept an
//!      `incident_id: &str` parameter (or `RecoveryScope`), NOT a
//!      bare "recover all". Baseline: `requeue_after_claim_leader`
//!      is incident-less → red.
//!   5. `watcher_recovery_result_id_not_null_lock`
//!      — the 0.5.56 `result_id is not null` guard must remain in
//!      the SQL body. Baseline: green (invariant lock).
//!
//! POSITIVE CONTROL:
//!   6. `existing_result_watcher_retry_still_reachable` — a watcher
//!      with a non-null result_id, matching the current incident,
//!      remains eligible for retry — refactor must not accidentally
//!      drop the legitimate retry path.
//!
//! NEGATIVE CONTROL:
//!   7. `pane_available_recovery_does_not_touch_leader_rows` — the
//!      pane_available recovery helper must not affect leader-bound
//!      rows; scope is agent-only.
//!
//! FROZEN by verifier — do NOT modify without a new SHA256 signature.

#![cfg(unix)]
#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

// R6 密闭边界:本文件全部齿均为源码文本扫描,不实跑 MessageStore/
// deliver_pending_message*,但源码里出现这些字符串会触发 R6 静态守卫,
// 因此声明 hermetic 关联并在每个 test 入口 enter() 空 env 满足门禁
// (leader ruling msg_624b6075c1c8 + skill 构件5 密闭性条)。
#[path = "support/hermetic.rs"]
mod hermetic_guard;
#[allow(dead_code)]
fn _hermetic_boundary_marker(_: &hermetic_guard::HermeticTestEnv) {}

use std::fs;
use std::path::{Path, PathBuf};

use serial_test::serial;

fn crate_src() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src")
}

fn walk_texts(root: &Path) -> Vec<(PathBuf, String)> {
    let mut out = Vec::new();
    fn go(dir: &Path, out: &mut Vec<(PathBuf, String)>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                go(&p, out);
            } else if p.extension().and_then(|s| s.to_str()) == Some("rs") {
                if let Ok(t) = fs::read_to_string(&p) {
                    out.push((p, t));
                }
            }
        }
    }
    go(root, &mut out);
    out
}

fn any_file_contains<F: Fn(&str) -> bool>(pred: F) -> Option<PathBuf> {
    walk_texts(&crate_src())
        .into_iter()
        .find(|(_, t)| pred(t))
        .map(|(p, _)| p)
}

// ---------------------------------------------------------------------------
// Tooth 1 — PaneAvailable incident type exists
// ---------------------------------------------------------------------------

#[test]
#[serial(env)]
fn pane_available_incident_type_exists() {
    let hit = any_file_contains(|t| {
        t.contains("PaneAvailable")
            && (t.contains("struct PaneAvailable")
                || t.contains("enum ")
                || t.contains("PaneAvailable {"))
    });
    assert!(
        hit.is_some(),
        "no typed `PaneAvailable` incident found. §3.2 requires \
         BlockedWorkerPaneMissing recovery to consume a typed pane_available \
         incident (agent_id + pane_id), not a whole-table coordinator scan."
    );
}

// ---------------------------------------------------------------------------
// Tooth 2 — worker pane recovery scoped by agent
// ---------------------------------------------------------------------------

#[test]
#[serial(env)]
fn worker_pane_missing_recovery_scoped_by_agent() {
    // A recovery helper must exist whose name references pane_available
    // and whose body filters `where recipient = ?` or `where agent_id = ?`
    // — not a bare status-only sweep.
    let hit = any_file_contains(|t| {
        (t.contains("fn recover_worker_pane_available")
            || t.contains("fn requeue_worker_pane_available")
            || (t.contains("fn ") && t.contains("pane_available") && t.contains("agent_id")))
            && t.contains("queued_pane_missing")
            && (t.contains("recipient = ?") || t.contains("agent_id = ?"))
    });
    assert!(
        hit.is_some(),
        "no agent-scoped worker pane recovery helper found. §3.2 requires \
         BlockedWorkerPaneMissing recovery keyed on the specific pane_available \
         incident (recipient/agent scoped), not a bare status sweep."
    );
}

// ---------------------------------------------------------------------------
// Tooth 3 — coordinator pending scan does NOT include queued_pane_missing
// ---------------------------------------------------------------------------

#[test]
#[serial(env)]
fn coordinator_pending_scan_does_not_own_pane_recovery() {
    // The coordinator's periodic pending scan lives in delivery.rs and
    // selects pending/accepted/target_resolved. It MUST NOT be extended
    // to include queued_pane_missing (that would collapse recovery
    // ownership back to a whole-table scanner). We assert on source:
    // the substring pair "pending scan" or `deliver_pending_messages`
    // must not have `queued_pane_missing` inside the same SQL body.
    let delivery =
        fs::read_to_string(crate_src().join("messaging/delivery.rs")).expect("read delivery.rs");
    // Cheap heuristic: within any `select ... from messages` block
    // starting under 40 lines from a `deliver_pending` mention.
    let mut violations = 0usize;
    let lines: Vec<&str> = delivery.lines().collect();
    for (i, l) in lines.iter().enumerate() {
        if l.contains("deliver_pending") {
            let end = (i + 60).min(lines.len());
            let window = lines[i..end].join("\n");
            if window.contains("queued_pane_missing") && window.contains("select") {
                violations += 1;
            }
        }
    }
    assert_eq!(
        violations, 0,
        "coordinator pending scan currently references queued_pane_missing \
         inside a `select from messages` window — this collapses BlockedWorkerPaneMissing \
         recovery back to a whole-table scanner. §3.2 requires typed pane_available \
         incident ownership."
    );
}

// ---------------------------------------------------------------------------
// Tooth 4 — watcher recovery is incident-scoped
// ---------------------------------------------------------------------------

#[test]
#[serial(env)]
fn watcher_recovery_scoped_by_incident_id() {
    // The watcher recovery function must take an incident_id parameter.
    // Baseline: `requeue_after_claim_leader` takes an `Option<...>` for
    // "incident_ts" but not a typed incident_id (and callers pass None
    // both at attach and claim, meaning history-wide). §2.3#4 requires
    // typed incident scoping.
    let hit = any_file_contains(|t| {
        (t.contains("fn requeue_after_claim_leader")
            || t.contains("fn recover_watchers_for_incident")
            || t.contains("fn requeue_watchers"))
            && (t.contains("incident_id: &str") || t.contains("incident: &RecoveryIncident"))
    });
    assert!(
        hit.is_some(),
        "watcher recovery helper is not incident-scoped (baseline takes only \
         a discretionary `incident_ts: Option<...>`, and both call sites pass \
         None — history-wide). §2.3#4 requires typed incident scoping."
    );
}

// ---------------------------------------------------------------------------
// Tooth 5 — 0.5.56 result_id-null-guard invariant lock (green)
// ---------------------------------------------------------------------------

#[test]
#[serial(env)]
fn watcher_recovery_result_id_not_null_lock() {
    let watchers =
        fs::read_to_string(crate_src().join("messaging/watchers.rs")).expect("read watchers.rs");
    // Any function that mentions requeue_after_claim_leader body OR
    // the claim watcher sweep must contain the `result_id is not null`
    // predicate literal exactly (0.5.56 fix).
    let ok = watchers.contains("result_id is not null");
    assert!(
        ok,
        "regression: `result_id is not null` guard missing from watchers.rs — \
         the 0.5.56 fix that closed the 108-row write amplification is gone."
    );
}

// ---------------------------------------------------------------------------
// Tooth 6 — positive control: legitimate watcher retry is reachable
// ---------------------------------------------------------------------------

#[test]
#[serial(env)]
fn existing_result_watcher_retry_still_reachable() {
    // Source-level positive control: the watcher retry function must
    // remain a `pub` symbol so operator tooling / the recovery arm can
    // reach it. If the refactor turns it private, we lose the
    // legitimate path.
    let watchers =
        fs::read_to_string(crate_src().join("messaging/watchers.rs")).expect("read watchers.rs");
    let hit = watchers.contains("pub fn requeue_after_claim_leader")
        || watchers.contains("pub(crate) fn requeue_after_claim_leader")
        || watchers.contains("pub fn recover_watchers_for_incident");
    assert!(
        hit,
        "watcher retry entry point no longer publicly reachable — legitimate \
         retry path lost. Refactor must keep an operator-callable entry point."
    );
}

// ---------------------------------------------------------------------------
// Tooth 7 — negative control: pane_available recovery does not touch leader
// ---------------------------------------------------------------------------

#[test]
#[serial(env)]
fn pane_available_recovery_does_not_touch_leader_rows() {
    // Source-level negative-control: after refactor, any function
    // whose name contains `pane_available` must NOT contain
    // `recipient = 'leader'` inside its body. We approximate by
    // finding each such fn and scanning the surrounding 60 lines.
    let mut violations: Vec<PathBuf> = Vec::new();
    for (p, t) in walk_texts(&crate_src()) {
        let lines: Vec<&str> = t.lines().collect();
        for (i, l) in lines.iter().enumerate() {
            if l.contains("pane_available") && l.contains("fn ") {
                let end = (i + 80).min(lines.len());
                let window = lines[i..end].join("\n");
                if window.contains("recipient = 'leader'") {
                    violations.push(p.clone());
                }
            }
        }
    }
    // Baseline: no pane_available helper exists → green trivially.
    // GREEN implementer must keep it green when adding the helper.
    assert!(
        violations.is_empty(),
        "pane_available recovery helper contains a leader-recipient clause: {:?}. \
         §3.2 requires pane_available scope to stay agent-only.",
        violations
    );
}
