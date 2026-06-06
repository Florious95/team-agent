//! #236 notification redesign negative/retention contracts.
//!
//! User-facing invariant: deterministic notifications replace idle/stuck/deadlock nag. Product
//! delivery notifications for report_result / send-to-leader / request_human / broadcast-to-leader
//! remain on the N31/N32 funnel.

#![allow(clippy::expect_used, clippy::panic)]

use std::path::Path;

#[test]
fn idle_stuck_deadlock_nag_paths_are_not_generated_by_coordinator_tick() {
    let tick = source("src/coordinator/tick.rs");
    let mut failures = Vec::new();

    for (label, haystack, needle) in [
        ("coordinator tick", tick.as_str(), "detect_stuck_agents("),
        ("coordinator tick", tick.as_str(), "record_unknown_idle_nodes("),
        ("coordinator tick", tick.as_str(), "evaluate_takeover("),
        ("coordinator tick", tick.as_str(), "detect_cross_worker_deadlocks("),
    ] {
        if haystack.contains(needle) {
            failures.push(format!(
                "{label} must no longer call old idle/stuck/deadlock nag path `{needle}`"
            ));
        }
    }
    for stale_event in [
        "idle_takeover.reminder",
        "idle_takeover.ping",
        "idle_takeover.unknown_persistent",
        "\"cross_worker_deadlock\"",
    ] {
        if tick.contains(stale_event) {
            failures.push(format!(
                "coordinator tick must not emit old nag event `{stale_event}`"
            ));
        }
    }

    assert!(
        failures.is_empty(),
        "old idle/stuck/deadlock nag generation must be removed:\n{}",
        failures.join("\n")
    );
}

#[test]
fn product_delivery_notifications_stay_on_n31_n32_funnel() {
    let results = source("src/messaging/results.rs");
    let send = source("src/messaging/send.rs");
    let leader_receiver = source("src/messaging/leader_receiver.rs");
    let tools = source("src/mcp_server/tools.rs");
    let mut failures = Vec::new();

    if !leader_receiver.contains("deliver_to_leader.submit")
        || !leader_receiver.contains("leader_notification_log")
    {
        failures.push(
            "N31/N32 leader delivery primitive must keep deliver_to_leader.submit and leader_notification_log"
                .to_string(),
        );
    }
    if !results.contains("send_to_leader_receiver") || !results.contains("mcp.report_result") {
        failures.push("report_result notification must remain on send_to_leader_receiver".to_string());
    }
    if !tools.contains("request_human") || !tools.contains("send_to_leader_receiver") {
        failures.push("request_human must remain on the same leader-delivery primitive".to_string());
    }
    if !send.contains("send_to_leader_receiver") || !send.contains("MessageTarget::Broadcast") {
        failures.push("send(to=leader)/broadcast-to-leader must remain on N31/N32 delivery".to_string());
    }
    for forbidden in ["notification_status\", \"queued\"", "notification_status=queued_only", "queued_only"] {
        if results.contains(forbidden) || leader_receiver.contains(forbidden) {
            failures.push(format!(
                "delivery notifications must not regress to queued-only fake success: `{forbidden}`"
            ));
        }
    }

    assert!(
        failures.is_empty(),
        "product delivery notifications must be preserved:\n{}",
        failures.join("\n")
    );
}

fn source(rel: &str) -> String {
    std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(rel)).expect("read source")
}
