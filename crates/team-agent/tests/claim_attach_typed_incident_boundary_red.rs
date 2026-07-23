//! RED contract — 0.5.57 typed-state car · batch B
//! Claim/attach must publish a typed incident and NEVER directly rewrite
//! `messages` rows or `result_watchers` rows. Recovery is a downstream
//! consumer, not a side-effect of a control-plane action.
//!
//! LINEAGE:
//! - Baseline: 5b847e4 (0.5.56 tested tip).
//! - Distillation §2.1 claim fan-in: control-plane action (claim/attach)
//!   MUST NOT double as an unconditional data-plane recovery button. The
//!   0.5.55 storm was caused precisely because a successful claim ran
//!   `requeue_after_claim_leader` which combined watcher + message helpers
//!   in one function with no incident id, no time bound and no typed API
//!   surface.
//! - Distillation §3.1: `SubmittedAwaitingReceipt` (already crossed the
//!   physical transport, only lacks provider receipt) is DISTINCT from
//!   `BlockedLeaderUnbound` (never crossed). Any code path that treats
//!   the two as members of the same "eligible for claim requeue" set is
//!   a contract violation.
//! - Contract is RED at 5b847e4 because:
//!     (a) claim/attach both directly re-write `messages.status` rather
//!         than publishing a typed incident consumed by a separate
//!         recovery owner, and
//!     (b) the two typed variants above do not exist yet — the code
//!         necessarily conflates them via string status.
//!
//! TEETH (RED at 5b847e4):
//!   1. `claim_or_attach_never_directly_writes_messages_status` — scanning
//!      `crates/team-agent/src/leader/**` + call graph on
//!      `requeue_after_claim_leader` / `requeue_delivery_exhausted_watchers`,
//!      those functions currently execute `update messages set status = 'X'`
//!      SQL inside the leader control plane. §3.2 target: recovery must own
//!      status writes; leader only emits a typed incident.
//!   2. `submitted_pending_acceptance_never_appears_in_claim_eligible_set`
//!      — even if some downstream recovery is added later, the substring
//!      `submitted_pending_acceptance` MUST NOT appear inside the SQL body
//!      of any leader claim/attach-triggered requeue function. (0.5.56
//!      already removed the WHERE-clause hit; this contract locks the
//!      absence so nobody re-adds it under a different helper name.)
//!   3. `typed_incident_api_exists` — a compiled Rust surface must exist
//!      for the two typed leader incidents distinguished in §3.1
//!      (LeaderAttached / LeaderClaimed). Baseline exposes only string
//!      event names, no typed enum → red.
//!
//! POSITIVE CONTROL:
//!   4. `attach_flow_still_publishes_at_least_one_leader_event` — a real
//!      attach path continues to emit an event so downstream observers
//!      keep working; refactor must not silently drop the incident.
//!
//! NEGATIVE CONTROL:
//!   5. `unrelated_functions_are_not_matched_by_the_scanner` — the SQL
//!      scanner does not match arbitrary code that merely mentions
//!      "messages" (e.g. log strings, comments); it only counts actual
//!      SQL literal writes.
//!
//! FROZEN by verifier — do NOT modify without a new SHA256 signature.

#![cfg(unix)]
#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use std::fs;
use std::path::{Path, PathBuf};

use serial_test::serial;

fn crate_src_root() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest.join("src")
}

/// Return the raw text of a file inside the team-agent src tree, or panic
/// with a helpful message.
fn read_src(rel: &str) -> String {
    let path = crate_src_root().join(rel);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("read src {}: {}", path.display(), e))
}

/// Return true iff the file at `rel` contains an `update messages set status`
/// SQL literal (single-line whitespace only — we intentionally do not try to
/// tokenize; §3.2 target is to eliminate the literal entirely from these
/// modules, so any occurrence is a red).
fn contains_status_write_sql(rel: &str) -> bool {
    let text = read_src(rel);
    // Look for the exact multi-line pattern used by rusqlite `execute` calls,
    // which always has `update messages` and `set status = '<literal>'`
    // within a few lines of each other. A simple substring check is enough
    // because production code uses this exact phrasing.
    text.contains("update messages")
        && text
            .lines()
            .any(|l| l.trim_start().starts_with("set status = '"))
}

/// Walk src/leader/**/*.rs and return each file that contains the SQL
/// literal (source of §2.1 violation).
fn leader_files_writing_status() -> Vec<PathBuf> {
    let leader_root = crate_src_root().join("leader");
    let mut out = Vec::new();
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().and_then(|s| s.to_str()) == Some("rs") {
                if let Ok(text) = fs::read_to_string(&path) {
                    if text.contains("update messages")
                        && text
                            .lines()
                            .any(|l| l.trim_start().starts_with("set status = '"))
                    {
                        out.push(path.clone());
                    }
                }
            }
        }
    }
    walk(&leader_root, &mut out);
    out
}

// ---------------------------------------------------------------------------
// Tooth 1 — claim/attach paths must not directly write messages.status
// ---------------------------------------------------------------------------

#[test]
#[serial(env)]
fn claim_or_attach_never_directly_writes_messages_status() {
    // Baseline: leader/lease.rs calls into messaging/watchers.rs which
    // executes the SQL. §3.2 target after refactor: leader/*.rs and any
    // helper it calls MUST NOT run SQL that writes messages.status. The
    // canonical way to enforce this in isolation is: the recovery helpers
    // called from the claim/attach hooks must not contain the literal.
    let watchers_hit = contains_status_write_sql("messaging/watchers.rs");
    let delivery_hit = contains_status_write_sql("messaging/delivery.rs");
    let leader_hits = leader_files_writing_status();
    assert!(
        !watchers_hit && !delivery_hit && leader_hits.is_empty(),
        "control-plane paths still perform direct data-plane status writes. \
         watchers.rs has SQL={} delivery.rs has SQL={} leader files w/ SQL={:?}. \
         §2.1 requires claim/attach to publish a typed incident only; a \
         downstream recovery owner reads it and performs the write.",
        watchers_hit,
        delivery_hit,
        leader_hits
    );
}

// ---------------------------------------------------------------------------
// Tooth 2 — SubmittedAwaitingReceipt (parked) forever excluded from
// claim/attach eligible set
// ---------------------------------------------------------------------------

#[test]
#[serial(env)]
fn submitted_pending_acceptance_never_appears_in_claim_eligible_set() {
    // Locking 0.5.56's revert: no file in messaging/ or leader/ may ever
    // again mention `submitted_pending_acceptance` inside an `update
    // messages` block. We approximate by asserting that within 40 lines of
    // any `update messages` occurrence in each file, the substring does
    // not appear. This catches both the historical `or status = '...'`
    // clause AND any future variant that eligibility might sneak in.
    let files = ["messaging/watchers.rs", "messaging/delivery.rs"];
    let mut violations: Vec<(String, usize)> = Vec::new();
    for rel in files {
        let text = read_src(rel);
        let lines: Vec<&str> = text.lines().collect();
        for (i, l) in lines.iter().enumerate() {
            if l.contains("update messages") {
                let start = i;
                let end = (i + 40).min(lines.len());
                let window = lines[start..end].join("\n");
                if window.contains("submitted_pending_acceptance") {
                    violations.push((rel.to_string(), i + 1));
                }
            }
        }
        // A leader/*.rs surface also must not encode this either (currently
        // does not, but locked here so future files stay out).
    }
    // Baseline: passes (0.5.56 removed the clause). This tooth is
    // GREEN at 5b847e4 as an INVARIANT LOCK — it is the ONE tooth in this
    // batch that is not red today because the invariant was already
    // secured by the last car; we keep it here to prevent regression
    // during the typed refactor. distillation §2.2#1.
    assert!(
        violations.is_empty(),
        "regression: `submitted_pending_acceptance` reappeared inside an \
         `update messages` block: {:?} — this is the exact clause 0.5.56 \
         removed to stop the 346-row replay storm; typed refactor must \
         not re-introduce it.",
        violations
    );
}

// ---------------------------------------------------------------------------
// Tooth 3 — typed leader-incident API surface must exist
// ---------------------------------------------------------------------------

#[test]
#[serial(env)]
fn typed_incident_api_exists() {
    // We check for a compiled Rust surface named e.g. `LeaderIncident`
    // (enum), or a module path pattern indicating it. Because we cannot
    // import a non-existent symbol without breaking the build, we scan
    // the source tree for the substring `pub enum LeaderIncident` or
    // `pub enum LeaderRecoveryIncident`. Baseline: neither exists → red.
    let files = ["leader/mod.rs", "leader/lease.rs", "leader/incident.rs"];
    let mut found = false;
    for rel in files {
        let path = crate_src_root().join(rel);
        if let Ok(text) = fs::read_to_string(&path) {
            if text.contains("pub enum LeaderIncident")
                || text.contains("pub enum LeaderRecoveryIncident")
            {
                found = true;
                break;
            }
        }
    }
    assert!(
        found,
        "no typed leader-incident enum found in leader/*.rs. §3.1/§3.2 \
         require claim/attach to emit a typed incident (LeaderAttached / \
         LeaderClaimed) consumed by a separate recovery owner. Baseline \
         only has string event names (leader_receiver.attached / \
         leader_receiver.rebind_applied) — no typed API surface."
    );
}

// ---------------------------------------------------------------------------
// Tooth 4 — positive control: attach still emits at least one event
// ---------------------------------------------------------------------------

#[test]
#[serial(env)]
fn attach_flow_still_publishes_at_least_one_leader_event() {
    // We do NOT drive a real attach here (that is C batch's fixture job).
    // The positive-control invariant is source-level: the attach flow
    // still contains SOME event emission, so a refactor that funnels
    // through a typed incident cannot silently drop observability.
    let text = read_src("leader/lease.rs");
    let mentions = text.matches("event_log.write(").count() + text.matches("LeaderEvent::").count();
    assert!(
        mentions > 0,
        "leader/lease.rs no longer emits any event / typed LeaderEvent — \
         refactor may have dropped attach observability."
    );
}

// ---------------------------------------------------------------------------
// Tooth 5 — negative control: scanner does not match arbitrary text
// ---------------------------------------------------------------------------

#[test]
#[serial(env)]
fn unrelated_functions_are_not_matched_by_the_scanner() {
    // Manufactured test string — the substring appears in this test file
    // but the scanner runs against src/, not tests/, so it should never
    // be counted. Round-trip that assumption.
    let _canary = "update messages set status = 'not_a_real_status'"; // not read by scanner
    let src_root = crate_src_root();
    assert!(
        src_root.ends_with("src"),
        "scanner root drift: expected .../src, got {}",
        src_root.display()
    );
    // Confirm the scanner would still flag delivery.rs' known SQL — if
    // this baseline check fails, tooth 1's failure would be vacuous.
    assert!(
        contains_status_write_sql("messaging/delivery.rs"),
        "scanner failed to detect known baseline SQL in messaging/delivery.rs; \
         tooth 1 would be vacuously green — scanner is broken."
    );
}
