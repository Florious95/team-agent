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


fn pi_receipt_case(tag: &str) -> (Case, serde_json::Value) {
    let mut case = Case::new_without_rollout(tag);
    let root = case.workspace.join("pi-sessions");
    std::fs::create_dir_all(&root).unwrap();
    case.rollout = root.join("not-derived-from-the-session-id.jsonl");
    let receiver = &mut case.state["leader_receiver"];
    receiver["provider"] = serde_json::json!("pi");
    receiver["tmux_socket"] = serde_json::json!(case.workspace.join("fixture-tmux.sock"));
    receiver["leader_session_uuid"] = serde_json::json!("team-owner-not-native-id");
    receiver["owner_epoch"] = serde_json::json!(1);
    receiver["native_session"] = serde_json::json!({
        "expected_session_id": "native-pi-session",
        "provider_projects_root": root,
        "spawn_cwd": case.workspace,
        "spawned_at": "2026-10-10T03:59:07Z",
        "pane_id": "%leader",
        "tmux_socket": receiver["tmux_socket"],
        "leader_session_uuid": "team-owner-not-native-id",
        "owner_epoch": 1,
    });
    crate::state::persist::save_runtime_state(&case.workspace, &case.state).unwrap();
    let header = serde_json::json!({
        "type": "session", "version": 3, "id": "native-pi-session",
        "cwd": case.workspace, "timestamp": "2026-10-10T03:59:08Z",
    });
    (case, header)
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
    endpoint: Option<String>,
}

impl RecordingTransport {
    fn new(capture: &str) -> Self {
        Self {
            injects: Arc::new(AtomicUsize::new(0)),
            capture: Arc::new(Mutex::new(capture.to_string())),
            endpoint: None,
        }
    }

    fn inject_count(&self) -> usize {
        self.injects.load(Ordering::Relaxed)
    }
}

impl Transport for RecordingTransport {
    fn tmux_endpoint(&self) -> Option<String> {
        self.endpoint.clone()
    }

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



