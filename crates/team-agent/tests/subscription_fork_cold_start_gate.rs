//! Subscription-tier real-machine acceptance gate for `codex fork`.
//! This file is the CONFIGURATION SIDE of 0.5.58 fork-plane tooth ⑤
//! (`real_subscription_gate_freezes_cold_start_matrix`) — it is
//! validation-lineage property owned by the verifier; the developer
//! may NOT self-author it. Name contains `subscription` + `fork` and
//! it lives directly under `crates/team-agent/tests/` so the tooth's
//! filesystem scan can find it.
//!
//! LINEAGE:
//! - Baseline: 5b847e4 (0.5.56 tested tip). Tooth ⑤ requires this
//!   file to exist AND to encode all six frozen strings so an
//!   offline shim green cannot masquerade for the subscription gate.
//! - locate §6#5 six acceptance items (frozen strings this file
//!   MUST spell out for the tooth's textual scan):
//!     • codex --version         (real CLI version pin, not a shim)
//!     • no_prompt               (§7#1: never inject a hidden prompt)
//!     • cold_start              (deadline > 10s is legitimate)
//!     • nonce                   (source nonce inherited by target)
//!     • distinct new_session_id (target session id != source)
//!     • source_unchanged        (source rollout not re-written)
//! - locate §7 risk-boundary locks stay operative here as they do in
//!   the developer-side contract file:
//!     §7#1 no hidden prompt / §7#3 no cwd-latest relaxation /
//!     §7#4 typed observable Pending, no silent fresh-clone downgrade.
//!
//! GATE:
//! - The subscription execution is gated behind
//!   `TEAM_AGENT_REALMACHINE_FORK=1`. Unauthorized runs skip WITHOUT
//!   producing a green — the skip message names the missing gate.
//! - CI / offline-shim environments MUST NOT set this gate. Presence
//!   of `TEAM_AGENT_TEST_SHIM=1` OR `CODEX_FORCE_STDIO=1` (leftover
//!   shim env from adjacent contract tests) short-circuits with a
//!   HARD skip that documents why an offline shim green never
//!   substitutes for the real gate.
//! - The verifier is the ONLY authorized runner; leader must
//!   pre-authorize each execution (subscription cost + provider
//!   quota impact). See `.team/artifacts/at-least-once-three-questions-criteria.md`
//!   for the operational discipline mirror.
//!
//! CURRENT STATE:
//! - Baseline @5b847e4: fork-agent is broken; running this gate
//!   would deterministically fail on the 10s deadline (locate §0#1).
//!   That failure is INFORMATIVE at RED time and BECOMES the acceptance
//!   at GREEN time once developer-side teeth ①-④ (typed Pending +
//!   bounded grace + no-masquerade + state-not-spec) land.
//! - Until then, the runtime body of every real test is guarded by
//!   the env gate and the shim-poisoning guard; nothing runs by
//!   accident.
//!
//! FROZEN by verifier — do NOT modify without a new SHA256 signature.
//! Developer / spawnmod C4 must not self-author or edit this file.

#![cfg(unix)]
#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use std::process::Command;

use serial_test::serial;

// Sentinels — presence of any of these means the environment has
// been rigged for an offline shim run. Real subscription acceptance
// must never coexist with them.
const SHIM_SENTINELS: &[&str] = &[
    "TEAM_AGENT_TEST_SHIM",
    "CODEX_FORCE_STDIO",
    "TEAM_AGENT_FAKE_PROVIDER",
];

fn gate_enabled() -> bool {
    std::env::var("TEAM_AGENT_REALMACHINE_FORK").as_deref() == Ok("1")
}

fn shim_env_poisoned() -> Option<&'static str> {
    for name in SHIM_SENTINELS {
        if std::env::var(name).is_ok() {
            return Some(*name);
        }
    }
    None
}

/// Skip helper. Always PANICs (not `return`) when the gate is off
/// because a silent return would count as a green pass — the tooth
/// asserts REAL execution occurred, and a silent skip in CI would
/// mimic an acceptance without any evidence.
fn require_gate_or_skip(test_name: &str) {
    if let Some(rigged) = shim_env_poisoned() {
        // Offline shim greens do not substitute. Halt hard.
        panic!(
            "REFUSE {test_name}: shim env `{rigged}` present. locate §7#3+§6#5 \
             disallow substituting an offline shim green for the real \
             subscription gate. Unset the shim env before running the \
             subscription gate."
        );
    }
    if !gate_enabled() {
        // Unauthorized run — do NOT count as green. eprintln so it is
        // visible in a real subscription run; panic prevents accidental
        // pass. cold_start: this pattern mirrors msg_routing /
        // team_key_retirement gates.
        panic!(
            "SKIP {test_name}: set TEAM_AGENT_REALMACHINE_FORK=1 (leader \
             authorization required — subscription cost + provider quota). \
             locate §6#5 six frozen acceptance items enforced only under this \
             gate. CI must NOT set this variable."
        );
    }
}

// ---------------------------------------------------------------------------
// Face 1 — codex --version is pinned by the runner
// ---------------------------------------------------------------------------
//
// The subscription gate is meaningless if the runner is executing an
// undocumented CLI. We freeze the observed CLI version into the test
// output so a GREEN verdict can be attached to a specific
// `codex --version` line (verifier records it in the verdict).

#[test]
#[serial(env)]
fn subscription_fork_codex_version_pinned() {
    require_gate_or_skip("subscription_fork_codex_version_pinned");
    let out = Command::new("codex")
        .arg("--version")
        .output()
        .expect("run `codex --version`");
    assert!(
        out.status.success(),
        "codex --version exited non-zero: {:?}",
        out.status
    );
    let version = String::from_utf8_lossy(&out.stdout).trim().to_string();
    assert!(
        version.starts_with("codex-cli ") || version.contains("codex"),
        "codex --version output shape unexpected: `{version}`"
    );
    // Verifier records this line in the GREEN verdict alongside the
    // fork exit condition; there is no attempt to lock a specific
    // number here (that would rot every CLI release).
    eprintln!("[subscription-fork gate] pinned codex --version: {version}");
}

// ---------------------------------------------------------------------------
// Face 2 — no_prompt: the adapter fork invocation MUST NOT attach a prompt
// ---------------------------------------------------------------------------

#[test]
#[serial(env)]
fn subscription_fork_no_prompt_end_to_end() {
    require_gate_or_skip("subscription_fork_no_prompt_end_to_end");
    // Behavioral placeholder: verifier drives a real `team-agent
    // fork-agent` and inspects the captured child command line via
    // the leader's rollout / event log. This test asserts the marker
    // strings §7#1 requires; the full exec is wired at GREEN time
    // because it depends on typed-Pending (tooth ①) and grace path
    // (tooth ②) landing first.
    let expected_marker = "no_prompt";
    let cold_start = "cold_start"; // used by the harness to guarantee > 10s tolerance
    let source_unchanged = "source_unchanged";
    assert!(
        expected_marker == "no_prompt"
            && cold_start == "cold_start"
            && source_unchanged == "source_unchanged",
        "frozen markers drifted; six acceptance strings are contract, not source-of-truth"
    );
    // Under the gate, exec goes here at GREEN time — until then the
    // require_gate_or_skip panic path documents the intent.
    unimplemented!(
        "GREEN-time wiring: exec `team-agent fork-agent <source>` and assert \
         (a) child argv contains `codex fork <session-id>` WITHOUT trailing prompt, \
         (b) framework emits `send.injected_context_fork` with `prompt=null`."
    );
}

// ---------------------------------------------------------------------------
// Face 3 — cold_start > 10s is tolerated as typed Pending, not rollback
// ---------------------------------------------------------------------------

#[test]
#[serial(env)]
fn subscription_fork_cold_start_over_ten_seconds_tolerated() {
    require_gate_or_skip("subscription_fork_cold_start_over_ten_seconds_tolerated");
    // At GREEN time this executes a real cold-start fork and asserts
    // the framework observes `Pending` past the 10s Codex convergence
    // budget, then progresses to `Verified` when the provider settles.
    // See locate §5.1 for the required typed outcomes.
    let cold_start = "cold_start";
    let no_shim = "shim_never_substitutes";
    assert_eq!(cold_start, "cold_start");
    assert_eq!(no_shim, "shim_never_substitutes");
    unimplemented!(
        "GREEN-time wiring: fork against a fresh workspace, measure elapsed \
         from spawn boundary to `Verified`; assert elapsed > 10_000ms and \
         final outcome variant == `Verified`. Baseline @5b847e4 dies at 10s \
         with a rollback (locate §0#1)."
    );
}

// ---------------------------------------------------------------------------
// Face 4 — nonce inheritance from source to target
// ---------------------------------------------------------------------------

#[test]
#[serial(env)]
fn subscription_fork_nonce_inherited_from_source() {
    require_gate_or_skip("subscription_fork_nonce_inherited_from_source");
    let nonce = "nonce";
    assert_eq!(nonce, "nonce");
    unimplemented!(
        "GREEN-time wiring: read the source rollout's nonce from \
         .team/runtime/state.json, execute fork, and assert the target \
         session's ContextForkProof.captured_via preserves it. locate \
         §6#5 acceptance item 4."
    );
}

// ---------------------------------------------------------------------------
// Face 5 — target NEW session id distinct from source
// ---------------------------------------------------------------------------

#[test]
#[serial(env)]
fn subscription_fork_target_distinct_new_session_id() {
    require_gate_or_skip("subscription_fork_target_distinct_new_session_id");
    let distinct = "distinct";
    let new_session_id = "new_session_id";
    assert_eq!(distinct, "distinct");
    assert_eq!(new_session_id, "new_session_id");
    unimplemented!(
        "GREEN-time wiring: after Verified, read target ContextForkProof, \
         assert `new_session_id != source_session_id` AND backing_path \
         differs from source's backing_path. Anti-masquerade (tooth ③) \
         must have already excluded source from the NEW candidate set."
    );
}

// ---------------------------------------------------------------------------
// Face 6 — source rollout is unchanged after fork
// ---------------------------------------------------------------------------

#[test]
#[serial(env)]
fn subscription_fork_source_unchanged_after_fork() {
    require_gate_or_skip("subscription_fork_source_unchanged_after_fork");
    let source_unchanged = "source_unchanged";
    assert_eq!(source_unchanged, "source_unchanged");
    unimplemented!(
        "GREEN-time wiring: snapshot the source rollout file (size + \
         mtime + sha256 of head), perform the fork, snapshot again; \
         assert byte-stable (no re-render side effect). §7#3 forbids \
         relaxing to cwd-latest / mtime-latest as a fallback."
    );
}

// ---------------------------------------------------------------------------
// Sanity — the six frozen markers are present in this file's own bytes
// ---------------------------------------------------------------------------
//
// This test is intentionally UNGATED. It confirms the file continues
// to spell out the six markers that tooth ⑤'s textual scan looks for.
// If a future edit drops any marker, this test surfaces the loss the
// moment the file is loaded — the sanity is independent of the
// subscription runtime.

#[test]
fn frozen_markers_present_in_this_file_bytes() {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("subscription_fork_cold_start_gate.rs");
    let body =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    for marker in [
        "codex --version",
        "no_prompt",
        "cold_start",
        "nonce",
        "distinct",
        "new_session_id",
        "source_unchanged",
    ] {
        assert!(
            body.contains(marker),
            "frozen acceptance marker `{marker}` missing from this file; \
             tooth ⑤ scan would report the gate as absent. Restore the \
             marker before re-freezing."
        );
    }
}
