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
    // cfd9eb90: delivery may stamp worker-1's first interaction and the state
    // writer normalizes the document (top-level mirror, explicit nulls). Any
    // other change (pane_id, tmux_socket, leader, status) still fails.
    let before: serde_json::Value = serde_json::from_slice(&state_before).unwrap();
    let after: serde_json::Value = serde_json::from_slice(
        &std::fs::read(leader.root.join(".team/runtime/state.json")).unwrap()).unwrap();
    let (observed, mut expected) = (&after["teams"]["current"], before["teams"]["current"].clone());
    if let Some(stamp) = observed["agents"]["worker-1"].get("first_send_at") {
        assert!(stamp.as_str().is_some_and(|stamp| chrono::DateTime::parse_from_rfc3339(stamp).is_ok()), "{stamp}");
        expected["agents"]["worker-1"]["first_send_at"] = stamp.clone();
    }
    for (key, value) in observed.as_object().unwrap() {
        match expected.get(key) {
            Some(old) => assert_eq!(old, value, "report_result must not change {key}"),
            None => assert!(value.is_null() || key == "active_team_key", "report_result added {key}: {value}"),
        }
    }
    for key in expected.as_object().unwrap().keys() {
        assert!(observed.get(key).is_some(), "report_result removed {key}");
    }
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
    // c28a675f: the fixture leader is a distinct pane (no leader/worker collision).
    harness.leader_received("Team Agent result from worker-1:\n\nTP10_REAL_MCP_OPAQUE_STATUS");
    assert!(!harness.root.join(".team/runtime/team.db").exists());
}
