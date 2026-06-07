//! Team-in-team sibling quick-start contracts.
//!
//! #241: two team directories under the same parent workspace must be independent teams. An
//! existing runtime for `teamA` must not make `quick-start teamB` return `ExistingRuntime` before
//! reaching the existing CR-040/042 per-team state/session merge path.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use serial_test::serial;
use serde_json::Value;
use team_agent::lifecycle::{quick_start_with_transport, QuickStartReport};
use team_agent::state::persist::load_runtime_state;
use team_agent::transport::{
    AttachOutcome, BackendKind, CaptureRange, CapturedText, InjectPayload, InjectReport,
    InjectStage, InjectVerification, Key, PaneField, PaneId, PaneInfo, SessionName,
    SetEnvOutcome, SpawnResult, SubmitVerification, Target, Transport, TransportError,
    TurnVerification, WindowName,
};

#[test]
#[serial(env)]
fn quick_start_sibling_teamdir_in_same_workspace_starts_new_team_not_existing_runtime() {
    let _env = EnvGuard::unset([
        "TMUX",
        "TMUX_PANE",
        "TEAM_AGENT_ID",
        "TEAM_AGENT_TEAM_ID",
        "TEAM_AGENT_LEADER_PANE_ID",
        "TEAM_AGENT_LEADER_SESSION_UUID",
        "TEAM_AGENT_LEADER_SESSION_UUID_OVERRIDE",
        "TEAM_AGENT_LEADER_PROVIDER",
    ]);
    let root = tmp_dir("sibling-teamdirs");
    seed_healthy_coordinator(&root);
    let team_a = team_dir(&root, "teamA", "worker_a");
    let team_b = team_dir(&root, "teamB", "worker_b");
    let transport = SessionRecordingTransport::default();

    let first = quick_start_with_transport(
        &team_a,
        Some("teamA"),
        true,
        false,
        Some("teamA"),
        &transport,
    )
    .expect("fixture: first teamA quick-start should succeed");
    assert_ready_team("teamA first quick-start", &first, "team-teamA");

    let second = quick_start_with_transport(
        &team_b,
        Some("teamB"),
        true,
        false,
        Some("teamB"),
        &transport,
    )
    .expect("teamB quick-start must not be refused by teamA's runtime");

    assert_ready_team(
        "sibling teamB quick-start",
        &second,
        "team-teamB",
    );
    let state = load_runtime_state(&root).expect("state.json should exist in shared parent workspace");
    assert_team_present(&state, "teamA", "team-teamA");
    assert_team_present(&state, "teamB", "team-teamB");
    assert_eq!(
        state.get("active_team_key").and_then(Value::as_str),
        Some("teamB"),
        "the newly launched sibling team should become the active top-level projection; state={state}"
    );
    let sessions = transport.spawned_sessions();
    assert!(
        sessions.iter().any(|session| session == "team-teamA")
            && sessions.iter().any(|session| session == "team-teamB"),
        "sibling teamdirs must spawn independent tmux sessions derived from requested team identity; sessions={sessions:?}"
    );
}

#[test]
#[serial(env)]
fn quick_start_sibling_teamdir_without_team_arg_infers_compiled_spec_name() {
    let _env = EnvGuard::unset([
        "TMUX",
        "TMUX_PANE",
        "TEAM_AGENT_ID",
        "TEAM_AGENT_TEAM_ID",
        "TEAM_AGENT_LEADER_PANE_ID",
        "TEAM_AGENT_LEADER_SESSION_UUID",
        "TEAM_AGENT_LEADER_SESSION_UUID_OVERRIDE",
        "TEAM_AGENT_LEADER_PROVIDER",
    ]);
    let root = tmp_dir("sibling-no-team-infers-spec-name");
    seed_healthy_coordinator(&root);
    let team_a = team_dir(&root, "teamA", "worker_a");
    let team_b = team_dir(&root, "teamB", "worker_b");
    let transport = SessionRecordingTransport::default();

    let first = quick_start_with_transport(
        &team_a,
        Some("teamA"),
        true,
        false,
        Some("teamA"),
        &transport,
    )
    .expect("fixture: first teamA quick-start should succeed");
    assert_ready_team("teamA first quick-start", &first, "team-teamA");

    let second = quick_start_with_transport(
        &team_b,
        None,
        true,
        false,
        None,
        &transport,
    )
    .expect("quick-start teamB without --team should still use TEAM.md name=teamB");

    assert_ready_team(
        "sibling teamB quick-start without --team",
        &second,
        "team-teamB",
    );
    let state = load_runtime_state(&root).expect("state.json should exist in shared parent workspace");
    assert_team_present(&state, "teamA", "team-teamA");
    assert_team_present(&state, "teamB", "team-teamB");
    assert_eq!(
        state.get("active_team_key").and_then(Value::as_str),
        Some("teamB"),
        "quick-start <teamBdir> without --team must infer compiled spec name=teamB and activate that sibling team; state={state}"
    );
    let sessions = transport.spawned_sessions();
    assert!(
        sessions.iter().any(|session| session == "team-teamA")
            && sessions.iter().any(|session| session == "team-teamB"),
        "missing --team must not collapse sibling teamB into teamA's existing runtime; sessions={sessions:?}"
    );
}

#[test]
#[serial(env)]
fn quick_start_sibling_teamdir_without_team_arg_same_spec_name_still_returns_existing_runtime() {
    let _env = EnvGuard::unset([
        "TMUX",
        "TMUX_PANE",
        "TEAM_AGENT_ID",
        "TEAM_AGENT_TEAM_ID",
        "TEAM_AGENT_LEADER_PANE_ID",
        "TEAM_AGENT_LEADER_SESSION_UUID",
        "TEAM_AGENT_LEADER_SESSION_UUID_OVERRIDE",
        "TEAM_AGENT_LEADER_PROVIDER",
    ]);
    let root = tmp_dir("sibling-no-team-same-spec-name");
    seed_healthy_coordinator(&root);
    let team_a = team_dir(&root, "teamA", "worker_a");
    let team_b_same_name = team_dir_with_name(&root, "teamB", "teamA", "worker_b");
    let transport = SessionRecordingTransport::default();

    let first = quick_start_with_transport(
        &team_a,
        Some("teamA"),
        true,
        false,
        Some("teamA"),
        &transport,
    )
    .expect("fixture: first teamA quick-start should succeed");
    assert_ready_team("teamA first quick-start", &first, "team-teamA");

    let duplicate = quick_start_with_transport(
        &team_b_same_name,
        None,
        true,
        false,
        None,
        &transport,
    )
    .expect("same spec.name should return a typed ExistingRuntime report");

    match duplicate {
        QuickStartReport::ExistingRuntime {
            session_name,
            state_path,
            ..
        } => {
            assert_eq!(session_name.as_ref().map(|s| s.as_str()), Some("team-teamA"));
            assert!(
                state_path.as_ref().is_some_and(|path| path.starts_with(&root)),
                "same-name ExistingRuntime should point at the shared workspace state path; state_path={state_path:?}"
            );
        }
        other => panic!(
            "when sibling teamdir compiles to existing spec.name=teamA, quick-start without --team should remain ExistingRuntime; got {other:?}"
        ),
    }
    assert_eq!(
        transport.spawned_sessions(),
        vec!["team-teamA".to_string()],
        "same compiled spec.name must not spawn a second sibling session without --fresh"
    );
}

#[test]
#[serial(env)]
fn quick_start_same_existing_team_still_returns_existing_runtime() {
    let _env = EnvGuard::unset([
        "TMUX",
        "TMUX_PANE",
        "TEAM_AGENT_ID",
        "TEAM_AGENT_TEAM_ID",
        "TEAM_AGENT_LEADER_PANE_ID",
        "TEAM_AGENT_LEADER_SESSION_UUID",
        "TEAM_AGENT_LEADER_SESSION_UUID_OVERRIDE",
        "TEAM_AGENT_LEADER_PROVIDER",
    ]);
    let root = tmp_dir("same-team-existing");
    seed_healthy_coordinator(&root);
    let team_a = team_dir(&root, "teamA", "worker_a");
    let transport = SessionRecordingTransport::default();

    let first = quick_start_with_transport(
        &team_a,
        Some("teamA"),
        true,
        false,
        Some("teamA"),
        &transport,
    )
    .expect("fixture: first teamA quick-start should succeed");
    assert_ready_team("teamA first quick-start", &first, "team-teamA");

    let duplicate = quick_start_with_transport(
        &team_a,
        Some("teamA"),
        true,
        false,
        Some("teamA"),
        &transport,
    )
    .expect("same-team duplicate should return a typed report, not an error");

    match duplicate {
        QuickStartReport::ExistingRuntime {
            team,
            session_name,
            state_path,
            ..
        } => {
            assert_eq!(team.as_deref(), Some("teamA"));
            assert_eq!(session_name.as_ref().map(|s| s.as_str()), Some("team-teamA"));
            assert!(
                state_path.as_ref().is_some_and(|path| path.starts_with(&root)),
                "ExistingRuntime should point at the shared workspace state path; state_path={state_path:?}"
            );
        }
        other => panic!(
            "same requested team with an existing runtime should remain ExistingRuntime; got {other:?}"
        ),
    }
    assert_eq!(
        transport.spawned_sessions(),
        vec!["team-teamA".to_string()],
        "same-team duplicate must not spawn another session without --fresh"
    );
}

fn assert_ready_team(label: &str, report: &QuickStartReport, expected_session: &str) {
    match report {
        QuickStartReport::Ready { session_name, .. } => assert_eq!(
            session_name.as_str(),
            expected_session,
            "{label}: session_name should derive from requested team identity"
        ),
        other => panic!("{label}: expected Ready, got {other:?}"),
    }
}

fn assert_team_present(state: &Value, team_key: &str, expected_session: &str) {
    let Some(team) = state.get("teams").and_then(|teams| teams.get(team_key)) else {
        panic!("state.teams.{team_key} must exist after sibling quick-start; state={state}");
    };
    assert_eq!(
        team.get("session_name").and_then(Value::as_str),
        Some(expected_session),
        "state.teams.{team_key}.session_name must be isolated per requested team; team={team}"
    );
    assert!(
        team.get("agents").and_then(Value::as_object).is_some_and(|agents| !agents.is_empty()),
        "state.teams.{team_key}.agents must be retained; team={team}"
    );
}

fn team_dir(root: &Path, name: &str, agent_id: &str) -> PathBuf {
    team_dir_with_name(root, name, name, agent_id)
}

fn team_dir_with_name(root: &Path, dir_name: &str, spec_name: &str, agent_id: &str) -> PathBuf {
    let team = root.join(dir_name);
    std::fs::create_dir_all(team.join("agents")).unwrap();
    std::fs::write(
        team.join("TEAM.md"),
        format!(
            "---\nname: {spec_name}\nobjective: Sibling team quick-start contract.\nprovider: codex\n---\n\n{spec_name} team.\n"
        ),
    )
    .unwrap();
    std::fs::write(team.join("agents").join(format!("{agent_id}.md")), role_doc(agent_id)).unwrap();
    team
}

fn role_doc(name: &str) -> String {
    format!(
        "---\nname: {name}\nrole: Worker\nprovider: codex\nmodel: gpt-5.5\nauth_mode: subscription\ntools:\n  - mcp_team\n---\n\nWorker.\n"
    )
}

fn seed_healthy_coordinator(workspace: &Path) {
    let workspace = team_agent::coordinator::WorkspacePath::new(workspace.to_path_buf());
    std::fs::create_dir_all(team_agent::model::paths::runtime_dir(workspace.as_path())).unwrap();
    let _ = team_agent::message_store::MessageStore::open(workspace.as_path()).unwrap();
    let pid = team_agent::coordinator::Pid::new(std::process::id());
    team_agent::coordinator::write_coordinator_metadata(
        &workspace,
        pid,
        team_agent::coordinator::MetadataSource::Boot,
    )
    .unwrap();
    std::fs::write(
        team_agent::coordinator::coordinator_pid_path(&workspace),
        pid.to_string(),
    )
    .unwrap();
}

fn tmp_dir(tag: &str) -> PathBuf {
    static N: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "ta-rs-team-in-team-sibling-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::canonicalize(dir).unwrap()
}

struct EnvGuard {
    previous: Vec<(&'static str, Option<String>)>,
}

impl EnvGuard {
    fn unset(keys: [&'static str; 8]) -> Self {
        let previous = keys
            .iter()
            .map(|key| (*key, std::env::var(key).ok()))
            .collect::<Vec<_>>();
        for key in keys {
            unsafe {
                std::env::remove_var(key);
            }
        }
        Self { previous }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (key, value) in self.previous.drain(..).rev() {
            unsafe {
                if let Some(value) = value {
                    std::env::set_var(key, value);
                } else {
                    std::env::remove_var(key);
                }
            }
        }
    }
}

#[derive(Debug, Default)]
struct SessionRecordingTransport {
    sessions: Mutex<HashSet<String>>,
    spawned: Mutex<Vec<String>>,
}

impl SessionRecordingTransport {
    fn spawned_sessions(&self) -> Vec<String> {
        self.spawned.lock().unwrap().clone()
    }

    fn spawn_result(
        &self,
        session: &SessionName,
        window: &WindowName,
        kind: &'static str,
    ) -> SpawnResult {
        self.sessions
            .lock()
            .unwrap()
            .insert(session.as_str().to_string());
        let mut spawned = self.spawned.lock().unwrap();
        spawned.push(session.as_str().to_string());
        SpawnResult {
            pane_id: PaneId::new(format!("%{}-{kind}", spawned.len())),
            session: session.clone(),
            window: window.clone(),
            child_pid: None,
        }
    }
}

impl Transport for SessionRecordingTransport {
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
        Ok(self.spawn_result(session, window, "first"))
    }

    fn spawn_into(
        &self,
        session: &SessionName,
        window: &WindowName,
        _argv: &[String],
        _cwd: &Path,
        _env: &BTreeMap<String, String>,
    ) -> Result<SpawnResult, TransportError> {
        Ok(self.spawn_result(session, window, "into"))
    }

    fn inject(
        &self,
        _target: &Target,
        _payload: &InjectPayload,
        _submit: Key,
        _bracketed: bool,
    ) -> Result<InjectReport, TransportError> {
        Ok(InjectReport {
            stage_reached: InjectStage::Submit,
            inject_verification: InjectVerification::CaptureContainsToken,
            submit_verification: SubmitVerification::EnterSentWithoutPlaceholderCheck,
            turn_verification: TurnVerification::NotYetObserved,
            attempts: 1,
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
            text: String::new(),
            range,
        })
    }

    fn query(
        &self,
        _target: &Target,
        _field: PaneField,
    ) -> Result<Option<String>, TransportError> {
        Ok(None)
    }

    fn liveness(
        &self,
        _pane: &PaneId,
    ) -> Result<team_agent::transport::PaneLiveness, TransportError> {
        Ok(team_agent::transport::PaneLiveness::Live)
    }

    fn list_targets(&self) -> Result<Vec<PaneInfo>, TransportError> {
        Ok(Vec::new())
    }

    fn has_session(&self, session: &SessionName) -> Result<bool, TransportError> {
        Ok(self.sessions.lock().unwrap().contains(session.as_str()))
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
