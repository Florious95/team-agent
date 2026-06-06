//! #236 abnormal-exit notification contracts.
//!
//! User-facing invariant: a worker abnormal-exit notification is deterministic and zero-false-positive:
//! provider process is dead AND the latest transcript/rollout fact is an explicit error. Either signal
//! alone must stay silent. The notification goes through the N32 leader funnel.

#![allow(clippy::expect_used, clippy::panic)]

use std::path::Path;

use team_agent::provider::{read_fault_facts, FactKind, Provider};

#[test]
fn abnormal_exit_requires_dead_process_and_latest_explicit_error_before_notifying_leader() {
    let codex_failed = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "turn/completed",
        "params": {"turn": {"id": "turn-failed", "status": "failed"}}
    });
    let facts = read_fault_facts(&[codex_failed], Provider::Codex);
    assert_eq!(facts.len(), 1, "fixture precondition: latest explicit Codex failure is a fault fact");
    assert_eq!(facts[0].kind, FactKind::Failed);

    let src = production_sources();
    let mut failures = Vec::new();

    if !src.contains("worker.abnormal_exit") {
        failures.push(
            "must emit a dedicated worker.abnormal_exit event for the deterministic notification class"
                .to_string(),
        );
    }
    if !src.contains("ProcessLiveness::Dead")
        && !src.contains("process_dead")
        && !src.contains("provider_process_dead")
    {
        failures.push(
            "worker.abnormal_exit must require provider process liveness == dead".to_string(),
        );
    }
    if !src.contains("read_fault_facts")
        && !src.contains("turn_failed")
        && !src.contains("tool_result_is_error")
        && !src.contains("api_error")
    {
        failures.push(
            "worker.abnormal_exit must require the latest transcript/rollout fact to be an explicit error"
                .to_string(),
        );
    }
    if !src.contains("abnormal_exit.single_signal_suppressed")
        && !src.contains("single_signal_suppressed")
        && !src.contains("requires_dead_and_error")
    {
        failures.push(
            "single-signal cases must be explicitly suppressed/auditable: dead-only and error-only are not notifications"
                .to_string(),
        );
    }
    if !src.contains("send_to_leader_receiver") || !src.contains("deliver_to_leader.submit") {
        failures.push(
            "worker.abnormal_exit must notify through the shared N32 deliver-to-leader funnel"
                .to_string(),
        );
    }

    assert!(
        failures.is_empty(),
        "abnormal_exit deterministic notification contract failed:\n{}",
        failures.join("\n")
    );
}

#[test]
fn abnormal_exit_contract_does_not_repurpose_generic_abnormal_fact_notifications() {
    let orphan = source("src/coordinator/orphan.rs");
    let mut failures = Vec::new();

    if orphan.contains("process_abnormal_records")
        && orphan.contains("notifications.push")
        && !orphan.contains("worker.abnormal_exit")
    {
        failures.push(
            "generic process_abnormal_records notifications are transcript-only; #236 worker.abnormal_exit needs the dead-process AND latest-error gate"
                .to_string(),
        );
    }
    if production_sources().contains("worker.abnormal_exit")
        && !production_sources().contains("leader_notification_log")
    {
        failures.push(
            "worker.abnormal_exit notification must share leader_notification_log/dedupe behavior with other leader-bound notifications"
                .to_string(),
        );
    }

    assert!(
        failures.is_empty(),
        "abnormal_exit must not be a renamed generic abnormal fact:\n{}",
        failures.join("\n")
    );
}

fn source(rel: &str) -> String {
    std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(rel)).expect("read source")
}

fn production_sources() -> String {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut out = String::new();
    append_rs_sources(&root, &mut out);
    out
}

fn append_rs_sources(path: &Path, out: &mut String) {
    if path.is_dir() {
        let mut entries = std::fs::read_dir(path)
            .expect("read source dir")
            .map(|entry| entry.expect("read source entry").path())
            .collect::<Vec<_>>();
        entries.sort();
        for entry in entries {
            append_rs_sources(&entry, out);
        }
        return;
    }
    if path.extension().and_then(|v| v.to_str()) == Some("rs") {
        out.push_str(&std::fs::read_to_string(path).expect("read source file"));
        out.push('\n');
    }
}
