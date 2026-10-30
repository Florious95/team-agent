//! Restart must preserve provider session context before destructive teardown.
//!
//! #240 root cause: `restart`/`shutdown` decide or kill from runtime state before
//! force-refreshing missing provider `session_id`s. A real provider session can exist under the
//! provider home while `state.agents[*].session_id` is still null; destructive lifecycle must capture
//! it first and resume from that durable id.
//!
//! The Codex-home rollout fixtures left with the legacy Codex worker runtime;
//! the remaining checks are source guards over the capture/convergence seams.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[path = "../../../tests/support/hermetic.rs"]
mod hermetic_guard;
#[allow(dead_code)]
fn _hermetic_boundary_marker(_: &hermetic_guard::HermeticTestEnv) {}

#[path = "../../../tests/support/composite_source.rs"]
mod composite_source;

#[test]
fn status_and_quick_start_surfaces_include_session_capture_completeness() {
    let diagnose =
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/cli/diagnose.rs"))
            .unwrap();
    let status = composite_source::composite_source("src/cli/status_port.rs");
    let launch = composite_source::composite_source("src/lifecycle/launch.rs");
    let adapters =
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/cli/adapters.rs"))
            .unwrap();

    for (name, source) in [
        ("status readiness formula", diagnose.as_str()),
        ("status --json", status.as_str()),
        ("quick-start readiness", launch.as_str()),
        ("quick-start --json", adapters.as_str()),
    ] {
        assert!(
            source.contains("session_capture_incomplete")
                && source.contains("all_resumable_have_session"),
            "C6-C8: {name} must expose readiness = all spawned + attached receiver + all resumable agents have sessions; source lacks session completeness guard"
        );
    }
}

#[test]
fn restart_session_convergence_deadline_is_configurable_and_bounded() {
    let restart = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/lifecycle/restart/rebuild.rs"
    ))
    .unwrap();
    let types =
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/cli/types.rs")).unwrap();
    let emit =
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/cli/emit.rs")).unwrap();

    assert!(
        restart.contains("session_converge_deadline")
            || restart.contains("SessionConvergenceDeadline"),
        "C14: restart must have a bounded session convergence deadline, not one opportunistic pass or an unbounded wait"
    );
    assert!(
        restart.contains("10") || restart.contains("15"),
        "C14: default convergence deadline should be a bounded 10-15s class value; restart={restart}"
    );
    assert!(
        types.contains("session_converge_deadline")
            && emit.contains("--session-converge-deadline")
            && emit.contains("deadline"),
        "C14: tests/scenarios need an explicit deadline knob; CLI types/parser must expose --session-converge-deadline for restart"
    );
}

#[test]
fn attribution_ambiguous_is_final_only_after_convergence_deadline() {
    let common = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/lifecycle/restart/common.rs"
    ))
    .unwrap();

    assert!(
        !common.contains(r#""attribution_ambiguous""#)
            || common.contains("deadline_expired")
            || common.contains("final_ambiguous"),
        "C9-C11/C16: restart capture must not durably write attribution_ambiguous during an in-window poll; final ambiguous is allowed only after deadline with unallocatable candidates"
    );
    // M3 excision: the coordinator tick and its opportunistic capture are retired (duty 8 moved to
    // the spawn receipt plus lazy capture), so the former "tick capture must not be deleted" clause
    // has no subject.
}

#[test]
fn session_capture_enumerates_candidates_and_carries_per_agent_context_grep_guard() {
    let adapter = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/provider/adapter.rs"
    ))
    .unwrap();
    let restart = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/lifecycle/restart/common.rs"
    ))
    .unwrap();
    let launch = composite_source::composite_source("src/lifecycle/launch.rs");

    assert!(
        adapter.contains("capture_session_candidates")
            || adapter.contains("matching_session_candidates")
            || adapter.contains("SessionAttributionContext"),
        "C9: provider capture must expose/enumerate all cwd-matching session candidates instead of returning only the first path-ordered candidate"
    );
    assert!(
        !restart.contains(".capture_session_id("),
        "C9/C10 (M3: the coordinator tick is retired): restart refresh must allocate across all candidates with per-agent context, not call capture_session_id(agent,cwd) one agent at a time"
    );
    assert!(
        adapter.contains("pane_pid") && adapter.contains("spawned_at") && adapter.contains("TEAM_AGENT_ID"),
        "C10/C11: attribution context/decision must include pane_pid, per-agent spawned_at/process time, and positive MCP env TEAM_AGENT_ID before bounded fallback"
    );
    assert!(
        launch.contains("spawned_at") && !launch.contains("let spawned_at = spawn_timestamp();"),
        "C10: launch must persist a per-agent spawned_at/process time, not one shared timestamp for the whole launch"
    );
}
