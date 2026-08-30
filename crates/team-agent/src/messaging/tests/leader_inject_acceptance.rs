//! Unit regressions for leader injection receipt acceptance.
//!
//! These fixtures intentionally seed DB/state/transcript internals. Per MUST-15,
//! that synthetic setup belongs only in src/**/tests and is not acceptance proof.

use super::*;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use crate::event_log::EventLog;
use crate::message_store::MessageStore;
use crate::transport::{
    AttachOutcome, BackendKind, CaptureRange, CapturedText, InjectPayload, InjectReport,
    InjectStage, InjectVerification, Key, PaneField, PaneId, PaneInfo, PaneLiveness, SessionName,
    SetEnvOutcome, SpawnResult, SubmitVerification, Target, Transport, TransportError,
    TurnVerification, WindowName,
};
use rusqlite::params;

#[test]
fn tmux_leader_submit_without_provider_receipt_stays_pending_then_accepts_same_row() {
    let case = Case::new("receipt");
    let transport = RecordingTransport::new("");
    let message_id = case.seed_message("receipt canary");

    let first = deliver_pending_message(
        &case.workspace,
        &case.store,
        &transport,
        &message_id,
        &case.event_log,
        &case.state,
    )
    .expect("first delivery attempt");

    assert!(
        first.ok,
        "verified transport submission is accepted even while provider receipt is pending: {first:?}"
    );
    assert_eq!(
        case.message_status(&message_id),
        "submitted_pending_acceptance"
    );
    assert_eq!(transport.inject_count(), 1);
    assert!(
        case.events().contains("leader_receiver.acceptance_pending"),
        "missing receipt must be observable"
    );

    std::fs::write(
        &case.rollout,
        format!(r#"{{"type":"user","message":"[team-agent-token:{message_id}]"}}"#),
    )
    .unwrap();
    let second = deliver_pending_message(
        &case.workspace,
        &case.store,
        &transport,
        &message_id,
        &case.event_log,
        &case.state,
    )
    .expect("receipt observation");

    assert!(
        second.ok,
        "provider receipt advances the same row: {second:?}"
    );
    assert_eq!(case.message_status(&message_id), "delivered");
    assert_eq!(
        transport.inject_count(),
        1,
        "receipt observation must not inject the message a second time"
    );
    assert!(case.delivery_consumed_at(&message_id).is_some());
}

#[test]
fn attached_leader_mailbox_is_rechecked_by_the_normal_delivery_tick() {
    let case = Case::new("mailbox");
    let transport = RecordingTransport::new("");
    let message_id = case.seed_message("mailbox canary");
    case.store
        .mark(&message_id, "queued_until_leader_attach", None)
        .unwrap();

    let delivered =
        deliver_pending_messages(&case.workspace, &case.state, &transport, &case.event_log)
            .expect("normal coordinator delivery pass");

    assert!(delivered.is_empty(), "provider receipt is still pending");
    assert_eq!(
        case.message_status(&message_id),
        "submitted_pending_acceptance"
    );
    assert_eq!(transport.inject_count(), 1);
    assert!(case.events().contains("leader_receiver.mailbox_requeued"));
}

#[test]
fn claimed_leader_delivery_revalidates_a_stale_unattached_snapshot() {
    let workspace = tmp_ws("leader-inject-acceptance-stale-attach");
    let rollout = workspace.join("leader.jsonl");
    std::fs::write(&rollout, "").unwrap();
    let current_state = serde_json::json!({
        "active_team_key": "acceptance-team",
        "teams": {
            "acceptance-team": {
                "session_name": "team-acceptance",
                "leader_receiver": {
                    "pane_id": "%leader",
                    "status": "attached",
                    "provider": "claude",
                    "rollout_path": rollout,
                }
            }
        }
    });
    crate::state::persist::save_runtime_state(&workspace, &current_state).unwrap();
    let store = MessageStore::open(&workspace).unwrap();
    let event_log = EventLog::new(&workspace);
    let message_id = store
        .create_message(
            None,
            "worker",
            "leader",
            "stale attach canary",
            None,
            false,
            Some("acceptance-team"),
        )
        .unwrap();
    let stale_state = serde_json::json!({
        "active_team_key": "acceptance-team",
        "teams": {
            "acceptance-team": {
                "session_name": "team-acceptance"
            }
        }
    });
    let transport = RecordingTransport::new("");

    let outcome = deliver_pending_message(
        &workspace,
        &store,
        &transport,
        &message_id,
        &event_log,
        &stale_state,
    )
    .expect("delivery from stale coordinator snapshot");

    assert_ne!(
        outcome.reason,
        Some(DeliveryRefusal::LeaderNotAttached),
        "a claim made after attach must revalidate durable state before writing leader_not_attached"
    );
    assert_ne!(
        outcome.message_status.0, "failed",
        "a stale pre-attach snapshot must not strand the requeued row"
    );
    assert_eq!(transport.inject_count(), 1);
}

#[test]
fn leader_submit_without_receipt_source_cannot_park_forever_without_a_completion_writer() {
    let case = Case::new_without_rollout("missing-receipt-source");
    let transport = RecordingTransport::new("");
    let message_id = case.seed_message("missing receipt source canary");

    let first = deliver_pending_message(
        &case.workspace,
        &case.store,
        &transport,
        &message_id,
        &case.event_log,
        &case.state,
    )
    .expect("verified leader transport submission");

    assert_ne!(
        case.message_status(&message_id),
        "submitted_pending_acceptance",
        "F4 acceptance-stall: a verified leader transport submit with no reachable \
         receipt source must not enter submitted_pending_acceptance, because that \
         protected state has no completion writer. The runtime may close the \
         transport-acceptance fact or expose a provider-session receipt writer, \
         but it must not report an indefinitely retryable fact-terminal. outcome={first:?}"
    );
    assert_eq!(
        transport.inject_count(),
        1,
        "the acceptance repair must not obtain progress by physically injecting twice"
    );

    let _ = deliver_pending_message(
        &case.workspace,
        &case.store,
        &transport,
        &message_id,
        &case.event_log,
        &case.state,
    );
    assert_eq!(
        transport.inject_count(),
        1,
        "a second coordinator observation must not replay the already-submitted leader message"
    );
}

#[test]
fn nonempty_missing_receipt_source_is_not_token_absent() {
    assert_unavailable_rollout_converges(
        "missing-rollout",
        PathBuf::from("definitely-missing-leader-rollout.jsonl"),
        false,
    );
}

#[test]
fn nonempty_unreadable_receipt_source_is_not_token_absent() {
    assert_unavailable_rollout_converges(
        "unreadable-rollout",
        PathBuf::from("leader-rollout-is-a-directory"),
        true,
    );
}

fn assert_unavailable_rollout_converges(tag: &str, rollout: PathBuf, directory: bool) {
    let case = Case::new_with_unavailable_rollout(tag, rollout, directory);
    let transport = RecordingTransport::new("");
    let message_id = case.seed_message("unavailable receipt source canary");

    let first = deliver_pending_message(
        &case.workspace,
        &case.store,
        &transport,
        &message_id,
        &case.event_log,
        &case.state,
    )
    .expect("verified leader transport submission");

    assert_eq!(
        case.message_status(&message_id),
        "injected_awaiting_receipt",
        "F4 receipt observation must distinguish an unreadable nonempty rollout_path \
         from a readable rollout that lacks the token. The former is \
         receipt-source-unavailable and must converge to the typed parked state, \
         not remain submitted_pending_acceptance. shape={tag} outcome={first:?}"
    );
    assert_eq!(
        first.verification.as_deref(),
        Some("leader_receipt_source_unavailable"),
        "the outcome must expose read-unavailable rather than TokenAbsent; shape={tag}"
    );
    assert_eq!(transport.inject_count(), 1);

    let _ = deliver_pending_message(
        &case.workspace,
        &case.store,
        &transport,
        &message_id,
        &case.event_log,
        &case.state,
    );
    assert_eq!(
        transport.inject_count(),
        1,
        "read-unavailable convergence must not obtain progress by a second physical \
         injection; shape={tag}"
    );
}

#[test]
fn worker_consumption_writer_remains_a_positive_control() {
    let case = Case::new("worker-positive-control");
    let message_id = case
        .store
        .create_message(
            None,
            "leader",
            "worker",
            "worker consume canary",
            None,
            false,
            None,
        )
        .unwrap();

    case.store.mark(&message_id, "consumed", None).unwrap();

    assert_eq!(
        case.message_status(&message_id),
        "consumed",
        "positive control: the independent worker consumption writer must remain reachable"
    );
}

struct Case {
    workspace: PathBuf,
    rollout: PathBuf,
    store: MessageStore,
    event_log: EventLog,
    state: serde_json::Value,
}

impl Case {
    fn new(tag: &str) -> Self {
        let workspace = tmp_ws(&format!("leader-inject-acceptance-{tag}"));
        let rollout = workspace.join("leader.jsonl");
        std::fs::write(&rollout, "").unwrap();
        let state = serde_json::json!({
            "active_team_key": "acceptance-team",
            "session_name": "team-acceptance",
            "leader_receiver": {
                "pane_id": "%leader",
                "status": "attached",
                "provider": "claude",
                "rollout_path": rollout,
            }
        });
        crate::state::persist::save_runtime_state(&workspace, &state).unwrap();
        let store = MessageStore::open(&workspace).unwrap();
        let event_log = EventLog::new(&workspace);
        Self {
            workspace,
            rollout,
            store,
            event_log,
            state,
        }
    }

    fn new_without_rollout(tag: &str) -> Self {
        let workspace = tmp_ws(&format!("leader-inject-acceptance-{tag}"));
        let rollout = workspace.join("absent-leader.jsonl");
        let state = serde_json::json!({
            "active_team_key": "acceptance-team",
            "session_name": "team-acceptance",
            "leader_receiver": {
                "pane_id": "%leader",
                "status": "attached",
                "provider": "claude"
            }
        });
        crate::state::persist::save_runtime_state(&workspace, &state).unwrap();
        let store = MessageStore::open(&workspace).unwrap();
        let event_log = EventLog::new(&workspace);
        Self {
            workspace,
            rollout,
            store,
            event_log,
            state,
        }
    }

    fn new_with_unavailable_rollout(tag: &str, relative: PathBuf, directory: bool) -> Self {
        let workspace = tmp_ws(&format!("leader-inject-acceptance-{tag}"));
        let rollout = workspace.join(relative);
        if directory {
            std::fs::create_dir_all(&rollout).unwrap();
        }
        let state = serde_json::json!({
            "active_team_key": "acceptance-team",
            "session_name": "team-acceptance",
            "leader_receiver": {
                "pane_id": "%leader",
                "status": "attached",
                "provider": "claude",
                "rollout_path": rollout,
            }
        });
        crate::state::persist::save_runtime_state(&workspace, &state).unwrap();
        let store = MessageStore::open(&workspace).unwrap();
        let event_log = EventLog::new(&workspace);
        Self {
            workspace,
            rollout,
            store,
            event_log,
            state,
        }
    }

    fn seed_message(&self, content: &str) -> String {
        self.store
            .create_message(None, "worker", "leader", content, None, false, None)
            .unwrap()
    }

    fn message_status(&self, message_id: &str) -> String {
        let conn = crate::db::schema::open_db(self.store.db_path()).unwrap();
        conn.query_row(
            "select status from messages where message_id = ?1",
            [message_id],
            |row| row.get(0),
        )
        .unwrap()
    }

    fn delivery_consumed_at(&self, message_id: &str) -> Option<String> {
        let conn = crate::db::schema::open_db(self.store.db_path()).unwrap();
        conn.query_row(
            "select consumed_at from delivery_tokens where message_id = ?1",
            params![message_id],
            |row| row.get(0),
        )
        .unwrap()
    }

    fn events(&self) -> String {
        self.event_log
            .tail(0)
            .unwrap()
            .into_iter()
            .map(|event| event.to_string())
            .collect::<Vec<_>>()
            .join("\n")
    }
}

#[derive(Clone)]
struct RecordingTransport {
    injects: Arc<AtomicUsize>,
    capture: Arc<Mutex<String>>,
}

impl RecordingTransport {
    fn new(capture: &str) -> Self {
        Self {
            injects: Arc::new(AtomicUsize::new(0)),
            capture: Arc::new(Mutex::new(capture.to_string())),
        }
    }

    fn inject_count(&self) -> usize {
        self.injects.load(Ordering::Relaxed)
    }
}

impl Transport for RecordingTransport {
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
        unreachable!("delivery contract does not spawn")
    }

    fn spawn_into(
        &self,
        _session: &SessionName,
        _window: &WindowName,
        _argv: &[String],
        _cwd: &Path,
        _env: &BTreeMap<String, String>,
    ) -> Result<SpawnResult, TransportError> {
        unreachable!("delivery contract does not spawn")
    }

    fn inject(
        &self,
        _target: &Target,
        _payload: &InjectPayload,
        _submit: Key,
        _bracketed_paste: bool,
    ) -> Result<InjectReport, TransportError> {
        self.injects.fetch_add(1, Ordering::Relaxed);
        Ok(InjectReport {
            stage_reached: InjectStage::Submit,
            inject_verification: InjectVerification::CaptureContainsToken,
            submit_verification: SubmitVerification::EnterSentWithoutPlaceholderCheck,
            turn_verification: TurnVerification::NotYetObserved,
            attempts: 1,
            submit_diagnostics: None,
        })
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
            text: self.capture.lock().unwrap().clone(),
            range,
        })
    }

    fn query(&self, _target: &Target, _field: PaneField) -> Result<Option<String>, TransportError> {
        Ok(None)
    }

    fn liveness(&self, _pane: &PaneId) -> Result<PaneLiveness, TransportError> {
        Ok(PaneLiveness::Live)
    }

    fn list_targets(&self) -> Result<Vec<PaneInfo>, TransportError> {
        Ok(vec![PaneInfo {
            pane_id: PaneId::new("%leader"),
            session: SessionName::new("team-acceptance"),
            window_index: None,
            window_name: Some(WindowName::new("leader")),
            pane_index: None,
            tty: None,
            current_command: Some("claude".to_string()),
            current_path: None,
            active: true,
            pane_pid: None,
            leader_env: BTreeMap::new(),
        }])
    }

    fn has_session(&self, _session: &SessionName) -> Result<bool, TransportError> {
        Ok(true)
    }

    fn list_windows(&self, _session: &SessionName) -> Result<Vec<WindowName>, TransportError> {
        Ok(vec![WindowName::new("leader")])
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
