//! 0.5.61 RED: a worker's structured `report_result.envelope` remains canonical
//! through the real MCP stdio ingress, SQLite persistence, and first collect.
//!
//! Requirements:
//! - `result_route` B03: the worker reports facts once and the result is durable first.
//! - `result_route` B06/F6: the framework transports result facts; it does not silently
//!   reinterpret or discard accepted structured content.
//! - F6 result atomicity: rejected input must not partially create a result row.

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
use team_agent::messaging;

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
fn tooth1_mcp_report_result_preserves_nested_extension_fields_in_sqlite() {
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
    let result_id = call.body["result_id"]
        .as_str()
        .expect("tooth1 backing: accepted report_result must return result_id");
    let row = harness.result_row(result_id).unwrap_or_else(|| {
        panic!(
            "tooth1 backing: accepted report_result must create results row {result_id}; body={}",
            call.body
        )
    });
    let stored: Value =
        serde_json::from_str(&row.envelope).expect("tooth1 backing: results.envelope is JSON");

    assert_eq!(
        stored["summary"], submitted["summary"],
        "tooth1 backing: canonical summary proves the submitted result reached SQLite"
    );
    assert_eq!(
        stored["result_id"],
        json!(result_id),
        "tooth1 backing: SQLite envelope and returned result_id must name the same result"
    );

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
fn tooth2_collect_returns_the_same_full_envelope_that_sqlite_persisted() {
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
    let result_id = call.body["result_id"]
        .as_str()
        .expect("tooth2 backing: report_result returns result_id");
    let row = harness
        .result_row(result_id)
        .expect("tooth2 backing: persisted result row exists");
    let persisted: Value =
        serde_json::from_str(&row.envelope).expect("tooth2 backing: persisted envelope is JSON");

    let collected = messaging::collect(harness.workspace_path(), None, false)
        .expect("tooth2: collect uncollected result from the existing fixture");
    let returned = collected["collected"]
        .as_array()
        .expect("tooth2: collect exposes full collected[]")
        .iter()
        .find(|value| value["result_id"] == json!(result_id))
        .unwrap_or_else(|| {
            panic!(
                "tooth2 backing: collect must return persisted result {result_id}; output={collected}"
            )
        });

    assert_eq!(
        returned, &persisted,
        "tooth2 regression point: collect `collected[]` must return the exact canonical SQLite envelope without a second projection or narrowing"
    );
}

#[test]
#[serial(envelope_fidelity)]
fn reverse_control_reserved_owner_scope_conflict_is_rejected_without_result_row() {
    let _hermetic = HermeticTestEnv::enter("envelope-fidelity-reverse");
    let harness = McpSimHarness::new();
    let mut worker = harness.spawn_mcp_client("worker_a", "teamA");
    let result_id = "res_envelope_reserved_scope_conflict";
    let mut submitted = fidelity_envelope("REVERSE");
    let object = submitted.as_object_mut().expect("fixture envelope object");
    object.insert("result_id".to_string(), json!(result_id));
    object.insert("owner_team_id".to_string(), json!("teamB"));

    let call = worker.call_tool("report_result", json!({"envelope": submitted}));
    let refusal = call.body.to_string();
    assert!(
        call.is_error && refusal.contains("scope") && refusal.contains("teamB"),
        "reverse control: an envelope must not use an extension-shaped reserved owner key to override the worker's team scope; body={} raw={}",
        call.body,
        call.raw
    );
    assert!(
        harness.result_row(result_id).is_none(),
        "reverse control atomicity: rejected reserved-key conflict must not partially persist result_id={result_id}"
    );
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
