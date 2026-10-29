//! 0.5.61 RED: a worker's structured `report_result.envelope` remains canonical
//! through real MCP stdio ingress and the append-only leader inbox.
//!
//! Requirements:
//! - `result_route` B03: the worker reports facts once and the result is durable first.
//! - `result_route` B06/F6: the framework transports result facts; it does not silently
//!   reinterpret or discard accepted structured content.
//! - Rejected input must not append a partial or cross-scope inbox record.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[path = "support/mcp_sim_harness.rs"]
#[allow(dead_code)]
mod mcp_sim_harness;

#[path = "support/hermetic.rs"]
mod hermetic_guard;

use std::collections::BTreeSet;

use hermetic_guard::HermeticTestEnv;
use mcp_sim_harness::McpSimHarness;
use serde_json::{json, Value};
use serial_test::serial;

const EXTENSION_KEYS: [&str; 3] = ["answer", "evidence", "refs"];
const FRAMEWORK_KEYS: [&str; 14] = [
    "schema_version",
    "result_id",
    "task_id",
    "agent_id",
    "status",
    "summary",
    "changes",
    "tests",
    "risks",
    "artifacts",
    "next_actions",
    "presentation",
    "warnings",
    "owner_team_id",
];

#[test]
#[serial(envelope_fidelity)]
fn tooth1_mcp_report_result_preserves_nested_extension_fields_in_inbox() {
    let _hermetic = HermeticTestEnv::enter("envelope-fidelity-ingress");
    let harness = McpSimHarness::new();
    let mut worker = harness.spawn_mcp_client("worker_a", "teamA");
    let submitted = fidelity_envelope("INGRESS");

    let call = worker.call_tool("report_result", json!({"envelope": submitted.clone()}));
    assert!(
        !call.is_error,
        "tooth1 setup: the documented nested envelope input must be accepted; body={} raw={}",
        call.body, call.raw
    );
    let message_id = call.body["message_id"]
        .as_str()
        .expect("accepted report_result must name its appended inbox record");
    let row = inbox_record(&harness, message_id);
    assert_eq!(row["sender"], "worker_a");
    assert_eq!(row["recipient"], "leader");
    assert_eq!(row["team"], "teamA");
    assert_eq!(row["kind"], "result");
    let stored = row["envelope"].clone();

    assert_eq!(
        stored["summary"], submitted["summary"],
        "canonical summary proves the submitted result reached the inbox"
    );
    assert_eq!(row["message_id"], json!(message_id));

    let mut failures = Vec::new();
    for field in EXTENSION_KEYS {
        if stored.get(field) != submitted.get(field) {
            failures.push(format!(
                "lost or changed extension field `{field}`: expected_type={} actual_type={}",
                value_kind(submitted.get(field)),
                value_kind(stored.get(field))
            ));
        }
    }
    let actual_extension_keys = stored
        .as_object()
        .expect("stored envelope object")
        .keys()
        .filter(|key| !FRAMEWORK_KEYS.contains(&key.as_str()))
        .cloned()
        .collect::<BTreeSet<_>>();
    let expected_extension_keys = EXTENSION_KEYS
        .into_iter()
        .map(str::to_string)
        .collect::<BTreeSet<_>>();
    if actual_extension_keys != expected_extension_keys {
        failures.push(format!(
            "extension key set changed (no extra, missing, or renamed keys allowed): expected={expected_extension_keys:?} actual={actual_extension_keys:?}"
        ));
    }
    assert!(
        failures.is_empty(),
        "tooth1 RED — MCP typed normalization narrowed an accepted nested envelope before persistence:\n{}",
        failures.join("\n")
    );
}

#[test]
#[serial(envelope_fidelity)]
fn tooth2_repeated_reports_append_without_collect_or_narrowing_the_envelopes() {
    let _hermetic = HermeticTestEnv::enter("envelope-fidelity-collect");
    let harness = McpSimHarness::new();
    harness.prepare_collect();
    let mut worker = harness.spawn_mcp_client("worker_a", "teamA");
    let submitted = fidelity_envelope("COLLECT");

    let call = worker.call_tool("report_result", json!({"envelope": submitted}));
    assert!(
        !call.is_error,
        "tooth2 setup: report_result must reach persistence; body={} raw={}",
        call.body, call.raw
    );
    let message_id = call.body["message_id"].as_str().expect("inbox message id");
    let first = inbox_record(&harness, message_id);
    let persisted = first["envelope"].clone();
    let inbox = harness.workspace_path().join(".team/inbox/leader.jsonl");
    let before = std::fs::read(&inbox).expect("durable inbox");
    let mut expected = submitted.clone();
    let expected_object = expected
        .as_object_mut()
        .expect("tooth2 submitted envelope object");
    expected_object.insert(
        "presentation".to_string(),
        persisted["presentation"].clone(),
    );
    assert_eq!(
        persisted, expected,
        "the inbox must retain the full accepted envelope without collect-time projection"
    );
    assert_eq!(inbox_record(&harness, message_id), first);
    assert_eq!(std::fs::read(&inbox).unwrap(), before, "reading does not collect or consume");

    let next = fidelity_envelope("APPENDED");
    let call = worker.call_tool("report_result", json!({"envelope": next}));
    assert!(!call.is_error, "{}", call.body);
    let next_id = call.body["message_id"].as_str().expect("second inbox message id");
    assert_ne!(next_id, message_id);
    let second = inbox_record(&harness, next_id);
    for field in EXTENSION_KEYS {
        assert_eq!(second["envelope"][field], next[field]);
    }
    assert_eq!(inbox_record(&harness, message_id), first, "append must not rewrite prior records");
    let after = std::fs::read(&inbox).unwrap();
    assert!(after.starts_with(&before));
    assert_eq!(std::str::from_utf8(&after).unwrap().lines().count(), 2);
}

#[test]
#[serial(envelope_fidelity)]
fn reverse_control_reserved_owner_scope_conflict_does_not_append() {
    let _hermetic = HermeticTestEnv::enter("envelope-fidelity-reverse");
    let harness = McpSimHarness::new();
    let mut worker = harness.spawn_mcp_client("worker_a", "teamA");
    let result_id = "res_envelope_reserved_scope_conflict";
    let mut submitted = fidelity_envelope("REVERSE");
    let object = submitted.as_object_mut().expect("fixture envelope object");
    object.insert("result_id".to_string(), json!(result_id));
    object.insert("owner_team_id".to_string(), json!("teamB"));

    let inbox = harness.workspace_path().join(".team/inbox/leader.jsonl");
    let before = std::fs::read(&inbox).unwrap_or_default();
    let call = worker.call_tool("report_result", json!({"envelope": submitted}));
    let refusal = call.body.to_string();
    assert!(
        call.is_error && refusal.contains("scope") && refusal.contains("teamB"),
        "reverse control: an envelope must not use an extension-shaped reserved owner key to override the worker's team scope; body={} raw={}",
        call.body,
        call.raw
    );
    assert_eq!(std::fs::read(&inbox).unwrap_or_default(), before,
        "rejected cross-scope input must not append an inbox record");
}

fn inbox_record(harness: &McpSimHarness, message_id: &str) -> Value {
    let inbox = team_agent::inbox_store::read(harness.workspace_path(), "leader", 64, Some("teamA"))
        .expect("read append-only inbox");
    inbox["messages"].as_array().unwrap().iter()
        .find(|record| record["message_id"] == message_id)
        .cloned().expect("returned message id names a durable inbox record")
}

fn fidelity_envelope(tag: &str) -> Value {
    let long_unicode = format!("{tag}-开头🚀{}结尾✅", "保真长文本片段-αβγ-".repeat(512));
    json!({
        "schema_version": "result_envelope_v1",
        "task_id": "task_mcp",
        "agent_id": "worker_a",
        "status": "success",
        "summary": format!("ENVELOPE_FIDELITY_{tag}_BACKING"),
        "changes": [],
        "tests": [{"command": "synthetic fidelity canary", "status": "passed"}],
        "risks": [],
        "artifacts": [],
        "next_actions": [],
        "answer": {
            "headline": format!("{tag}-答案标题"),
            "body": long_unicode,
            "decision": {"accepted": true, "score": 0.875},
            "labels": ["入口", "持久化", "roundtrip"]
        },
        "evidence": [
            {"kind": "command", "exit_code": 0, "stdout": format!("{tag}-证据-一")},
            {"kind": "observation", "facts": [true, false, null, 42]}
        ],
        "refs": {
            "report": format!("artifact://{tag}/报告.md"),
            "source": {"path": "wiki/功能/F6 任务、结果与交付闭环.md", "anchor": "结果结构与原子接收"}
        }
    })
}

fn value_kind(value: Option<&Value>) -> &'static str {
    match value {
        None => "missing",
        Some(Value::Null) => "null",
        Some(Value::Bool(_)) => "bool",
        Some(Value::Number(_)) => "number",
        Some(Value::String(_)) => "string",
        Some(Value::Array(_)) => "array",
        Some(Value::Object(_)) => "object",
    }
}
