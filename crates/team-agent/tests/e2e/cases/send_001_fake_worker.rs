//! A live fake seat receives direct input; its explicit MCP report is readable without collect.
use crate::framework::*;
use serde_json::json;
#[path = "../../support/mcp_sim_harness.rs"]
#[allow(dead_code)]
mod mcp_sim_harness;

#[test]
fn send_001_delivers_to_fake_worker() {
    let ws = TestWorkspace::new("send001").with_fake_spec(&["a"]);
    let workspace = ws.path().to_str().unwrap();
    let quick = run_ta(&ws, &["quick-start", "--workspace", workspace, "--json"]);
    assert!(quick.is_success(), "{} {}", quick.stdout, quick.stderr);
    let token = "SEND001_DIRECT_INPUT";
    let send = run_ta(
        &ws,
        &["send", "a", token, "--workspace", workspace, "--json"],
    );
    assert!(send.is_success(), "{} {}", send.stdout, send.stderr);
    assert_physical_input(&ws, &send.json(), token);

    let state = ws.read_state();
    let owner = state["active_team_key"].as_str().unwrap();
    let mut worker = mcp_sim_harness::spawn_mcp_client(ws.path(), "a", owner);
    let report = worker.call_tool(
        "report_result",
        json!({"summary": token, "status": "success",
        "tests": [{"command": "physical receipt", "status": "passed"}]}),
    );
    assert!(!report.is_error, "{}", report.body);
    let inbox = run_ta(
        &ws,
        &["inbox", "leader", "--workspace", workspace, "--json"],
    );
    assert!(inbox.is_success());
    let records = inbox.json();
    let record = records["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|record| record["message_id"] == report.body["message_id"])
        .unwrap();
    assert_eq!(record["content"], token);
    assert_eq!(record["envelope"]["task_id"], "manual");
    assert_eq!(record["envelope"]["agent_id"], "a");
    assert_eq!(record["kind"], "result");
    assert_eq!(
        run_ta(
            &ws,
            &["inbox", "leader", "--workspace", workspace, "--json"]
        )
        .stdout,
        inbox.stdout
    );
    let _ = run_ta(
        &ws,
        &[
            "shutdown",
            "--workspace",
            workspace,
            "--keep-logs",
            "--json",
        ],
    );
}
