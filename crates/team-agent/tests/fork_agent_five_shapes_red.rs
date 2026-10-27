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
//!
//! REVISION LINEAGE:
//! - 7f97919c (V1) — teeth 1/3 scanned single file provider/session/context_fork.rs.
//! - THIS revision (V2, A 案 msg_ef0fa4adad80) — teeth 1/3 aggregate
//!   context_fork.rs + context_fork/ 子模块树(claude.rs / codex.rs /
//!   outcome.rs) after d8 refactor abb0ccf. Semantic asserts unchanged.
//!   B 案(留死字面注释喂扫描)驳回=注释欺骗扫描器。
//!   台账新增 §二·补4:源码扫描齿禁锚单文件路径,应锚模块树(文件拆分=
//!   常规重构,不该震碎契约)。
//! - V3 (legacy Codex/Grok worker excision) — tooth 3 re-anchored: the
//!   programmatic excluded set lived in the deleted Codex rollout scanner;
//!   the surviving faces are explicit NEW == source rejection + no
//!   cwd/mtime-latest relaxation. Tooth 5 deleted: it froze the real
//!   `codex fork` subscription gate (tests/subscription_fork_cold_start_gate.rs)
//!   and the adapter's `codex fork` hidden-prompt lock, both of which guarded
//!   the excised legacy Codex fork path (Codex forks only via its contract leaf).

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

/// Source text with every whitespace character removed, so a shape scan
/// matches the *token sequence* and not the indentation rustfmt happens to
/// pick. `selected\n        .state\n        .get("agents")`,
/// `selected\n            .state\n            .get("agents")` and
/// `selected.state.get("agents")` all normalize to the same text.
///
/// Deliberately does NOT strip comments: cutting each line at `//` also cuts
/// string literals that contain `//` (a URL), which loses real code and can
/// flip a face green. Whitespace is the only thing this tooth may ignore.
fn code_no_ws(src: &str) -> String {
    src.chars().filter(|ch| !ch.is_whitespace()).collect()
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

/// Read `context_fork.rs` PLUS every `*.rs` under the sibling
/// `context_fork/` submodule tree, concatenated into one aggregate text.
/// leader ruling msg_ef0fa4adad80 A 案:module refactor (splitting
/// context_fork.rs into outcome.rs / claude.rs / codex.rs) MUST NOT
/// shatter contract-side textual scans — the semantic invariants live
/// in the module TREE, not a single file. Anchoring on a single file
/// path is now台账-forbidden(§二·补4).
fn read_context_fork_module_text() -> String {
    let mut aggregate = String::new();
    // Main file (kept post-split as the module head).
    let main_path = crate_src()
        .join("provider")
        .join("session")
        .join("context_fork.rs");
    if let Ok(t) = fs::read_to_string(&main_path) {
        aggregate.push_str(&t);
        aggregate.push('\n');
    }
    // Sibling submodule tree.
    let sub_root = crate_src()
        .join("provider")
        .join("session")
        .join("context_fork");
    for (_, t) in walk_texts(&sub_root) {
        aggregate.push_str(&t);
        aggregate.push('\n');
    }
    aggregate
}

// ---------------------------------------------------------------------------
// Tooth 1 — slow start produces typed Pending, not immediate rollback
// ---------------------------------------------------------------------------

#[test]
#[serial(env)]
fn slow_start_produces_typed_pending_not_immediate_rollback() {
    // A 案(msg_ef0fa4adad80):read context_fork.rs PLUS the
    // context_fork/ submodule tree. Splitting into outcome.rs /
    // claude.rs / codex.rs is a normal refactor and MUST NOT
    // invalidate the contract.
    let src = read_context_fork_module_text();
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
        "context_fork module tree missing typed Pending outcome. locate §5.1: \
         return Verified(ContextForkProof) | Pending(PendingContextFork) | \
         Rejected(ProviderError). Baseline synchronously errors when the \
         10s deadline elapses. has_pending_variant={} has_pending_struct={}",
        has_pending_variant,
        has_pending_struct
    );
    // Face 3 — the deadline is no longer treated as a business-failure
    // threshold; keep it as a fast-path budget only. We assert that the
    // deadline-consumption site no longer returns an error variant on
    // expiration (rough source scan: presence of a `Pending(` in the
    // deadline block). Same skip-until search works over concatenated
    // module text (deadline & Pending sites end up in the same aggregate).
    let deadline_block_returns_pending = src
        .lines()
        .skip_while(|l| !l.contains("context_fork_convergence_deadline"))
        .take(400)
        .collect::<Vec<_>>()
        .join("\n")
        .contains("Pending(");
    assert!(
        deadline_block_returns_pending,
        "context_fork module tree deadline path still short-circuits to a hard \
         error instead of a typed Pending. locate §5.1 requires the deadline \
         to be a fast-path budget, not a business rejection."
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
    // V3: the programmatic excluded set lived in the Codex rollout scanner
    // (context_fork/codex.rs), excised with the legacy Codex worker. The only
    // remaining verifier proves the NEW session by its exact expected id, so
    // the surviving faces are an explicit NEW == source rejection and no
    // latest-file relaxation anywhere in the module tree.
    let src = read_context_fork_module_text();
    // Face 1 — the verifier explicitly rejects a NEW session equal to the
    // source (whitespace-blind, same projection as tooth 4).
    let rejects_source_collision = code_no_ws(&src).contains("==source_session_id");
    // Face 2 — scanner MUST NOT be relaxed to cwd-latest / mtime-latest.
    // We assert on the negative: no source line inside the module tree
    // may contain `cwd_latest` or `mtime_latest` as a scanner mode.
    let has_relaxation = src.contains("cwd_latest")
        || src.contains("mtime_latest")
        || src.contains("CwdLatest")
        || src.contains("MtimeLatest");
    assert!(
        rejects_source_collision && !has_relaxation,
        "context_fork module tree lets the source masquerade as the NEW backing. \
         locate §5.1+§7#3: a NEW session equal to source_session_id must be \
         rejected explicitly; the verifier must NOT relax to \
         cwd-latest/mtime-latest. rejects_source_collision={} has_relaxation={}",
        rejects_source_collision,
        has_relaxation
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
    // Both hits are taken on the same whitespace-free projection so the
    // ordering below stays comparable and neither pattern is pinned to a
    // particular indentation (0.5.x: rustfmt re-wrapped the chain from 12
    // to 8 spaces and every pattern here went to `None` while the
    // invariant itself still held).
    let scan = code_no_ws(&fork_agent);
    let state_hit = scan
        .find("state.get(\"agents\")")
        .or_else(|| scan.find(".agents.get(source_agent_id"));
    let spec_hit = scan.find("find_spec_agent(&spec,source_agent_id)");
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
    // Tooth 4's matcher must be indentation-blind AND still able to tell
    // state-authority from spec-authority. Independent canary: fixed text,
    // no product source.
    let twelve = "let agent = selected\n            .state\n            .get(\"agents\")";
    let eight = "let agent = selected\n        .state\n        .get(\"agents\")";
    let flat = "let agent = selected.state.get(\"agents\")";
    for (label, text) in [("12", twelve), ("8", eight), ("flat", flat)] {
        assert!(
            code_no_ws(text).contains("state.get(\"agents\")"),
            "state-authority shape must match at indentation `{label}`"
        );
    }
    let spec_authority = "let agent = find_spec_agent(&spec, source_agent_id)";
    assert!(
        !code_no_ws(spec_authority).contains("state.get(\"agents\")")
            && code_no_ws(spec_authority).contains("find_spec_agent(&spec,source_agent_id)"),
        "matcher must still separate spec-authority from state-authority"
    );
    // A `//` inside a string literal must not cost us the rest of the line:
    // ignoring whitespace is allowed, dropping code is not.
    let url_line = "let d = \"https://internal.example/agents\"; let agent = selected.state.get(\"agents\")";
    assert!(
        code_no_ws(url_line).contains("state.get(\"agents\")"),
        "a `//` inside a string literal must not hide the state-authority shape"
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
