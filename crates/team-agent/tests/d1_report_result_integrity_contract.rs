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

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use rusqlite::params;
use serde_json::{json, Value};
use team_agent::mcp_server::TeamOrchestratorTools;
use team_agent::messaging;
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

#[test]
fn d1_report_result_integrity_does_not_parse_model_prose_for_evidence() {
    let offenders = prose_parse_offenders(&[
        "src/mcp_server/normalize.rs",
        "src/mcp_server/tools.rs",
        "src/messaging/results.rs",
    ]);

    assert!(
        offenders.is_empty(),
        "D1 RED guard: report_result integrity must use structured tests[]/changes[] fields only; it must not parse summary/prose with regex/tokenization to infer evidence. Offenders: {offenders:#?}"
    );
}

#[test]
fn d1_collect_preserves_warning_envelope_without_verified_upgrade() {
    let ws = temp_workspace("d1-collect");
    seed_collect_workspace(&ws);
    let envelope = json!({
        "schema_version": "result_envelope_v1",
        "result_id": "res_d1_collect_warning",
        "task_id": "task_d1_collect",
        "agent_id": "worker-d1",
        "status": "success",
        "summary": "success with warning-only evidence",
        "changes": [],
        "tests": [{"command": "cargo test --tests", "status": "not_run"}],
        "warnings": [{
            "code": "result_success_without_executed_tests",
            "field": "tests",
            "severity": "warning",
            "advisory": true
        }],
        "risks": [],
        "artifacts": [],
        "next_actions": []
    });

    messaging::report_result(&ws, &envelope).expect("store warning result");
    let out = messaging::collect(&ws, None, false).expect("collect warning result");
    assert_eq!(
        out.get("ok").and_then(Value::as_bool),
        Some(true),
        "D1 RED setup: collect should process the warning-only result fixture; got {out}"
    );

    let (status, stored_envelope) = stored_result(&ws, "res_d1_collect_warning");
    assert_eq!(
        status, "collected",
        "D1 RED setup: collect marks the result collected while preserving envelope evidence"
    );
    assert_warning(
        &stored_envelope,
        "result_success_without_executed_tests",
        "tests",
        "D1 RED: collect must not rewrite warning-only result evidence into verified/success-without-warning",
    );
    assert!(
        stored_envelope.get("verified").is_none()
            && stored_envelope.get("verification_status").and_then(Value::as_str) != Some("verified")
            && stored_envelope.get("evidence_status").and_then(Value::as_str) != Some("verified"),
        "D1 RED: collect must not upgrade warning-only evidence into verified authority; stored envelope: {stored_envelope}"
    );
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

fn source(rel: &str) -> String {
    std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(rel)).unwrap_or_default()
}

fn prose_parse_offenders(files: &[&str]) -> Vec<(String, usize, String)> {
    let risky = [
        "Regex::new",
        "regex::Regex",
        ".split_whitespace(",
        "summary.split",
        "summary.contains(\"test",
        "summary.contains(\"Test",
        "summary.contains(\"change",
        "summary.contains(\"Change",
        "parse_tests_from_summary",
        "parse_changes_from_summary",
        "infer_tests_from_summary",
        "infer_changes_from_summary",
    ];
    let mut offenders = Vec::new();
    for rel in files {
        let text = source(rel);
        for (idx, line) in text.lines().enumerate() {
            if risky.iter().any(|needle| line.contains(needle)) {
                offenders.push(((*rel).to_string(), idx + 1, line.trim().to_string()));
            }
        }
    }
    offenders
}

fn seed_collect_workspace(ws: &Path) {
    std::fs::write(ws.join("team.spec.yaml"), "version: 1\nteam:\n  name: d1\n")
        .expect("write team.spec.yaml");
    team_agent::state::persist::save_runtime_state(
        ws,
        &json!({
            "agents": {"worker-d1": {"status": "idle"}},
            "tasks": [{
                "id": "task_d1_collect",
                "title": "D1 collect warning fixture",
                "type": "test",
                "assignee": "worker-d1",
                "deps": [],
                "acceptance": ["warning envelope preserved"],
                "status": "pending",
                "requires_tools": [],
                "files": [],
                "risk": "low"
            }],
            "session_name": Value::Null,
            "active_team_key": Value::Null,
            "spec_path": ws.join("team.spec.yaml").to_string_lossy()
        }),
    )
    .expect("seed collect runtime state");
}

fn stored_result(ws: &Path, result_id: &str) -> (String, Value) {
    let store = team_agent::message_store::MessageStore::open(ws).expect("open message store");
    let conn = team_agent::db::schema::open_db(store.db_path()).expect("open db");
    let (status, envelope): (String, String) = conn
        .query_row(
            "select status, envelope from results where result_id = ?1",
            params![result_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("read stored result");
    (
        status,
        serde_json::from_str(&envelope).expect("stored envelope json"),
    )
}
