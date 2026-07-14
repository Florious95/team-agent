//! #236 abnormal-exit notification contracts.
//!
//! User-facing invariant: a worker abnormal-exit notification is deterministic and zero-false-positive:
//! the latest transcript/rollout fact must be an explicit provider error that is fresh for the
//! current worker cohort. Process liveness is an audit field; dead-without-error remains silent.
//! The notification goes through the N32 leader funnel.

#![allow(clippy::expect_used, clippy::panic)]

use std::path::Path;

use team_agent::provider::{latest_explicit_error_fact, FactKind, Provider};

#[test]
fn notification_abnormal_exit_requires_fresh_latest_explicit_error_before_notifying_leader() {
    let codex_failed = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "turn/completed",
        "params": {"turn": {"id": "turn-failed", "status": "failed"}}
    });
    let fact = latest_explicit_error_fact(Provider::Codex, &format!("{codex_failed}\n"))
        .expect("fixture precondition: latest explicit Codex failure is a fault fact");
    assert_eq!(fact.kind, FactKind::Failed);

    let src = production_sources();
    let mut failures = Vec::new();

    if !src.contains("worker.abnormal_exit") {
        failures.push(
            "must emit a dedicated worker.abnormal_exit event for the deterministic notification class"
                .to_string(),
        );
    }
    if !src.contains("ErrorRecency")
        || !src.contains("error_recency")
        || !src.contains("fresh_error")
    {
        failures
            .push("worker.abnormal_exit must persist/report explicit-error recency".to_string());
    }
    if !src.contains("latest_explicit_error_fact")
        || !src.contains("turn_failed")
        || !src.contains("api_error")
    {
        failures.push(
            "worker.abnormal_exit must require the latest transcript/rollout fact to be an explicit error"
                .to_string(),
        );
    }
    if src.contains("error_only") {
        failures.push(
            "error_only suppression must be removed; stale/fresh error recency owns that path"
                .to_string(),
        );
    }
    if !src.contains("dead_only") || !src.contains("abnormal_exit.single_signal_suppressed") {
        failures.push(
            "dead-without-explicit-error must remain auditable as dead_only suppression"
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
        "abnormal_exit fresh explicit-error notification contract failed:\n{}",
        failures.join("\n")
    );
}

#[test]
fn notification_abnormal_exit_contract_does_not_repurpose_generic_abnormal_fact_notifications() {
    let orphan = source("src/coordinator/orphan.rs");
    let mut failures = Vec::new();

    if orphan.contains("process_abnormal_records")
        && orphan.contains("notifications.push")
        && !orphan.contains("worker.abnormal_exit")
    {
        failures.push(
            "generic process_abnormal_records notifications are transcript-only; #236 worker.abnormal_exit needs the fresh latest-error gate"
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

#[test]
fn notification_abnormal_exit_claude_api_error_shape_contract_is_single_source() {
    let classify = source("src/provider/classify.rs");
    let faults = source("src/provider/faults.rs");
    let abnormal = source("src/coordinator/steps/abnormal.rs");
    let mut failures = Vec::new();

    if !classify.contains("claude_record_has_error_tool_result(record)")
        || !classify.contains("claude_explicit_error_fact(record)")
    {
        failures.push(
            "classify.rs must keep latest-record/tool_result gating and delegate Claude explicit errors to faults.rs"
                .to_string(),
        );
    }
    if classify.contains("isApiErrorMessage") || classify.contains("apiErrorStatus") {
        failures.push(
            "Claude assistant API-error shape must stay single-sourced in provider/faults.rs"
                .to_string(),
        );
    }
    for needle in [
        "type",
        "assistant",
        "message",
        "role",
        "isApiErrorMessage",
        "apiErrorStatus",
        "requestId",
    ] {
        if !faults.contains(needle) {
            failures.push(format!(
                "provider/faults.rs missing assistant API-error gate: {needle}"
            ));
        }
    }
    for needle in ["subtype", "api_error", "level"] {
        if !faults.contains(needle) {
            failures.push(format!(
                "provider/faults.rs must preserve old system/api_error branch: {needle}"
            ));
        }
    }
    for needle in ["apiErrorStatus", "error", "requestId", "assistant_uuid"] {
        if !abnormal.contains(needle) {
            failures.push(format!(
                "worker.abnormal_exit payload missing structured field: {needle}"
            ));
        }
    }

    assert!(
        failures.is_empty(),
        "Claude assistant API-error classifier contract failed:\n{}",
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
