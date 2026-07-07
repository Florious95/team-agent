//! D1 RED contract: report_result integrity warning-only gate.
//!
//! References:
//! - plan `.team/artifacts/next-version-staged-plan.md` §4 D and §5 Phase-Integrity.
//! - leader dispatch for 0.5.9 train: D1 warning-only, not strict reject.
//! - CR red-line spirit from `.team/artifacts/phase-dx-invariant-review.md`:
//!   warnings are diagnostic/advisory and must not become task attribution authority.
//!
//! Contract: in D1, low-evidence `success` reports remain accepted but return an
//! explicit structured `warnings[]` array. Partial/blocked reports with not-run
//! tests are accepted and honestly marked as unverified.

#![allow(clippy::expect_used)]

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{json, Value};
use team_agent::mcp_server::TeamOrchestratorTools;
use team_agent::model::enums::ResultStatus;
use team_agent::model::ids::{AgentId, TeamKey};

static COUNTER: AtomicU64 = AtomicU64::new(0);

#[test]
fn d1_success_with_all_not_run_tests_returns_ok_true_with_warning() {
    let out = report(json!({
        "schema_version": "result_envelope_v1",
        "result_id": "res_d1_all_not_run",
        "task_id": "task_d1",
        "agent_id": "worker-d1",
        "status": "success",
        "summary": "success claimed without executed tests",
        "changes": [],
        "tests": [{"command": "cargo test --tests", "status": "not_run"}],
        "risks": [],
        "artifacts": [],
        "next_actions": []
    }));

    assert_eq!(out.get("ok").and_then(Value::as_bool), Some(true));
    assert_warning(
        &out,
        "result_success_without_executed_tests",
        "tests",
        "D1 RED: success + all tests not_run must stay ok:true in warning-only mode, but must include warnings[] with code=result_success_without_executed_tests, field=tests, severity=warning, advisory=true",
    );
}

#[test]
fn d1_success_with_scalar_tests_normalized_to_not_run_warns() {
    let out = report(json!({
        "schema_version": "result_envelope_v1",
        "result_id": "res_d1_scalar_tests",
        "task_id": "task_d1",
        "agent_id": "worker-d1",
        "status": "success",
        "summary": "scalar tests are not execution evidence",
        "changes": [],
        "tests": ["cargo test --tests"],
        "risks": [],
        "artifacts": [],
        "next_actions": []
    }));

    assert_eq!(out.get("ok").and_then(Value::as_bool), Some(true));
    assert_warning(
        &out,
        "result_success_without_executed_tests",
        "tests",
        "D1 RED: scalar tests normalize to status=not_run, so success + scalar tests must return the same structured warnings[] as explicit all-not_run tests",
    );
}

#[test]
fn d1_success_with_change_path_missing_description_warns() {
    let out = report(json!({
        "schema_version": "result_envelope_v1",
        "result_id": "res_d1_change_no_description",
        "task_id": "task_d1",
        "agent_id": "worker-d1",
        "status": "success",
        "summary": "changed one file",
        "changes": [{"path": "src/lib.rs"}],
        "tests": [{"command": "cargo test --tests", "status": "passed"}],
        "risks": [],
        "artifacts": [],
        "next_actions": []
    }));

    assert_eq!(out.get("ok").and_then(Value::as_bool), Some(true));
    assert_warning(
        &out,
        "result_change_missing_description",
        "changes",
        "D1 RED: success + changes[] path without a short description must return a structured warning code=result_change_missing_description instead of silently treating the path as the description",
    );
}

#[test]
fn d1_partial_or_blocked_with_not_run_tests_is_accepted_but_marked_unverified() {
    for (status, result_id) in [
        ("partial", "res_d1_partial_not_run"),
        ("blocked", "res_d1_blocked_not_run"),
    ] {
        let out = report(json!({
            "schema_version": "result_envelope_v1",
            "result_id": result_id,
            "task_id": "task_d1",
            "agent_id": "worker-d1",
            "status": status,
            "summary": format!("{status} with no executed tests"),
            "changes": [],
            "tests": [{"command": "cargo test --tests", "status": "not_run"}],
            "risks": [],
            "artifacts": [],
            "next_actions": []
        }));

        assert_eq!(
            out.get("ok").and_then(Value::as_bool),
            Some(true),
            "D1 RED: {status}+not_run remains accepted in warning-only mode; got {out}"
        );
        assert_warning(
            &out,
            "result_not_verified",
            "tests",
            "D1 RED: partial/blocked + not_run tests must be accepted but must warn result_not_verified so the caller cannot mistake it for verified evidence",
        );
    }
}

fn report(envelope: Value) -> Value {
    let ws = temp_workspace("d1");
    let tools = TeamOrchestratorTools::with_identity(
        &ws,
        Some(AgentId::new("worker-d1")),
        Some(TeamKey::new("current")),
    );
    let ok = tools
        .report_result(
            Some(&envelope),
            None,
            ResultStatus::Success,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .expect("D1 contract setup: report_result should accept the envelope");
    Value::Object(ok.fields)
}

fn assert_warning(out: &Value, code: &str, field: &str, red_reason: &str) {
    let warnings = out
        .get("warnings")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("{red_reason}; missing warnings[] in output: {out}"));
    let matched = warnings.iter().any(|warning| {
        warning.get("code").and_then(Value::as_str) == Some(code)
            && warning.get("field").and_then(Value::as_str) == Some(field)
            && warning.get("severity").and_then(Value::as_str) == Some("warning")
            && warning.get("advisory").and_then(Value::as_bool) == Some(true)
    });
    assert!(
        matched,
        "{red_reason}; warnings[] must contain {{code:{code}, field:{field}, severity:warning, advisory:true}}; got {warnings:?}"
    );
}

fn temp_workspace(tag: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!("ta-059-{tag}-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).expect("create temp workspace");
    path
}
