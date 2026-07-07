//! Phase-DX E1 RED contract: status stale-health honesty.
//!
//! References:
//! - plan `.team/artifacts/next-version-staged-plan.md` §4 E1 and §5 Phase-DX.
//! - CR `.team/artifacts/phase-dx-invariant-review.md` E1 supplements A/B.
//! - CR P0 red lines #2 and #6.
//!
//! Contract: stale status is a diagnostic surface backed by physical liveness
//! facts. It must expose structured stale fields without mutating lifecycle
//! `agents.<id>.status`.

#![allow(clippy::expect_used)]

use std::path::Path;

#[test]
fn e1_status_json_exposes_structured_stale_reason_from_physical_liveness() {
    let status_port = source("src/cli/status_port.rs");
    let tick = source("src/coordinator/tick.rs");
    let combined = format!("{status_port}\n{tick}");

    let mut missing = Vec::new();
    for required in [
        "\"stale\"",
        "\"stale_reason\"",
        "process_dead",
        "pane_dead",
        "both",
    ] {
        if !combined.contains(required) {
            missing.push(required);
        }
    }

    assert!(
        missing.is_empty(),
        "E1 RED: status --json must surface stale=true plus structured stale_reason=(process_dead|pane_dead|both) from physical process/pane liveness, even when agent_health.updated_at is recent. Missing markers: {missing:?}"
    );
}

#[test]
fn e1_stale_detection_must_not_write_lifecycle_agent_status() {
    for rel in [
        "src/cli/status_port.rs",
        "src/cli/diagnose.rs",
        "src/coordinator/tick.rs",
    ] {
        let text = source(rel);
        assert!(
            !contains_stale_status_write(&text),
            "E1 RED guard: {rel} stale diagnostic path must not rewrite lifecycle agents.<id>.status; stale is observational, not a stop/restart side effect"
        );
    }
}

fn contains_stale_status_write(text: &str) -> bool {
    text.lines().any(|line| {
        let compact = line.split_whitespace().collect::<String>();
        let mentions_stale = compact.contains("stale") || compact.contains("STALE");
        let writes_status = compact.contains("[\"status\"]")
            || compact.contains(".status=")
            || compact.contains("status\":\"stopped")
            || compact.contains("status\":\"STALE");
        mentions_stale && writes_status
    })
}

fn source(rel: &str) -> String {
    std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(rel)).expect("read source")
}
