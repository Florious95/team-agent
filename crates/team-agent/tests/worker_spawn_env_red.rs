//! Worker provider spawn environment contracts.
//!
//! #229 B-layer root cause: spawned Codex workers must inherit the parent `team-agent`
//! process environment (proxy/CA/PATH/etc.) just like a user typing `codex` in the same terminal,
//! then overlay the Team Agent identity env. The contract is path-generic and covers the three
//! worker spawn surfaces that can launch Codex: quick-start/launch, restart, and add-agent.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[path = "support/hermetic.rs"]
mod hermetic_guard;
#[allow(dead_code)]
fn _hermetic_boundary_marker(_: &hermetic_guard::HermeticTestEnv) {}

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use serde_json::json;
use serial_test::serial;
use team_agent::lifecycle::{
    add_agent_with_transport, fork_agent_with_transport, launch_with_transport,
    restart_with_transport,
};
use team_agent::model::ids::AgentId;
use team_agent::transport::{
    AttachOutcome, BackendKind, CaptureRange, CapturedText, InjectPayload, InjectReport,
    InjectStage, InjectVerification, Key, PaneField, PaneId, PaneInfo, SessionName, SetEnvOutcome,
    SpawnResult, SubmitVerification, Target, Transport, TransportError, TurnVerification,
    WindowName,
};

const RUNTIME_TEAM_KEY: &str = "teamdir";

#[test]
#[serial(env)]
fn worker_spawn_inherits_parent_process_env_for_proxy_and_ca() {
    let _guard = EnvGuard::set([
        ("TA_SPAWN_ENV_CANARY", "FOO"),
        ("HTTP_PROXY", "http://canary-proxy:1"),
        ("NODE_EXTRA_CA_CERTS", "/canary/ca.pem"),
        ("TMUX", "/tmp/tmux-canary/default,1"),
        ("TMUX_PANE", "%canary"),
    ]);

    let launch_team = compiled_team_dir("launch", &[("worker_a", "Launch Worker")]);
    let launch_transport = RecordingTransport::new().with_session_present(false);
    launch_with_transport(
        &launch_team.join("team.spec.yaml"),
        false,
        true,
        true,
        &launch_transport,
    )
    .expect("launch fixture should spawn worker");
    let launch_command_line = launch_transport.single_spawn_command_line();

    let restart_team = compiled_team_dir("restart", &[("worker_a", "Restart Worker")]);
    seed_restart_state(&restart_team, "worker_a");
    let restart_transport = RecordingTransport::new().with_session_present(false);
    restart_with_transport(&restart_team, true, None, &restart_transport)
        .expect("restart fixture should spawn worker");
    let restart_command_line = restart_transport.single_spawn_command_line();

    let add_team = compiled_team_dir("add-agent", &[("worker_a", "Existing Worker")]);
    seed_running_add_agent_state(&add_team, "worker_a");
    let role_file = add_team.parent().unwrap().join("worker_b.md");
    std::fs::write(&role_file, role_doc("worker_b", "Added Worker")).unwrap();
    let add_transport = RecordingTransport::new().with_session_present(true);
    add_agent_with_transport(
        &add_team,
        &AgentId::new("worker_b"),
        &role_file,
        false,
        None,
        &add_transport,
    )
    .expect("add-agent fixture should spawn worker");
    let add_command_line = add_transport.single_spawn_command_line();

    let failures = [
        ("quick-start launch", launch_command_line, "worker_a"),
        ("restart", restart_command_line, "worker_a"),
        ("add-agent", add_command_line, "worker_b"),
    ]
    .into_iter()
    .flat_map(|(surface, command_line, agent_id)| {
        spawn_env_contract_failures(surface, &command_line, agent_id)
    })
    .collect::<Vec<_>>();

    assert!(
        failures.is_empty(),
        "worker spawn env contract failed:\n{}",
        failures.join("\n")
    );
}

#[test]
#[serial(env)]
fn worker_spawn_scrubs_cross_provider_process_identity_but_keeps_config_env() {
    let _guard = EnvGuard::set([
        ("CLAUDECODE", "1"),
        ("CLAUDE_CODE_SESSION_ID", "leader-session"),
        ("CLAUDE_CODE_SENTINEL", "foreign"),
        ("CLAUDE_EFFORT", "high"),
        ("CODEX_THREAD_ID", "thread-from-parent"),
        ("OPENAI_API_KEY", "sk-test-config"),
        ("CODEX_HOME", "/tmp/codex-home-config"),
        ("TA_SPAWN_ENV_CANARY", "KEEP"),
        ("HTTP_PROXY", "http://canary-proxy:1"),
        ("NODE_EXTRA_CA_CERTS", "/canary/ca.pem"),
    ]);

    let launch_team = compiled_team_dir("cross-launch", &[("worker_a", "Launch Worker")]);
    let launch_transport = RecordingTransport::new().with_session_present(false);
    launch_with_transport(
        &launch_team.join("team.spec.yaml"),
        false,
        true,
        true,
        &launch_transport,
    )
    .expect("launch fixture should spawn worker");

    let restart_team = compiled_team_dir("cross-restart", &[("worker_a", "Restart Worker")]);
    seed_restart_state(&restart_team, "worker_a");
    let restart_transport = RecordingTransport::new().with_session_present(false);
    restart_with_transport(&restart_team, true, None, &restart_transport)
        .expect("restart fixture should spawn worker");

    let add_team = compiled_team_dir("cross-add", &[("worker_a", "Existing Worker")]);
    seed_running_add_agent_state(&add_team, "worker_a");
    let role_file = add_team.parent().unwrap().join("worker_b.md");
    std::fs::write(&role_file, role_doc("worker_b", "Added Worker")).unwrap();
    let add_transport = RecordingTransport::new().with_session_present(true);
    add_agent_with_transport(
        &add_team,
        &AgentId::new("worker_b"),
        &role_file,
        false,
        None,
        &add_transport,
    )
    .expect("add-agent fixture should spawn worker");

    let fork_team = compiled_team_dir("cross-fork", &[("worker_a", "Fork Source")]);
    seed_forkable_source_state(&fork_team, "worker_a");
    let fork_transport = RecordingTransport::new().with_session_present(true);
    fork_agent_with_transport(
        &fork_team,
        &AgentId::new("worker_a"),
        &AgentId::new("worker_fork"),
        None,
        false,
        None,
        &fork_transport,
    )
    .expect("fork-agent fixture should spawn worker");

    let failures = [
        ("quick-start launch", launch_transport.single_spawn()),
        ("restart", restart_transport.single_spawn()),
        ("add-agent", add_transport.single_spawn()),
        ("fork-agent", fork_transport.single_spawn()),
    ]
    .into_iter()
    .flat_map(|(surface, spawn)| cross_provider_identity_failures(surface, &spawn))
    .collect::<Vec<_>>();

    assert!(
        failures.is_empty(),
        "cross-provider worker env isolation contract failed:\n{}",
        failures.join("\n")
    );
}

#[test]
#[serial(env)]
fn worker_spawn_stays_out_of_coordinator_tick_and_daemon_preserves_parent_env() {
    let tick = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/coordinator/tick.rs"
    ))
    .unwrap();
    assert!(
        !tick.contains("transport.spawn_first") && !tick.contains("transport.spawn_into"),
        "coordinator tick must never spawn worker panes; worker spawn belongs to user-invoked lifecycle paths so it inherits the user's shell env"
    );

    let health = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/coordinator/health.rs"
    ))
    .unwrap();
    let start = health
        .find("pub fn start_coordinator")
        .map(|idx| &health[idx..])
        .unwrap_or(&health);
    let start = start
        .find("pub fn stop_coordinator")
        .map(|idx| &start[..idx])
        .unwrap_or(start);
    assert!(
        !start.contains(".env_clear(") && !start.contains(".env("),
        "start_coordinator must inherit the parent environment and must not clear/override daemon env; source={start}"
    );
}

fn spawn_env_contract_failures(surface: &str, command_line: &str, agent_id: &str) -> Vec<String> {
    let mut failures = Vec::new();
    for expected in [
        "TA_SPAWN_ENV_CANARY=FOO",
        "HTTP_PROXY=http://canary-proxy:1",
        "NODE_EXTRA_CA_CERTS=/canary/ca.pem",
        "TEAM_AGENT_WORKSPACE=",
        "TEAM_AGENT_OWNER_TEAM_ID=teamdir",
    ] {
        if !command_line.contains(expected) {
            failures.push(format!(
                "{surface}: missing `{expected}`; worker spawn must inherit parent env and overlay Team Agent identity; command_line={command_line:?}"
            ));
        }
    }
    if !command_line.contains(&format!("TEAM_AGENT_AGENT_ID={agent_id}")) {
        failures.push(format!(
            "{surface}: missing `TEAM_AGENT_AGENT_ID={agent_id}` overlay; command_line={command_line:?}"
        ));
    }
    if command_line.contains("TMUX=") || command_line.contains("TMUX_PANE=") {
        failures.push(format!(
            "{surface}: worker spawn must filter tmux control env from inherited parent env; command_line={command_line:?}"
        ));
    }
    if !command_line.split_whitespace().any(|part| part == "codex") {
        failures.push(format!(
            "{surface}: provider executable must remain the command name `codex`; command_line={command_line:?}"
        ));
    }
    if command_line
        .split_whitespace()
        .any(|part| part.ends_with("/codex"))
    {
        failures.push(format!(
            "{surface}: provider executable must not be hard-coded as an absolute codex path; command_line={command_line:?}"
        ));
    }
    failures
}

fn cross_provider_identity_failures(surface: &str, spawn: &RecordedSpawn) -> Vec<String> {
    let mut failures = Vec::new();
    let identity_keys = [
        "CLAUDECODE",
        "CLAUDE_CODE_SESSION_ID",
        "CLAUDE_CODE_SENTINEL",
        "CLAUDE_EFFORT",
        "CODEX_THREAD_ID",
    ];
    let present_identity = identity_keys
        .iter()
        .copied()
        .filter(|key| spawn.env.contains_key(*key))
        .collect::<Vec<_>>();
    if !present_identity.is_empty() {
        failures.push(format!(
            "{surface}: worker env map must scrub process/session identity keys; present={present_identity:?}"
        ));
    }
    let missing_unsets = identity_keys
        .iter()
        .copied()
        .filter(|key| !spawn.env_unset.iter().any(|candidate| candidate == key))
        .collect::<Vec<_>>();
    if !missing_unsets.is_empty() {
        failures.push(format!(
            "{surface}: worker env_unset must clear stale tmux/server identity keys; missing={missing_unsets:?}; env_unset={:?}",
            spawn.env_unset
        ));
    }
    for (key, value) in [
        ("OPENAI_API_KEY", "sk-test-config"),
        ("CODEX_HOME", "/tmp/codex-home-config"),
        ("TA_SPAWN_ENV_CANARY", "KEEP"),
        ("HTTP_PROXY", "http://canary-proxy:1"),
        ("NODE_EXTRA_CA_CERTS", "/canary/ca.pem"),
    ] {
        if spawn.env.get(key).map(String::as_str) != Some(value) {
            failures.push(format!(
                "{surface}: worker env must preserve config/generic key `{key}` with the expected test value; present={}",
                spawn.env.contains_key(key)
            ));
        }
        if spawn.env_unset.iter().any(|candidate| candidate == key) {
            failures.push(format!(
                "{surface}: worker env_unset must not remove config/generic key `{key}`; env_unset={:?}",
                spawn.env_unset
            ));
        }
    }
    failures
}

fn compiled_team_dir(label: &str, agents: &[(&str, &str)]) -> PathBuf {
    let team = tmp_dir(label).join("teamdir");
    std::fs::create_dir_all(team.join("agents")).unwrap();
    std::fs::write(
        team.join("TEAM.md"),
        "---\nname: envteam\nobjective: Worker spawn env contract.\nprovider: codex\n---\n\nTeam.\n",
    )
    .unwrap();
    for (name, role) in agents {
        std::fs::write(
            team.join("agents").join(format!("{name}.md")),
            role_doc(name, role),
        )
        .unwrap();
    }
    let spec = team_agent::compiler::compile_team(&team).unwrap();
    std::fs::write(
        team.join("team.spec.yaml"),
        team_agent::model::yaml::dumps(&spec),
    )
    .unwrap();
    team
}

fn role_doc(name: &str, role: &str) -> String {
    format!(
        "---\nname: {name}\nrole: {role}\nprovider: codex\nmodel: codex-test-model\nauth_mode: subscription\ntools:\n  - mcp_team\n---\n\n{role}.\n"
    )
}

fn seed_restart_state(team: &Path, agent_id: &str) {
    team_agent::state::persist::save_runtime_state(
        team,
        &json!({
            "active_team_key": RUNTIME_TEAM_KEY,
            "team_dir": team.to_string_lossy().to_string(),
            "spec_path": team.join("team.spec.yaml").to_string_lossy().to_string(),
            "session_name": "team-envteam",
            "agents": {
                agent_id: {
                    "status": "running",
                    "provider": "codex",
                    "role": "Restart Worker",
                    "tools": ["mcp_team"],
                    "window": agent_id,
                    "owner_team_id": RUNTIME_TEAM_KEY,
                    "session_id": "sess-worker-a",
                    "first_send_at": "2026-06-05T09:00:00+00:00"
                }
            }
        }),
    )
    .unwrap();
    seed_healthy_coordinator(team);
}

fn seed_running_add_agent_state(team: &Path, agent_id: &str) {
    let workspace = team_agent::model::paths::team_workspace(team).unwrap();
    team_agent::state::persist::save_runtime_state(
        &workspace,
        &json!({
            "active_team_key": RUNTIME_TEAM_KEY,
            "team_dir": team.to_string_lossy().to_string(),
            "spec_path": team.join("team.spec.yaml").to_string_lossy().to_string(),
            "session_name": "team-envteam",
            "agents": {
                agent_id: {
                    "status": "running",
                    "provider": "codex",
                    "role": "Existing Worker",
                    "tools": ["mcp_team"],
                    "window": agent_id,
                    "owner_team_id": RUNTIME_TEAM_KEY
                }
            }
        }),
    )
    .unwrap();
    seed_healthy_coordinator(&workspace);
}

fn seed_forkable_source_state(team: &Path, agent_id: &str) {
    let workspace = team_agent::model::paths::team_workspace(team).unwrap();
    let rollout = workspace.join(format!("{agent_id}-rollout.jsonl"));
    std::fs::write(&rollout, "{\"session_id\":\"sess-worker-a\"}\n").unwrap();
    team_agent::state::persist::save_runtime_state(
        &workspace,
        &json!({
            "active_team_key": RUNTIME_TEAM_KEY,
            "team_dir": team.to_string_lossy().to_string(),
            "spec_path": team.join("team.spec.yaml").to_string_lossy().to_string(),
            "session_name": "team-envteam",
            "agents": {
                agent_id: {
                    "status": "running",
                    "provider": "codex",
                    "auth_mode": "subscription",
                    "role": "Fork Source",
                    "window": agent_id,
                    "owner_team_id": RUNTIME_TEAM_KEY,
                    "session_id": "sess-worker-a",
                    "rollout_path": rollout.to_string_lossy().to_string(),
                    "captured_at": "2026-07-16T00:00:00+00:00",
                    "captured_via": "session.captured",
                    "tools": ["mcp_team"]
                }
            }
        }),
    )
    .unwrap();
    seed_healthy_coordinator(&workspace);
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
        "ta-rs-worker-spawn-env-{tag}-{}-{}",
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
    fn set<const N: usize>(values: [(&'static str, &'static str); N]) -> Self {
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

#[derive(Debug, Clone)]
struct RecordedSpawn {
    argv: Vec<String>,
    env: BTreeMap<String, String>,
    env_unset: Vec<String>,
    session: SessionName,
    window: WindowName,
    pane_id: PaneId,
}

impl RecordedSpawn {
    fn command_line(&self) -> String {
        let mut parts = self
            .env
            .iter()
            .map(|(key, value)| format!("{key}={value}"))
            .collect::<Vec<_>>();
        parts.extend(self.argv.iter().cloned());
        parts.join(" ")
    }
}

#[derive(Debug, Default)]
struct RecordingTransport {
    spawns: Mutex<Vec<RecordedSpawn>>,
    session_present: bool,
}

impl RecordingTransport {
    fn new() -> Self {
        Self::default()
    }

    fn with_session_present(mut self, present: bool) -> Self {
        self.session_present = present;
        self
    }

    fn single_spawn_command_line(&self) -> String {
        self.single_spawn().command_line()
    }

    fn single_spawn(&self) -> RecordedSpawn {
        let spawns = self.spawns.lock().unwrap();
        assert_eq!(
            spawns.len(),
            1,
            "fixture should record exactly one worker spawn; spawns={spawns:?}"
        );
        spawns[0].clone()
    }

    fn spawn_result(
        &self,
        session: &SessionName,
        window: &WindowName,
        argv: &[String],
        env: &BTreeMap<String, String>,
        env_unset: &[String],
    ) -> SpawnResult {
        let mut spawns = self.spawns.lock().unwrap();
        let pane_id = PaneId::new(format!("%{}", spawns.len() + 1));
        spawns.push(RecordedSpawn {
            argv: argv.to_vec(),
            env: env.clone(),
            env_unset: env_unset.to_vec(),
            session: session.clone(),
            window: window.clone(),
            pane_id: pane_id.clone(),
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
        argv: &[String],
        _cwd: &Path,
        env: &BTreeMap<String, String>,
    ) -> Result<SpawnResult, TransportError> {
        Ok(self.spawn_result(session, window, argv, env, &[]))
    }

    fn spawn_into(
        &self,
        session: &SessionName,
        window: &WindowName,
        argv: &[String],
        _cwd: &Path,
        env: &BTreeMap<String, String>,
    ) -> Result<SpawnResult, TransportError> {
        Ok(self.spawn_result(session, window, argv, env, &[]))
    }

    fn spawn_first_with_env_unset(
        &self,
        session: &SessionName,
        window: &WindowName,
        argv: &[String],
        _cwd: &Path,
        env: &BTreeMap<String, String>,
        env_unset: &[String],
    ) -> Result<SpawnResult, TransportError> {
        Ok(self.spawn_result(session, window, argv, env, env_unset))
    }

    fn spawn_into_with_env_unset(
        &self,
        session: &SessionName,
        window: &WindowName,
        argv: &[String],
        _cwd: &Path,
        env: &BTreeMap<String, String>,
        env_unset: &[String],
    ) -> Result<SpawnResult, TransportError> {
        Ok(self.spawn_result(session, window, argv, env, env_unset))
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
            text: String::new(),
            range,
        })
    }

    fn query(&self, _target: &Target, field: PaneField) -> Result<Option<String>, TransportError> {
        match field {
            PaneField::PaneWidth => Ok(Some("120".to_string())),
            _ => Ok(None),
        }
    }

    fn liveness(
        &self,
        _pane: &PaneId,
    ) -> Result<team_agent::transport::PaneLiveness, TransportError> {
        Ok(team_agent::transport::PaneLiveness::Live)
    }

    fn list_targets(&self) -> Result<Vec<PaneInfo>, TransportError> {
        Ok(self
            .spawns
            .lock()
            .unwrap()
            .iter()
            .map(|spawn| PaneInfo {
                pane_id: spawn.pane_id.clone(),
                session: spawn.session.clone(),
                window_index: None,
                window_name: Some(spawn.window.clone()),
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

    fn has_session(&self, _session: &SessionName) -> Result<bool, TransportError> {
        Ok(self.session_present || !self.spawns.lock().unwrap().is_empty())
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
