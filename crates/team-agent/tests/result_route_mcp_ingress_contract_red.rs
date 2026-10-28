//! Real MCP results retain opaque business metadata without granting routing authority.
//! Every report goes to the leader inbox/pane; result_route cannot redirect it.

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

#[path = "support/hermetic.rs"]
mod hermetic_guard;
#[path = "support/mcp_sim_harness.rs"]
#[allow(dead_code)]
mod mcp_sim_harness;

#[path = "support/direct_fixture.rs"]
mod direct_fixture;
use direct_fixture::DirectFixture;
use serde_json::json;
use serial_test::serial;

#[test]
#[serial(env)]
fn tp03_worker_result_route_is_inert_and_cannot_redirect_leader_delivery() {
    let _env = hermetic_guard::HermeticTestEnv::enter("result-route-mcp-tp03");
    let leader = DirectFixture::new();
    let other = DirectFixture::new();
    leader.set_state(&json!({"teams": {"current": {
        "tmux_socket": leader.socket,
        "leader": {"pane_id": leader.pane},
        "agents": {"worker-1": {"pane_id": other.pane, "tmux_socket": other.socket}}
    }}}));
    let mut worker = mcp_sim_harness::spawn_mcp_client(&leader.root, "worker-1", "current");
    let state_before = std::fs::read(leader.root.join(".team/runtime/state.json")).unwrap();
    for route in ["pipeline", "worker-1"] {
        let summary = format!("TP03_INERT_ROUTE_{route}");
        let call = worker.call_tool(
            "report_result",
            json!({
                "envelope": {
                    "task_id": "task_mcp", "agent_id": "worker-1",
                    "status": "success", "summary": summary, "result_route": route
                }
            }),
        );
        assert!(
            !call.is_error,
            "opaque metadata is not a routing command: {}",
            call.body
        );
        assert_eq!(call.body["status"], "reported");
        let records = leader.inbox("leader");
        let record = records.last().unwrap();
        assert_eq!(record["message_id"], call.body["message_id"]);
        assert_eq!(record["recipient"], "leader");
        assert_eq!(record["content"], summary);
        assert_eq!(record["envelope"]["result_route"], route);
        leader.received(&format!("Team Agent result from worker-1:\n\n{summary}"));
    }
    let legal = worker.call_tool(
        "report_result",
        json!({
            "task_id": "task_mcp", "summary": "TP03_LEGAL_BACKING"
        }),
    );
    assert!(!legal.is_error, "{}", legal.body);
    leader.received("Team Agent result from worker-1:\n\nTP03_LEGAL_BACKING");
    let records = leader.inbox("leader");
    assert_eq!(records.len(), 3);
    assert_eq!(records[2]["message_id"], legal.body["message_id"]);
    assert!(
        std::fs::read(other.root.join("received"))
            .unwrap()
            .is_empty(),
        "worker route metadata must never hijack physical delivery"
    );
    assert!(!leader.root.join(".team/inbox/worker-1.jsonl").exists());
    assert!(!leader.root.join(".team/inbox/pipeline.jsonl").exists());
    assert!(!leader.root.join(".team/runtime/team.db").exists());
    assert_eq!(
        std::fs::read(leader.root.join(".team/runtime/state.json")).unwrap(),
        state_before
    );
}

#[test]
#[serial(env)]
fn tp10_real_mcp_preserves_novel_business_status_byte_for_byte() {
    let _env = hermetic_guard::HermeticTestEnv::enter("result-route-mcp-tp10");
    let harness = DirectFixture::new();
    let mut worker = mcp_sim_harness::spawn_mcp_client(&harness.root, "worker-1", "current");
    let novel_status = "novel/outcome";

    let call = worker.call_tool(
        "report_result",
        json!({
            "task_id": "task_mcp",
            "agent_id": "worker-1",
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
    let records = harness.inbox("leader");
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["message_id"], call.body["message_id"]);
    assert_eq!(records[0]["envelope"]["status"], novel_status);
    harness.received("Team Agent result from worker-1:\n\nTP10_REAL_MCP_OPAQUE_STATUS");
    assert!(!harness.root.join(".team/runtime/team.db").exists());
}
