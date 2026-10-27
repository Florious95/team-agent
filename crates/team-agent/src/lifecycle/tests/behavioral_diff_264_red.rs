//! #264 behavioral diff regression contracts (D4-D6, D8-D9).
//!
//! Source of truth: Python runtime 0.2.11 (`~/.team-agent/runtime/0.2.11/src/team_agent/`).
//! Every asserted value below was grounded by executing the Python truth source
//! (compile_system_prompt / providers.shell_command) —
//! see `.team/artifacts/264-behavioral-diff-report.md` for the per-item file:line evidence.
//! D1-D3 (codex argv golden) and D7 (codex `/fast` toggle) retired with the legacy
//! Codex worker runtime.
//!
//! D4: worker shell env TEAM_AGENT_ID=<worker self> (overrides parent)  — providers.py:131
//! D5: fresh launch spawn cwd == workspace (tmux cwd + state.spawn_cwd) — providers.py:145 / launch/core.py:253
//! D6: compiled system prompt first section is identity                 — prompt.py:39
//! D8: launch session_name fallback = team-{name}, not a constant       — launch/core.py:56
//! D9: profile env_unset truly unsets in the worker shell line          — providers.py:142-145 / provider_env.py:82-90

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[path = "../../../tests/support/hermetic.rs"]
mod hermetic_guard;
#[allow(dead_code)]
fn _hermetic_boundary_marker(_: &hermetic_guard::HermeticTestEnv) {}

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use serial_test::serial;
use team_agent::lifecycle::{launch_with_transport, quick_start_with_transport_in_workspace};
use team_agent::state::persist::load_runtime_state;
use team_agent::tmux_backend::{CommandOutput, CommandRunner, TmuxBackend};
use team_agent::transport::{
    AttachOutcome, BackendKind, CaptureRange, CapturedText, InjectPayload, InjectReport,
    InjectStage, InjectVerification, Key, PaneField, PaneId, PaneInfo, SessionName, SetEnvOutcome,
    SpawnResult, SubmitVerification, Target, Transport, TransportError, TurnVerification,
    WindowName,
};

const ANCESTRY_KEY: &str = "TEAM_AGENT_TEST_PROCESS_ANCESTRY_ARGV_JSON";
const NEUTRAL_ANCESTRY: &str = "[\"/bin/zsh\"]";

/// D4: providers.py:131 — the worker shell env must export TEAM_AGENT_ID=<the worker itself>,
/// overriding any TEAM_AGENT_ID inherited from the launching process (e.g. an add-agent issued
/// from another worker's MCP server, whose environ carries the CALLER's TEAM_AGENT_ID).
#[test]
#[serial(env)]
fn d4_worker_shell_env_sets_team_agent_id_to_worker_self() {
    let _hermetic = hermetic_guard::HermeticTestEnv::enter_with_leader("behavioral-d4-shell-env");
    let _guard = EnvGuard::set(&[
        (ANCESTRY_KEY, NEUTRAL_ANCESTRY),
        ("TEAM_AGENT_ID", "stale-mcp-caller"),
    ]);
    let ws = tmp_dir("d4-spawn-env");
    let team_dir = write_team(&ws, &simple_fake_team("d4team", "worker_a", "Env Worker"));
    seed_healthy_coordinator(&ws);
    let transport = RecordingTransport::new();

    quick_start_with_transport_in_workspace(&ws, &team_dir, None, true, Some("d4team"), &transport)
        .expect("quick-start should spawn the worker");
    let spawn = transport.single_spawn();
    let mut failures = Vec::new();
    match spawn.env.get("TEAM_AGENT_ID").map(String::as_str) {
        Some("worker_a") => {}
        other => failures.push(format!(
            "D4: worker spawn env must set TEAM_AGENT_ID=worker_a (Python providers.py:131), overriding the inherited caller value; got {other:?}"
        )),
    }
    match spawn.env.get("TEAM_AGENT_AGENT_ID").map(String::as_str) {
        Some("worker_a") => {}
        other => failures.push(format!(
            "D4: TEAM_AGENT_AGENT_ID=worker_a must stay alongside TEAM_AGENT_ID (#229 compat); got {other:?}"
        )),
    }
    assert!(
        failures.is_empty(),
        "D4 worker shell env contract failed:\n{}\nenv_keys={:?}",
        failures.join("\n"),
        spawn.env.keys().collect::<Vec<_>>()
    );
}

/// D5: providers.py:145 + launch/core.py:253 — fresh launch runs the worker with cwd=WORKSPACE
/// (`cd <workspace> && ... exec`), and persists state.agents[*].spawn_cwd = workspace.
/// RS fork/add (launch.rs:2304) and restart (restart/common.rs:125-133) already use workspace;
/// fresh launch passing team_dir is the odd one out.
#[test]
#[serial(env)]
fn d5_fresh_launch_spawn_cwd_and_state_use_workspace() {
    let _hermetic = hermetic_guard::HermeticTestEnv::enter_with_leader("behavioral-d5-spawn-cwd");
    let _ancestry = EnvGuard::set(&[(ANCESTRY_KEY, NEUTRAL_ANCESTRY)]);
    let ws = tmp_dir("d5-spawn-cwd");
    let team_dir = write_team(&ws, &simple_fake_team("d5team", "worker_a", "Cwd Worker"));
    seed_healthy_coordinator(&ws);
    let transport = RecordingTransport::new();

    quick_start_with_transport_in_workspace(&ws, &team_dir, None, true, Some("d5team"), &transport)
        .expect("quick-start should spawn the worker");
    let spawn = transport.single_spawn();
    let mut failures = Vec::new();
    if spawn.cwd != ws {
        failures.push(format!(
            "D5: transport spawn cwd must be the workspace {} (Python providers.py:145 `cd <workspace>`); got {}",
            ws.display(),
            spawn.cwd.display()
        ));
    }
    let state = load_runtime_state(&ws).expect("load runtime state after quick-start");
    let spawn_cwd = state
        .pointer("/agents/worker_a/spawn_cwd")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string);
    if spawn_cwd.as_deref() != Some(&*ws.to_string_lossy()) {
        failures.push(format!(
            "D5: persisted agents.worker_a.spawn_cwd must equal the workspace {:?} (Python launch/core.py:253); got {spawn_cwd:?}",
            ws.to_string_lossy()
        ));
    }
    assert!(
        failures.is_empty(),
        "D5 fresh-launch spawn cwd contract failed:\n{}",
        failures.join("\n")
    );
}

/// D6: prompt.py:39 — chunks = [identity, TEAMMATE_SYSTEM_PROMPT, ...]: the compiled system
/// prompt must START with the worker identity line (live Python worker argv confirms this),
/// with the shared runtime contract section second.
#[test]
#[serial(env)]
fn d6_compiled_system_prompt_first_section_is_identity() {
    let _hermetic = hermetic_guard::HermeticTestEnv::enter_with_leader("behavioral-d6-prompt");
    let _ancestry = EnvGuard::set(&[(ANCESTRY_KEY, NEUTRAL_ANCESTRY)]);
    let ws = tmp_dir("d6-prompt-order");
    let team_dir = write_team(
        &ws,
        &TeamFixture {
            name: "d6team",
            provider: "codex",
            agents: &[AgentDoc {
                id: "worker_a",
                role: "Prompt Order Worker",
                provider: "copilot",
                auth_mode: "subscription",
                model: None,
                profile: None,
                tools: &["mcp_team"],
                body: "ROLE BODY SENTINEL for prompt ordering.",
            }],
        },
    );
    seed_healthy_coordinator(&ws);
    let transport = RecordingTransport::new();

    quick_start_with_transport_in_workspace(&ws, &team_dir, None, true, Some("d6team"), &transport)
        .expect("quick-start should spawn the copilot worker");
    let spawn = transport.single_spawn();
    // Copilot receives the compiled system prompt verbatim through the per-worker
    // AGENTS.md that COPILOT_CUSTOM_INSTRUCTIONS_DIRS points at (never argv).
    let instructions_dir = spawn
        .env
        .get("COPILOT_CUSTOM_INSTRUCTIONS_DIRS")
        .expect("copilot spawn env must carry COPILOT_CUSTOM_INSTRUCTIONS_DIRS");
    let prompt = std::fs::read_to_string(Path::new(instructions_dir).join("AGENTS.md"))
        .expect("copilot per-worker AGENTS.md must hold the compiled system prompt");

    let identity = "You are Team Agent worker `worker_a` with role `Prompt Order Worker`. \
When asked about your role or identity, answer with this Team Agent worker identity first, \
not only the generic provider product identity.";
    assert!(
        prompt.starts_with(identity),
        "D6: compiled system prompt must START with the identity section (Python prompt.py:39 \
chunks=[identity, TEAMMATE_SYSTEM_PROMPT, ...]; live ps confirms identity-first); got head={:?}",
        prompt.chars().take(220).collect::<String>()
    );
    let contract_idx = prompt
        .find("# Team Agent Teammate Runtime Contract")
        .expect("prompt must contain the shared runtime contract section");
    assert!(
        contract_idx >= identity.len(),
        "D6: the runtime contract section must come AFTER the identity section; contract_idx={contract_idx}"
    );
}

/// D8: launch/core.py:56 — when the spec carries no runtime.session_name, the session falls
/// back to `team-{team.name}`, never a fixed constant.
#[test]
#[serial(env)]
fn d8_launch_session_name_fallback_derives_from_team_name() {
    let _ancestry = EnvGuard::set(&[(ANCESTRY_KEY, NEUTRAL_ANCESTRY)]);
    let ws = tmp_dir("d8-session-name");
    let team_dir = write_team(
        &ws,
        &simple_fake_team("d8team", "worker_a", "Session Worker"),
    );
    let spec = team_agent::compiler::compile_team(&team_dir).expect("compile fixture team");
    let spec_text = team_agent::model::yaml::dumps(&spec);
    let stripped: String = spec_text
        .lines()
        .filter(|line| !line.trim_start().starts_with("session_name:"))
        .map(|line| format!("{line}\n"))
        .collect();
    assert!(
        stripped.len() < spec_text.len(),
        "fixture must actually remove the compiled session_name line"
    );
    let spec_path = team_dir.join("team.spec.yaml");
    std::fs::write(&spec_path, stripped).unwrap();
    let transport = RecordingTransport::new();

    launch_with_transport(&spec_path, false, true, true, &transport)
        .expect("launch should spawn the worker without an explicit session_name");
    let spawn = transport.single_spawn();
    assert_eq!(
        spawn.session, "team-d8team",
        "D8: session_name fallback must derive `team-{{team.name}}` (Python launch/core.py:56), \
not a constant; argv={:?}",
        spawn.argv
    );
}

/// D9: provider_env.py:82-90 + providers.py:142-145 — profile env_unset keys (gemini_cli
/// compatible_api → GEMINI_API_KEY) must be REALLY unset in the worker
/// shell line (Python sources the env file whose first lines are `unset <KEY>`), because the
/// tmux server environment can still carry stale values that plain map-removal cannot clear.
#[test]
#[serial(env)]
fn d9_profile_env_unset_reaches_worker_shell_line() {
    let _guard = EnvGuard::set(&[
        (ANCESTRY_KEY, NEUTRAL_ANCESTRY),
        ("GEMINI_API_KEY", "stale-parent-key"),
    ]);
    let ws = tmp_dir("d9-env-unset");
    let team_dir = write_team(
        &ws,
        &TeamFixture {
            name: "d9team",
            provider: "codex",
            agents: &[AgentDoc {
                id: "worker_a",
                role: "Unset Worker",
                provider: "gemini_cli",
                auth_mode: "compatible_api",
                model: Some("gemini-2.5-pro"),
                profile: Some("relay-nokey"),
                tools: &["mcp_team"],
                body: "Unset worker.",
            }],
        },
    );
    // No API_KEY on purpose: GEMINI_API_KEY lands in env_unset (provider_env.py:51-52) and is
    // never re-exported, so the stale tmux-server/parent value leaks unless the shell really
    // unsets it. (With API_KEY present the overlay re-export would mask the missing unset.)
    let profiles = team_dir.join("profiles");
    std::fs::create_dir_all(&profiles).unwrap();
    std::fs::write(
        profiles.join("relay-nokey.env"),
        "AUTH_MODE=compatible_api\nBASE_URL=https://relay.example/v1\n",
    )
    .unwrap();
    let spec = team_agent::compiler::compile_team(&team_dir).expect("compile fixture team");
    let spec_path = team_dir.join("team.spec.yaml");
    std::fs::write(&spec_path, team_agent::model::yaml::dumps(&spec)).unwrap();

    let runner = RecordingRunner::new();
    let argv_log = runner.argv_log();
    let backend = TmuxBackend::with_runner(Box::new(runner));
    // The spawn may be marked dead by the canned runner after the shell line is built;
    // the contract only needs the recorded `sh -lc` line, so the report itself is not asserted.
    let _ = launch_with_transport(&spec_path, false, true, true, &backend);

    let recorded = argv_log.lock().unwrap();
    let shell_line = recorded
        .iter()
        .flatten()
        .find(|arg| arg.starts_with("cd ") && arg.contains(" exec "))
        .cloned()
        .unwrap_or_else(|| {
            panic!("no worker `cd … exec …` shell line recorded; tmux calls={recorded:?}")
        });

    let env_file = ws.join(".team/runtime/provider-env/worker_a.env");
    let mut failures = Vec::new();
    let env_file_text = std::fs::read_to_string(&env_file).unwrap_or_default();
    if !env_file_text.contains("unset GEMINI_API_KEY") {
        failures.push(format!(
            "D9: provider env file must contain `unset GEMINI_API_KEY` (Python provider_env.py:86); path={} content={env_file_text:?}",
            env_file.display()
        ));
    }
    let unsets_inline = shell_line.contains("unset GEMINI_API_KEY");
    let sources_env_file = shell_line.contains(&*env_file.to_string_lossy());
    if !(unsets_inline || sources_env_file) {
        failures.push(format!(
            "D9: worker shell line must really unset GEMINI_API_KEY — either inline `unset GEMINI_API_KEY` \
or by sourcing the env file `{}` before exec (Python providers.py:142-145); shell_line={shell_line:?}",
            env_file.display()
        ));
    }
    assert!(
        failures.is_empty(),
        "D9 env_unset shell contract failed:\n{}",
        failures.join("\n")
    );
}

// ---------------------------------------------------------------------------
// fixtures
// ---------------------------------------------------------------------------

struct TeamFixture<'a> {
    name: &'a str,
    provider: &'a str,
    agents: &'a [AgentDoc<'a>],
}

struct AgentDoc<'a> {
    id: &'a str,
    role: &'a str,
    provider: &'a str,
    auth_mode: &'a str,
    model: Option<&'a str>,
    profile: Option<&'a str>,
    tools: &'a [&'a str],
    body: &'a str,
}

fn simple_fake_team(
    name: &'static str,
    agent: &'static str,
    role: &'static str,
) -> TeamFixture<'static> {
    let agents: &'static [AgentDoc<'static>] = Box::leak(Box::new([AgentDoc {
        id: agent,
        role,
        provider: "fake",
        auth_mode: "subscription",
        model: Some("fake"),
        profile: None,
        tools: &["mcp_team"],
        body: Box::leak(format!("{role}.").into_boxed_str()),
    }]));
    TeamFixture {
        name,
        provider: "codex",
        agents,
    }
}

fn write_team(workspace: &Path, fixture: &TeamFixture<'_>) -> PathBuf {
    let team = workspace.join(fixture.name);
    std::fs::create_dir_all(team.join("agents")).unwrap();
    std::fs::write(
        team.join("TEAM.md"),
        format!(
            "---\nname: {}\nobjective: 264 behavioral diff fixture.\nprovider: {}\n---\n\nTeam fixture.\n",
            fixture.name, fixture.provider
        ),
    )
    .unwrap();
    for agent in fixture.agents {
        let tools = agent
            .tools
            .iter()
            .map(|tool| format!("  - {tool}\n"))
            .collect::<String>();
        let model = agent
            .model
            .map(|model| format!("model: {model}\n"))
            .unwrap_or_default();
        let profile = agent
            .profile
            .map(|profile| format!("profile: {profile}\n"))
            .unwrap_or_default();
        std::fs::write(
            team.join("agents").join(format!("{}.md", agent.id)),
            format!(
                "---\nname: {}\nrole: {}\nprovider: {}\n{model}auth_mode: {}\n{profile}dangerously_skip_permissions: false\ntools:\n{tools}---\n\n{}\n",
                agent.id, agent.role, agent.provider, agent.auth_mode, agent.body
            ),
        )
        .unwrap();
    }
    team
}

fn seed_healthy_coordinator(workspace: &Path) {
    let workspace = team_agent::coordinator::WorkspacePath::new(workspace.to_path_buf());
    let pid = team_agent::coordinator::Pid::new(std::process::id());
    std::fs::create_dir_all(
        team_agent::coordinator::coordinator_pid_path(&workspace)
            .parent()
            .unwrap(),
    )
    .unwrap();
    team_agent::coordinator::write_coordinator_metadata(
        &workspace,
        pid,
        team_agent::coordinator::MetadataSource::Boot,
    )
    .expect("write coordinator metadata");
    std::fs::write(
        team_agent::coordinator::coordinator_pid_path(&workspace),
        pid.to_string(),
    )
    .expect("write coordinator pid");
}

fn tmp_dir(tag: &str) -> PathBuf {
    static N: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "ta-rs-264-{tag}-{}-{}",
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
    fn set(values: &[(&'static str, &'static str)]) -> Self {
        let previous = values
            .iter()
            .map(|(key, _)| (*key, std::env::var(key).ok()))
            .collect::<Vec<_>>();
        for (key, value) in values {
            std::env::set_var(key, value);
        }
        Self { previous }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (key, value) in self.previous.drain(..).rev() {
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
    }
}

// ---------------------------------------------------------------------------
// recording transport (argv + env + cwd + session)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct RecordedSpawn {
    argv: Vec<String>,
    env: BTreeMap<String, String>,
    cwd: PathBuf,
    session: String,
}

#[derive(Default)]
struct RecordingTransport {
    spawns: Mutex<Vec<RecordedSpawn>>,
    session_present: Mutex<bool>,
}

impl RecordingTransport {
    fn new() -> Self {
        Self::default()
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

    fn record_spawn(
        &self,
        session: &SessionName,
        window: &WindowName,
        argv: &[String],
        cwd: &Path,
        env: &BTreeMap<String, String>,
    ) -> SpawnResult {
        let mut spawns = self.spawns.lock().unwrap();
        spawns.push(RecordedSpawn {
            argv: argv.to_vec(),
            env: env.clone(),
            cwd: cwd.to_path_buf(),
            session: session.as_str().to_string(),
        });
        *self.session_present.lock().unwrap() = true;
        SpawnResult {
            pane_id: PaneId::new(format!("%{}", spawns.len())),
            session: session.clone(),
            window: window.clone(),
            child_pid: Some(20_000 + spawns.len() as u32),
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
        cwd: &Path,
        env: &BTreeMap<String, String>,
    ) -> Result<SpawnResult, TransportError> {
        Ok(self.record_spawn(session, window, argv, cwd, env))
    }

    fn spawn_into(
        &self,
        session: &SessionName,
        window: &WindowName,
        argv: &[String],
        cwd: &Path,
        env: &BTreeMap<String, String>,
    ) -> Result<SpawnResult, TransportError> {
        Ok(self.record_spawn(session, window, argv, cwd, env))
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
        // A ready Copilot screen ends the D6 startup-trust poll immediately.
        Ok(CapturedText {
            text: team_agent::provider::COPILOT_READY_MARKER.to_string(),
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
        Ok(Vec::new())
    }

    fn has_session(&self, _session: &SessionName) -> Result<bool, TransportError> {
        Ok(*self.session_present.lock().unwrap())
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
        *self.session_present.lock().unwrap() = false;
        Ok(())
    }

    fn kill_window(&self, _target: &Target) -> Result<(), TransportError> {
        Ok(())
    }

    fn attach_session(&self, _session: &SessionName) -> Result<AttachOutcome, TransportError> {
        Ok(AttachOutcome::Attached)
    }
}

// ---------------------------------------------------------------------------
// recording tmux runner (D9: observes the real TmuxBackend shell line)
// ---------------------------------------------------------------------------

type ArgvLog = std::sync::Arc<Mutex<Vec<Vec<String>>>>;

struct RecordingRunner {
    argv_log: ArgvLog,
}

impl RecordingRunner {
    fn new() -> Self {
        Self {
            argv_log: std::sync::Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn argv_log(&self) -> ArgvLog {
        std::sync::Arc::clone(&self.argv_log)
    }
}

impl CommandRunner for RecordingRunner {
    fn run(&self, argv: &[String]) -> Result<CommandOutput, std::io::Error> {
        self.argv_log.lock().unwrap().push(argv.to_vec());
        let subcommand = argv
            .iter()
            .find(|arg| !arg.starts_with('-') && *arg != "tmux");
        let (success, stdout) = match subcommand.map(String::as_str) {
            Some("has-session") => (false, String::new()),
            Some("display-message") => (true, "%1\n".to_string()),
            Some("capture-pane") => (true, "OpenAI Codex\ncodex>\n".to_string()),
            _ => (true, String::new()),
        };
        Ok(CommandOutput {
            success,
            code: Some(if success { 0 } else { 1 }),
            stdout,
            stderr: String::new(),
        })
    }
}
