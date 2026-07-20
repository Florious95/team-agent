//! Phase-DX E4 RED contract: all-gone recovery short-circuit hints.
//!
//! References:
//! - plan `.team/artifacts/next-version-staged-plan.md` §4 E4 and §5 Phase-DX.
//! - CR `.team/artifacts/phase-dx-invariant-review.md` E4 supplements A/B/C.
//! - CR P0 red lines #4 and #6.
//!
//! Contract: diagnose/status may print actionable recovery text, but must not
//! execute hidden provider checks or recreate anything. Repeated broken-state
//! hints must be deduped.

#![allow(clippy::expect_used)]

#[path = "support/hermetic.rs"]
mod hermetic_guard;
#[allow(dead_code)]
fn _hermetic_boundary_marker(_: &hermetic_guard::HermeticTestEnv) {}

#[path = "support/composite_source.rs"]
mod composite_source;
use std::path::Path;

#[test]
fn e4_all_gone_status_diagnose_outputs_structured_advisory_hint() {
    let diagnose = source("src/cli/diagnose.rs");
    let status = source("src/cli/status_port.rs");
    let coordinator = source("src/coordinator/types.rs");
    let combined = format!("{diagnose}\n{status}\n{coordinator}");
    let mut missing = Vec::new();
    for required in [
        "\"action_required\"",
        "\"advisory\"",
        "\"broken_class\"",
        "\"hint_action\"",
        "team-agent restart",
        "team-agent claim-leader",
        "team-agent quick-start",
        "team-agent attach-leader",
    ] {
        if !combined.contains(required) {
            missing.push(required);
        }
    }

    assert!(
        missing.is_empty(),
        "E4 RED: whole-team-gone/unbound diagnose/status must short-circuit into structured advisory action_required with broken_class, hint_action, and an existing team-agent command. Missing markers: {missing:?}"
    );
}

#[test]
fn e4_diagnose_status_do_not_spawn_provider_binaries() {
    for rel in ["src/cli/diagnose.rs", "src/cli/status_port.rs"] {
        let text = source(rel);
        let offenders = provider_spawn_lines(&text);
        assert!(
            offenders.is_empty(),
            "E4 RED guard: {rel} may print copy-paste hints but must not spawn provider binaries for health checks. Offenders: {offenders:?}"
        );
    }
}

#[test]
fn e4_broken_state_hint_events_are_deduped_by_team_and_class() {
    let combined = format!(
        "{}\n{}",
        source("src/cli/diagnose.rs"),
        source("src/coordinator/types.rs")
    );
    let has_dedupe_contract = combined.contains("broken_class")
        && (combined.contains("dedupe") || combined.contains("dedupe_key"));
    assert!(
        has_dedupe_contract,
        "E4 RED: repeated all-gone broken-state hints must be deduped by team+broken_class so status/diagnose/tick do not nag the leader"
    );
}

fn provider_spawn_lines(text: &str) -> Vec<String> {
    text.lines()
        .filter(|line| {
            let compact = line.split_whitespace().collect::<String>();
            (compact.contains("Command::new(\"codex\")")
                || compact.contains("Command::new(\"claude\")")
                || compact.contains("Command::new(\"copilot\")")
                || compact.contains("process::Command::new(\"codex\")")
                || compact.contains("process::Command::new(\"claude\")")
                || compact.contains("process::Command::new(\"copilot\")"))
                && (compact.contains(".spawn(")
                    || compact.contains(".output(")
                    || compact.contains(".status("))
        })
        .map(ToOwned::to_owned)
        .collect()
}

fn source(rel: &str) -> String {
    composite_source::composite_source(rel)
}
