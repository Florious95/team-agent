//! Team-in-team explicit identity / workspace / add-agent scope contracts.
//!
//! #245/#246/#247 share the same failure family: the user-visible selected team
//! identity is accepted on the command surface, but later state/projection code
//! falls back to the team directory basename or raw top-level runtime state.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use serde_json::{json, Value};
use serial_test::serial;
use team_agent::lifecycle::{
    add_agent_with_transport, quick_start_with_transport, start_agent_with_transport,
    QuickStartReport,
};
use team_agent::model::ids::AgentId;
use team_agent::state::persist::{load_runtime_state, save_runtime_state};
use team_agent::transport::{
    AttachOutcome, BackendKind, CaptureRange, CapturedText, InjectPayload, InjectReport,
    InjectStage, InjectVerification, Key, PaneField, PaneId, PaneInfo, PaneLiveness,
    SessionName, SetEnvOutcome, SpawnResult, SubmitVerification, Target, Transport,
    TransportError, TurnVerification, WindowName,
};

#[test]
#[serial(env)]
fn quick_start_team_id_persists_requested_team_key_for_lifecycle_selection() {
    let _env = EnvGuard::unset();
    let root = tmp_dir("team-id-key");
    seed_healthy_coordinator(&root);
    let team_dir = write_team_dir(&root, "teamdir", "w1");
    let transport = RecordingTransport::default();

    let report = quick_start_with_transport(
        &team_dir,
        None,
        true,
        Some("lifeteam"),
        &transport,
    )
    .expect("quick-start --team-id lifeteam should launch via fake transport");
    assert_ready("quick-start --team-id lifeteam", &report, "team-lifeteam");

    let state = load_runtime_state(&root).expect("runtime state after quick-start");
    assert!(
        state.pointer("/teams/lifeteam").is_some(),
        "quick-start --team-id lifeteam must persist state.teams.lifeteam so lifecycle commands can select it; state={state}"
    );
    assert!(
        state.pointer("/teams/teamdir").is_none(),
        "requested --team-id lifeteam must not be silently replaced by the teamdir basename key; state={state}"
    );

    let start = start_agent_with_transport(
        &root,
        &AgentId::new("w1"),
        false,
        false,
        true,
        Some("lifeteam"),
        &transport,
    );
    assert!(
        start.is_ok(),
        "lifecycle command start-agent --team lifeteam must select the quick-started team by requested team id, not only by teamdir basename; result={start:?} state={state}"
    );
}

#[test]
fn quick_start_workspace_override_is_not_dropped_before_runtime_state_write() {
    let cli_types = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/cli/types.rs"
    ))
    .unwrap();
    let quick_start_args = source_section(&cli_types, "pub struct QuickStartArgs", "pub struct InitArgs");
    assert!(
        quick_start_args.contains("pub workspace: PathBuf"),
        "quick-start TEAMDIR --workspace WS must preserve WS in QuickStartArgs; otherwise runtime state is written to TEAMDIR's parent before lifecycle can honor --workspace. QuickStartArgs={quick_start_args}"
    );

    let cli_mod = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/cli/mod.rs"
    ))
    .unwrap();
    let lifecycle_port = source_section(&cli_mod, "pub fn quick_start(", "pub fn start_leader");
    assert!(
        !lifecycle_port.contains("let _ = workspace;"),
        "lifecycle_port::quick_start must not discard --workspace; runtime state must be written to the explicit WS. source={lifecycle_port}"
    );

    let emit = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/cli/emit.rs"
    ))
    .unwrap();
    let quick_start_args_fn = source_section(&emit, "fn quick_start_args", "fn init_args");
    assert!(
        quick_start_args_fn.contains("workspace: workspace.clone()")
            || quick_start_args_fn.contains("workspace,"),
        "quick_start_args must carry parsed --workspace into QuickStartArgs, not use it only to resolve a relative TEAMDIR. source={quick_start_args_fn}"
    );
}

#[test]
#[serial(env)]
fn add_agent_writes_new_agent_into_selected_team_projection_used_by_start_agent() {
    let _env = EnvGuard::set([
        ("TEAM_AGENT_ID", "worker_b"),
        ("TEAM_AGENT_TEAM_ID", "lifeteam"),
    ]);
    let root = tmp_dir("add-agent-team-projection");
    seed_healthy_coordinator(&root);
    let team_dir = write_team_dir(&root, "teamdir", "worker_a");
    seed_lifeteam_state(&root, &team_dir);
    let role_file = root.join("worker_b.md");
    std::fs::write(&role_file, role_doc("worker_b")).unwrap();
    let transport = RecordingTransport::default().with_session_present(true);

    let add = add_agent_with_transport(
        &team_dir,
        &AgentId::new("worker_b"),
        &role_file,
        false,
        Some("lifeteam"),
        &transport,
    );
    assert!(
        add.is_ok(),
        "add-agent --team lifeteam must write worker_b into the same teams.lifeteam projection start-agent reads; current failure is the real 'agent added not found' split. result={add:?}"
    );

    let state = load_runtime_state(&root).expect("runtime state after add-agent");
    assert!(
        state.pointer("/teams/lifeteam/agents/worker_b").is_some(),
        "add-agent --team lifeteam must persist the new worker in state.teams.lifeteam.agents, not only raw top-level agents; state={state}"
    );
    let start = start_agent_with_transport(
        &root,
        &AgentId::new("worker_b"),
        false,
        false,
        true,
        Some("lifeteam"),
        &transport,
    );
    assert!(
        start.is_ok(),
        "start-agent --team lifeteam worker_b must see the worker added by add-agent in the same selected-team scope; result={start:?} state={state}"
    );
}

#[test]
#[serial(env)]
fn add_agent_committed_roster_survives_live_coordinator_stale_snapshot_save() {
    let _env = EnvGuard::unset();
    let root = tmp_dir("add-agent-live-coordinator-clobber");
    let team_dir = write_team_dir(&root, "teamdir", "worker_a");
    seed_lifeteam_state(&root, &team_dir);

    let stale_coordinator_snapshot =
        load_runtime_state(&root).expect("coordinator captured pre-add snapshot");
    let mut after_add = stale_coordinator_snapshot.clone();
    insert_agent_in_state(&mut after_add, "worker_b");
    save_runtime_state(&root, &after_add).expect("simulate add-agent committed worker_b");

    save_runtime_state(&root, &stale_coordinator_snapshot)
        .expect("simulate live coordinator saving an older tick snapshot after add-agent");

    let final_state = load_runtime_state(&root).expect("state after stale coordinator save");
    assert!(
        final_state.pointer("/agents/worker_b").is_some(),
        "live coordinator stale snapshot save must not clobber an add-agent committed top-level worker; final_state={final_state}"
    );
    assert!(
        final_state.pointer("/teams/lifeteam/agents/worker_b").is_some(),
        "live coordinator stale snapshot save must not clobber add-agent's team-scoped roster entry; save_runtime_state must merge lock-held disk state with stale snapshots. final_state={final_state}"
    );
}

fn assert_ready(label: &str, report: &QuickStartReport, expected_session: &str) {
    match report {
        QuickStartReport::Ready { session_name, .. } => assert_eq!(
            session_name.as_str(),
            expected_session,
            "{label}: session_name must derive from explicit requested identity"
        ),
        other => panic!("{label}: expected Ready, got {other:?}"),
    }
}

fn source_section<'a>(source: &'a str, start: &str, end: &str) -> &'a str {
    let idx = source.find(start).expect("source start marker");
    let rest = &source[idx..];
    let end_idx = rest.find(end).unwrap_or(rest.len());
    &rest[..end_idx]
}

fn insert_agent_in_state(state: &mut Value, agent_id: &str) {
    let agent = agent_state("lifeteam", agent_id);
    state
        .get_mut("agents")
        .and_then(Value::as_object_mut)
        .expect("top-level agents")
        .insert(agent_id.to_string(), agent.clone());
    state
        .pointer_mut("/teams/lifeteam/agents")
        .and_then(Value::as_object_mut)
        .expect("team-scoped agents")
        .insert(agent_id.to_string(), agent);
}

fn write_team_dir(root: &Path, dir_name: &str, agent: &str) -> PathBuf {
    let team = root.join(dir_name);
    std::fs::create_dir_all(team.join("agents")).unwrap();
    std::fs::write(
        team.join("TEAM.md"),
        format!(
            "---\nname: {dir_name}\nobjective: Identity scope contract.\nprovider: codex\n---\n\nTeam.\n"
        ),
    )
    .unwrap();
    std::fs::write(team.join("agents").join(format!("{agent}.md")), role_doc(agent)).unwrap();
    team
}

fn role_doc(agent: &str) -> String {
    format!(
        "---\nname: {agent}\nrole: Worker\nprovider: codex\nmodel: gpt-5.5\nauth_mode: subscription\ntools:\n  - mcp_team\n---\n\nWorker.\n"
    )
}

fn seed_lifeteam_state(root: &Path, team_dir: &Path) {
    let state = json!({
        "active_team_key": "lifeteam",
        "session_name": "team-lifeteam",
        "team_dir": team_dir.to_string_lossy().to_string(),
        "spec_path": team_dir.join("team.spec.yaml").to_string_lossy().to_string(),
        "owner_epoch": 1,
        "agents": {
            "worker_a": agent_state("lifeteam", "worker_a")
        },
        "tasks": [],
        "teams": {
            "lifeteam": {
                "status": "alive",
                "session_name": "team-lifeteam",
                "team_dir": team_dir.to_string_lossy().to_string(),
                "spec_path": team_dir.join("team.spec.yaml").to_string_lossy().to_string(),
                "owner_epoch": 1,
                "agents": {
                    "worker_a": agent_state("lifeteam", "worker_a")
                },
                "tasks": []
            }
        }
    });
    save_runtime_state(root, &state).unwrap();
    let spec = team_agent::compiler::compile_team(team_dir).unwrap();
    std::fs::write(team_dir.join("team.spec.yaml"), team_agent::model::yaml::dumps(&spec)).unwrap();
}

fn agent_state(team: &str, agent: &str) -> Value {
    json!({
        "agent_id": agent,
        "owner_team_id": team,
        "status": "running",
        "provider": "codex",
        "role": "Worker",
        "window": agent
    })
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
        "ta-rs-team-in-team-identity-scope-{tag}-{}-{}",
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
    fn unset() -> Self {
        let keys = [
            "TMUX",
            "TMUX_PANE",
            "TEAM_AGENT_ID",
            "TEAM_AGENT_TEAM_ID",
            "TEAM_AGENT_OWNER_TEAM_ID",
            "TEAM_AGENT_LEADER_PANE_ID",
            "TEAM_AGENT_LEADER_SESSION_UUID",
            "TEAM_AGENT_LEADER_SESSION_UUID_OVERRIDE",
            "TEAM_AGENT_LEADER_PROVIDER",
        ];
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

    fn set(values: [(&'static str, &'static str); 2]) -> Self {
        let previous = values
            .iter()
            .map(|(key, _)| (*key, std::env::var(key).ok()))
            .collect::<Vec<_>>();
        for (key, value) in values {
            unsafe {
                std::env::set_var(key, value);
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
struct RecordingTransport {
    sessions: Mutex<HashSet<String>>,
    spawned: Mutex<Vec<SpawnedPane>>,
    session_present: Mutex<bool>,
}

#[derive(Debug, Clone)]
struct SpawnedPane {
    pane_id: PaneId,
    session: SessionName,
    window: WindowName,
}

impl RecordingTransport {
    fn with_session_present(self, present: bool) -> Self {
        *self.session_present.lock().unwrap() = present;
        self
    }

    fn spawn_result(&self, session: &SessionName, window: &WindowName) -> SpawnResult {
        self.sessions.lock().unwrap().insert(session.as_str().to_string());
        let mut spawned = self.spawned.lock().unwrap();
        let pane_id = PaneId::new(format!("%{}", spawned.len() + 1));
        spawned.push(SpawnedPane {
            pane_id: pane_id.clone(),
            session: session.clone(),
            window: window.clone(),
        });
        SpawnResult {
            pane_id,
            session: session.clone(),
            window: window.clone(),
            child_pid: None,
        }
    }
}

impl Transport for RecordingTransport {
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
        Ok(self.spawn_result(session, window))
    }

    fn spawn_into(
        &self,
        session: &SessionName,
        window: &WindowName,
        _argv: &[String],
        _cwd: &Path,
        _env: &BTreeMap<String, String>,
    ) -> Result<SpawnResult, TransportError> {
        Ok(self.spawn_result(session, window))
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
        Ok(self
            .spawned
            .lock()
            .unwrap()
            .iter()
            .map(|pane| PaneInfo {
                pane_id: pane.pane_id.clone(),
                session: pane.session.clone(),
                window_index: None,
                window_name: Some(pane.window.clone()),
                pane_index: None,
                tty: None,
                current_command: Some("codex".to_string()),
                current_path: None,
                active: false,
                pane_pid: None,
                leader_env: BTreeMap::new(),
            })
            .collect())
    }

    fn has_session(&self, session: &SessionName) -> Result<bool, TransportError> {
        Ok(*self.session_present.lock().unwrap()
            || self.sessions.lock().unwrap().contains(session.as_str()))
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
