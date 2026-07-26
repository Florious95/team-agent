//! 0.5.61 `result_route` real MCP-ingress RED contracts.
//!
//! Requirement anchors:
//! - `result_route 与流水线结果消费` B03/C03: a worker cannot write or
//!   override a task's result route, and rejection is atomic.
//! - The same page B06/C12: task-local business status is transported and
//!   persisted byte-for-byte without a framework vocabulary.
//!
//! These teeth intentionally drive the compiled `team-agent mcp-server`
//! through JSON-RPC `tools/call`. Lower-level result functions are not a
//! substitute for the public worker ingress.

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

#[path = "support/hermetic.rs"]
mod hermetic_guard;
#[path = "support/mcp_sim_harness.rs"]
#[allow(dead_code)]
mod mcp_sim_harness;

use mcp_sim_harness::McpSimHarness;
use serde_json::json;
use serial_test::serial;
use team_agent::db::schema::open_db;
use team_agent::message_store::MessageStore;

fn result_count(harness: &McpSimHarness) -> i64 {
    let store = MessageStore::open(harness.workspace_path()).expect("open MCP result store");
    let conn = open_db(store.db_path()).expect("open MCP runtime DB");
    conn.query_row("select count(*) from results", [], |row| row.get(0))
        .expect("count MCP result rows")
}

#[test]
#[serial(env)]
fn tp03_real_mcp_rejects_worker_result_route_atomically_and_keeps_legal_backing_live() {
    let _env = hermetic_guard::HermeticTestEnv::enter("result-route-mcp-tp03");
    let harness = McpSimHarness::new();
    let mut worker = harness.spawn_mcp_client("worker_a", "teamA");
    let rejected_result_id = "res-tp03-real-mcp-rejected";
    let rejected_canary = "TP03_REAL_MCP_UNAUTHORIZED_ROUTE";

    let rejected = worker.call_tool(
        "report_result",
        json!({
            "envelope": {
                "schema_version": "result_envelope_v1",
                "result_id": rejected_result_id,
                "task_id": "task_mcp",
                "agent_id": "worker_a",
                "status": "success",
                "summary": rejected_canary,
                "result_route": "pipeline",
                "changes": [],
                "tests": [],
                "risks": [],
                "artifacts": [],
                "next_actions": []
            }
        }),
    );
    let rows_after_rejection = result_count(&harness);
    let messages_after_rejection = harness.message_rows_containing(rejected_canary);

    // Independent positive control: rejection must not poison the worker,
    // task, result store, or report_result ingress. The same worker's next
    // authorized report must still create real durable backing.
    let legal = worker.call_tool(
        "report_result",
        json!({
            "task_id": "task_mcp",
            "agent_id": "worker_a",
            "status": "success",
            "summary": "TP03_REAL_MCP_LEGAL_BACKING"
        }),
    );
    let legal_result_id = legal.body["result_id"]
        .as_str()
        .expect("TP03 positive control must return its durable result_id");

    assert!(
        rejected.is_error,
        "TP03 RED ingress_route_field_was_silently_dropped: real MCP tools/call \
         must return isError=true when a worker envelope contains result_route; \
         body={} raw={}",
        rejected.body, rejected.raw
    );
    assert!(
        rejected.body.to_string().contains("result_route"),
        "TP03 rejection must identify result_route as the unauthorized worker \
         field; body={} raw={}",
        rejected.body,
        rejected.raw
    );
    assert_eq!(
        rows_after_rejection, 0,
        "TP03 RED unauthorized_result_was_persisted: rejected worker result_route \
         input must leave zero result rows"
    );
    assert!(
        messages_after_rejection.is_empty(),
        "TP03 RED unauthorized_result_reached_leader_lane: rejected worker \
         result_route input must leave zero leader messages"
    );
    assert!(
        !legal.is_error,
        "TP03 positive control: the same worker's subsequent legal report_result \
         must succeed; body={} raw={}",
        legal.body, legal.raw
    );
    assert!(
        harness.result_row(legal_result_id).is_some(),
        "TP03 positive control: legal report_result must create durable backing"
    );
    assert_eq!(
        result_count(&harness),
        1,
        "TP03 positive control: only the legal report may create durable backing"
    );
}

#[test]
#[serial(env)]
fn tp10_real_mcp_preserves_novel_business_status_byte_for_byte() {
    let _env = hermetic_guard::HermeticTestEnv::enter("result-route-mcp-tp10");
    let harness = McpSimHarness::new();
    let mut worker = harness.spawn_mcp_client("worker_a", "teamA");
    let novel_status = "novel/outcome";

    let call = worker.call_tool(
        "report_result",
        json!({
            "task_id": "task_mcp",
            "agent_id": "worker_a",
            "status": novel_status,
            "summary": "TP10_REAL_MCP_OPAQUE_STATUS"
        }),
    );
    assert!(
        !call.is_error,
        "TP10 positive backing: real MCP tools/call with a custom business status \
         must be accepted; body={} raw={}",
        call.body, call.raw
    );
    let result_id = call.body["result_id"]
        .as_str()
        .expect("TP10 positive backing must return the durable result_id");
    let row = harness
        .result_row(result_id)
        .unwrap_or_else(|| panic!("TP10 positive backing missing result row {result_id}"));
    let envelope: serde_json::Value =
        serde_json::from_str(&row.envelope).expect("TP10 durable envelope JSON");

    assert_eq!(
        row.status, novel_status,
        "TP10 RED ingress_status_was_semantically_normalized: the results.status \
         column must preserve the task-local business status byte-for-byte, not \
         rewrite it to partial"
    );
    assert_eq!(
        envelope["status"],
        json!(novel_status),
        "TP10 RED canonical_envelope_status_was_rewritten: the durable canonical \
         envelope must preserve the same task-local business status byte-for-byte"
    );
}
