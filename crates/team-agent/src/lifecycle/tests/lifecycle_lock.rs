use std::path::{Path, PathBuf};
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

#[test]
fn lifecycle_lock_phase_b_golden_source_guard() {
    r2_lifecycle_lock_exists_precondition();
}

#[test]
fn r2_lifecycle_lock_exists_precondition() {
    let src = manifest_src();
    let lock = read_src("lifecycle/lock.rs");
    assert!(
        lock.contains("AGENT_LIFECYCLE_LOCK_NAME: &str = \"agent-lifecycle\""),
        "lifecycle lock name constant must remain explicit"
    );
    assert!(
        lock.contains("LIFECYCLE_LOCK_TIMEOUT: Duration = Duration::from_secs(30)"),
        "lifecycle lock timeout constant must remain 30s"
    );
    assert!(
        lock.contains("write_lock_held_long_event") && lock.contains("lifecycle.lock_held_long"),
        "long-held lifecycle lock event must remain wired"
    );

    let state_hits = source_files(&src.join("state"))
        .into_iter()
        .filter(|path| {
            let text = std::fs::read_to_string(path).unwrap();
            text.contains("RuntimeLock::acquire") && text.contains("\"state-save\"")
        })
        .collect::<Vec<_>>();
    assert_eq!(
        state_hits,
        vec![src.join("state").join("persist.rs")],
        "state-save lock must stay owned by state/persist.rs"
    );

    for path in source_files(&src.join("state")) {
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            !text.contains("LifecycleLock")
                && !text.contains("acquire_agent_lifecycle_lock")
                && !text.contains("agent-lifecycle"),
            "state module must not acquire or know lifecycle lock: {}",
            path.display()
        );
    }

    for path in source_files(&src.join("coordinator")) {
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            !text.contains("acquire_agent_lifecycle_lock") && !text.contains("agent-lifecycle"),
            "coordinator tick path must not block on lifecycle lock: {}",
            path.display()
        );
    }

    for path in source_files(&src) {
        if path == src.join("state").join("persist.rs") || is_test_source(&path) {
            continue;
        }
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            !text.contains("RuntimeLock::acquire"),
            "production source outside state/persist.rs must not acquire runtime locks directly: {}",
            path.display()
        );
    }

    for path in source_files(&src) {
        if path.starts_with(src.join("state")) || path.starts_with(src.join("lifecycle")) {
            continue;
        }
        let text = std::fs::read_to_string(&path).unwrap();
        let lifecycle_topology_authority_entries = [
            "save_runtime_state_with_lifecycle_topology_authority",
            "save_team_scoped_state_with_lifecycle_topology_authority",
            "save_runtime_state_with_team_tombstone_lifecycle_topology_authority",
            "save_team_scoped_state_with_tombstone_lifecycle_topology_authority",
        ];
        assert!(
            !lifecycle_topology_authority_entries
                .iter()
                .any(|entry| text.contains(entry)),
            "lifecycle topology authority save entry must not be referenced outside lifecycle/state modules: {}",
            path.display()
        );
    }

    let agent = read_src("lifecycle/restart/agent.rs");
    assert_public_operation(&agent, "start-agent");
    assert_public_operation(&agent, "stop-agent");
    assert_public_operation(&agent, "reset-agent");
    assert_body_is_unlocked(&agent, "pub(crate) fn start_agent_at_paths");
    assert_body_is_unlocked(&agent, "pub(super) fn stop_agent_at_paths");
    assert_body_is_unlocked(&agent, "fn reset_agent_at_paths");

    let remove = read_src("lifecycle/restart/remove.rs");
    assert_public_operation(&remove, "remove-agent");
    assert_body_is_unlocked(&remove, "fn remove_agent_at_paths");
    assert!(
        !remove.contains("start_agent_with_transport"),
        "remove rollback runs while remove-agent holds the lifecycle lock; it must use lock-free start_agent_at_paths"
    );

    let launch = read_src("lifecycle/launch.rs");
    assert_public_operation(&launch, "add-agent");
    assert_public_operation(&launch, "fork-agent");
    assert_body_is_unlocked(&launch, "fn add_agent_with_transport_at_paths");

    let rebuild = read_src("lifecycle/restart/rebuild.rs");
    assert_public_operation(&rebuild, "restart");
    let drop_idx = rebuild.find("drop(lifecycle_lock);").unwrap();
    let coordinator_idx = rebuild
        .find("start_coordinator_for_workspace(&selected.run_workspace, Some(&selected.team_key))")
        .unwrap();
    let readiness_idx = rebuild.find("wait_restart_readiness_or_timeout(").unwrap();
    assert!(
        rebuild.contains("let lifecycle_lock = acquire_agent_lifecycle_lock")
            && drop_idx < coordinator_idx
            && coordinator_idx < readiness_idx,
        "restart must release lifecycle lock before coordinator readiness wait"
    );
}

fn is_test_source(path: &Path) -> bool {
    path.components()
        .any(|component| component.as_os_str() == "tests")
        || path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name == "tests.rs" || name.ends_with("_tests.rs"))
}

fn read_events(workspace: &Path) -> Vec<serde_json::Value> {
    let path = crate::model::paths::logs_dir(workspace).join("events.jsonl");
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

fn manifest_src() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src")
}

fn read_src(path: &str) -> String {
    std::fs::read_to_string(manifest_src().join(path)).unwrap()
}

fn source_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    collect_rs_files(root, &mut out);
    out.sort();
    out
}

fn collect_rs_files(root: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(root).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            collect_rs_files(&path, out);
        } else if path.extension().and_then(|ext| ext.to_str()) == Some("rs") {
            out.push(path);
        }
    }
}

fn assert_public_operation(source: &str, operation: &str) {
    assert!(
        source.contains("acquire_agent_lifecycle_lock")
            && source.contains(&format!("operation: \"{operation}\"")),
        "public lifecycle entry must acquire lifecycle lock for {operation}"
    );
}

fn assert_body_is_unlocked(source: &str, signature: &str) {
    let body = source
        .split_once(signature)
        .unwrap_or_else(|| panic!("missing signature: {signature}"))
        .1;
    let body = body
        .split_once("\n}\n")
        .map(|(body, _)| body)
        .unwrap_or(body);
    assert!(
        !body.contains("acquire_agent_lifecycle_lock"),
        "{signature} must remain lock-free to avoid nested reset deadlocks"
    );
}
