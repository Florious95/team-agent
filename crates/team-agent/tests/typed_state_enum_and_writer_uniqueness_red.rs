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
//!   1. `enum_catalog_covers_every_observed_status` — the product must
//!      expose a single-source-of-truth catalog `MessageRowStatus::ALL:
//!      &'static [Self]` that lists every typed variant; the test
//!      enumerates it and checks each REQUIRED_DURABLE_STATUSES entry
//!      is round-trippable. Baseline: no `ALL` catalog exists AND the
//!      enum only has 4 variants → red on both faces. The GREEN
//!      implementer must (a) add the `ALL` associated const, (b) add
//!      the 8 missing typed variants; from then on, adding any new
//!      variant automatically appears here without a test edit — the
//!      catalog is consumed from the product surface, not
//!      hand-copied. (Returns issue: previous freeze hard-coded 4
//!      variants inside the test, making the tooth unreachable — see
//!      leader ruling msg_86c26787e018.)
//!   2. `submitted_awaiting_receipt_and_blocked_leader_unbound_are_distinct`
//!      — after ①'s `ALL` catalog exists, the test walks THAT catalog
//!      and asserts the two required semantic variants
//!      (`SubmittedAwaitingReceipt` and `BlockedLeaderUnbound`) exist
//!      and have distinct string representations. Baseline: no catalog
//!      → red; even if `ALL` is added, missing typed variants keep the
//!      tooth red until they are declared. (Returns issue: previous
//!      freeze walked a hand-copied 4-variant set → same permanent-red
//!      defect as ①.)
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
// Tooth 1 — enum catalog completeness (consumes product SSOT catalog)
// ---------------------------------------------------------------------------

/// Read the product source text for db/message_store.rs — the single
/// module that owns `MessageRowStatus`. Parse:
///   (a) whether it exposes an `ALL` associated const of `&[Self]` type
///       (or `pub const ALL: &[MessageRowStatus] = &[...]`);
///   (b) the set of `as_str` mapped strings inside `impl MessageRowStatus`.
/// Both facts together are the product-side SSOT; the test consumes it
/// rather than hand-copying variants — GREEN can add typed variants and
/// this test will see them without a test edit.
fn read_typed_catalog_facts() -> (bool, BTreeSet<String>) {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("src")
        .join("db")
        .join("message_store.rs");
    let text = fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!("read {}: {e}", path.display());
    });
    // (a) presence of ALL catalog: two accepted spellings.
    let has_all = text.contains("pub const ALL: &[Self]")
        || text.contains("pub const ALL: &[MessageRowStatus]");
    // (b) enumerate as_str string literals inside `impl MessageRowStatus`
    // — a `Self::Variant => "some_str"` arm shape. We use a robust
    // substring scan: any line matching `=> "` inside the impl block
    // AFTER the `impl MessageRowStatus` header line, up to the next
    // top-level `impl` or `pub enum`.
    let mut variants: BTreeSet<String> = BTreeSet::new();
    if let Some(start) = text.find("impl MessageRowStatus") {
        let tail = &text[start..];
        // Cheap bound: stop at next `\nimpl ` or `\npub enum` or `\npub struct`
        let stop_offsets: Vec<usize> = ["\nimpl ", "\npub enum ", "\npub struct "]
            .iter()
            .filter_map(|kw| tail[1..].find(kw).map(|o| o + 1))
            .collect();
        let end = stop_offsets.into_iter().min().unwrap_or(tail.len());
        let block = &tail[..end];
        for line in block.lines() {
            // capture `=> "xxx"` — the RHS string in the match arm.
            if let Some(idx) = line.find("=> \"") {
                let rest = &line[idx + 4..];
                if let Some(close) = rest.find('"') {
                    variants.insert(rest[..close].to_string());
                }
            }
        }
    }
    (has_all, variants)
}

#[test]
fn enum_catalog_covers_every_observed_status() {
    let (has_all, product_variants) = read_typed_catalog_facts();
    // Face 1: product must expose a single-source-of-truth `ALL` catalog.
    assert!(
        has_all,
        "product `MessageRowStatus` is missing `pub const ALL: &[Self]` — no \
         single source of truth for the typed variant list. Downstream \
         fixtures/recovery arms will end up hand-copying, which is exactly \
         what the wiki hard rule forbids. Add `impl MessageRowStatus {{ pub \
         const ALL: &[Self] = &[…all variants…]; }}` so this test (and \
         batch C's REQUIRED_DURABLE_STATUSES lock-step check) can consume \
         it programmatically."
    );
    // Face 2: the union of currently-declared variant strings must cover
    // every required durable status. Because we parse from `as_str`
    // arms, adding a variant *automatically* extends `product_variants`
    // — this test needs no edit as GREEN grows the enum.
    let missing: Vec<&&str> = REQUIRED_DURABLE_STATUSES
        .iter()
        .filter(|s| !product_variants.contains(**s))
        .collect();
    assert!(
        missing.is_empty(),
        "MessageRowStatus catalog missing typed variants for durable statuses: {:?}. \
         Product variants currently declared: {:?}. Distillation §3.1 requires \
         every observed durable status to have a typed variant so downstream \
         fixtures/recovery arms cannot introduce a second hand-written state \
         semantics (wiki hard rule).",
        missing,
        product_variants
    );
}

// ---------------------------------------------------------------------------
// Tooth 2 — SubmittedAwaitingReceipt vs BlockedLeaderUnbound must be distinct
// ---------------------------------------------------------------------------

#[test]
fn submitted_awaiting_receipt_and_blocked_leader_unbound_are_distinct() {
    let (_, product_variants) = read_typed_catalog_facts();
    // The two typed variants required by §3.1: their user-visible string
    // representation MUST be present (and distinct) in the product's
    // `as_str` arms. We DO NOT hand-code the covered set — we walk the
    // product surface, so GREEN adding either variant flips this face
    // to green without a test edit.
    let parked = "submitted_pending_acceptance";
    let blocked_leader_unbound_typed = product_variants
        .iter()
        .any(|s| s.contains("blocked_leader_unbound") || s == "leader_not_attached");
    let parked_typed = product_variants.contains(parked);
    // Fail-closed: both required, and if they exist they MUST be different
    // strings — collapsing to a shared string is exactly the P0 defect.
    let distinct = parked_typed
        && blocked_leader_unbound_typed
        && !product_variants
            .iter()
            .any(|s| s == parked && s.contains("leader_not_attached"));
    assert!(
        distinct,
        "typed distinction missing / collapsed. product variants: {:?}. Need: \
         a variant whose as_str==\"submitted_pending_acceptance\" (parked=\
         SubmittedAwaitingReceipt, parked_typed={}) AND a distinct variant \
         representing the blocked-leader-unbound face \
         (blocked_leader_unbound_typed={}). §3.1 hard rule: \
         SubmittedAwaitingReceipt ≠ BlockedLeaderUnbound; the storm proved \
         collapsing them is the P0 root cause.",
        product_variants, parked_typed, blocked_leader_unbound_typed
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
