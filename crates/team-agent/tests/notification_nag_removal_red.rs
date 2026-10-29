//! #236 notification redesign negative/retention contracts.
//!
//! User-facing invariant: deterministic notifications replace idle/stuck/deadlock nag. Product
//! delivery notifications for report_result / send-to-leader / broadcast-to-leader remain on the
//! N31/N32 funnel.

#![allow(clippy::expect_used, clippy::panic)]

#[path = "support/hermetic.rs"]
mod hermetic_guard;
#[allow(dead_code)]
fn _hermetic_boundary_marker(_: &hermetic_guard::HermeticTestEnv) {}

use std::path::Path;

#[test]
fn idle_stuck_deadlock_nag_paths_are_not_generated_by_coordinator_tick() {
    let tick = source("src/coordinator/tick.rs");
    let mut failures = Vec::new();

    for (label, haystack, needle) in [
        ("coordinator tick", tick.as_str(), "detect_stuck_agents("),
        (
            "coordinator tick",
            tick.as_str(),
            "record_unknown_idle_nodes(",
        ),
        ("coordinator tick", tick.as_str(), "evaluate_takeover("),
        (
            "coordinator tick",
            tick.as_str(),
            "detect_cross_worker_deadlocks(",
        ),
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
fn product_delivery_notifications_use_physical_delivery_and_append_only_inbox() {
    let tools = source("src/mcp_server/tools.rs");
    assert!(tools.contains("crate::inbox_store::append"));
    assert!(tools.contains("crate::direct_delivery::directory"));
    assert!(tools.contains("Team Agent message from {sender}"));
    assert!(tools.contains("Team Agent result from {sender}"));
    assert!(tools.contains("delivery_error"), "failed physical delivery must remain visible");
    let direct = source("src/direct_delivery.rs");
    assert!(direct.contains("paste-buffer") && direct.contains("send-keys"));
}


fn source(rel: &str) -> String {
    std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(rel)).expect("read source")
}
