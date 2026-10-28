//! End-to-end regression for lossless Teammate completion-summary notifications.
//!
//! Exercises real MCP ingress, append-only persistence and physical leader delivery.

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

#[path = "support/hermetic.rs"]
mod hermetic;
#[path = "support/mcp_sim_harness.rs"]
#[allow(dead_code)]
mod sim;

use hermetic::HermeticTestEnv;
use serde_json::json;
#[path = "support/direct_fixture.rs"]
mod direct_fixture;

#[test]
#[serial_test::serial(env)]
fn report_result_notification_preserves_long_multiline_summary_verbatim() {
    let _hermetic = HermeticTestEnv::enter("report-summary-lossless");
    let fixture = direct_fixture::DirectFixture::new();
    let long_commit = "0123456789abcdef".repeat(16);
    let summary = [
        format!("Commit: {long_commit}"),
        "Branch: feature/team-agent/notification-summary-lossless-red-validation".to_string(),
        "Critical files: crates/team-agent/src/messaging/results.rs; crates/team-agent/tests/report_result_summary_lossless_red.rs".to_string(),
        "Verification: the full summary must survive report_result formatting and leader delivery.".to_string(),
        "Details: preserve this second paragraph and its explicit line boundary.".to_string(),
        "Final marker: FULL_SUMMARY_END_fef4a83e21d54829b5d0dcd43d6aa1c1".to_string(),
    ]
    .join("\n");
    assert!(
        summary.len() > 160,
        "fixture must exceed the removed hard limit"
    );
    assert!(
        !summary.contains("..."),
        "fixture must not supply the forbidden truncation marker"
    );

    let mut worker =
        sim::spawn_mcp_client_without_catalog_check(&fixture.root, "worker-1", "current");
    let report = worker.call_tool(
        "report_result",
        json!({
            "task_id": "task_mcp",
            "status": "success",
            "summary": summary.clone(),
            "changes": [],
            "tests": [{"command": "model-fuzzy-lookup RED contract", "status": "passed"}],
            "risks": [],
            "artifacts": [],
            "next_actions": []
        }),
    );
    assert!(!report.is_error, "report_result failed: {}", report.body);
    assert_eq!(report.body["status"], "reported");
    let records = fixture.inbox("leader");
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["message_id"], report.body["message_id"]);
    assert_eq!(records[0]["envelope"]["summary"], summary);
    assert_eq!(records[0]["content"], summary);
    fixture.received(&format!("Team Agent result from worker-1:\n\n{summary}"));
    assert!(!fixture.root.join(".team/runtime/team.db").exists());
}
