//! Real-machine residual defects G1-G4.
//!
//! Anchors:
//! `.team/artifacts/macmini-e2e/6eeb968-clusterfix-confirm-20260605T051800Z/six-fail-triage.md`.
//! These are MUST-15 canonical command behaviors; the framework changes rather than weakening the
//! command files.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[path = "support/hermetic.rs"]
mod hermetic_guard;
#[allow(dead_code)]
fn _hermetic_boundary_marker(_: &hermetic_guard::HermeticTestEnv) {}

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use serde_json::{json, Value};
use team_agent::lifecycle::{
    add_agent_with_transport, remove_agent_with_transport, restart_with_transport,
};
use team_agent::messaging::{send_message, MessageTarget, SendOptions};
use team_agent::model::ids::AgentId;
use team_agent::transport::{
    AttachOutcome, BackendKind, CaptureRange, CapturedText, InjectPayload, InjectReport,
    InjectStage, InjectVerification, Key, PaneField, PaneId, PaneInfo, SessionName,
    SetEnvOutcome, SpawnResult, SubmitVerification, Target, Transport, TransportError,
    TurnVerification, WindowName,
};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_team-agent")
}

fn tmp_dir(tag: &str) -> PathBuf {
    static N: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "ta-rs-rm-residual-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::canonicalize(dir).unwrap()
}

fn run(args: &[&str], cwd: &Path) -> Output {
    Command::new(bin())
        .args(args)
        .current_dir(cwd)
        .output()
        .unwrap()
}

fn stdout_json(out: &Output) -> Value {
    serde_json::from_slice(&out.stdout).unwrap_or_else(|_| {
        panic!(
            "stdout must be JSON; code={:?} stdout={} stderr={}",
            out.status.code(),
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )
    })
}

#[test]
#[ignore = "real-machine: command-file gate uses real team-agent binary/lifecycle"]
fn g1_shutdown_after_stopped_runtime_is_idempotent_when_tmux_server_is_absent() {
    let fixture = seed_active_team_fixture("g1-shutdown");
    clear_coordinator_markers(&fixture.root);

    let out = run(
        &[
            "shutdown",
            "--workspace",
            fixture.root.to_str().unwrap(),
            "--keep-logs",
            "--json",
        ],
        &fixture.root,
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let combined = format!("{stdout}\n{stderr}");

    assert_eq!(
        out.status.code(),
        Some(0),
        "CR-005/G1: shutdown after a prior stop must be idempotent/no-op even when the socketed tmux server is already absent; got code={:?} stdout={stdout:?} stderr={stderr:?}",
        out.status.code()
    );
    assert!(
        !combined.contains("no server running"),
        "CR-005/G1: absent tmux server must not surface as kill-session failure; got {combined:?}"
    );
    let value = stdout_json(&out);
    assert!(
        value["ok"].as_bool().unwrap_or(true),
        "CR-005/G1: idempotent shutdown should return a shaped success/no-op envelope; got {value}"
    );
}

#[test]
#[ignore = "real-machine: command-file gate uses real team-agent binary/lifecycle"]
fn g2_restart_replaces_or_reuses_existing_team_current_session_in_all_real_machine_contexts() {
    let mut failures = Vec::new();
    for context in [
        RestartContext::Bare,
        RestartContext::AfterAcknowledgeIdle,
        RestartContext::CoordinatorPendingRecovery,
    ] {
        let fixture = seed_restart_fixture(context);
        let transport = DuplicateSessionTrapTransport::new_existing("team-current");
        let result = restart_with_transport(&fixture.root, false, None, &transport);

        if result.is_err() {
            failures.push(format!(
                "{context:?}: restart must not attempt tmux new-session while team-current already exists; result={result:?}; calls={:?}",
                transport.calls()
            ));
            continue;
        }

        let calls = transport.calls();
        let kill_before_first = calls
            .iter()
            .position(|call| *call == "kill_session")
            .zip(calls.iter().position(|call| *call == "spawn_first"))
            .is_some_and(|(kill, spawn)| kill < spawn);
        let reused_existing = calls
            .iter()
            .position(|call| *call == "has_session")
            .zip(calls.iter().position(|call| *call == "spawn_into"))
            .is_some_and(|(has, spawn)| has < spawn);
        if !(kill_before_first || reused_existing) {
            failures.push(format!(
                "{context:?}: restart must either kill the old session before rebuilding or detect/reuse it before spawning; calls={calls:?}"
            ));
        }
        if transport.active_session_count("team-current") != 1 {
            failures.push(format!(
                "{context:?}: restart should leave exactly one active team-current session; sessions={:?}",
                transport.sessions()
            ));
        }
    }

    assert!(
        failures.is_empty(),
        "CR-007/021/052 G2: restart must not fail with `duplicate session: team-current` in bare, acknowledge-idle, or pending-recovery contexts:\n{}",
        failures.join("\n")
    );
}

#[test]
#[ignore = "real-machine: command-file gate uses real team-agent binary/lifecycle"]
fn g3_add_send_remove_readd_send_keeps_runtime_and_spec_rosters_coherent() {
    let fixture = seed_active_team_fixture("g3-add-remove");
    let role_file = fixture.root.join(".team").join("roles").join("worker_b.md");
    let transport = DuplicateSessionTrapTransport::new_existing("team-current");
    let worker_b = AgentId::new("worker_b");

    let add = add_agent_with_transport(
        &fixture.teamdir,
        &worker_b,
        &role_file,
        false,
        None,
        &transport,
    );
    assert!(
        add.is_ok(),
        "CR-030/G3 fixture sanity: add-agent worker_b should succeed before send/remove; got {add:?}"
    );

    let first_send = send_message(
        &fixture.root,
        &MessageTarget::Single("worker_b".to_string()),
        "first ping",
        &SendOptions {
            wait_visible: false,
            block_until_delivered: false,
            ..SendOptions::default()
        },
    )
    .unwrap();
    assert!(
        first_send.ok,
        "CR-030/G3: send after add-agent must route to worker_b, proving runtime roster recognizes the new worker; got {first_send:?}"
    );

    let remove = remove_agent_with_transport(
        &fixture.root,
        &worker_b,
        true,
        true,
        None,
        &transport,
    );
    assert!(
        remove.is_ok(),
        "CR-030/G3: remove-agent worker_b after successful add+send must succeed and must not report `unknown worker agent id: worker_b`; got {remove:?}"
    );
    let state_after_remove = team_agent::state::persist::load_runtime_state(&fixture.root).unwrap();
    assert!(
        !state_after_remove
            .get("agents")
            .and_then(Value::as_object)
            .is_some_and(|agents| agents.contains_key("worker_b")),
        "CR-030/G3: state.json roster must cleanly remove worker_b; state={state_after_remove}"
    );

    let readd = add_agent_with_transport(
        &fixture.teamdir,
        &worker_b,
        &role_file,
        false,
        None,
        &transport,
    );
    assert!(
        readd.is_ok(),
        "CR-030/G3: re-add worker_b after removal should be accepted; got {readd:?}"
    );
    let second_send = send_message(
        &fixture.root,
        &MessageTarget::Single("worker_b".to_string()),
        "second ping",
        &SendOptions {
            wait_visible: false,
            block_until_delivered: false,
            ..SendOptions::default()
        },
    )
    .unwrap();
    assert!(
        second_send.ok,
        "CR-030/G3: send after re-add must route to worker_b again; got {second_send:?}"
    );
}

#[test]
#[ignore = "real-machine: command-file gate uses real team-agent binary/lifecycle"]
fn g4_help_only_short_circuits_before_validation_for_residual_commands() {
    let cwd = tmp_dir("g4-help");
    let mut failures = Vec::new();

    for command in [
        "attach-leader",
        "add-agent",
        "stop-agent",
        "reset-agent",
        "claim-leader",
    ] {
        let out = run(&[command, "--help"], &cwd);
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        let combined = format!("{stdout}\n{stderr}");
        let validation_leaked = [
            "invalid choice:",
            "missing agent",
            "missing --role-file",
            "caller_not_leader_shaped",
            "team_owner_mismatch",
            "missing TEAM.md",
            "spec compile failed",
        ]
        .iter()
        .any(|needle| combined.contains(needle));

        if out.status.code() != Some(0)
            || validation_leaked
            || !(combined.contains("usage") && combined.contains(command))
        {
            failures.push(format!(
                "{command}: expected zero-token help-only exit 0 before validation; code={:?} stdout={stdout:?} stderr={stderr:?}",
                out.status.code()
            ));
        }
    }

    if cwd.join(".team").exists() {
        failures.push(format!(
            "help-only commands created runtime state/logs under cwd: {}",
            cwd.join(".team").display()
        ));
    }
    assert!(
        failures.is_empty(),
        "CR-063/G4: every subcommand --help must short-circuit before argument, leader, or runtime validation:\n{}",
        failures.join("\n")
    );
}

#[derive(Debug, Clone, Copy)]
enum RestartContext {
    Bare,
    AfterAcknowledgeIdle,
    CoordinatorPendingRecovery,
}

#[derive(Debug)]
struct Fixture {
    root: PathBuf,
    teamdir: PathBuf,
}

fn seed_restart_fixture(context: RestartContext) -> Fixture {
    let tag = match context {
        RestartContext::Bare => "g2-bare",
        RestartContext::AfterAcknowledgeIdle => "g2-idle",
        RestartContext::CoordinatorPendingRecovery => "g2-pending",
    };
    let fixture = seed_active_team_fixture(tag);
    let mut state = team_agent::state::persist::load_runtime_state(&fixture.root).unwrap();
    if let Some(obj) = state.as_object_mut() {
        match context {
            RestartContext::Bare => {}
            RestartContext::AfterAcknowledgeIdle => {
                obj.insert(
                    "idle_acknowledgements".to_string(),
                    json!({"worker_a": {"acknowledged": true, "acknowledged_at": "2026-06-05T05:18:00+00:00"}}),
                );
            }
            RestartContext::CoordinatorPendingRecovery => {
                obj.insert(
                    "coordinator".to_string(),
                    json!({"status": "recovering", "pending_messages": 1}),
                );
                obj.insert(
                    "tasks".to_string(),
                    json!([{
                        "id": "task_pending",
                        "title": "Pending recovery task",
                        "assignee": "worker_a",
                        "status": "running"
                    }]),
                );
            }
        }
    }
    team_agent::state::persist::save_runtime_state(&fixture.root, &state).unwrap();
    fixture
}

fn seed_active_team_fixture(tag: &str) -> Fixture {
    let root = tmp_dir(tag);
    let teamdir = root.join("teamdir");
    std::fs::create_dir_all(teamdir.join("agents")).unwrap();
    std::fs::create_dir_all(root.join(".team").join("roles")).unwrap();
    std::fs::write(
        teamdir.join("TEAM.md"),
        "---\nname: current\nobjective: Real-machine residual fixture.\nprovider: fake\n---\n\nTeam.\n",
    )
    .unwrap();
    std::fs::write(
        teamdir.join("agents").join("worker_a.md"),
        role_doc("worker_a", "Worker A"),
    )
    .unwrap();
    std::fs::write(
        root.join(".team").join("roles").join("worker_b.md"),
        role_doc("worker_b", "Worker B"),
    )
    .unwrap();

    let spec = team_agent::compiler::compile_team(&teamdir).unwrap();
    let spec_path = teamdir.join("team.spec.yaml");
    std::fs::write(&spec_path, team_agent::model::yaml::dumps(&spec)).unwrap();

    let state = json!({
        "active_team_key": "current",
        "spec_path": spec_path.to_string_lossy().to_string(),
        "team_dir": teamdir.to_string_lossy().to_string(),
        "session_name": "team-current",
        "leader": {"id": "leader"},
        "agents": {
            "worker_a": running_agent("worker_a", "%1", "sess-worker-a")
        },
        "tasks": [{
            "id": "task_initial",
            "title": "Initial task",
            "assignee": "worker_a",
            "status": "running"
        }],
        "teams": {
            "current": {
                "status": "alive",
                "spec_path": spec_path.to_string_lossy().to_string(),
                "team_dir": teamdir.to_string_lossy().to_string(),
                "session_name": "team-current",
                "leader": {"id": "leader"},
                "agents": {
                    "worker_a": running_agent("worker_a", "%1", "sess-worker-a")
                },
                "tasks": [{
                    "id": "task_initial",
                    "title": "Initial task",
                    "assignee": "worker_a",
                    "status": "running"
                }]
            }
        }
    });
    team_agent::state::persist::save_runtime_state(&root, &state).unwrap();
    seed_healthy_coordinator(&root);
    Fixture { root, teamdir }
}

fn role_doc(name: &str, role: &str) -> String {
    format!(
        "---\nname: {name}\nrole: {role}\nprovider: fake\ntools:\n  - mcp_team\n---\n\n{role}.\n"
    )
}

fn running_agent(id: &str, pane: &str, session_id: &str) -> Value {
    json!({
        "agent_id": id,
        "provider": "fake",
        "role": id,
        "status": "running",
        "session_id": session_id,
        "first_send_at": "2026-06-05T05:18:00+00:00",
        "pane_id": pane,
        "window": id
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

fn clear_coordinator_markers(workspace: &Path) {
    let workspace = team_agent::coordinator::WorkspacePath::new(workspace.to_path_buf());
    let _ = std::fs::remove_file(team_agent::coordinator::coordinator_pid_path(&workspace));
    let _ = std::fs::remove_file(team_agent::coordinator::coordinator_meta_path(&workspace));
}

#[derive(Debug)]
struct DuplicateSessionTrapTransport {
    sessions: Mutex<HashSet<String>>,
    windows: Mutex<HashSet<String>>,
    calls: Mutex<Vec<&'static str>>,
}

impl DuplicateSessionTrapTransport {
    fn new_existing(session: &str) -> Self {
        let mut sessions = HashSet::new();
        sessions.insert(session.to_string());
        let mut windows = HashSet::new();
        windows.insert("worker_a".to_string());
        Self {
            sessions: Mutex::new(sessions),
            windows: Mutex::new(windows),
            calls: Mutex::new(Vec::new()),
        }
    }

    fn calls(&self) -> Vec<&'static str> {
        self.calls.lock().unwrap().clone()
    }

    fn sessions(&self) -> Vec<String> {
        self.sessions.lock().unwrap().iter().cloned().collect()
    }

    fn active_session_count(&self, session: &str) -> usize {
        usize::from(self.sessions.lock().unwrap().contains(session))
    }

    fn record(&self, call: &'static str) {
        self.calls.lock().unwrap().push(call);
    }

    fn spawn_result(
        &self,
        kind: &'static str,
        session: &SessionName,
        window: &WindowName,
    ) -> SpawnResult {
        self.record(kind);
        self.sessions
            .lock()
            .unwrap()
            .insert(session.as_str().to_string());
        self.windows
            .lock()
            .unwrap()
            .insert(window.as_str().to_string());
        SpawnResult {
            pane_id: PaneId::new(format!("%{kind}-{}", window.as_str())),
            session: session.clone(),
            window: window.clone(),
            child_pid: None,
        }
    }
}

impl Transport for DuplicateSessionTrapTransport {
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
        if self.sessions.lock().unwrap().contains(session.as_str()) {
            self.record("spawn_first_duplicate");
            return Err(TransportError::Subprocess {
                argv: vec![
                    "tmux".to_string(),
                    "new-session".to_string(),
                    "-s".to_string(),
                    session.as_str().to_string(),
                ],
                code: Some(1),
                stderr: format!("duplicate session: {}", session.as_str()),
            });
        }
        Ok(self.spawn_result("spawn_first", session, window))
    }

    fn spawn_into(
        &self,
        session: &SessionName,
        window: &WindowName,
        _argv: &[String],
        _cwd: &Path,
        _env: &BTreeMap<String, String>,
    ) -> Result<SpawnResult, TransportError> {
        Ok(self.spawn_result("spawn_into", session, window))
    }

    fn inject(
        &self,
        _target: &Target,
        _payload: &InjectPayload,
        _submit: Key,
        _bracketed: bool,
    ) -> Result<InjectReport, TransportError> {
        self.record("inject");
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
        self.record("send_keys");
        Ok(())
    }

    fn capture(
        &self,
        _target: &Target,
        range: CaptureRange,
    ) -> Result<CapturedText, TransportError> {
        self.record("capture");
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
        self.record("query");
        Ok(None)
    }

    fn liveness(
        &self,
        _pane: &PaneId,
    ) -> Result<team_agent::transport::PaneLiveness, TransportError> {
        self.record("liveness");
        Ok(team_agent::transport::PaneLiveness::Live)
    }

    fn list_targets(&self) -> Result<Vec<PaneInfo>, TransportError> {
        self.record("list_targets");
        Ok(Vec::new())
    }

    fn has_session(&self, session: &SessionName) -> Result<bool, TransportError> {
        self.record("has_session");
        Ok(self.sessions.lock().unwrap().contains(session.as_str()))
    }

    fn list_windows(&self, _session: &SessionName) -> Result<Vec<WindowName>, TransportError> {
        self.record("list_windows");
        Ok(self
            .windows
            .lock()
            .unwrap()
            .iter()
            .cloned()
            .map(WindowName::new)
            .collect())
    }

    fn set_session_env(
        &self,
        _session: &SessionName,
        _key: &str,
        _value: &str,
    ) -> Result<SetEnvOutcome, TransportError> {
        self.record("set_session_env");
        Ok(SetEnvOutcome::Applied)
    }

    fn kill_session(&self, session: &SessionName) -> Result<(), TransportError> {
        self.record("kill_session");
        self.sessions.lock().unwrap().remove(session.as_str());
        Ok(())
    }

    fn kill_window(&self, target: &Target) -> Result<(), TransportError> {
        self.record("kill_window");
        if let Target::SessionWindow { window, .. } = target {
            self.windows.lock().unwrap().remove(window.as_str());
        }
        Ok(())
    }

    fn attach_session(&self, _session: &SessionName) -> Result<AttachOutcome, TransportError> {
        self.record("attach_session");
        Ok(AttachOutcome::Attached)
    }
}
