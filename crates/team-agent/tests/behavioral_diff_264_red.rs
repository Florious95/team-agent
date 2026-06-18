//! #264 behavioral diff regression contracts (D1-D9).
//!
//! Source of truth: Python runtime 0.2.11 (`~/.team-agent/runtime/0.2.11/src/team_agent/`).
//! Every asserted value below was grounded by executing the Python truth source
//! (CodexAdapter.build_command / compile_system_prompt / providers.shell_command) —
//! see `.team/artifacts/264-behavioral-diff-report.md` for the per-item file:line evidence.
//!
//! D1: codex developer_instructions triple escape (\\ then " then \n)  — codex.py:120-121
//! D2: codex MCP `-c mcp_servers.<name>.tool_timeout_sec=600.0`        — codex.py:129
//! D3: codex `--profile <CODEX_PROFILE>` + `-c <codex_config>` items   — codex.py:105-107,117-118 / provider_env.py:62-79
//! D4: worker shell env TEAM_AGENT_ID=<worker self> (overrides parent)  — providers.py:131
//! D5: fresh launch spawn cwd == workspace (tmux cwd + state.spawn_cwd) — providers.py:145 / launch/core.py:253
//! D6: compiled system prompt first section is identity                 — prompt.py:39
//! D7: runtime.fast → codex fast-mode toggle "/fast" after spawn        — launch/core.py:235-237
//! D8: launch session_name fallback = team-{name}, not a constant       — launch/core.py:56
//! D9: profile env_unset truly unsets in the worker shell line          — providers.py:142-145 / provider_env.py:82-90

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

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

/// D1+D2+D3: one codex argv golden locks the three codex command-construction gaps.
///
/// Python golden (executed 0.2.11 CodexAdapter.build_command with the same profile values):
/// `codex --no-alt-screen --disable shell_snapshot --disable apps --profile relay-fast
///  --sandbox read-only --ask-for-approval on-request --model gpt-5.5
///  -c model_provider="relay" -c model_providers.relay.base_url="https://relay.example/v1"
///  -c model_providers.relay.env_key="TEAM_AGENT_PROVIDER_API_KEY"
///  -c model_providers.relay.wire_api="chat" -c model_providers.relay.name="Relay"
///  -c developer_instructions="...\n...ESCAPE-CHECK back\\slash \"dq\" end..."
///  -c mcp_servers.team_orchestrator.command="..." ... -c mcp_servers.team_orchestrator.tool_timeout_sec=600.0`
#[test]
#[serial(env)]
fn d1_d2_d3_codex_argv_matches_python_golden_escape_timeout_profile() {
    let _ancestry = EnvGuard::set(&[(ANCESTRY_KEY, NEUTRAL_ANCESTRY)]);
    let ws = tmp_dir("d123-codex-argv");
    let team_dir = write_team(
        &ws,
        &TeamFixture {
            name: "d123team",
            provider: "codex",
            fast: false,
            agents: &[AgentDoc {
                id: "worker_a",
                role: "Codex Diff Worker",
                provider: "codex",
                auth_mode: "compatible_api",
                model: Some("gpt-5.5"),
                profile: Some("relay"),
                tools: &["mcp_team"],
                body: "ESCAPE-CHECK back\\slash \"dq\" end",
            }],
        },
    );
    write_relay_profile(&team_dir);
    seed_healthy_coordinator(&ws);
    let transport = RecordingTransport::new();

    quick_start_with_transport_in_workspace(&ws, &team_dir, None, true, true, Some("d123team"), &transport)
        .expect("quick-start should reach codex command construction");
    let spawn = transport.single_spawn();
    let argv = &spawn.argv;
    let mut failures = Vec::new();

    // D1: triple escape, Python order \\ -> \" -> \n (codex.py:120-121).
    let dev = argv
        .iter()
        .find(|arg| arg.starts_with("developer_instructions=\""));
    match dev {
        None => failures.push("D1: no `-c developer_instructions=\"...\"` value in codex argv".to_string()),
        Some(dev) => {
            if dev.contains('\n') {
                failures.push(format!(
                    "D1: developer_instructions value contains RAW newlines; Python escapes them as literal \\n (codex.py:120). head={:?}",
                    dev.chars().take(120).collect::<String>()
                ));
            }
            // Raw marker `ESCAPE-CHECK back\slash "dq" end` must arrive as `back\\slash \"dq\" end`.
            let escaped_marker = "ESCAPE-CHECK back\\\\slash \\\"dq\\\" end";
            if !dev.contains(escaped_marker) {
                failures.push(format!(
                    "D1: developer_instructions must contain the Python-escaped marker {escaped_marker:?} (backslash doubled before quote/newline escaping)"
                ));
            }
        }
    }

    // D2: codex.py:129 — every MCP server gets tool_timeout_sec=600.0.
    let timeout_item = "mcp_servers.team_orchestrator.tool_timeout_sec=600.0";
    if !argv.iter().any(|arg| arg == timeout_item) {
        failures.push(format!("D2: codex argv missing `-c {timeout_item}`"));
    }

    // D3: profile command overrides (provider_env.py:62-79 + codex.py:105-107,117-118).
    if flag_value(argv, "--profile").as_deref() != Some("relay-fast") {
        failures.push(format!(
            "D3: codex argv must carry `--profile relay-fast` from profile CODEX_PROFILE; got {:?}",
            flag_value(argv, "--profile")
        ));
    }
    for config in [
        "model_provider=\"relay\"",
        "model_providers.relay.base_url=\"https://relay.example/v1\"",
        "model_providers.relay.env_key=\"TEAM_AGENT_PROVIDER_API_KEY\"",
        "model_providers.relay.wire_api=\"chat\"",
        "model_providers.relay.name=\"Relay\"",
    ] {
        if !argv.iter().any(|arg| arg == config) {
            failures.push(format!("D3: codex argv missing `-c {config}` (profile codex_config)"));
        }
    }
    // Python golden order: --profile before --sandbox; codex_config -c before developer_instructions.
    if let (Some(profile_idx), Some(sandbox_idx)) = (
        argv.iter().position(|arg| arg == "--profile"),
        argv.iter().position(|arg| arg == "--sandbox"),
    ) {
        if profile_idx > sandbox_idx {
            failures.push("D3: `--profile` must precede `--sandbox` (codex.py:105-113)".to_string());
        }
    }
    if let (Some(config_idx), Some(dev_idx)) = (
        argv.iter().position(|arg| arg == "model_provider=\"relay\""),
        argv.iter().position(|arg| arg.starts_with("developer_instructions=")),
    ) {
        if config_idx > dev_idx {
            failures.push(
                "D3: profile codex_config `-c` items must precede developer_instructions (codex.py:117-121)"
                    .to_string(),
            );
        }
    }

    assert!(
        failures.is_empty(),
        "D1/D2/D3 codex argv contract failed:\n{}\nargv={argv:?}",
        failures.join("\n")
    );
}

/// D4: providers.py:131 — the worker shell env must export TEAM_AGENT_ID=<the worker itself>,
/// overriding any TEAM_AGENT_ID inherited from the launching process (e.g. an add-agent issued
/// from another worker's MCP server, whose environ carries the CALLER's TEAM_AGENT_ID).
#[test]
#[serial(env)]
fn d4_worker_shell_env_sets_team_agent_id_to_worker_self() {
    let _guard = EnvGuard::set(&[
        (ANCESTRY_KEY, NEUTRAL_ANCESTRY),
        ("TEAM_AGENT_ID", "stale-mcp-caller"),
    ]);
    let ws = tmp_dir("d4-spawn-env");
    let team_dir = write_team(&ws, &simple_codex_team("d4team", "worker_a", "Env Worker"));
    seed_healthy_coordinator(&ws);
    let transport = RecordingTransport::new();

    quick_start_with_transport_in_workspace(&ws, &team_dir, None, true, true, Some("d4team"), &transport)
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
    let _ancestry = EnvGuard::set(&[(ANCESTRY_KEY, NEUTRAL_ANCESTRY)]);
    let ws = tmp_dir("d5-spawn-cwd");
    let team_dir = write_team(&ws, &simple_codex_team("d5team", "worker_a", "Cwd Worker"));
    seed_healthy_coordinator(&ws);
    let transport = RecordingTransport::new();

    quick_start_with_transport_in_workspace(&ws, &team_dir, None, true, true, Some("d5team"), &transport)
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
    let _ancestry = EnvGuard::set(&[(ANCESTRY_KEY, NEUTRAL_ANCESTRY)]);
    let ws = tmp_dir("d6-prompt-order");
    let team_dir = write_team(
        &ws,
        &TeamFixture {
            name: "d6team",
            provider: "claude",
            fast: false,
            agents: &[AgentDoc {
                id: "worker_a",
                role: "Prompt Order Worker",
                provider: "claude",
                auth_mode: "subscription",
                model: Some("fake"),
                profile: None,
                tools: &["mcp_team"],
                body: "ROLE BODY SENTINEL for prompt ordering.",
            }],
        },
    );
    seed_healthy_coordinator(&ws);
    let transport = RecordingTransport::new();

    quick_start_with_transport_in_workspace(&ws, &team_dir, None, true, true, Some("d6team"), &transport)
        .expect("quick-start should spawn the claude worker");
    let spawn = transport.single_spawn();
    let prompt = flag_value(&spawn.argv, "--append-system-prompt")
        .expect("claude argv must carry --append-system-prompt");

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

/// D7: launch/core.py:235-237 — runtime.fast + provider==codex must toggle codex fast mode
/// after spawn by sending `/fast` + Enter to the worker pane (tmux_prompt.py:124-129).
#[test]
#[serial(env)]
fn d7_runtime_fast_sends_codex_fast_mode_toggle() {
    let _ancestry = EnvGuard::set(&[(ANCESTRY_KEY, NEUTRAL_ANCESTRY)]);
    let ws = tmp_dir("d7-fast-mode");
    let mut fixture = simple_codex_team("d7team", "worker_a", "Fast Worker");
    fixture.fast = true;
    let team_dir = write_team(&ws, &fixture);
    seed_healthy_coordinator(&ws);
    let transport = RecordingTransport::new();

    quick_start_with_transport_in_workspace(&ws, &team_dir, None, true, true, Some("d7team"), &transport)
        .expect("quick-start should spawn the codex worker");
    transport.single_spawn();
    let sent = transport.sent_text();
    assert!(
        sent.iter().any(|text| text.contains("/fast")),
        "D7: TEAM.md `fast: true` (compiled to runtime.fast) must send the `/fast` toggle to the \
codex worker after spawn (Python launch/core.py:235-237 + tmux_prompt.py:124-129); recorded \
keystrokes/injections={sent:?}"
    );
}

/// D8: launch/core.py:56 — when the spec carries no runtime.session_name, the session falls
/// back to `team-{team.name}`, never a fixed constant.
#[test]
#[serial(env)]
fn d8_launch_session_name_fallback_derives_from_team_name() {
    let _ancestry = EnvGuard::set(&[(ANCESTRY_KEY, NEUTRAL_ANCESTRY)]);
    let ws = tmp_dir("d8-session-name");
    let team_dir = write_team(&ws, &simple_codex_team("d8team", "worker_a", "Session Worker"));
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

/// D9: provider_env.py:82-90 + providers.py:142-145 — profile env_unset keys (codex
/// compatible_api → OPENAI_API_KEY / OPENAI_BASE_URL) must be REALLY unset in the worker
/// shell line (Python sources the env file whose first lines are `unset <KEY>`), because the
/// tmux server environment can still carry stale values that plain map-removal cannot clear.
#[test]
#[serial(env)]
fn d9_profile_env_unset_reaches_worker_shell_line() {
    let _guard = EnvGuard::set(&[
        (ANCESTRY_KEY, NEUTRAL_ANCESTRY),
        ("OPENAI_API_KEY", "stale-parent-key"),
    ]);
    let ws = tmp_dir("d9-env-unset");
    let team_dir = write_team(
        &ws,
        &TeamFixture {
            name: "d9team",
            provider: "codex",
            fast: false,
            agents: &[AgentDoc {
                id: "worker_a",
                role: "Unset Worker",
                provider: "codex",
                auth_mode: "compatible_api",
                model: Some("gpt-5.5"),
                profile: Some("relay-nokey"),
                tools: &["mcp_team"],
                body: "Unset worker.",
            }],
        },
    );
    // No API_KEY on purpose: OPENAI_API_KEY lands in env_unset (provider_env.py:51-52) and is
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
        .unwrap_or_else(|| panic!("no worker `cd … exec …` shell line recorded; tmux calls={recorded:?}"));

    let env_file = ws.join(".team/runtime/provider-env/worker_a.env");
    let mut failures = Vec::new();
    let env_file_text = std::fs::read_to_string(&env_file).unwrap_or_default();
    if !env_file_text.contains("unset OPENAI_API_KEY") {
        failures.push(format!(
            "D9: provider env file must contain `unset OPENAI_API_KEY` (Python provider_env.py:86); path={} content={env_file_text:?}",
            env_file.display()
        ));
    }
    let unsets_inline = shell_line.contains("unset OPENAI_API_KEY");
    let sources_env_file = shell_line.contains(&*env_file.to_string_lossy());
    if !(unsets_inline || sources_env_file) {
        failures.push(format!(
            "D9: worker shell line must really unset OPENAI_API_KEY — either inline `unset OPENAI_API_KEY` \
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
    fast: bool,
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

fn simple_codex_team(
    name: &'static str,
    agent: &'static str,
    role: &'static str,
) -> TeamFixture<'static> {
    let agents: &'static [AgentDoc<'static>] = Box::leak(Box::new([AgentDoc {
        id: agent,
        role,
        provider: "codex",
        auth_mode: "subscription",
        model: Some("gpt-5.5"),
        profile: None,
        tools: &["mcp_team"],
        body: Box::leak(format!("{role}.").into_boxed_str()),
    }]));
    TeamFixture {
        name,
        provider: "codex",
        fast: false,
        agents,
    }
}

fn write_team(workspace: &Path, fixture: &TeamFixture<'_>) -> PathBuf {
    let team = workspace.join(fixture.name);
    std::fs::create_dir_all(team.join("agents")).unwrap();
    let fast = if fixture.fast { "fast: true\n" } else { "" };
    std::fs::write(
        team.join("TEAM.md"),
        format!(
            "---\nname: {}\nobjective: 264 behavioral diff fixture.\nprovider: {}\n{fast}---\n\nTeam fixture.\n",
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
                "---\nname: {}\nrole: {}\nprovider: {}\n{model}auth_mode: {}\n{profile}tools:\n{tools}---\n\n{}\n",
                agent.id, agent.role, agent.provider, agent.auth_mode, agent.body
            ),
        )
        .unwrap();
    }
    team
}

/// Profile values mirror the Python golden run: provider_env.py:62-79 turns these into
/// codex_profile=relay-fast plus the five `-c` codex_config items asserted in D3.
fn write_relay_profile(team_dir: &Path) {
    let profiles = team_dir.join("profiles");
    std::fs::create_dir_all(&profiles).unwrap();
    std::fs::write(
        profiles.join("relay.env"),
        "AUTH_MODE=compatible_api\n\
         BASE_URL=https://relay.example/v1\n\
         API_KEY=sk-relay-264\n\
         MODEL_PROVIDER=relay\n\
         WIRE_API=chat\n\
         PROVIDER_NAME=Relay\n\
         CODEX_PROFILE=relay-fast\n",
    )
    .unwrap();
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

fn flag_value(argv: &[String], flag: &str) -> Option<String> {
    argv.windows(2)
        .find_map(|pair| (pair[0] == flag).then(|| pair[1].clone()))
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
// recording transport (argv + env + cwd + session + keystrokes)
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
    sent_text: Mutex<Vec<String>>,
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

    fn sent_text(&self) -> Vec<String> {
        self.sent_text.lock().unwrap().clone()
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
        payload: &InjectPayload,
        _submit: Key,
        _bracketed: bool,
    ) -> Result<InjectReport, TransportError> {
        if let Some(text) = payload.text() {
            self.sent_text.lock().unwrap().push(text.to_string());
        }
        Ok(InjectReport {
            stage_reached: InjectStage::Submit,
            inject_verification: InjectVerification::CaptureContainsToken,
            submit_verification: SubmitVerification::EnterSentWithoutPlaceholderCheck,
            turn_verification: TurnVerification::NotYetObserved,
            attempts: 1,
            submit_diagnostics: None,
        })
    }

    fn send_keys(&self, _target: &Target, keys: &[Key]) -> Result<(), TransportError> {
        let text: String = keys
            .iter()
            .map(|key| match key {
                Key::Char(c) => *c,
                Key::Enter => '\n',
                _ => '\u{FFFD}',
            })
            .collect();
        self.sent_text.lock().unwrap().push(text);
        Ok(())
    }

    fn capture(&self, _target: &Target, range: CaptureRange) -> Result<CapturedText, TransportError> {
        Ok(CapturedText {
            text: "Claude Code\n> \nOpenAI Codex\ncodex>".to_string(),
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
        let subcommand = argv.iter().find(|arg| !arg.starts_with('-') && *arg != "tmux");
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
