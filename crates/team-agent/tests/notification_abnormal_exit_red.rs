//! #236 abnormal-exit notification contracts.
//!
//! User-facing invariant: a worker abnormal-exit notification is deterministic and zero-false-positive:
//! the latest transcript/rollout fact must be an explicit provider error that is fresh for the
//! current worker cohort. Process liveness is an audit field; dead-without-error remains silent.
//! The notification goes through the N32 leader funnel.

#![allow(clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use rusqlite::Connection;
use team_agent::coordinator::types::{ProviderRegistry, WorkspacePath};
use team_agent::coordinator::Coordinator;
use team_agent::event_log::EventLog;
use team_agent::message_store::MessageStore;
use team_agent::messaging::{send_to_leader_receiver, DeliveryStatus};
use team_agent::model::ids::TaskId;
use team_agent::provider::ProviderAdapter;
use team_agent::provider::{latest_explicit_error_fact, FactKind, Provider};
use team_agent::transport::{
    AttachOutcome, BackendKind, CaptureRange, CapturedText, InjectPayload, InjectReport, Key,
    PaneField, PaneId, PaneInfo, PaneLiveness, SessionName, SetEnvOutcome, SpawnResult, Target,
    Transport, TransportError, WindowName,
};

#[test]
fn notification_abnormal_exit_real_tick_matrix_requires_fresh_latest_error_once() {
    let workspace = temp_workspace("real-tick-matrix");
    let rollout = workspace.join("rollout-w1.jsonl");
    let old = codex_failed_turn("turn-old");
    let new = codex_failed_turn("turn-new");
    let completed = codex_completed_turn("turn-complete");
    std::fs::write(&rollout, &old).unwrap();
    seed_abnormal_state(&workspace, &rollout, "alive");
    let coordinator = abnormal_test_coordinator(&workspace);

    coordinator.tick().unwrap();
    let first_events = read_test_events(&workspace);
    assert!(find_events(&first_events, "worker.abnormal_exit").is_empty());
    assert_eq!(
        find_event(&first_events, "worker.abnormal_exit.check")["error_recency"],
        "stale"
    );

    std::fs::OpenOptions::new()
        .append(true)
        .open(&rollout)
        .unwrap()
        .write_all(new.as_bytes())
        .unwrap();
    coordinator.tick().unwrap();
    let fresh_events = read_test_events(&workspace);
    let abnormal = find_event(&fresh_events, "worker.abnormal_exit");
    assert_eq!(abnormal["turn_id"], "turn-new");
    assert_eq!(abnormal["error_recency"], "fresh");
    assert_eq!(abnormal["notification_status"], "queued");
    assert_eq!(leader_abnormal_claims(&workspace), 1);
    let (result_id, message_id) = leader_abnormal_claim(&workspace);
    assert!(result_id.starts_with("worker.abnormal_exit:"));
    assert_eq!(
        abnormal["notification_message_id"].as_str(),
        Some(message_id.as_str())
    );
    for event in find_events(&fresh_events, "deliver_to_leader.submit") {
        assert_eq!(event["result_id"].as_str(), Some(result_id.as_str()));
    }

    coordinator.tick().unwrap();
    let repeated_events = read_test_events(&workspace);
    assert_eq!(
        find_events(&repeated_events, "worker.abnormal_exit").len(),
        1
    );
    assert_eq!(leader_abnormal_claims(&workspace), 1);

    std::fs::OpenOptions::new()
        .append(true)
        .open(&rollout)
        .unwrap()
        .write_all(completed.as_bytes())
        .unwrap();
    coordinator.tick().unwrap();
    let stale_events = read_test_events(&workspace);
    assert_eq!(find_events(&stale_events, "worker.abnormal_exit").len(), 1);
    assert_eq!(
        find_event(&stale_events, "worker.abnormal_exit.check")["notification"],
        false
    );

    let dead_workspace = temp_workspace("dead-only");
    let dead_rollout = dead_workspace.join("rollout-w1.jsonl");
    std::fs::write(&dead_rollout, completed).unwrap();
    seed_abnormal_state(&dead_workspace, &dead_rollout, "dead");
    abnormal_test_coordinator(&dead_workspace).tick().unwrap();
    let dead_events = read_test_events(&dead_workspace);
    assert!(find_events(&dead_events, "worker.abnormal_exit").is_empty());
    assert_eq!(
        find_event(&dead_events, "abnormal_exit.single_signal_suppressed")["reason"],
        "dead_only"
    );

    std::fs::remove_dir_all(workspace).unwrap();
    std::fs::remove_dir_all(dead_workspace).unwrap();
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

struct NormalRegistry;

impl ProviderRegistry for NormalRegistry {
    fn adapter_for(&self, provider: Provider) -> Box<dyn ProviderAdapter> {
        team_agent::provider::get_adapter(provider)
    }
}

#[derive(Default)]
struct HermeticTransport;

impl Transport for HermeticTransport {
    fn kind(&self) -> BackendKind {
        BackendKind::Tmux
    }

    fn spawn_first(
        &self,
        _session: &SessionName,
        _window: &WindowName,
        _argv: &[String],
        _cwd: &Path,
        _env: &BTreeMap<String, String>,
    ) -> Result<SpawnResult, TransportError> {
        Err(unsupported())
    }

    fn spawn_into(
        &self,
        _session: &SessionName,
        _window: &WindowName,
        _argv: &[String],
        _cwd: &Path,
        _env: &BTreeMap<String, String>,
    ) -> Result<SpawnResult, TransportError> {
        Err(unsupported())
    }

    fn inject(
        &self,
        _target: &Target,
        _payload: &InjectPayload,
        _submit: Key,
        _bracketed: bool,
    ) -> Result<InjectReport, TransportError> {
        Err(unsupported())
    }

    fn send_keys(&self, _target: &Target, _keys: &[Key]) -> Result<(), TransportError> {
        Ok(())
    }

    fn capture(
        &self,
        _target: &Target,
        range: CaptureRange,
    ) -> Result<CapturedText, TransportError> {
        Ok(CapturedText {
            text: String::new(),
            range,
        })
    }

    fn query(&self, _target: &Target, _field: PaneField) -> Result<Option<String>, TransportError> {
        Ok(None)
    }

    fn liveness(&self, _pane: &PaneId) -> Result<PaneLiveness, TransportError> {
        Ok(PaneLiveness::Unknown)
    }

    fn list_targets(&self) -> Result<Vec<PaneInfo>, TransportError> {
        Ok(Vec::new())
    }

    fn has_session(&self, _session: &SessionName) -> Result<bool, TransportError> {
        Ok(true)
    }

    fn list_windows(&self, _session: &SessionName) -> Result<Vec<WindowName>, TransportError> {
        Ok(Vec::new())
    }

    fn set_session_env(
        &self,
        _session: &SessionName,
        _key: &str,
        _value: &str,
    ) -> Result<SetEnvOutcome, TransportError> {
        Ok(SetEnvOutcome::Applied)
    }

    fn kill_session(&self, _session: &SessionName) -> Result<(), TransportError> {
        Ok(())
    }

    fn kill_window(&self, _target: &Target) -> Result<(), TransportError> {
        Ok(())
    }

    fn attach_session(&self, _session: &SessionName) -> Result<AttachOutcome, TransportError> {
        Ok(AttachOutcome::Attached)
    }
}

fn unsupported() -> TransportError {
    TransportError::MuxUnavailable {
        backend: BackendKind::Tmux,
        detail: "not part of abnormal notification fixture".to_string(),
    }
}

fn abnormal_test_coordinator(workspace: &Path) -> Coordinator {
    Coordinator::new(
        WorkspacePath::new(workspace.to_path_buf()),
        Box::new(NormalRegistry),
        Box::new(HermeticTransport),
    )
}

fn seed_abnormal_state(workspace: &Path, rollout: &Path, liveness: &str) {
    team_agent::state::persist::save_runtime_state(
        workspace,
        &serde_json::json!({
            "active_team_key": "team",
            "agents": {
                "w1": {
                    "provider": "codex",
                    "status": "running",
                    "agent_id": "w1",
                    "session_id": "session-w1",
                    "rollout_path": rollout,
                    "spawn_cwd": workspace,
                    "spawn_epoch": 1,
                    "process_liveness": liveness
                }
            }
        }),
    )
    .unwrap();
}

fn read_test_events(workspace: &Path) -> Vec<serde_json::Value> {
    let path = team_agent::model::paths::logs_dir(workspace).join("events.jsonl");
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

fn find_events(events: &[serde_json::Value], name: &str) -> Vec<serde_json::Value> {
    events
        .iter()
        .filter(|event| event["event"] == name)
        .cloned()
        .collect()
}

fn find_event(events: &[serde_json::Value], name: &str) -> serde_json::Value {
    events
        .iter()
        .rev()
        .find(|event| event["event"] == name)
        .cloned()
        .unwrap_or_else(|| panic!("missing event {name}: {events:?}"))
}

fn leader_abnormal_claims(workspace: &Path) -> i64 {
    let store = MessageStore::open(workspace).unwrap();
    let conn = Connection::open(store.db_path()).unwrap();
    conn.query_row(
        "select count(*) from leader_notification_log where result_id like 'worker.abnormal_exit:%'",
        [],
        |row| row.get(0),
    )
    .unwrap()
}

fn leader_abnormal_claim(workspace: &Path) -> (String, String) {
    let store = MessageStore::open(workspace).unwrap();
    let conn = Connection::open(store.db_path()).unwrap();
    conn.query_row(
        "select result_id, notified_message_id from leader_notification_log where result_id like 'worker.abnormal_exit:%'",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )
    .unwrap()
}

#[test]
fn notification_abnormal_exit_claude_api_error_shape_is_classified() {
    let record = serde_json::json!({
        "type": "assistant",
        "uuid": "assistant-404",
        "message": {"role": "assistant", "content": [{"type": "text", "text": "API Error"}]},
        "error": "model_not_found",
        "isApiErrorMessage": true,
        "apiErrorStatus": 404,
        "requestId": "req-404"
    });
    let fact = latest_explicit_error_fact(Provider::ClaudeCode, &format!("{record}\n"))
        .expect("assistant API-error record is an explicit Claude fault");
    assert_eq!(fact.kind, FactKind::Error);
    assert_eq!(
        fact.turn_id.as_ref().map(|id| id.as_str()),
        Some("assistant-404")
    );
    assert_eq!(fact.api_error_status, Some(404));
    assert_eq!(fact.error.as_deref(), Some("model_not_found"));
    assert_eq!(fact.request_id.as_deref(), Some("req-404"));
    assert_eq!(fact.assistant_uuid.as_deref(), Some("assistant-404"));
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
