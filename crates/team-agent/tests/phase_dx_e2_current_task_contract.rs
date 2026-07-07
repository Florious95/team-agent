//! Phase-DX E2 RED contract: current_task and heartbeat status display.
//!
//! References:
//! - plan `.team/artifacts/next-version-staged-plan.md` §4 E2 and §5 Phase-DX.
//! - CR `.team/artifacts/phase-dx-invariant-review.md` E2 supplements A/B.
//! - CR P0 red lines #5 and #6.
//!
//! Contract: current_task is an honest best-effort display field in Phase-DX.
//! It is not an authoritative task FSM and must not be consumed by delivery,
//! collect, or result attribution logic.

#![allow(clippy::expect_used)]

use std::path::{Path, PathBuf};

#[test]
fn e2_status_json_marks_current_task_as_structured_best_effort() {
    let status_port = source("src/cli/status_port.rs");
    let mut missing = Vec::new();
    for required in [
        "\"current_task_id\"",
        "\"last_output_at\"",
        "\"health_updated_at\"",
        "\"current_task_source\"",
        "\"current_task_confidence\"",
        "best_effort",
    ] {
        if !status_port.contains(required) {
            missing.push(required);
        }
    }

    assert!(
        missing.is_empty(),
        "E2 RED: status --json --agent must include current_task_id, last_output_at, health_updated_at, and structured current_task_source/current_task_confidence=best_effort. Missing markers: {missing:?}"
    );
}

#[test]
fn e2_current_task_health_reads_are_display_only() {
    let mut offenders = Vec::new();
    for (rel, text) in rs_sources("src") {
        if rel.contains("/tests/") || rel.ends_with("/tests.rs") {
            continue;
        }
        if !text.contains("current_task_id") {
            continue;
        }
        let allowed = rel == "src/cli/status_port.rs"
            || rel == "src/cli/diagnose.rs"
            || rel == "src/coordinator/tick.rs"
            || rel == "src/db/schema.rs"
            || rel == "src/db/migration.rs";
        if !allowed
            && (rel.contains("messaging")
                || rel.contains("mcp_server")
                || rel.contains("lifecycle"))
        {
            offenders.push(rel);
        }
    }

    assert!(
        offenders.is_empty(),
        "E2 RED guard: agent_health.current_task_id is Phase-DX display-only; messaging/results/collect/lifecycle must not read it as authoritative task state. Offenders: {offenders:?}"
    );
}

fn source(rel: &str) -> String {
    std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(rel)).expect("read source")
}

fn rs_sources(rel: &str) -> Vec<(String, String)> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join(rel);
    let mut out = Vec::new();
    append_rs_sources(&root, Path::new(env!("CARGO_MANIFEST_DIR")), &mut out);
    out
}

fn append_rs_sources(path: &Path, base: &Path, out: &mut Vec<(String, String)>) {
    if path.is_dir() {
        let mut entries = std::fs::read_dir(path)
            .expect("read source dir")
            .map(|entry| entry.expect("read source entry").path())
            .collect::<Vec<PathBuf>>();
        entries.sort();
        for entry in entries {
            append_rs_sources(&entry, base, out);
        }
        return;
    }
    if path.extension().and_then(|v| v.to_str()) == Some("rs") {
        let rel = path
            .strip_prefix(base)
            .unwrap_or(path)
            .to_string_lossy()
            .to_string();
        out.push((
            rel,
            std::fs::read_to_string(path).expect("read source file"),
        ));
    }
}
