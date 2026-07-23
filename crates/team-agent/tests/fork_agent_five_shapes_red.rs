//! RED contract — 0.5.58 fork-plane car
//! Five shapes derived from `.team/artifacts/fork-agent-broken-0.5.56-locate.md
//! §6` + §7 risk-boundary constraints. Every tooth is a red-tooth design
//! constraint — no real sleep, no cwd-latest relaxation, no hidden prompt,
//! no silent fresh-clone downgrade.
//!
//! LINEAGE:
//! - Baseline: 5b847e4 (0.5.56 tested tip; ships fork-agent as broken).
//! - Root causes (locate §0):
//!     (a) `context_fork.rs:8-13` hard-codes Codex proof deadline = 10s;
//!         same-machine restart spawn already measured 11.881s → business
//!         failure threshold precedes cold-start completion.
//!     (b) fork resolves source via **spec** (`fork_agent.rs:60`
//!         `find_spec_agent`), clone via **state**
//!         (`clone_agent.rs:16-29` `state.get("agents")`); dynamically
//!         reserved seats without a spec entry are refused by fork,
//!         accepted by clone — double truth source.
//!     (c) fork accepts only synchronous `Result<Proof>` from
//!         provider observation; there is no typed `Pending`, so a
//!         provider that persists rollout later than 10s cannot be
//!         admitted as pending-then-verified.
//!     (d) source rollout re-render is not excluded from `NEW` scanner
//!         set — a slow provider that only rewrites source rollout can
//!         be mis-attributed as the new backing.
//!     (e) 0.5.53 acceptance was subscribed-CLI-shim green; no real
//!         subscription gate over `codex --version` + no-prompt +
//!         cold-start + nonce inheritance is frozen.
//!
//! TEETH (RED at 5b847e4):
//!   1. `slow_start_produces_typed_pending_not_immediate_rollback` —
//!      when the observation deadline expires before a new backing is
//!      visible, the API must return a typed `Pending{...}` state that
//!      keeps the seat registered and records source_id, target agent,
//!      spawned_at + canonical scanner context. Baseline: only
//!      `Verified` / synchronous timeout error exists → red.
//!   2. `pending_seat_has_bounded_grace_then_typed_failure` —
//!      a pending seat MUST transition to a typed failure
//!      (`transcript_missing` / `context_fork_unverified`) after a
//!      bounded grace expires without a real trigger; NEVER permanent
//!      pending and NEVER a forged tuple. Baseline: no pending state
//!      → red. Also risk-boundary §7#4: no silent fresh-clone
//!      downgrade path may exist in `fork_agent.rs`.
//!   3. `source_rollout_cannot_masquerade_as_new_backing` —
//!      canonical scanner must exclude `source_session_id` and its
//!      backing_path from the NEW candidate set; a re-render of the
//!      source rollout (updated stamp, same session_id) MUST NOT be
//!      admitted as fork proof. §7#3: scanner must NOT relax to
//!      cwd-latest / mtime-latest. Baseline: exclusion is present
//!      only implicitly via `!= source` in one call site (§1.2#4);
//!      claim/excluded set for source/target/sibling must be
//!      programmatic — assert on source-code shape.
//!   4. `source_existence_uses_state_not_spec` —
//!      fork's source-existence gate MUST read the same authority
//!      clone reads (`selected.state.agents[id]`), NOT the spec
//!      (`find_spec_agent`). Baseline: `fork_agent.rs:60`
//!      `find_spec_agent(&spec, source_agent_id)` → red.
//!   5. `real_subscription_gate_freezes_cold_start_matrix` —
//!      a subscription-tier acceptance test file must exist that
//!      exercises: real `codex --version`, no injected prompt, cold
//!      start > 10s tolerated, source nonce inherited, target NEW
//!      session id distinct, source unchanged. Its name must contain
//!      `subscription` + `fork` and it must live under
//!      `crates/team-agent/tests/` (offline shim greens do not
//!      substitute). Baseline: no such file → red.
//!
//! RISK-BOUNDARY LOCKS (already at §7 as inviolable):
//!   - No hidden prompt injection (§7#1): `provider/adapter.rs` must
//!     NOT pass a non-empty prompt in the `codex fork` command line
//!     (part of tooth 5).
//!   - No cwd-latest relaxation (§7#3): part of tooth 3 assertion.
//!   - Pending must be typed / observable / eventually failable
//!     (§7#4): tooth 2.
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

fn crate_tests() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests")
}

fn read_src(rel: &str) -> String {
    let path = crate_src().join(rel);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
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

/// Pure-text canary for the "typed pending arm exists" scan. Independent
/// of product state, so remains valid as the refactor changes the file
/// (returns-issue防线,同 leader msg_86c26787e018 判据台账 §二·补).
fn synthetic_pending_arm() -> &'static str {
    "enum ContextForkOutcome {\n  Verified(ContextForkProof),\n  Pending(PendingContextFork),\n  Rejected(ProviderError),\n}"
}

// ---------------------------------------------------------------------------
// Tooth 1 — slow start produces typed Pending, not immediate rollback
// ---------------------------------------------------------------------------

#[test]
#[serial(env)]
fn slow_start_produces_typed_pending_not_immediate_rollback() {
    let src = read_src("provider/session/context_fork.rs");
    // Sanity canary: the shape we're looking for is parseable — locks
    // the scanner semantics independently of product state.
    assert!(
        synthetic_pending_arm().contains("Pending(PendingContextFork)"),
        "sanity canary broken: synthetic arm text missing Pending variant"
    );
    // Face 1 — a typed outcome enum with a `Pending` variant.
    let has_pending_variant = (src.contains("enum ContextForkOutcome")
        || src.contains("enum ForkVerification"))
        && (src.contains("Pending(") || src.contains("Pending {"));
    // Face 2 — `PendingContextFork` (or equivalent) struct exists and
    // carries source_id + target agent + spawned_at + scanner context.
    // We accept any of these field name shapes:
    let has_pending_struct = src.contains("PendingContextFork")
        && (src.contains("source_session_id") || src.contains("source_id"))
        && (src.contains("target_agent") || src.contains("agent_id"))
        && (src.contains("spawned_at") || src.contains("spawn_boundary"));
    assert!(
        has_pending_variant && has_pending_struct,
        "context_fork.rs missing typed Pending outcome. locate §5.1: return \
         Verified(ContextForkProof) | Pending(PendingContextFork) | \
         Rejected(ProviderError). Baseline synchronously errors when the \
         10s deadline elapses. has_pending_variant={} has_pending_struct={}",
        has_pending_variant,
        has_pending_struct
    );
    // Face 3 — the deadline is no longer treated as a business-failure
    // threshold; keep it as a fast-path budget only. We assert that the
    // deadline-consumption site no longer returns an error variant on
    // expiration (rough source scan: presence of a `Pending(` in the
    // deadline block).
    let deadline_block_returns_pending = src
        .lines()
        .skip_while(|l| !l.contains("context_fork_convergence_deadline"))
        .take(200)
        .collect::<Vec<_>>()
        .join("\n")
        .contains("Pending(");
    assert!(
        deadline_block_returns_pending,
        "context_fork.rs deadline path still short-circuits to a hard error \
         instead of a typed Pending. locate §5.1 requires the deadline to be \
         a fast-path budget, not a business rejection."
    );
}

// ---------------------------------------------------------------------------
// Tooth 2 — pending seat has bounded grace then typed failure
// ---------------------------------------------------------------------------

#[test]
#[serial(env)]
fn pending_seat_has_bounded_grace_then_typed_failure() {
    let fork_agent = read_src("lifecycle/launch/fork_agent.rs");
    let files = walk_texts(&crate_src());
    // Face 1 — `PendingContextFork` (or equivalent) has a grace field
    // with an upper bound (`grace_deadline` / `pending_grace_secs`).
    let has_grace = files.iter().any(|(_, t)| {
        t.contains("pending_grace")
            || t.contains("grace_deadline")
            || t.contains("PendingContextFork") && t.contains("grace")
    });
    // Face 2 — a typed failure variant exists for either
    // `transcript_missing` or `context_fork_unverified` and it is
    // produced by a NAMED transition function (not a magic string).
    let has_typed_failure = files.iter().any(|(_, t)| {
        (t.contains("TranscriptMissing") || t.contains("ContextForkUnverified"))
            && (t.contains("fn expire_pending") || t.contains("fn transition_pending"))
    });
    // Face 3 — no silent fresh-clone downgrade: fork_agent.rs must not
    // contain a call that turns a pending fork into a `clone_agent`
    // invocation on the same path (§7#4).
    let silent_downgrade = fork_agent.contains("clone_agent(") && fork_agent.contains("pending");
    assert!(
        has_grace && has_typed_failure && !silent_downgrade,
        "pending-seat safety broken. locate §5.1+§7#4: bounded grace + typed \
         failure required; NO silent fresh-clone downgrade. \
         has_grace={} has_typed_failure={} silent_downgrade={}",
        has_grace,
        has_typed_failure,
        silent_downgrade
    );
}

// ---------------------------------------------------------------------------
// Tooth 3 — source rollout cannot masquerade as NEW; no cwd-latest relaxation
// ---------------------------------------------------------------------------

#[test]
#[serial(env)]
fn source_rollout_cannot_masquerade_as_new_backing() {
    let src = read_src("provider/session/context_fork.rs");
    // Face 1 — canonical scanner call site must explicitly build a
    // `claimed` / `excluded` set containing source_session_id AND its
    // backing_path AND (per locate §5.1#5) sibling agents.
    let excludes_source = src.contains("excluded") || src.contains("claimed_set");
    let excludes_source_id =
        src.contains("source_session_id") && (src.contains("insert") || src.contains("push"));
    // Face 2 — scanner MUST NOT be relaxed to cwd-latest / mtime-latest.
    // We assert on the negative: no source line inside context_fork.rs
    // may contain `cwd_latest` or `mtime_latest` as a scanner mode.
    let has_relaxation = src.contains("cwd_latest")
        || src.contains("mtime_latest")
        || src.contains("CwdLatest")
        || src.contains("MtimeLatest");
    // Face 3 — `!=source` alone is insufficient; must be programmatic
    // claim/excluded set (the "boolean not equal" implementation is
    // what §1.2#4 flagged as fragile).
    // We enforce Face 3 by requiring the exclusion be expressed as a
    // set operation, not merely a `!=` comparison.
    let only_neq = !excludes_source && src.contains("!= source") || src.contains("!= self.source");
    assert!(
        excludes_source && excludes_source_id && !has_relaxation && !only_neq,
        "canonical scanner claim/excluded set incomplete. locate §5.1+§7#3: \
         source_session_id + backing_path + sibling agents must be in a \
         programmatic excluded set; scanner must NOT relax to \
         cwd-latest/mtime-latest. excludes_source={} excludes_source_id={} \
         has_relaxation={} only_neq={}",
        excludes_source,
        excludes_source_id,
        has_relaxation,
        only_neq
    );
}

// ---------------------------------------------------------------------------
// Tooth 4 — source-existence uses state, not spec (matches clone)
// ---------------------------------------------------------------------------

#[test]
#[serial(env)]
fn source_existence_uses_state_not_spec() {
    let fork_agent = read_src("lifecycle/launch/fork_agent.rs");
    // Face 1 — source existence must be read from `state.get("agents")`
    // BEFORE any spec resolution (the same authority clone uses at
    // clone_agent.rs:16-29). We scan the file top-to-bottom: the FIRST
    // occurrence of source-existence resolution must reference state.
    let state_hit = fork_agent
        .find("state\n            .get(\"agents\")")
        .or_else(|| fork_agent.find("state.get(\"agents\")"))
        .or_else(|| fork_agent.find(".agents.get(source_agent_id"));
    let spec_hit = fork_agent.find("find_spec_agent(&spec, source_agent_id)");
    let state_before_spec = match (state_hit, spec_hit) {
        (Some(s), Some(sp)) => s < sp,
        (Some(_), None) => true,
        _ => false,
    };
    // Face 2 — the spec-based existence gate must be gone entirely, OR
    // demoted to a role/config readback (never a source-existence
    // authority).
    let spec_still_authoritative = fork_agent
        .contains("unknown worker agent id: {source_agent_id}")
        && fork_agent.contains("find_spec_agent");
    assert!(
        state_before_spec && !spec_still_authoritative,
        "fork source-existence authority mismatch with clone. locate §5.2: \
         fork_agent.rs must read source from state.agents (same as \
         clone_agent.rs:16-29), NOT from spec via find_spec_agent. \
         state_hit={:?} spec_hit={:?} state_before_spec={} \
         spec_still_authoritative={}",
        state_hit,
        spec_hit,
        state_before_spec,
        spec_still_authoritative
    );
}

// ---------------------------------------------------------------------------
// Tooth 5 — real subscription gate freezes the cold-start matrix
// ---------------------------------------------------------------------------

#[test]
#[serial(env)]
fn real_subscription_gate_freezes_cold_start_matrix() {
    let tests_root = crate_tests();
    // Face 1 — a file whose name contains BOTH `subscription` and
    // `fork` must exist under crates/team-agent/tests/.
    let mut found_file: Option<PathBuf> = None;
    if let Ok(entries) = fs::read_dir(&tests_root) {
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().to_lowercase();
            if name.contains("subscription") && name.contains("fork") && name.ends_with(".rs") {
                found_file = Some(e.path());
                break;
            }
        }
    }
    // Face 2 — its body must reference the ordered acceptance items
    // frozen by locate §6#5: `codex --version` + no prompt +
    // cold_start + nonce + NEW session id + source_unchanged.
    let mut face2 = false;
    if let Some(p) = &found_file {
        if let Ok(t) = fs::read_to_string(p) {
            face2 = t.contains("codex --version")
                && t.contains("no_prompt")
                && (t.contains("cold_start") || t.contains("cold-start"))
                && t.contains("nonce")
                && (t.contains("distinct") || t.contains("new_session_id"))
                && t.contains("source_unchanged");
        }
    }
    // Face 3 (§7#1) — provider/adapter.rs must NOT pass a non-empty
    // prompt to `codex fork`. Baseline currently passes only
    // `<session_id>` (§2.2); we lock it here so a "helpful" retry
    // patch cannot silently activate a hidden turn.
    let adapter = read_src("provider/adapter.rs");
    let hidden_prompt = adapter.contains("codex")
        && adapter.contains("fork")
        && adapter.contains("prompt")
        && (adapter.contains(".arg(\"fork\")") || adapter.contains("\"fork\","))
        && adapter.contains(".arg(&prompt)");
    assert!(
        found_file.is_some() && face2 && !hidden_prompt,
        "real subscription gate missing / hidden-prompt risk. locate §6#5+§7#1: \
         a tests/*.rs file containing `subscription`+`fork` in its name must \
         exercise codex --version + no_prompt + cold_start + nonce + \
         distinct new_session_id + source_unchanged. Also adapter must not \
         attach a prompt to `codex fork`. found_file={:?} face2={} \
         hidden_prompt={}",
        found_file,
        face2,
        hidden_prompt
    );
}

// ---------------------------------------------------------------------------
// Sanity control — scanner-shape canary independent of product state
// ---------------------------------------------------------------------------

#[test]
#[serial(env)]
fn scanner_canaries_independent_of_product_state() {
    let good = "enum Foo { Pending(Bar) }";
    let bad = "enum Foo { Verified(Bar), Rejected(Err) }";
    assert!(
        good.contains("Pending(Bar)") && !bad.contains("Pending(Bar)"),
        "scanner-shape sanity broken — the substring test that Tooth 1 \
         depends on can't tell 'good' from 'bad' text. This is an \
         independent canary (does not read product source) so it stays \
         valid regardless of what the fork refactor does."
    );
    // Confirm src/tests roots resolve.
    assert!(
        crate_src().ends_with("src"),
        "crate_src drift: {}",
        crate_src().display()
    );
    assert!(
        crate_tests().ends_with("tests"),
        "crate_tests drift: {}",
        crate_tests().display()
    );
}
