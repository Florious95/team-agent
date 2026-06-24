#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use serde_json::{json, Value};
use serial_test::serial;
use team_agent::lifecycle::{quick_start_with_transport_in_workspace, restart_with_transport};
use team_agent::state::persist::{load_runtime_state, save_runtime_state};
use team_agent::transport::{
    AttachOutcome, BackendKind, CaptureRange, CapturedText, InjectPayload, InjectReport,
    InjectStage, InjectVerification, Key, PaneField, PaneId, PaneInfo, SessionName, SetEnvOutcome,
    SpawnResult, SubmitVerification, Target, Transport, TransportError, TurnVerification,
    WindowName,
};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_team-agent")
}

#[test]
#[serial(env)]
fn claude_worker_spawn_argv_uses_compiled_prompt_and_resolved_role_tools() {
    let _ancestry = EnvGuard::set("TEAM_AGENT_TEST_PROCESS_ANCESTRY_ARGV_JSON", "[\"/bin/zsh\"]");
    let ws = tmp_dir("claude-argv");
    let team_dir = write_team(
        &ws,
        "worker-argv",
        "claude",
        None,
        &[AgentDoc {
            id: "worker_a",
            role: "Implementation Worker",
            provider: "claude",
            tools: &[
                "fs_read",
                "fs_write",
                "fs_list",
                "execute_bash",
                "mcp_team",
            ],
            body: "ROLE BODY SENTINEL: build the porcelain adapter without hiding failures.",
        }],
    );
    seed_healthy_coordinator(&ws);
    let transport = RecordingTransport::new();

    quick_start_with_transport_in_workspace(
        &ws,
        &team_dir,
        None,
        true,
        Some("workerargv"),
        &transport,
    )
    .expect("quick-start should reach the real lifecycle command-construction path");

    let spawn = transport.single_spawn();
    let prompt = flag_value(&spawn.argv, "--append-system-prompt").unwrap_or_default();
    let mut failures = Vec::new();
    for marker in [
        "worker_a",
        "Implementation Worker",
        "Team Agent Teammate Runtime Contract",
        "send_message(to='leader'",
        "report_result exactly once",
        "ROLE BODY SENTINEL",
        "Permission note",
    ] {
        if !prompt.contains(marker) {
            failures.push(format!("B2 missing compiled prompt marker {marker:?}"));
        }
    }
    if prompt == "Implementation Worker" {
        failures.push("B2 prompt collapsed to the bare role label".to_string());
    }

    let disallowed = flag_values(&spawn.argv, "--disallowedTools");
    for forbidden in [
        "Bash",
        "Read",
        "Edit",
        "Write",
        "MultiEdit",
        "NotebookEdit",
        "Glob",
        "Grep",
    ] {
        if disallowed.iter().any(|tool| tool == forbidden) {
            failures.push(format!(
                "B1 declared capability still disallowed via --disallowedTools {forbidden:?}"
            ));
        }
    }
    if argv_has_flag(&spawn.argv, "--allowedTools") {
        failures.push("B1 Python parity violated: Claude argv must not switch to --allowedTools".to_string());
    }
    assert!(
        failures.is_empty(),
        "B1/B2 worker argv contract failed:\n{}\nprompt={prompt:?}\ndisallowed={disallowed:?}\nargv={:?}",
        failures.join("\n"),
        spawn.argv
    );
}

#[test]
#[serial(env)]
fn claude_control_role_without_native_tools_still_disallows_native_tools() {
    let _ancestry = EnvGuard::set("TEAM_AGENT_TEST_PROCESS_ANCESTRY_ARGV_JSON", "[\"/bin/zsh\"]");
    let ws = tmp_dir("claude-control");
    let team_dir = write_team(
        &ws,
        "control-argv",
        "claude",
        None,
        &[AgentDoc {
            id: "control",
            role: "Control Worker",
            provider: "claude",
            tools: &["mcp_team"],
            body: "CONTROL BODY SENTINEL: route only through Team Agent MCP.",
        }],
    );
    seed_healthy_coordinator(&ws);
    let transport = RecordingTransport::new();

    quick_start_with_transport_in_workspace(
        &ws,
        &team_dir,
        None,
        true,
        Some("controlargv"),
        &transport,
    )
    .expect("quick-start should record the control worker command");

    let spawn = transport.single_spawn();
    let disallowed = flag_values(&spawn.argv, "--disallowedTools");
    for expected in [
        "Bash",
        "Read",
        "Edit",
        "Write",
        "MultiEdit",
        "NotebookEdit",
        "Glob",
        "Grep",
    ] {
        assert!(
            disallowed.iter().any(|tool| tool == expected),
            "B1 negative case: mcp_team-only control roles must still disallow native Claude tools; \
             missing={expected} disallowed={disallowed:?}\nargv={:?}",
            spawn.argv
        );
    }
}

#[test]
#[serial(env)]
fn restart_claude_worker_resolves_tools_from_spec_when_runtime_state_has_no_raw_tools() {
    let _ancestry = EnvGuard::set("TEAM_AGENT_TEST_PROCESS_ANCESTRY_ARGV_JSON", "[\"/bin/zsh\"]");
    let ws = tmp_dir("claude-restart-tools");
    let team_dir = write_team(
        &ws,
        "restart-tools",
        "claude",
        Some("none"),
        &[AgentDoc {
            id: "worker_a",
            role: "Implementation Worker",
            provider: "claude",
            tools: &[
                "fs_read",
                "fs_write",
                "fs_list",
                "execute_bash",
                "mcp_team",
            ],
            body: "ROLE BODY SENTINEL: restart must keep declared native tools.",
        }],
    );
    seed_healthy_coordinator(&ws);
    quick_start_with_transport_in_workspace(
        &ws,
        &team_dir,
        None,
        true,
        Some("restarttools"),
        &RecordingTransport::new(),
    )
    .expect("quick-start should seed restart state");
    seed_running_resumable_state(&ws, "restarttools", "worker_a", "claude");
    remove_runtime_raw_tools(&ws, "restarttools", "worker_a");

    let restart_transport = RecordingTransport::new();
    let report = restart_with_transport(&ws, true, Some("restarttools"), &restart_transport)
        .expect("restart should reach worker command construction");
    let spawn = restart_transport.single_spawn();
    let disallowed = flag_values(&spawn.argv, "--disallowedTools");

    for forbidden in [
        "Bash",
        "Read",
        "Edit",
        "Write",
        "MultiEdit",
        "NotebookEdit",
        "Glob",
        "Grep",
    ] {
        assert!(
            !disallowed.iter().any(|tool| tool == forbidden),
            "B1 restart contract: when runtime state lacks raw tools, restart must resolve tools from the selected spec/role through resolve_permissions. \
             {forbidden} must not be disallowed for this role; disallowed={disallowed:?}\nargv={:?}\nreport={report:?}",
            spawn.argv
        );
    }
    assert!(
        !argv_has_flag(&spawn.argv, "--allowedTools"),
        "B1 Python parity: restart must keep --disallowedTools semantics, not switch to --allowedTools; argv={:?}",
        spawn.argv
    );
}

#[test]
fn worker_command_context_uses_single_prompt_and_permission_sources() {
    let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let src_root = crate_root.join("src");
    let launch = read_to_string(&src_root.join("lifecycle/launch.rs"));
    let restart_common = read_to_string(&src_root.join("lifecycle/restart/common.rs"));
    let restart_agent = read_to_string(&src_root.join("lifecycle/restart/agent.rs"));
    let adapter = read_to_string(&src_root.join("provider/adapter.rs"));
    let all_src = read_rust_sources(&src_root);
    let command_sources = format!("{launch}\n{restart_common}\n{restart_agent}");

    assert!(
        all_src.contains("compile_worker_system_prompt")
            && all_src.contains("runtime_contract_section")
            && all_src.contains("identity_section")
            && all_src.contains("role_body")
            && all_src.contains("output_contract")
            && all_src.contains("permission_notes"),
        "B2 grep guard: worker command construction needs one compiled prompt helper with the CR-approved five sections \
         (runtime_contract_section, identity_section, role_body, output_contract, permission_notes)."
    );
    assert!(
        !command_sources.contains("system_prompt: role"),
        "B2 grep guard: launch/restart must not pass the raw role label as ProviderCommandContext.system_prompt."
    );
    assert!(
        all_src.matches("Team Agent Teammate Runtime Contract").count() == 1,
        "B2 grep guard: the common teammate runtime contract must have a single production source; count={}",
        all_src.matches("Team Agent Teammate Runtime Contract").count()
    );
    assert!(
        all_src.matches("529").count() <= 1 && all_src.matches("Overloaded").count() <= 1,
        "B2 grep guard: common 529/Overloaded slowdown wording belongs in the one runtime contract template, not scattered role/provider code."
    );

    assert!(
        all_src.contains("resolve_permissions") && all_src.contains("resolved_tool_strings"),
        "B1 grep guard: command argv tools must be calculated through a single resolve_permissions helper, including role defaults and leader ceiling."
    );
    assert!(
        !command_sources.contains("worker_tool_refs(agent_tool_strings")
            && !command_sources.contains("tools: &tool_refs"),
        "B1 grep guard: command construction must not raw-pass agent.tools/tool_refs into ProviderCommandContext."
    );
    assert!(
        adapter.contains("--disallowedTools") && !adapter.contains("--allowedTools"),
        "B1 Python parity: provider adapter must keep Claude --disallowedTools and not invent --allowedTools."
    );
    assert!(
        !adapter.contains("resolve_permissions"),
        "B1 grep guard: adapter.rs should stay a dumb argv renderer; provider-specific permission fallback guesses belong in the shared command-context helper."
    );
}

#[test]
#[serial(env)]
fn quick_start_default_display_adaptive_and_none_escape_hatch_are_observable() {
    let default_ws = tmp_dir("display-default");
    let default_team = write_team(
        &default_ws,
        "display-default-team",
        "fake",
        None,
        &[AgentDoc {
            id: "worker",
            role: "Fake Worker",
            provider: "fake",
            tools: &["mcp_team"],
            body: "Fake worker.",
        }],
    );
    seed_healthy_coordinator(&default_ws);
    let default_report = quick_start_with_transport_in_workspace(
        &default_ws,
        &default_team,
        None,
        true,
        Some("defaultdisplay"),
        &RecordingTransport::new(),
    )
    .expect("default quick-start should run with recording transport");
    let default_state = load_runtime_state(&default_ws).expect("load default display state");

    assert_eq!(
        state_display_backend(&default_state),
        Some("adaptive"),
        "adaptive layout contract: omitting display_backend in TEAM.md must persist display_backend=adaptive. \
         state={default_state}\nreport={default_report:?}"
    );
    assert!(
        format!("{default_report:?}").contains("display_backend")
            && format!("{default_report:?}").contains("adaptive"),
        "adaptive layout contract: quick-start report/JSON surface must visibly report display_backend=adaptive, not hide the default. report={default_report:?}"
    );
    // 0.3.28 Step 4b: per-agent adaptive `display` records removed in favour
    // of 1-window-per-agent layout. The state-root `display_backend=adaptive`
    // (asserted above) is the contract. Per-agent display dicts were the
    // pre-0.3.28 adaptive overlay bookkeeping; the architecture moved that to
    // `layout::overlay` (display overlay only fires for the leader-session
    // overlay path, not per worker).
    // Skip state_has_adaptive_display_records — replaced by:
    assert!(
        default_state.pointer("/agents/worker/window").is_some(),
        "0.3.28 Step 4b: each worker has its own window in the worker session. \
         state={default_state}"
    );

    let none_ws = tmp_dir("display-none");
    let none_team = write_team(
        &none_ws,
        "display-none-team",
        "fake",
        Some("none"),
        &[AgentDoc {
            id: "worker",
            role: "Fake Worker",
            provider: "fake",
            tools: &["mcp_team"],
            body: "Fake worker.",
        }],
    );
    seed_healthy_coordinator(&none_ws);
    let none_report = quick_start_with_transport_in_workspace(
        &none_ws,
        &none_team,
        None,
        true,
        Some("nonedisplay"),
        &RecordingTransport::new(),
    )
    .expect("none quick-start should run with recording transport");
    let none_state = load_runtime_state(&none_ws).expect("load none display state");
    assert_eq!(
        state_display_backend(&none_state),
        Some("none"),
        "adaptive layout contract: explicit display_backend=none remains the escape hatch. state={none_state}\nreport={none_report:?}"
    );
    assert!(
        !state_has_adaptive_display_records(&none_state),
        "explicit display_backend=none must not create adaptive display records. state={none_state}"
    );
}

#[test]
#[serial(env)]
fn quick_start_and_restart_emit_copyable_workspace_socket_attach_commands() {
    let ws = tmp_dir("attach-command");
    let team = write_team(
        &ws,
        "attach-team",
        "fake",
        Some("none"),
        &[AgentDoc {
            id: "worker",
            role: "Fake Worker",
            provider: "fake",
            tools: &["mcp_team"],
            body: "Fake worker.",
        }],
    );
    seed_healthy_coordinator(&ws);
    let transport = RecordingTransport::new();
    let quick = quick_start_with_transport_in_workspace(
        &ws,
        &team,
        None,
        true,
        Some("attachcmd"),
        &transport,
    )
    .expect("quick-start should run with recording transport");
    let quick_text = format!("{quick:?}");
    let mut failures = Vec::new();
    if !report_has_attach_command(&quick_text, "team-attachcmd", "worker") {
        failures.push(format!(
            "quick-start missing `tmux -S <full-socket-path> attach -t team-attachcmd:worker`; report={quick:?}"
        ));
    }
    seed_running_resumable_state(&ws, "attachcmd", "worker", "fake");
    let restart = restart_with_transport(&ws, true, Some("attachcmd"), &RecordingTransport::new())
        .expect("restart should run with recording transport");
    let restart_text = format!("{restart:?}");
    if !report_has_attach_command(&restart_text, "team-attachcmd", "worker") {
        failures.push(format!(
            "restart missing `tmux -S <full-socket-path> attach -t team-attachcmd:worker`; report={restart:?}"
        ));
    }
    assert!(
        failures.is_empty(),
        "B3 attach-command contract: quick-start and restart report/JSON must include copyable workspace-socket attach commands.\n{}",
        failures.join("\n")
    );
}

#[test]
fn display_default_none_is_documented_in_help_and_changelog() {
    let help = Command::new(bin())
        .args(["quick-start", "--help"])
        .output()
        .expect("run quick-start --help");
    let help_text = format!(
        "{}\n{}",
        String::from_utf8_lossy(&help.stdout),
        String::from_utf8_lossy(&help.stderr)
    );
    assert!(
        help_text.contains("display_backend")
            && help_text.to_lowercase().contains("default")
            && help_text.to_lowercase().contains("adaptive")
            && help_text.to_lowercase().contains("none"),
        "adaptive layout guard: quick-start help must disclose default adaptive and none escape hatch. help={help_text:?}"
    );

    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .unwrap()
        .to_path_buf();
    let docs = read_optional_changelogs(&repo_root);
    assert!(
        docs.contains("display_backend")
            && docs.to_lowercase().contains("adaptive")
            && docs.to_lowercase().contains("none"),
        "adaptive layout guard: CHANGELOG/docs must disclose default adaptive and none escape hatch. searched_root={} docs_excerpt={}",
        repo_root.display(),
        &docs.chars().take(500).collect::<String>()
    );
}

#[derive(Clone)]
struct AgentDoc<'a> {
    id: &'a str,
    role: &'a str,
    provider: &'a str,
    tools: &'a [&'a str],
    body: &'a str,
}

#[derive(Debug, Clone)]
struct RecordedSpawn {
    argv: Vec<String>,
}

#[derive(Default)]
struct RecordingTransport {
    spawns: Mutex<Vec<RecordedSpawn>>,
    session_present: Mutex<bool>,
}

struct EnvGuard {
    key: &'static str,
    old: Option<String>,
}

impl EnvGuard {
    fn set(key: &'static str, value: &str) -> Self {
        let old = std::env::var(key).ok();
        std::env::set_var(key, value);
        Self { key, old }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        match self.old.take() {
            Some(value) => std::env::set_var(self.key, value),
            None => std::env::remove_var(self.key),
        }
    }
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
            "fixture should record one worker spawn; spawns={spawns:?}"
        );
        spawns[0].clone()
    }

    fn record_spawn(
        &self,
        session: &SessionName,
        window: &WindowName,
        argv: &[String],
    ) -> SpawnResult {
        let mut spawns = self.spawns.lock().unwrap();
        spawns.push(RecordedSpawn {
            argv: argv.to_vec(),
        });
        *self.session_present.lock().unwrap() = true;
        SpawnResult {
            pane_id: PaneId::new(format!("%{}", spawns.len())),
            session: session.clone(),
            window: window.clone(),
            child_pid: Some(10_000 + spawns.len() as u32),
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
        _env: &BTreeMap<String, String>,
    ) -> Result<SpawnResult, TransportError> {
        Ok(self.record_spawn(session, window, argv))
    }

    fn spawn_into(
        &self,
        session: &SessionName,
        window: &WindowName,
        argv: &[String],
        _cwd: &Path,
        _env: &BTreeMap<String, String>,
    ) -> Result<SpawnResult, TransportError> {
        Ok(self.record_spawn(session, window, argv))
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

    fn liveness(&self, _pane: &PaneId) -> Result<team_agent::transport::PaneLiveness, TransportError> {
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

fn tmp_dir(tag: &str) -> PathBuf {
    static N: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "ta-rs-core-034-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::canonicalize(dir).unwrap()
}

fn write_team(
    workspace: &Path,
    name: &str,
    provider: &str,
    display_backend: Option<&str>,
    agents: &[AgentDoc<'_>],
) -> PathBuf {
    let team = workspace.join(name);
    std::fs::create_dir_all(team.join("agents")).unwrap();
    let display = display_backend
        .map(|backend| format!("display_backend: {backend}\n"))
        .unwrap_or_default();
    std::fs::write(
        team.join("TEAM.md"),
        format!(
            "---\nname: {name}\nobjective: 0.3.4 core contract fixture.\nprovider: {provider}\n{display}---\n\nTeam fixture.\n"
        ),
    )
    .unwrap();
    for agent in agents {
        let tools = agent
            .tools
            .iter()
            .map(|tool| format!("  - {tool}\n"))
            .collect::<String>();
        std::fs::write(
            team.join("agents").join(format!("{}.md", agent.id)),
            format!(
                "---\nname: {}\nrole: {}\nprovider: {}\nmodel: fake\nauth_mode: subscription\ntools:\n{}---\n\n{}\n",
                agent.id, agent.role, agent.provider, tools, agent.body
            ),
        )
        .unwrap();
    }
    team
}

fn seed_healthy_coordinator(workspace: &Path) {
    let workspace = team_agent::coordinator::WorkspacePath::new(workspace.to_path_buf());
    let pid = team_agent::coordinator::Pid::new(std::process::id());
    std::fs::create_dir_all(team_agent::coordinator::coordinator_pid_path(&workspace).parent().unwrap()).unwrap();
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

fn seed_running_resumable_state(workspace: &Path, team: &str, agent: &str, provider: &str) {
    let mut state = load_runtime_state(workspace).expect("load quick-start state before restart");
    let agent_patch = json!({
        "status": "running",
        "provider": provider,
        "session_id": "11111111-2222-4333-8444-555555555555",
        "first_send_at": "2026-06-10T00:00:00+00:00",
        "window": agent,
    });
    merge_agent_patch(state.pointer_mut(&format!("/agents/{agent}")), &agent_patch);
    merge_agent_patch(
        state.pointer_mut(&format!("/teams/{team}/agents/{agent}")),
        &agent_patch,
    );
    save_runtime_state(workspace, &state).expect("save resumable state");
}

fn merge_agent_patch(target: Option<&mut Value>, patch: &Value) {
    let Some(target) = target.and_then(Value::as_object_mut) else {
        return;
    };
    for (key, value) in patch.as_object().unwrap() {
        target.insert(key.clone(), value.clone());
    }
}

fn remove_runtime_raw_tools(workspace: &Path, team: &str, agent: &str) {
    let mut state = load_runtime_state(workspace).expect("load state before stripping raw tools");
    for pointer in [
        format!("/agents/{agent}/tools"),
        format!("/teams/{team}/agents/{agent}/tools"),
    ] {
        if let Some(obj) = state
            .pointer_mut(pointer.trim_end_matches("/tools"))
            .and_then(Value::as_object_mut)
        {
            obj.remove("tools");
            obj.remove("role");
        }
    }
    save_runtime_state(workspace, &state).expect("save state after stripping raw tools");
}

fn flag_value(argv: &[String], flag: &str) -> Option<String> {
    argv.windows(2)
        .find_map(|pair| (pair[0] == flag).then(|| pair[1].clone()))
}

fn flag_values(argv: &[String], flag: &str) -> Vec<String> {
    argv.windows(2)
        .filter_map(|pair| (pair[0] == flag).then(|| pair[1].clone()))
        .collect()
}

fn argv_has_flag(argv: &[String], flag: &str) -> bool {
    argv.iter().any(|arg| arg == flag)
}

fn state_display_backend(state: &Value) -> Option<&str> {
    state
        .get("display_backend")
        .and_then(Value::as_str)
        .or_else(|| {
            state
                .get("runtime")
                .and_then(|runtime| runtime.get("display_backend"))
                .and_then(Value::as_str)
        })
}

fn state_has_adaptive_display_records(state: &Value) -> bool {
    let agents = state
        .get("agents")
        .and_then(Value::as_object)
        .into_iter()
        .flat_map(|agents| agents.values());
    agents.into_iter().any(|agent| {
        agent
            .get("display")
            .and_then(|display| display.get("backend"))
            .and_then(Value::as_str)
            == Some("adaptive")
    })
}

fn report_has_attach_command(text: &str, expected_session: &str, expected_window: &str) -> bool {
    text.contains("tmux -S")
        && text.contains(" attach -t ")
        && text.contains(&format!("{expected_session}:{expected_window}"))
}

fn read_to_string(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

fn read_rust_sources(root: &Path) -> String {
    let mut out = String::new();
    read_rust_sources_into(root, &mut out);
    out
}

fn read_rust_sources_into(root: &Path, out: &mut String) {
    let entries =
        std::fs::read_dir(root).unwrap_or_else(|e| panic!("read_dir {}: {e}", root.display()));
    for entry in entries {
        let path = entry.unwrap().path();
        if path.is_dir() {
            read_rust_sources_into(&path, out);
        } else if path.extension().and_then(|s| s.to_str()) == Some("rs") {
            out.push_str(&read_to_string(&path));
            out.push('\n');
        }
    }
}

fn read_optional_changelogs(repo_root: &Path) -> String {
    let mut text = String::new();
    for name in ["CHANGELOG.md", "CHANGELOG", "README.md"] {
        let path = repo_root.join(name);
        if path.exists() {
            text.push_str(&read_to_string(&path));
            text.push('\n');
        }
    }
    text
}
