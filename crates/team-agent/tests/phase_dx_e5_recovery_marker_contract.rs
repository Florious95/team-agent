//! Phase-DX E5 RED contract: recovery-task acceptance marker.
//!
//! References:
//! - plan `.team/artifacts/next-version-staged-plan.md` §4 E5 and §5 Phase-DX.
//! - CR `.team/artifacts/phase-dx-invariant-review.md` E5 supplements A/B/C.
//! - CR P0 red lines #3 and #6.
//!
//! Contract: recovery acceptance is a structured marker on recovery-originated
//! tasks only. Ordinary send/assign output must not leak it, and workers must
//! not parse free-text prefixes as authority.

#![allow(clippy::expect_used)]

use std::path::Path;

#[test]
fn e5_recovery_originated_task_shape_has_structured_marker() {
    let tools = source("src/mcp_server/tools.rs");
    let types = source("src/mcp_server/types.rs");
    let delivery = source("src/messaging/delivery.rs");
    let combined = format!("{tools}\n{types}\n{delivery}");

    let has_structured_marker = combined.contains("\"recovery\"")
        && combined.contains("json!(true)")
        && combined.contains("acceptance_marker");

    assert!(
        has_structured_marker,
        "E5 RED: recovery/restart-generated task delivery must expose a structured top-level marker such as recovery=true or acceptance_marker=\"recovery\"; natural-language prefixes are not authority"
    );
}

#[test]
fn e5_ordinary_send_assign_shape_has_no_recovery_marker() {
    let mcp_tests = source_tree("src/mcp_server/tests");
    let has_reverse_contract = mcp_tests.contains("ordinary_send")
        && mcp_tests.contains("contains_key(\"recovery\")")
        && mcp_tests.contains("false");

    assert!(
        has_reverse_contract,
        "E5 RED: MCP send/assign tests must include the reverse case: ordinary send has no recovery marker or recovery=false, while only recovery-originated tasks carry recovery=true"
    );
}

#[test]
fn e5_worker_side_must_not_parse_recovery_from_message_text_prefix() {
    let all = source_tree("src");
    for forbidden in [
        "[RECOVERY]",
        "RECOVERY:",
        "starts_with(\"[RECOVERY]\")",
        "contains(\"[RECOVERY]\")",
    ] {
        assert!(
            !all.contains(forbidden),
            "E5 RED guard: recovery marker must be read from structured fields, not regex/string prefixes in message text; forbidden={forbidden}"
        );
    }
}

fn source(rel: &str) -> String {
    std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(rel)).expect("read source")
}

fn source_tree(rel: &str) -> String {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join(rel);
    let mut out = String::new();
    append_rs_sources(&root, &mut out);
    out
}

fn append_rs_sources(path: &Path, out: &mut String) {
    if path.is_dir() {
        let mut entries = std::fs::read_dir(path)
            .expect("read source dir")
            .map(|entry| entry.expect("read source entry").path())
            .collect::<Vec<_>>();
        entries.sort();
        for entry in entries {
            append_rs_sources(&entry, out);
        }
        return;
    }
    if path.extension().and_then(|v| v.to_str()) == Some("rs") {
        out.push_str(&std::fs::read_to_string(path).expect("read source file"));
        out.push('\n');
    }
}
