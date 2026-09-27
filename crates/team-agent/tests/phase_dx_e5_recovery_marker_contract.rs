//! Phase-DX E5 retained guard: workers never infer recovery authority from free text.
//!
//! The legacy MCP `assign_task` surface was retired by the three-tool contract;
//! keep only the independent message-text parsing guard below.

#![allow(clippy::expect_used)]

use std::path::Path;

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
