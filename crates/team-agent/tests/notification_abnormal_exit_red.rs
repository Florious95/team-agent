//! #236 abnormal-exit notification contracts.
//!
//! User-facing invariant: a worker abnormal-exit notification is deterministic and zero-false-positive:
//! the latest transcript/rollout fact must be an explicit provider error that is fresh for the
//! current worker cohort. Process liveness is an audit field; dead-without-error remains silent.
//! The notification goes through the N32 leader funnel.

#![allow(clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};

use rusqlite::Connection;
use team_agent::event_log::EventLog;
use team_agent::message_store::MessageStore;
use team_agent::messaging::{send_to_leader_receiver, DeliveryStatus};
use team_agent::model::ids::TaskId;
use team_agent::provider::{latest_explicit_error_fact, FactKind, Provider};

#[test]
fn notification_abnormal_exit_matrix_requires_fresh_latest_explicit_error() {
    let old = codex_failed_turn("turn-old");
    let new = codex_failed_turn("turn-new");
    let completed = codex_completed_turn("turn-complete");

    let baseline = latest_explicit_error_fact(Provider::Codex, &old)
        .expect("fixture precondition: failed turn is an explicit fault fact");
    assert_eq!(baseline.kind, FactKind::Failed);

    let fresh = latest_explicit_error_fact(Provider::Codex, &format!("{old}{new}"))
        .expect("fixture precondition: latest failed turn remains an explicit fault fact");
    assert_eq!(
        fresh.turn_id.as_ref().map(|id| id.as_str()),
        Some("turn-new")
    );
    assert_ne!(fault_key(&fresh), fault_key(&baseline));

    let unchanged = latest_explicit_error_fact(Provider::Codex, &format!("{old}{completed}"))
        .expect("an older explicit error remains observable for stale recency");
    assert_eq!(fault_key(&unchanged), fault_key(&baseline));

    assert!(
        latest_explicit_error_fact(Provider::Codex, &completed).is_none(),
        "dead-only/completed transcript must contain no explicit provider error"
    );

    let abnormal = source("src/coordinator/steps/abnormal.rs");
    for needle in [
        "latest_explicit_error_fact",
        "abnormal_error_recency",
        "ErrorRecency",
        "fresh_error",
        "abnormal_last_notified_key",
        "mark_abnormal_notified",
        "send_to_leader_receiver",
        "worker.abnormal_exit",
    ] {
        assert!(
            abnormal.contains(needle),
            "abnormal path must retain the local behavior anchor: {needle}"
        );
    }
    assert!(
        abnormal.contains("abnormal_exit.single_signal_suppressed")
            && abnormal.contains("dead_only"),
        "dead-without-error must remain an auditable silent branch"
    );
    assert!(
        !abnormal.contains("process_abnormal_records"),
        "the live path must not delegate to the retired generic abnormal processor"
    );
}

#[test]
fn notification_abnormal_exit_repeated_fresh_fingerprint_is_deduped_at_leader_funnel() {
    let workspace = temp_workspace("dedupe-funnel");
    let state = serde_json::json!({
        "active_team_key": "team",
        "team_owner": {"owner_epoch": 1, "leader_session_uuid": "leader-session"},
        "leader_receiver": {
            "owner_epoch": 1,
            "leader_session_uuid": "leader-session",
            "pane_id": "%leader"
        }
    });
    team_agent::state::persist::save_runtime_state(&workspace, &state).unwrap();
    let event_log = EventLog::new(&workspace);
    let result_id = "worker.abnormal_exit:w1:turn-new";
    let content = "ABNORMAL_EXIT_FUNNEL_CANARY";

    let first = send_to_leader_receiver(
        &workspace,
        &state,
        "leader",
        content,
        Some(&TaskId::new("task-abnormal")),
        "w1",
        false,
        Some(result_id),
        &event_log,
    )
    .unwrap();
    assert_eq!(first.status, DeliveryStatus::Queued);
    let first_message_id = first.message_id.clone().expect("first message id");

    let duplicate = send_to_leader_receiver(
        &workspace,
        &state,
        "leader",
        content,
        Some(&TaskId::new("task-abnormal")),
        "w1",
        false,
        Some(result_id),
        &event_log,
    )
    .unwrap();
    assert_eq!(duplicate.status, DeliveryStatus::AlreadyDelivered);
    assert_eq!(
        duplicate.message_id.as_deref(),
        Some(first_message_id.as_str())
    );

    let store = MessageStore::open(&workspace).unwrap();
    let conn = Connection::open(store.db_path()).unwrap();
    let claims: i64 = conn
        .query_row(
            "select count(*) from leader_notification_log where result_id = ?1",
            [result_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(claims, 1, "same fresh fingerprint has one leader claim");

    let events = event_log.tail(32).unwrap();
    let submit_events = events
        .iter()
        .filter(|event| event["event"] == "deliver_to_leader.submit")
        .collect::<Vec<_>>();
    assert_eq!(
        submit_events.len(),
        2,
        "both attempts must remain funnel-auditable"
    );
    assert!(
        submit_events
            .iter()
            .all(|event| event["result_id"] == result_id),
        "funnel events must preserve the abnormal fingerprint correlation"
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| event["event"] == "leader_receiver.queued")
            .count(),
        1,
        "duplicate attempt must not create a second queued notification"
    );

    std::fs::remove_dir_all(&workspace).unwrap();
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

fn codex_failed_turn(id: &str) -> String {
    format!(
        "{}\n",
        serde_json::json!({
            "jsonrpc": "2.0",
            "method": "turn/completed",
            "params": {"turn": {"id": id, "status": "failed"}}
        })
    )
}

fn codex_completed_turn(id: &str) -> String {
    format!(
        "{}\n",
        serde_json::json!({
            "jsonrpc": "2.0",
            "method": "turn/completed",
            "params": {"turn": {"id": id, "status": "completed"}}
        })
    )
}

fn fault_key(fact: &team_agent::provider::FaultFact) -> (String, Option<String>) {
    (
        fact.signature.as_str().to_string(),
        fact.turn_id.as_ref().map(|id| id.as_str().to_string()),
    )
}

fn source(rel: &str) -> String {
    std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(rel)).expect("read source")
}

fn temp_workspace(tag: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "team-agent-notification-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&path).unwrap();
    path
}
