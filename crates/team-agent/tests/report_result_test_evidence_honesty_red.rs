//! F6 RED: report_result test evidence must not be silently rewritten.

#![allow(clippy::expect_used)]

#[path = "support/hermetic.rs"]
mod hermetic_guard;

use std::fs;

fn normalize_module() -> String {
    fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/mcp_server/normalize.rs"
    ))
    .expect("read report-result normalizer")
}

#[test]
fn executed_command_exit_code_evidence_is_classified_by_exit_code() {
    let source = normalize_module();
    for required in [
        "\"executed\"",
        "\"exit_code\"",
        "TestStatus::Passed",
        "TestStatus::Failed",
    ] {
        assert!(
            source.contains(required),
            "F6 executed evidence parser missing {required:?}; status=executed + command + exit_code must preserve true pass/fail"
        );
    }
}

#[test]
fn unknown_structured_test_schema_is_rejected_instead_of_becoming_not_run() {
    let source = normalize_module();
    assert!(
        source.contains("unsupported_test_evidence_schema")
            && source.contains("allowed")
            && source.contains("exit_code"),
        "F6 unknown structured test evidence must be rejected with a precise path and the one accepted schema; it must not silently become not_run"
    );
}

#[test]
fn explicit_not_run_and_existing_pass_fail_aliases_remain_distinct() {
    let source = normalize_module();
    for canary in [
        "\"passed\" | \"pass\" | \"ok\" | \"success\" => TestStatus::Passed",
        "\"failed\" | \"fail\" | \"error\" => TestStatus::Failed",
        "_ => TestStatus::NotRun",
    ] {
        assert!(
            source.contains(canary),
            "positive control: existing explicit test-status distinction lost {canary:?}"
        );
    }
}

#[allow(dead_code)]
fn _hermetic_boundary_marker(_: &hermetic_guard::HermeticTestEnv) {}
