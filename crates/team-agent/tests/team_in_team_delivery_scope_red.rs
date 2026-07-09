//! Team-in-team delivery scope contracts.
//!
//! #244: one workspace daemon must multiplex delivery for every runtime team in
//! `state.teams`. A child-team message row can carry the correct
//! `owner_team_id=teamB`; delivery still fails if the daemon resolves every pending
//! row through the top-level/active teamA projection.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[path = "support/hermetic.rs"]
mod hermetic_guard;
#[allow(dead_code)]
fn _hermetic_boundary_marker(_: &hermetic_guard::HermeticTestEnv) {}

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use rusqlite::params;
use serde_json::{json, Value};
use team_agent::event_log::EventLog;
use team_agent::message_store::MessageStore;
use team_agent::messaging::deliver_pending_messages;
use team_agent::transport::{
    AttachOutcome, BackendKind, CaptureRange, CapturedText, InjectPayload, InjectReport,
    InjectStage, InjectVerification, Key, PaneField, PaneId, PaneInfo, PaneLiveness,
    SessionName, SetEnvOutcome, SpawnResult, SubmitVerification, Target, Transport,
    TransportError, TurnVerification, WindowName,
};

#[test]
fn multi_team_daemon_flushes_child_team_pending_rows_using_owner_team_projection() {
    let workspace = tmp_dir("multi-team");
    let state = multi_team_state();
    let store = MessageStore::open(&workspace).unwrap();
    let log = EventLog::new(&workspace);
    let transport = RecordingDeliveryTransport::default();
    let team_a_msg = store
        .create_message(None, "leader", "a1", "teamA work", None, false, Some("teamA"))
        .unwrap();
    let team_b_msg = store
        .create_message(None, "leader", "w1", "teamB child work", None, false, Some("teamB"))
        .unwrap();

    let delivered = deliver_pending_messages(&workspace, &state, &transport, &log)
        .expect("workspace daemon should process pending delivery rows");

    assert!(
        delivered.contains(&team_a_msg) && delivered.contains(&team_b_msg),
        "one workspace daemon must flush pending rows for every owner_team_id, not only active/top teamA; delivered={delivered:?}"
    );
    assert_eq!(message_status(&workspace, &team_a_msg).as_deref(), Some("delivered"));
    assert_eq!(
        message_status(&workspace, &team_b_msg).as_deref(),
        Some("delivered"),
        "child teamB pending row must advance out of accepted/pending; attempts/status={:?}",
        message_row(&workspace, &team_b_msg)
    );
    assert_eq!(
        delivery_attempts(&workspace, &team_b_msg),
        Some(1),
        "child teamB pending row must be claimed/flushed by the daemon; delivery_attempts must advance from 0"
    );
    assert_eq!(
        owner_team_id(&workspace, &team_b_msg).as_deref(),
        Some("teamB"),
        "delivery must preserve the child row's owner_team_id for scoped retry/writeback"
    );

    let targets = transport.inject_targets();
    assert!(
        targets.iter().any(|target| session_window(target, "team-teamA", "a1")),
        "fixture guard: active teamA message should inject to team-teamA:a1; targets={targets:?}"
    );
    assert!(
        targets.iter().any(|target| session_window(target, "team-teamB", "w1")),
        "child teamB message must be resolved with the owner_team_id=teamB projection, not the active teamA session/window; targets={targets:?}"
    );
}

#[test]
fn single_team_pending_delivery_still_uses_the_top_level_projection() {
    let workspace = tmp_dir("single-team");
    let state = single_team_state();
    let store = MessageStore::open(&workspace).unwrap();
    let log = EventLog::new(&workspace);
    let transport = RecordingDeliveryTransport::default();
    let msg = store
        .create_message(None, "leader", "w1", "single team work", None, false, Some("teamB"))
        .unwrap();

    let delivered = deliver_pending_messages(&workspace, &state, &transport, &log)
        .expect("single-team pending delivery should keep working");

    assert_eq!(delivered, vec![msg.clone()]);
    assert_eq!(message_status(&workspace, &msg).as_deref(), Some("delivered"));
    assert_eq!(delivery_attempts(&workspace, &msg), Some(1));
    let targets = transport.inject_targets();
    assert_eq!(
        targets.len(),
        1,
        "single-team guard should perform one physical injection; targets={targets:?}"
    );
    assert!(
        session_window(&targets[0], "team-teamB", "w1"),
        "single-team guard must preserve existing top-level projection behavior; targets={targets:?}"
    );
}

fn multi_team_state() -> Value {
    json!({
        "active_team_key": "teamA",
        "session_name": "team-teamA",
        "agents": {
            "a1": {"status": "running", "provider": "codex", "window": "a1"}
        },
        "tasks": [],
        "teams": {
            "teamA": {
                "status": "alive",
                "session_name": "team-teamA",
                "agents": {
                    "a1": {"status": "running", "provider": "codex", "window": "a1"}
                },
                "tasks": []
            },
            "teamB": {
                "status": "alive",
                "session_name": "team-teamB",
                "agents": {
                    "w1": {"status": "running", "provider": "codex", "window": "w1"}
                },
                "tasks": []
            }
        }
    })
}

fn single_team_state() -> Value {
    json!({
        "active_team_key": "teamB",
        "session_name": "team-teamB",
        "agents": {
            "w1": {"status": "running", "provider": "codex", "window": "w1"}
        },
        "tasks": [],
        "teams": {
            "teamB": {
                "status": "alive",
                "session_name": "team-teamB",
                "agents": {
                    "w1": {"status": "running", "provider": "codex", "window": "w1"}
                },
                "tasks": []
            }
        }
    })
}

fn session_window(target: &Target, session: &str, window: &str) -> bool {
    matches!(
        target,
        Target::SessionWindow {
            session: actual_session,
            window: actual_window,
        } if actual_session.as_str() == session && actual_window.as_str() == window
    )
}

fn message_status(workspace: &Path, message_id: &str) -> Option<String> {
    message_row(workspace, message_id).map(|(status, _, _)| status)
}

fn owner_team_id(workspace: &Path, message_id: &str) -> Option<String> {
    message_row(workspace, message_id).and_then(|(_, owner, _)| owner)
}

fn delivery_attempts(workspace: &Path, message_id: &str) -> Option<i64> {
    message_row(workspace, message_id).map(|(_, _, attempts)| attempts)
}

fn message_row(workspace: &Path, message_id: &str) -> Option<(String, Option<String>, i64)> {
    let store = MessageStore::open(workspace).unwrap();
    let conn = team_agent::db::schema::open_db(store.db_path()).unwrap();
    conn.query_row(
        "select status, owner_team_id, delivery_attempts from messages where message_id = ?1",
        params![message_id],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )
    .ok()
}

fn tmp_dir(tag: &str) -> PathBuf {
    static N: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "ta-rs-team-in-team-delivery-scope-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::canonicalize(dir).unwrap()
}

#[derive(Debug, Default)]
struct RecordingDeliveryTransport {
    targets: Mutex<Vec<Target>>,
}

impl RecordingDeliveryTransport {
    fn inject_targets(&self) -> Vec<Target> {
        self.targets.lock().unwrap().clone()
    }

    fn spawn_result(session: &SessionName, window: &WindowName) -> SpawnResult {
        SpawnResult {
            pane_id: PaneId::new("%fixture"),
            session: session.clone(),
            window: window.clone(),
            child_pid: None,
        }
    }
}

impl Transport for RecordingDeliveryTransport {
    fn kind(&self) -> BackendKind {
        BackendKind::Tmux
    }

    fn spawn_first(
        &self,
        session: &SessionName,
        window: &WindowName,
        _argv: &[String],
        _cwd: &Path,
        _env: &BTreeMap<String, String>,
    ) -> Result<SpawnResult, TransportError> {
        Ok(Self::spawn_result(session, window))
    }

    fn spawn_into(
        &self,
        session: &SessionName,
        window: &WindowName,
        _argv: &[String],
        _cwd: &Path,
        _env: &BTreeMap<String, String>,
    ) -> Result<SpawnResult, TransportError> {
        Ok(Self::spawn_result(session, window))
    }

    fn inject(
        &self,
        target: &Target,
        _payload: &InjectPayload,
        _submit: Key,
        _bracketed: bool,
    ) -> Result<InjectReport, TransportError> {
        self.targets.lock().unwrap().push(target.clone());
        Ok(InjectReport {
            stage_reached: InjectStage::Submit,
            inject_verification: InjectVerification::CaptureContainsToken,
            submit_verification: SubmitVerification::PastedContentPromptAbsentAfterSubmit,
            turn_verification: TurnVerification::NotYetObserved,
            attempts: 1,
            submit_diagnostics: None,
        })
    }

    fn send_keys(&self, _target: &Target, _keys: &[Key]) -> Result<(), TransportError> {
        Ok(())
    }

    fn capture(&self, _target: &Target, range: CaptureRange) -> Result<CapturedText, TransportError> {
        Ok(CapturedText { text: String::new(), range })
    }

    fn query(&self, _target: &Target, _field: PaneField) -> Result<Option<String>, TransportError> {
        Ok(None)
    }

    fn liveness(&self, _pane: &PaneId) -> Result<PaneLiveness, TransportError> {
        Ok(PaneLiveness::Live)
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
