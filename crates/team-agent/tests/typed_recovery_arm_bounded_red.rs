//! RED contract — 0.5.57 typed-state car · batch D
//! Explicit typed recovery arm: any deliberate retry of a parked
//! (submitted-awaiting-receipt) or exhaustion-terminal row MUST go
//! through a typed API that answers all three at-least-once questions:
//!
//!   Q1 (触发频率):  triggered by an explicit incident / operator
//!                   action, NOT by every boot/attach.
//!   Q2 (集合大小):  bounded by incident_id scope; NEVER an O(history)
//!                   scan of the whole table.
//!   Q3 (幂等预算):  attempt budget + age (created_at) + owner_epoch
//!                   bounds; expired items are refused.
//!
//! LINEAGE:
//! - Baseline: 5b847e4 (0.5.56 tested tip).
//! - Distillation §3.1 Recovery block (RetryBlocked / RetrySubmittedExplicit)
//!   + §3.3#3 (null result / old epoch / incident_before boundaries) +
//!   §3.3#5 (Submitted→Delivered irreversibility).
//! - at-least-once three questions (canonical, leader-signed
//!   msg_d3861e8991a8 + verifier-signed
//!   at-least-once-three-questions-criteria.md).
//! - 0.5.56 explicitly deferred the "genuinely lost submit" recovery path
//!   to 0.5.57's typed recovery arm; baseline has NO such surface, so
//!   every tooth here is red by construction.
//!
//! TEETH (RED at 5b847e4, all four canonical questions locked):
//!   1. `typed_recovery_arm_api_exists` — a compiled Rust symbol named
//!      e.g. `retry_submitted_explicit` / `RetrySubmittedExplicit` /
//!      `submitted_recovery` must exist in `src/messaging/` or
//!      `src/recovery/`. Baseline: none → red.
//!   2. `recovery_requires_incident_id_and_operator_reason` (Q1 触发频率):
//!      any recovery call without a caller-supplied incident_id +
//!      operator reason must be refused; baseline: no such surface →
//!      red.
//!   3. `recovery_bounded_by_incident_scope_not_history` (Q2 集合大小):
//!      the recovery API must accept an explicit scope predicate (e.g.
//!      a message_id list, or an incident_id filter) and NOT sweep the
//!      whole table. Baseline: no API → red.
//!   4. `recovery_enforces_attempt_and_age_and_epoch_budget` (Q3 幂等
//!      预算): rows exceeding attempt_budget OR older than
//!      max_age_seconds OR belonging to a stale owner_epoch must be
//!      refused with a typed error. Baseline: no API → red.
//!
//! POSITIVE CONTROL:
//!   5. `within_budget_recovery_flips_only_named_row` — a recovery
//!      request that names one message_id, is within budget, and has a
//!      valid incident_id must successfully flip THAT row to a
//!      RetryRequested state (or accepted for redelivery) without
//!      touching any sibling row. Runs at RED time by asserting the
//!      SOURCE surface exists AND the shape is testable — this control
//!      goes green after batch D is implemented.
//!
//! NEGATIVE CONTROL:
//!   6. `no_recovery_arm_disguised_as_claim_or_attach` — no function in
//!      `src/leader/` may take a `RetrySubmittedExplicit` parameter and
//!      execute inline as part of claim/attach. Recovery is a SEPARATE
//!      owner; claim/attach may only publish an incident.
//!
//! FROZEN by verifier — do NOT modify without a new SHA256 signature.

#![cfg(unix)]
#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

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
// Tooth 1 — API surface exists
// ---------------------------------------------------------------------------

#[test]
#[serial(env)]
fn typed_recovery_arm_api_exists() {
    // We accept any of the canonical names distillation §3.1 allows.
    let hit = any_file_contains(|t| {
        t.contains("RetrySubmittedExplicit")
            || t.contains("retry_submitted_explicit")
            || (t.contains("submitted_recovery") && t.contains("pub fn"))
    });
    assert!(
        hit.is_some(),
        "no typed submitted-recovery arm symbol found in src/**/*.rs. \
         §3.1 requires an explicit RetrySubmittedExplicit / retry_submitted_explicit \
         entry point owned by a recovery module; baseline defers this to 0.5.57."
    );
}

// ---------------------------------------------------------------------------
// Tooth 2 — Q1 triggered by explicit incident + operator reason
// ---------------------------------------------------------------------------

#[test]
#[serial(env)]
fn recovery_requires_incident_id_and_operator_reason() {
    // Surface probe: the recovery API's signature must mention BOTH
    // `incident_id` and either `operator_reason` or `reason` (a
    // required parameter, not Option<>). We check the source text for
    // the pattern; baseline has no such function → red.
    let hit = any_file_contains(|t| {
        (t.contains("fn retry_submitted_explicit") || t.contains("fn submitted_recovery"))
            && t.contains("incident_id")
            && (t.contains("operator_reason") || t.contains("reason: "))
    });
    assert!(
        hit.is_some(),
        "recovery arm signature must require incident_id + operator_reason; \
         Q1 (triggered by explicit incident, not automatic boot/attach) unlocked. \
         Distillation §3.1 Recovery block requires this pair for typed \
         RetrySubmittedExplicit."
    );
}

// ---------------------------------------------------------------------------
// Tooth 3 — Q2 bounded scope (no O(history) sweep)
// ---------------------------------------------------------------------------

#[test]
#[serial(env)]
fn recovery_bounded_by_incident_scope_not_history() {
    // Surface probe: the API must accept a scope-narrowing parameter,
    // one of: `message_ids: &[..]`, `scope: RecoveryScope`, or
    // `message_id: &str`. It must NOT be a nullary "recover all" call.
    let hit = any_file_contains(|t| {
        (t.contains("fn retry_submitted_explicit") || t.contains("fn submitted_recovery"))
            && (t.contains("message_ids: &[")
                || t.contains("scope: RecoveryScope")
                || t.contains("message_id: &str"))
    });
    // Additionally, forbid any pattern that would clearly indicate a
    // whole-table sweep, e.g. `update messages set status = 'accepted'
    // where recipient = 'leader'` — this is a red flag for a hidden
    // "recover all" arm.
    let sweep_hit = walk_texts(&crate_src()).into_iter().any(|(_, t)| {
        t.contains("retry_submitted_explicit")
            && t.contains("where recipient = 'leader'")
            && !t.contains("message_id = ?")
    });
    assert!(
        hit.is_some() && !sweep_hit,
        "recovery arm must be bounded to an explicit message set or scope; \
         Q2 (集合大小 O(history) 禁扫) unlocked. Baseline: no bounded API surface \
         exists at all; distillation §3.3#3."
    );
}

// ---------------------------------------------------------------------------
// Tooth 4 — Q3 attempt/age/epoch budget refusal
// ---------------------------------------------------------------------------

#[test]
#[serial(env)]
fn recovery_enforces_attempt_and_age_and_epoch_budget() {
    // Surface probe: the recovery module must carry a typed budget /
    // refusal enum. We accept either signature mentioning
    // `attempt_budget` + `max_age_seconds` + `owner_epoch`, OR a
    // dedicated `RecoveryBudget` struct.
    let hit = any_file_contains(|t| {
        (t.contains("attempt_budget") && t.contains("max_age_seconds") && t.contains("owner_epoch"))
            || t.contains("struct RecoveryBudget")
            || t.contains("enum RecoveryRefusal")
    });
    assert!(
        hit.is_some(),
        "recovery arm must enforce attempt / age / owner_epoch budgets and \
         expose a typed refusal on breach; Q3 (幂等预算) unlocked. Baseline: \
         no such surface. §3.1 + at-least-once canonical Q3."
    );
}

// ---------------------------------------------------------------------------
// Tooth 5 — positive control: within-budget flips exactly the named row
// ---------------------------------------------------------------------------

#[test]
#[serial(env)]
fn within_budget_recovery_flips_only_named_row() {
    // At RED time this is asserted as a source-shape control: the
    // module must expose a testable behavior surface (a public fn OR a
    // trait). We look for a public entry point with signature
    // containing `message_id` and returning a Result. GREEN
    // implementation will add a runtime test in this same file (verifier
    // will sign the shape then).
    let hit = any_file_contains(|t| {
        t.contains("pub fn retry_submitted_explicit")
            || t.contains("pub fn submitted_recovery")
            || t.contains("pub async fn retry_submitted_explicit")
    });
    assert!(
        hit.is_some(),
        "no public recovery entry point exposed — positive control cannot be wired. \
         GREEN must add a pub fn returning Result<_, RecoveryRefusal> that flips \
         exactly the named row(s)."
    );
}

// ---------------------------------------------------------------------------
// Tooth 6 — negative control: no recovery arm disguised inside claim/attach
// ---------------------------------------------------------------------------

#[test]
#[serial(env)]
fn no_recovery_arm_disguised_as_claim_or_attach() {
    // A file inside src/leader/**/*.rs must NOT define a function whose
    // name mentions `claim` or `attach` AND takes a
    // `RetrySubmittedExplicit` parameter (which would collapse
    // control-plane and recovery ownership again).
    let leader = crate_src().join("leader");
    let mut violations: Vec<PathBuf> = Vec::new();
    for (p, t) in walk_texts(&leader) {
        // Look for `fn claim_...(...RetrySubmittedExplicit...)` or
        // `fn attach_...(...RetrySubmittedExplicit...)` — cheap
        // heuristic; the shape is unambiguous in Rust source.
        for func_kw in ["fn claim", "fn attach"] {
            if let Some(idx) = t.find(func_kw) {
                let end = t[idx..]
                    .find(')')
                    .map(|d| idx + d + 1)
                    .unwrap_or_else(|| t.len().min(idx + 400));
                let sig = &t[idx..end];
                if sig.contains("RetrySubmittedExplicit") {
                    violations.push(p.clone());
                }
            }
        }
    }
    // Baseline: leader/ has no such symbol at all, so this tooth is
    // GREEN by construction. It is included to LOCK the ownership
    // boundary — the typed refactor must not "helpfully" fold
    // recovery back into claim/attach.
    assert!(
        violations.is_empty(),
        "recovery arm parameter found in claim/attach signatures inside leader/: {:?}. \
         §3.2 requires claim/attach to only publish an incident; the recovery owner \
         consumes it separately.",
        violations
    );
}
