//! ---
//! purpose: lifecycle lock behavior contracts
//! contract:
//!   provides:
//!     - name: lifecycle-lock-timeout
//!       what: a held workspace lock returns a typed timeout and release restores acquisition
//!     - name: lifecycle-lock-held-long-event
//!       what: a blocked waiter records holder and waiter diagnostics
//! boundary:
//!   - prove lock behavior through runtime outcomes, not production source text
//!   - operation names remain internal diagnostic metadata
//! maturity: wired
//! ---

use std::path::Path;
use std::time::Duration;

use super::*;
use crate::lifecycle::lock::{
    acquire_agent_lifecycle_lock, acquire_agent_lifecycle_lock_for_test, LifecycleLockRequest,
    AGENT_LIFECYCLE_LOCK_NAME, LIFECYCLE_LOCK_HELD_LONG, LIFECYCLE_LOCK_TIMEOUT,
};

#[test]
fn lifecycle_lock_timeout_error_contract_and_constants() {
    assert_eq!(AGENT_LIFECYCLE_LOCK_NAME, "agent-lifecycle");
    assert_eq!(LIFECYCLE_LOCK_TIMEOUT, Duration::from_secs(30));
    assert_eq!(LIFECYCLE_LOCK_HELD_LONG, Duration::from_secs(5));

    let ws = temp_ws();
    let err = LifecycleError::LifecycleLockTimeout {
        lock_path: crate::model::paths::runtime_dir(&ws).join("agent-lifecycle.lock"),
        log_path: crate::model::paths::logs_dir(&ws).join("events.jsonl"),
        operation: "reset-agent".to_string(),
        waited_ms: 30_000,
    };
    assert_eq!(
        err.to_string(),
        format!(
            "error: lifecycle lock timeout after 30s\naction: retry or check for hung reset/start\nlog: {}",
            crate::model::paths::logs_dir(&ws).join("events.jsonl").display()
        )
    );
}

#[test]
fn lifecycle_lock_lifecycle_rollback_releases_after_timeout() {
    let ws = temp_ws();
    let agent = aid("alpha");
    let held = acquire_agent_lifecycle_lock(LifecycleLockRequest {
        workspace: &ws,
        operation: "reset-agent",
        team: Some("alpha"),
        agent_id: Some(&agent),
    })
    .expect("hold lifecycle lock");

    let waiter_ws = ws.clone();
    let waiter_agent = aid("beta");
    let result = std::thread::spawn(move || {
        acquire_agent_lifecycle_lock_for_test(
            LifecycleLockRequest {
                workspace: &waiter_ws,
                operation: "start-agent",
                team: Some("alpha"),
                agent_id: Some(&waiter_agent),
            },
            Duration::from_millis(200),
            Duration::from_secs(5),
        )
    })
    .join()
    .expect("waiter thread");

    assert!(
        matches!(result, Err(LifecycleError::LifecycleLockTimeout { operation, .. }) if operation == "start-agent"),
        "second lifecycle mutator must time out while lock is held"
    );

    drop(held);
    acquire_agent_lifecycle_lock_for_test(
        LifecycleLockRequest {
            workspace: &ws,
            operation: "stop-agent",
            team: Some("alpha"),
            agent_id: Some(&agent),
        },
        Duration::from_millis(200),
        Duration::from_secs(5),
    )
    .expect("lock released after guard drop");
}

#[test]
fn lifecycle_lock_long_held_event_records_holder_and_waiter() {
    let ws = temp_ws();
    let holder_agent = aid("holder");
    let waiter_agent = aid("waiter");
    let _held = acquire_agent_lifecycle_lock(LifecycleLockRequest {
        workspace: &ws,
        operation: "restart",
        team: Some("alpha"),
        agent_id: Some(&holder_agent),
    })
    .expect("hold lifecycle lock");

    let waiter_ws = ws.clone();
    let result = std::thread::spawn(move || {
        acquire_agent_lifecycle_lock_for_test(
            LifecycleLockRequest {
                workspace: &waiter_ws,
                operation: "reset-agent",
                team: Some("alpha"),
                agent_id: Some(&waiter_agent),
            },
            Duration::from_millis(220),
            Duration::from_millis(50),
        )
    })
    .join()
    .expect("waiter thread");
    assert!(matches!(
        result,
        Err(LifecycleError::LifecycleLockTimeout { .. })
    ));

    let events = read_events(&ws);
    let event = events
        .iter()
        .find(|event| {
            event.get("event").and_then(serde_json::Value::as_str)
                == Some("lifecycle.lock_held_long")
        })
        .unwrap_or_else(|| panic!("missing lifecycle.lock_held_long event: {events:?}"));
    assert_eq!(event["lock_name"], json!("agent-lifecycle"));
    assert_eq!(event["operation"], json!("reset-agent"));
    assert_eq!(event["holder"]["operation"], json!("restart"));
    assert_eq!(event["holder"]["agent_id"], json!("holder"));
    assert_eq!(event["threshold_ms"], json!(50));
    assert_eq!(event["timeout_ms"], json!(220));
    assert!(
        event["blocked_queue_len"].as_u64().unwrap_or(0) >= 1,
        "waiter sidecar should be visible while long-held event is emitted"
    );
}

fn read_events(workspace: &Path) -> Vec<serde_json::Value> {
    let path = crate::model::paths::logs_dir(workspace).join("events.jsonl");
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}
