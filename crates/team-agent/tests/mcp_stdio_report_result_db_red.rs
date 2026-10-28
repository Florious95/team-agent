//! Real MCP reports persist losslessly in the append-only inbox, not SQLite.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[path = "support/direct_fixture.rs"]
mod direct_fixture;
#[path = "support/mcp_sim_harness.rs"]
#[allow(dead_code)]
mod mcp_sim_harness;

use direct_fixture::DirectFixture;
use serde_json::json;

#[test]
fn mcp_stdio_report_result_appends_envelope_without_creating_database() {
    let fixture = DirectFixture::new();
    let mut worker = mcp_sim_harness::spawn_mcp_client(&fixture.root, "worker-1", "current");
    let canary = "CANARY_MCP_STDIO_RESULT_9d0a58f7e9d842a9b6b78a5ec2ed4e31";
    let worker_status = "triage/needs-review#β";
    let artifacts = json!([{
        "kind": "custom_evidence_bundle", "uri": "artifact://nested/raw",
        "nested": {"items": [{"n": 7, "ok": true, "tags": ["alpha", "beta"]},
            {"payload": {"key": "value", "count": 42}}], "meta": {"verified": true}}
    }]);
    let report = worker.call_tool(
        "report_result",
        json!({
            "task_id": "task_mcp", "status": worker_status, "summary": canary,
            "artifacts": artifacts
        }),
    );
    assert!(!report.is_error, "{}", report.body);
    assert_eq!(report.body["status"], "reported");
    let records = fixture.inbox("leader");
    assert_eq!(records.len(), 1);
    let record = &records[0];
    assert_eq!(record["message_id"], report.body["message_id"]);
    assert_eq!(record["sender"], "worker-1");
    assert_eq!(record["team"], "current");
    assert_eq!(record["kind"], "result");
    assert_eq!(record["envelope"]["task_id"], "task_mcp");
    assert_eq!(record["envelope"]["summary"], canary);
    assert_eq!(record["envelope"]["status"], worker_status);
    assert_eq!(
        serde_json::to_vec(&record["envelope"]["artifacts"]).unwrap(),
        serde_json::to_vec(&artifacts).unwrap()
    );
    fixture.received(&format!("Team Agent result from worker-1:\n\n{canary}"));
    assert!(!fixture.root.join(".team/runtime/team.db").exists());
}
