#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[path = "support/mcp_sim_harness.rs"]
#[allow(dead_code)]
mod mcp_sim_harness;

use mcp_sim_harness::McpSimHarness;
use serde_json::json;

#[test]
fn mcp_get_team_status_returns_scoped_rich_status_without_sibling_leak() {
    let harness = McpSimHarness::new();
    let mut worker = harness.spawn_mcp_client("worker_a", "teamA");

    let call = worker.call_tool("get_team_status", json!({}));

    assert!(
        !call.is_error,
        "MCP get_team_status should return a status object for the worker owner team; body={} raw={}",
        call.body,
        call.raw
    );
    // MCP get_team_status follows the same slim shape as `status --json` and
    // adds `teams` for sibling-leak safety. Rich diagnostics remain CLI-only.
    for key in ["agents", "ready", "not_ready", "session_name", "teams"] {
        assert!(
            call.body.get(key).is_some(),
            "get_team_status must include slim field `{key}`; body={}",
            call.body
        );
    }
    for forbidden in [
        "messages",
        "results",
        "coordinator",
        "readiness",
        "queued_messages",
        "latest_results",
    ] {
        assert!(
            call.body.get(forbidden).is_none(),
            "get_team_status slim payload must not include diagnostic `{forbidden}`; body={}",
            call.body
        );
    }
    let body_text = call.body.to_string();
    assert!(
        body_text.contains("worker_a") && !body_text.contains("worker_x"),
        "get_team_status from TEAM_AGENT_OWNER_TEAM_ID=teamA must show teamA agents and must not leak sibling teamB worker_x; body={}",
        call.body
    );
}
