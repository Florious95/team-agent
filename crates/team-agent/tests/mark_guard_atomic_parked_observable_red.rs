//! RED contract — 0.5.57 G-batch mini(补冻:异元审查裁定 msg_cf2603c5e8e0)
//!
//! Motivation: reviewer-c2 verdict flagged three concerns that leader ruled
//! together as the "storm single-row" pattern (livesnapshot parked=447 rows,
//! results.rs:870 retry + tick.rs:1117 both able to regress parked →
//! accepted without guard). This mini contract locks those invariants
//! against the typed-state car GREEN and the impending d3 hardening pass:
//!
//!   齿1 (CONCERN-1 mark() TOCTOU) — `mark()` MUST perform its terminal-
//!         state guard atomically via a single UPDATE bearing a status
//!         predicate; the previous SELECT-then-UPDATE split is a
//!         cross-process race window that the storm case belongs to.
//!   齿2 (CONCERN-5 parked residue) — `submitted_pending_acceptance`
//!         (parked) MUST be protected by the same guard. The only legal
//!         writers turning parked→accepted are the receipt observer path
//!         and `retry_submitted_explicit` — generic `mark(&str)` from
//!         results.rs / coordinator/tick.rs is forbidden.
//!   齿3 (CONCERN-2 silent Ok no-op) — when the guard refuses a write
//!         it MUST emit an observable event (leaving 0-row Ok(()) with
//!         no log is the故障不可见 family; a future incident could not be
//!         attributed).
//!   PC   正控 (anti-vacuous) — non-terminal, non-parked prior states
//!         still transition normally through mark(). The guard is
//!         scoped, not blanket.
//!
//! LINEAGE:
//! - Baseline: 5b847e4 (0.5.56 tested tip; baseline mark() has NO guard —
//!   every tooth is a target invariant to be reached by d3 GREEN pass).
//! - Sources for the three CONCERN anchors:
//!     .team/artifacts/0.5.57-c4-reviewer-c2-verdict.md CONCERN-1/2/5.
//! - Retains the audit-台账 defense lines from A/B v2 return-issue:
//!     * teeth consume product source (not hand-copied constants), and
//!     * an independent synthetic scanner canary so tooth semantics stay
//!       valid after refactor.
//!
//! FROZEN by verifier — do NOT modify without a new SHA256 signature.

#![cfg(unix)]
#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

// R6 密闭边界:本文件的实跑齿会调 MessageStore/mark 触发 R6 dangerous
// signals,声明 hermetic 触点并在实跑齿 enter() 满足门禁。
#[path = "support/hermetic.rs"]
mod hermetic_guard;
#[allow(dead_code)]
fn _hermetic_boundary_marker(_: &hermetic_guard::HermeticTestEnv) {}

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use rusqlite::params;
use serial_test::serial;
use team_agent::message_store::MessageStore;

fn crate_src() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src")
}

fn read_message_store_source() -> String {
    let path = crate_src().join("db").join("message_store.rs");
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// Extract the body of `pub fn mark(` — the ONE writer under test — up to
/// its closing `    }` at column 4. Cheap heuristic; enough to inspect
/// SELECT/UPDATE atomicity and event emission.
fn extract_mark_body(src: &str) -> String {
    let start = src
        .find("pub fn mark(")
        .expect("pub fn mark not found in message_store.rs");
    // Find the first `\n    }\n` (column-4 close brace) after the header.
    let tail = &src[start..];
    let end_off = tail
        .find("\n    }\n")
        .expect("could not locate closing brace of mark()");
    tail[..end_off].to_string()
}

// ---------------------------------------------------------------------------
// 齿1 — mark() guard atomicity (CONCERN-1)
// ---------------------------------------------------------------------------

/// Independent canary: prove the scanner shape can distinguish an atomic
/// guarded UPDATE from a SELECT+UPDATE split, without anchoring on
/// product state. Returns:
///   - `atomic(text)`  = text contains a single UPDATE bearing an
///                       inline `and status` (or `and status not in` /
///                       `and status in (...)`) predicate.
///   - `split(text)`   = text contains BOTH a SELECT-status pattern AND
///                       an UPDATE that lacks a `status` predicate.
fn scan_atomicity(text: &str) -> (bool, bool) {
    let has_select_status = text.contains("select status from messages");
    let text_l = text.to_lowercase();
    let update_hunks: Vec<&str> = text_l
        .split("update messages")
        .skip(1) // skip prefix before the first UPDATE
        .collect();
    // An UPDATE is "no-predicate" iff its WHERE clause names ONLY message_id
    // (i.e. `where message_id = ?1` — no `and status ...`).
    let no_predicate_update = update_hunks.iter().any(|hunk| {
        // Stop at the closing `,` or `;` of the SQL literal.
        let stop = hunk.find("\",").unwrap_or(hunk.len());
        let window = &hunk[..stop];
        window.contains("where message_id = ?1") && !window.contains("and status")
    });
    let atomic_update = update_hunks.iter().any(|hunk| {
        let stop = hunk.find("\",").unwrap_or(hunk.len());
        let window = &hunk[..stop];
        window.contains("and status")
    });
    let is_split = has_select_status && no_predicate_update;
    let is_atomic = atomic_update && !is_split;
    (is_atomic, is_split)
}

#[test]
#[serial(env)]
fn mark_guard_uses_single_atomic_update_with_status_predicate() {
    // Sanity canary — synthetic strings prove the scanner shape works
    // independent of product state (leader ruling msg_86c26787e018,
    // A/B v2 判据台账 §二·补).
    let atomic_canary = "update messages set status = ?2, updated_at = ?3 \
                         where message_id = ?1 and status not in ('acknowledged','consumed','delivered')";
    let split_canary = "select status from messages where message_id = ?1;\
                        update messages set status = ?2 where message_id = ?1";
    let (a1, s1) = scan_atomicity(atomic_canary);
    let (a2, s2) = scan_atomicity(split_canary);
    assert!(
        a1 && !s1,
        "scanner canary broken: atomic canary reported atomic={a1} split={s1}"
    );
    assert!(
        s2 && !a2,
        "scanner canary broken: split canary reported atomic={a2} split={s2}"
    );

    // Product face — mark() body must be atomic.
    let src = read_message_store_source();
    let body = extract_mark_body(&src);
    let (is_atomic, is_split) = scan_atomicity(&body);
    assert!(
        is_atomic && !is_split,
        "mark() terminal-state guard non-atomic. CONCERN-1: guard must use a \
         single UPDATE bearing a status predicate (e.g. `where message_id = ?1 \
         and status not in ('acknowledged','consumed','delivered')`) so a \
         concurrent writer cannot slip between SELECT-prior and no-predicate \
         UPDATE. Snapshot: is_atomic={is_atomic} is_split={is_split}. Body:\n{body}"
    );
}

// ---------------------------------------------------------------------------
// 齿2 — parked (submitted_pending_acceptance) covered by mark() guard
// ---------------------------------------------------------------------------

fn seed_parked_row(case: &Case, mid: &str) {
    let conn = team_agent::db::schema::open_db(case.store.db_path()).unwrap();
    conn.execute(
        "insert into messages (message_id, owner_team_id, sender, recipient, content, \
         status, presentation, created_at, updated_at, delivery_attempts) \
         values (?1, 'g-team', 'leader', 'leader', 'parked canary', \
         'submitted_pending_acceptance', '{\"sink\":\"leader\",\"class\":\"message\"}', \
         '2026-07-21T00:00:00Z', '2026-07-21T00:00:00Z', 1)",
        params![mid],
    )
    .unwrap();
}

fn status_of(case: &Case, mid: &str) -> String {
    let conn = team_agent::db::schema::open_db(case.store.db_path()).unwrap();
    conn.query_row(
        "select status from messages where message_id = ?1",
        params![mid],
        |row| row.get(0),
    )
    .unwrap()
}

struct Case {
    _env: hermetic_guard::HermeticTestEnv,
    #[allow(dead_code)]
    workspace: PathBuf,
    store: MessageStore,
}

impl Case {
    fn new(tag: &str) -> Self {
        static N: AtomicU64 = AtomicU64::new(0);
        let env = hermetic_guard::HermeticTestEnv::enter(tag);
        let workspace = env.workspace(&format!("g-{}", N.fetch_add(1, Ordering::Relaxed)));
        let store = MessageStore::open(&workspace).unwrap();
        Self {
            _env: env,
            workspace,
            store,
        }
    }
}

#[test]
#[serial(env)]
fn generic_mark_cannot_flip_parked_to_accepted() {
    let case = Case::new("g-parked");
    let mid = "msg_g_parked_1";
    seed_parked_row(&case, mid);
    // Baseline: mark() has no parked guard → this call succeeds and
    // silently regresses parked → accepted (storm single-row pattern,
    // CONCERN-5, results.rs:870 / tick.rs:1117 amplify it).
    // GREEN target: mark() refuses (typed InvalidTransition-like error
    // OR guarded no-op with observable event; either way the ROW must
    // STAY 'submitted_pending_acceptance').
    let _ = case.store.mark(mid, "accepted", None);
    let after = status_of(&case, mid);
    assert_eq!(
        after, "submitted_pending_acceptance",
        "CONCERN-5: generic mark() regressed a parked row to `{after}`. \
         parked=submitted_pending_acceptance is a designed already-crossed-\
         transport state; only receipt observer or retry_submitted_explicit \
         may transition it (never generic mark). Storm single-row pattern."
    );
}

// ---------------------------------------------------------------------------
// 齿3 — guard refusal is observable (CONCERN-2 故障不可见)
// ---------------------------------------------------------------------------

/// Independent canary: proves the scanner can distinguish "guard bearing
/// an observable event emission" from "guard bearing only Ok(())". This
/// is a pure-text canary, unrelated to product state.
fn scan_guard_observability(mark_body: &str) -> (bool, bool) {
    // A guarded early return is a `return Ok(());` (or `return Ok(())`)
    // that appears BEFORE the main UPDATE. The refusal is observable
    // iff within the guard block (heuristic: 10 lines before the
    // return) there's an event/log/audit emission.
    let mut has_silent_return = false;
    let mut has_observable_refusal = false;
    let lines: Vec<&str> = mark_body.lines().collect();
    for (i, l) in lines.iter().enumerate() {
        if l.trim_start().starts_with("return Ok(())") {
            let start = i.saturating_sub(10);
            let window: String = lines[start..i].join("\n");
            let emits_event = window.contains("event_log")
                || window.contains("write_event")
                || window.contains("audit_log")
                || window.contains("tracing::warn!")
                || window.contains("mark_refused");
            if emits_event {
                has_observable_refusal = true;
            } else {
                has_silent_return = true;
            }
        }
    }
    (has_observable_refusal, has_silent_return)
}

#[test]
#[serial(env)]
fn mark_guard_refusal_is_observable_not_silent_ok() {
    // Sanity canary — MUST be multi-line. scan_guard_observability walks
    // lines and matches `return Ok(())` only when it starts its own line
    // (after trim_start). A single-line block `if x { return Ok(()); }`
    // would never be seen (leader ruling msg_dcd6f7e79620: d5 exposed the
    // v1 canary as恒定行为假红=镜像型自缺陷). Semantics identical.
    let silent = "\
        if prior_terminal {\n\
        \x20   return Ok(());\n\
        }";
    let observed = "\
        if prior_terminal {\n\
        \x20   event_log.write(\"mark.refused\", json!({}))?;\n\
        \x20   return Ok(());\n\
        }";
    let (obs1, sil1) = scan_guard_observability(silent);
    let (obs2, sil2) = scan_guard_observability(observed);
    assert!(
        !obs1 && sil1,
        "scanner canary broken: silent case reported observable={obs1} silent={sil1}"
    );
    assert!(
        obs2 && !sil2,
        "scanner canary broken: observed case reported observable={obs2} silent={sil2}"
    );

    // Product face.
    let src = read_message_store_source();
    let body = extract_mark_body(&src);
    let (has_observable_refusal, has_silent_return) = scan_guard_observability(&body);
    assert!(
        !has_silent_return,
        "CONCERN-2: mark() guard uses silent `return Ok(());` with no observable \
         event emission — the故障不可见 family (0.5.41 lineage). A guard MUST \
         write an event so a caller / postmortem can attribute a no-op refusal. \
         Snapshot: observable_refusal={has_observable_refusal} silent_return={has_silent_return}\n\
         body:\n{body}"
    );
    // Additionally, at least ONE observable refusal must exist (i.e. the
    // guard is not simply absent — that would be tooth 1's failure).
    assert!(
        has_observable_refusal,
        "CONCERN-2: mark() has no observable refusal path. If the guard is \
         absent entirely, tooth 1 will already fail; this face requires that \
         WHEN the guard refuses, it emits an event."
    );
}

// ---------------------------------------------------------------------------
// 正控 PC — non-parked, non-terminal prior still transitions normally
// ---------------------------------------------------------------------------

#[test]
#[serial(env)]
fn non_parked_non_terminal_mark_still_transitions() {
    let case = Case::new("g-normal");
    let mid = case
        .store
        .create_message_with_id(
            "msg_g_normal_1",
            None,
            "leader",
            "worker-a",
            "normal transition canary",
            None,
            false,
            Some("g-team"),
        )
        .unwrap();
    // prior=Accepted (via create). mark(target_resolved) is a legitimate
    // delivery-lifecycle transition and MUST succeed. Anti-vacuous: the
    // guard must be scoped to terminal / parked, not blanket-refuse all
    // status writes.
    case.store
        .mark(&mid, "target_resolved", None)
        .expect("legitimate accepted→target_resolved must be allowed");
    let after = status_of(&case, &mid);
    assert_eq!(
        after, "target_resolved",
        "PC: normal accepted→target_resolved lifecycle transition regressed. \
         The mark() guard is over-broad — it must be scoped to (a) terminal \
         downgrade attempts and (b) parked-eligibility, never blanket."
    );
}
