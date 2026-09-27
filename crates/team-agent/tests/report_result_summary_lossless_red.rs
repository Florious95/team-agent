//! End-to-end regression for lossless Teammate completion-summary notifications.
//!
//! Exercises the public report_result tool through leader_receiver and checks the exact
//! durable notification content plus leader-pane delivery. The baseline's 160-byte summary
//! limiter and first-line projection must produce Initial RED.

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

#[path = "support/hermetic.rs"]
mod hermetic;
#[path = "support/mcp_sim_harness.rs"]
#[allow(dead_code)]
mod sim;

use hermetic::HermeticTestEnv;
use serde_json::{Value, json};

#[test]
#[serial_test::serial(env)]
fn report_result_notification_preserves_long_multiline_summary_verbatim() {
    let _hermetic = HermeticTestEnv::enter("report-summary-lossless");
    let harness = sim::McpSimHarness::new();
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
        sim::spawn_mcp_client_without_catalog_check(harness.workspace_path(), "worker_a", "teamA");
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
    let result_id = report.body["result_id"]
        .as_str()
        .expect("report_result returns a durable result id");
    let result = harness
        .result_row(result_id)
        .expect("report_result stores the complete result envelope");
    let envelope: Value = serde_json::from_str(&result.envelope).expect("stored envelope JSON");
    assert_eq!(
        envelope["summary"], summary,
        "input summary remains intact in the result record"
    );

    harness.drive_delivery_twice();
    let notifications = harness.message_rows_containing("Commit: ");
    assert_eq!(
        notifications.len(),
        1,
        "one leader notification must contain the summary"
    );
    let notification = &notifications[0];
    assert_eq!(notification.recipient, "leader");
    assert!(
        notification.content.contains(&summary),
        "leader notification must contain the entire original multiline summary; expected={summary:?}, actual={:?}",
        notification.content
    );
    assert!(
        !notification.content.contains("..."),
        "the formatter must not synthesize an ellipsis: {:?}",
        notification.content
    );
    assert!(
        notification.content.contains(
            "Branch: feature/team-agent/notification-summary-lossless-red-validation\nCritical files: crates/team-agent/src/messaging/results.rs"
        ),
        "branch and critical-file lines must retain their original boundary"
    );
    assert!(
        notification
            .content
            .contains("Verification: the full summary must survive report_result formatting and leader delivery.\nDetails: preserve this second paragraph"),
        "multi-line descriptive paragraphs must remain separated"
    );
    eprintln!(
        "report_result notification diagnostic: message_id={:?} status={:?} leader_notified={:?} channel={:?}; db_message_status={:?}\nevents.jsonl:\n{}",
        report.body.get("notification_message_id"),
        report.body.get("notification_status"),
        report.body.get("leader_notified"),
        report.body.get("notification_channel"),
        notification.status,
        harness.events_text(),
    );
    let leader_pane = harness.pane_text("leader");
    assert!(
        leader_pane.contains("FULL_SUMMARY_END_fef4a83e21d54829b5d0dcd43d6aa1c1"),
        "the complete summary notification must reach the leader pane; actual={leader_pane:?}"
    );
}
