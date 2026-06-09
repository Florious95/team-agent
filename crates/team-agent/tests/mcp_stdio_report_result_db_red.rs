#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[path = "support/mcp_sim_harness.rs"]
#[allow(dead_code)]
mod mcp_sim_harness;

use mcp_sim_harness::McpSimHarness;
use serde_json::json;

#[test]
fn mcp_stdio_report_result_persists_result_row_in_same_workspace_db() {
    let harness = McpSimHarness::new();
    let mut worker = harness.spawn_mcp_client("worker_a", "teamA");
    let canary = "MCP_STDIO_DB_PROBE_CANARY";

    let call = worker.call_tool(
        "report_result",
        json!({
            "task_id": "task_mcp",
            "agent_id": "worker_a",
            "status": "success",
            "summary": canary
        }),
    );

    assert!(
        !call.is_error,
        "true MCP stdio tools/call report_result must not return isError; body={} raw={}",
        call.body,
        call.raw
    );
    let result_id = call.body["result_id"]
        .as_str()
        .expect("MCP stdio report_result must return the DB result_id");
    let row = harness
        .result_row(result_id)
        .unwrap_or_else(|| panic!("true MCP stdio report_result did not persist result row {result_id} in the same workspace DB; body={} trace={:?}", call.body, worker.trace_entries()));

    assert_eq!(row.task_id, "task_mcp", "DB result row must preserve task_id; row={row:?}");
    assert_eq!(row.agent_id, "worker_a", "DB result row must preserve MCP worker identity; row={row:?}");
    assert_eq!(
        row.owner_team_id.as_deref(),
        Some("teamA"),
        "DB result row must preserve spawn-time TEAM_AGENT_OWNER_TEAM_ID scope; row={row:?}"
    );
    assert!(
        row.envelope.contains(canary),
        "DB result row envelope must contain the report_result summary canary; row={row:?}"
    );
}
