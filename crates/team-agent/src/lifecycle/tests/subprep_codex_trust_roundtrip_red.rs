//! SUB prep Codex trust + roundtrip contracts.
//!
//! Anchors:
//! - `.team/artifacts/rs-subprep-codex-trust-roundtrip-root-cause.md`
//! - `.team/artifacts/macmini-e2e/subprep-codex-probe5-20260605T071044Z/`
//!
//! Real provider subscription behavior still needs rt smoke for the provider pane/report_result
//! parts; these cargo contracts lock the local recognizer, delivery state, and command-shape
//! surfaces that caused the probe failures. The legacy Codex worker runtime (its rollout capture
//! and worker trust loop) is gone: the Codex recognizer/matcher checks stay (the `team-agent codex`
//! leader uses them), while delivery/roundtrip contracts run on a Copilot worker, the only legacy
//! worker the delivery trust gate peeks for.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[path = "../../../tests/support/hermetic.rs"]
mod hermetic_guard;
#[allow(dead_code)]
fn _hermetic_boundary_marker(_: &hermetic_guard::HermeticTestEnv) {}

#[path = "../../../tests/support/composite_source.rs"]
mod composite_source;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

#[path = "../../../tests/support/temp_workspace.rs"]
mod temp_workspace;

use rusqlite::params;
use serde_json::json;
use team_agent::event_log::EventLog;
use team_agent::lifecycle::launch_with_transport;
use team_agent::message_store::MessageStore;
use team_agent::messaging::delivery::deliver_pending_message;
use team_agent::messaging::trust::attempt_trust_auto_answer;
use team_agent::messaging::PaneWidthQuery;
use team_agent::model::enums::{AuthMode, Provider};
use team_agent::provider::{get_adapter, COPILOT_READY_MARKER, COPILOT_TRUST_PROMPT_MARKER};
use team_agent::transport::{
    AttachOutcome, BackendKind, CaptureRange, CapturedText, InjectPayload, InjectReport,
    InjectStage, InjectVerification, Key, PaneField, PaneId, PaneInfo, SessionName, SetEnvOutcome,
    SpawnResult, SubmitVerification, Target, Transport, TransportError, TurnVerification,
    WindowName,
};
use temp_workspace::TempWorkspace;

const PROBE5_TRUST_PANE: &str = r#"> You are in /private/tmp/ta-subprep-SUB-PROBE-1-trust-startup.Ey2Xk6/teamdir

  Do you trust the contents of this directory? Working with untrusted contents
  comes with higher risk of prompt injection. Trusting the directory allows
  project-local config, hooks, and exec policies to load.

› 1. Yes, continue
  2. No, quit

  Press enter to continue
"#;

const SUBROOT_FULL_TRUST_PANE_AFTER_QUICKSTART: &str = r#"> You are in /private/tmp/ta-subroot-SUB-PROBE-1-trust-startup.uL9Boc/teamdir

  Do you trust the contents of this directory? Working with untrusted contents
  comes with higher risk of prompt injection. Trusting the directory allows
  project-local config, hooks, and exec policies to load.

› 1. Yes, continue
  2. No, quit

  Press enter to continue

╭─────────────────────────────────────────────────╮
│ ✨ Update available! 0.136.0 -> 0.137.0         │
│ Run npm install -g @openai/codex to update.     │
│                                                 │
│ See full release notes:                         │
│ https://github.com/openai/codex/releases/latest │
╰─────────────────────────────────────────────────╯

╭───────────────────────────────────────╮
│ >_ OpenAI Codex (v0.136.0)            │
│                                       │
│ model:     loading   /model to change │
│ directory: /private/tmp/…/teamdir     │
╰───────────────────────────────────────╯

› Find and fix a bug in @filename

  gpt-5.5 default · /private/tmp/ta-subroot-SUB-PROBE-1-trust-startup.uL9Boc/te…

"#;

fn tmp_dir(tag: &str) -> TempWorkspace {
    TempWorkspace::new("ta-rs-subprep", tag)
}

#[test]
fn temp_workspace_guard_cleans_after_panic_unwind() {
    if TempWorkspace::keep_enabled() {
        return;
    }

    let mut created = None;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let ws = tmp_dir("guard-panic");
        std::fs::write(ws.join("marker"), "created").unwrap();
        created = Some(ws.to_path_buf());
        panic!("intentional panic to prove TempWorkspace Drop cleanup");
    }));

    assert!(
        result.is_err(),
        "fixture sanity: panic branch must actually unwind for cleanup verification"
    );
    let created = created.expect("fixture sanity: panic branch should record temp path");
    assert!(
        !created.exists(),
        "TempWorkspace must remove its root during panic unwinding unless TEAM_AGENT_KEEP_TEST_TMP=1; leaked={}",
        created.display()
    );
}

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn truncated_workspace_token(canonical_teamdir: &Path) -> String {
    let tail = canonical_teamdir
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("teamdir");
    let head = canonical_teamdir
        .parent()
        .and_then(Path::parent)
        .unwrap_or_else(|| canonical_teamdir.parent().unwrap_or(canonical_teamdir));
    let mut head = head.to_string_lossy().to_string();
    if !head.ends_with(std::path::MAIN_SEPARATOR) {
        head.push(std::path::MAIN_SEPARATOR);
    }
    format!("{head}…/{tail}")
}

fn foreign_workspace_path(canonical_teamdir: &Path) -> PathBuf {
    canonical_teamdir
        .parent()
        .and_then(Path::parent)
        .unwrap_or_else(|| canonical_teamdir.parent().unwrap_or(canonical_teamdir))
        .join("foreign-team")
        .join(
            canonical_teamdir
                .file_name()
                .unwrap_or_else(|| std::ffi::OsStr::new("teamdir")),
        )
}

#[test]
fn contract_a_subroot_full_pane_trust_menu_still_actionable_despite_bottom_ready_prompt() {
    use team_agent::provider::{classify_codex_startup_screen, StartupScreenDecision};

    let decision = classify_codex_startup_screen(SUBROOT_FULL_TRUST_PANE_AFTER_QUICKSTART);

    assert_eq!(
        decision,
        StartupScreenDecision::AnswerWorkspaceTrust,
        "Contract A real-machine fixture: the full Codex pane keeps an actionable trust menu even when an update box, Codex banner, and bottom `› Find...` input prompt are visible in the same capture"
    );
}

#[test]
fn contract_a_own_workspace_trust_is_actionable_not_ready_and_foreign_is_pending() {
    use team_agent::provider::{classify_codex_startup_screen, StartupScreenDecision};

    assert_eq!(
        classify_codex_startup_screen(PROBE5_TRUST_PANE),
        StartupScreenDecision::AnswerWorkspaceTrust,
        "Contract A/N15: real Codex trust menu with `› 1. Yes, continue` is an actionable trust prompt, not Ready from a bare single-glyph marker"
    );

    let root = tmp_dir("a-truncated-root");
    let teamdir = root.join("teamdir");
    std::fs::create_dir_all(&teamdir).unwrap();
    let canonical_teamdir = std::fs::canonicalize(&teamdir).unwrap();
    let own_tail = format!(
        "Do you trust the contents of this directory?\n  directory: {}",
        truncated_workspace_token(&canonical_teamdir)
    );
    let own_transport = RecordingTransport::new(vec![own_tail.to_string()]);
    let own_log = EventLog::new(&root);
    let own = attempt_trust_auto_answer(
        &canonical_teamdir,
        &own_transport,
        Some(&PaneId::new("%7")),
        &own_tail,
        &PaneWidthQuery::Ok { pane_width: 80 },
        &own_log,
    )
    .unwrap();
    assert!(
        own.answered,
        "Contract A/#167: own-workspace matcher must accept platform-local Codex truncated directory display via the shared realpath/truncation matcher; canonical_teamdir={} tail={own_tail:?} got {own:?}",
        canonical_teamdir.display()
    );
    assert_eq!(
        own_transport.inject_count(),
        1,
        "Contract A: own trust answer is exactly one local Enter/inject action"
    );

    let foreign_transport = RecordingTransport::new(vec![PROBE5_TRUST_PANE.to_string()]);
    let foreign = attempt_trust_auto_answer(
        &canonical_teamdir,
        &foreign_transport,
        Some(&PaneId::new("%8")),
        &format!(
            "Do you trust the contents of this directory?\n> You are in {}",
            foreign_workspace_path(&canonical_teamdir).display()
        ),
        &PaneWidthQuery::Ok { pane_width: 120 },
        &own_log,
    )
    .unwrap();
    assert!(
        !foreign.answered && foreign.reason == "workspace_dir_mismatch",
        "Contract A: foreign workspace trust prompt stays pending and is never auto-answered; got {foreign:?}"
    );
    assert_eq!(
        foreign_transport.inject_count(),
        0,
        "Contract A: foreign trust prompt must not receive Enter"
    );
}

#[test]
fn contract_a_runtime_readiness_is_false_while_actionable_trust_remains() {
    use team_agent::provider::{classify_copilot_startup_screen, StartupScreenDecision};

    assert_eq!(
        classify_copilot_startup_screen(COPILOT_TRUST_PROMPT_MARKER),
        StartupScreenDecision::AnswerWorkspaceTrust,
        "fixture sanity: the Copilot folder-trust pane is an actionable own-workspace trust prompt"
    );

    let ws = tmp_dir("a-runtime-readiness-trust");
    let mcp_config = ws.join(".team").join("runtime").join("mcp").join("w1.json");
    std::fs::create_dir_all(mcp_config.parent().unwrap()).unwrap();
    std::fs::write(&mcp_config, "{}").unwrap();
    // Keep every other readiness gate satisfied; an unattached leader would
    // make ready=false independently of the trust prompt under test.
    let mut state = json!({
        "session_name": "team-subprep",
        "tmux_session_present": true,
        "leader_receiver": {"status": "attached", "pane_id": "%9"},
        "agents": {
            "w1": {
                "provider": "copilot",
                "status": "awaiting_trust_prompt",
                "startup_prompt_status": "awaiting_trust_prompt",
                "startup_prompts": "awaiting_trust_prompt",
                "pane_id": "%7",
                "window": "w1",
                "mcp_config": mcp_config.to_string_lossy().to_string(),
                "task_prompt_delivered": true
            }
        }
    });

    let value = crate::cli::diagnose::wait_readiness(&state);
    assert_eq!(
        value["awaiting_trust_prompt"],
        json!(true),
        "Contract A: ready logic must surface awaiting_trust_prompt=true; got {value}"
    );
    assert_eq!(
        value["ready"],
        json!(false),
        "Contract A: ready must stay false until the actionable trust prompt is cleared; got {value}"
    );
    state["agents"]["w1"]["status"] = json!("running");
    state["agents"]["w1"]["startup_prompt_status"] = json!("complete");
    state["agents"]["w1"]["startup_prompts"] = json!("complete");
    let cleared = crate::cli::diagnose::wait_readiness(&state);
    assert_eq!(cleared["awaiting_trust_prompt"], json!(false));
    assert_eq!(cleared["ready"], json!(true), "positive control: {cleared}");
}




#[test]
fn contract_c_lifecycle_launch_command_carries_role_tools_and_real_mcp_context() {
    let root = tmp_dir("c-launch-context");
    let teamdir = root.join("teamdir");
    std::fs::create_dir_all(teamdir.join("agents")).unwrap();
    std::fs::write(
        teamdir.join("TEAM.md"),
        "---\nname: subprep\nobjective: Verify worker can report results.\nprovider: codex\n---\n\nTeam.\n",
    )
    .unwrap();
    std::fs::write(
        teamdir.join("agents").join("w1.md"),
        "---\nname: w1\nrole: Worker must call report_result exactly once.\nprovider: copilot\nmodel: gpt-5\nauth_mode: subscription\ndangerously_skip_permissions: false\ntools:\n  - mcp_team\n---\n\nAlways use report_result for completion.\n",
    )
    .unwrap();
    let spec = team_agent::compiler::compile_team(&teamdir).unwrap();
    let spec_path = teamdir.join("team.spec.yaml");
    std::fs::write(&spec_path, team_agent::model::yaml::dumps(&spec)).unwrap();

    let transport = RecordingTransport::new(vec![COPILOT_READY_MARKER.to_string()]);
    let report = launch_with_transport(&spec_path, false, true, true, &transport)
        .expect("launch should reach transport spawn");
    assert_eq!(
        report.started.len(),
        1,
        "fixture sanity: one Copilot worker starts"
    );
    let spawn_argv = transport.spawn_argv();
    let argv = spawn_argv.join(" ");
    let arg_after = |flag: &str| {
        spawn_argv
            .windows(2)
            .find(|pair| pair[0] == flag)
            .map(|pair| pair[1].clone())
            .unwrap_or_else(|| panic!("copilot worker argv must carry {flag}; argv={argv:?}"))
    };
    // Copilot takes role instructions via COPILOT_CUSTOM_INSTRUCTIONS_DIRS
    // (`<workspace>/.team/runtime/copilot-instructions/<agent>/AGENTS.md`), not argv.
    let workspace = PathBuf::from(arg_after("-C"));
    let instructions = std::fs::read_to_string(
        workspace
            .join(".team/runtime/copilot-instructions/w1/AGENTS.md"),
    )
    .unwrap_or_default();
    let mcp_file = arg_after("--additional-mcp-config");
    let mcp_text = mcp_file
        .strip_prefix('@')
        .and_then(|path| std::fs::read_to_string(path).ok())
        .unwrap_or_default();

    assert!(
        instructions.contains("report_result"),
        "Contract C/F6.4: lifecycle must pass resolved role/developer instructions into the Copilot worker instructions; workspace={} instructions={instructions:?} argv={argv:?}",
        workspace.display()
    );
    assert!(
        mcp_text.contains("team_orchestrator"),
        "Contract C/MUST-8: Copilot worker command must include a real Team Agent tool/MCP capability mechanism, not prompt-only state metadata; mcp_config={mcp_file:?} text={mcp_text:?} argv={argv:?}"
    );
    for text in [&argv, &mcp_text] {
        assert!(
            !text.contains("/tmp/ta-provider-ws")
                && !text.contains("worker1")
                && !text.contains("teamA"),
            "Contract C: provider command/config must not leak placeholder MCP context; text={text:?}"
        );
    }
}

#[test]
fn contract_c_lifecycle_sources_use_context_aware_provider_builders() {
    let launch = composite_source::composite_source("src/lifecycle/launch.rs");
    let restart_common =
        std::fs::read_to_string(manifest_dir().join("src/lifecycle/restart/common.rs")).unwrap();
    let restart_agent =
        std::fs::read_to_string(manifest_dir().join("src/lifecycle/restart/agent.rs")).unwrap();

    let offenders = [
        ("launch.rs", &launch),
        ("restart/common.rs", &restart_common),
        ("restart/agent.rs", &restart_agent),
    ]
    .into_iter()
    .filter_map(|(name, src)| {
        src.contains(".build_command(auth_mode, None, None, model)")
            .then_some(name)
    })
    .collect::<Vec<_>>();

    assert!(
        offenders.is_empty(),
        "Contract C: lifecycle launch/restart/start-agent must use build_command_with_tools/build_resume_command_with_context with resolved system_prompt, tools, and per-worker MCP config; context-free callsites remain: {offenders:?}"
    );
}

#[test]
fn contract_c_provider_mcp_config_is_parameterized_not_placeholder() {
    let adapter = get_adapter(Provider::Copilot);
    let config = adapter.mcp_config(AuthMode::Subscription).unwrap();
    let raw = config.raw.to_string();
    let command = config.raw["team_orchestrator"]["command"]
        .as_str()
        .expect("Contract C step3: team_orchestrator MCP config must expose a command string");

    assert!(
        !raw.contains("/tmp/ta-provider-ws") && !raw.contains("worker1") && !raw.contains("teamA"),
        "Contract C: provider MCP config must be parameterized by real workspace/agent/team, never placeholder-shaped; raw={raw}"
    );
    assert!(
        raw.contains("TEAM_AGENT_WORKSPACE")
            || raw.contains("--workspace")
            || raw.contains("team-agent"),
        "Contract C: provider MCP config must point at a real Team Agent MCP server mechanism; raw={raw}"
    );
    assert_ne!(
        command, "team-agent",
        "Contract C step3/MCP startup: provider config must not use bare `team-agent`, because PATH can resolve to an older installed Python CLI; raw={raw}"
    );
    assert!(
        Path::new(command).is_absolute(),
        "Contract C step3/MCP startup: provider config command must be the current team-agent binary as an absolute path (`std::env::current_exe` semantics), not PATH-dependent; command={command:?} raw={raw}"
    );
}

fn message_status(db_path: &Path, message_id: &str) -> String {
    let conn = team_agent::db::schema::open_db(db_path).unwrap();
    conn.query_row(
        "select status from messages where message_id = ?1",
        params![message_id],
        |row| row.get::<_, String>(0),
    )
    .unwrap()
}

fn message_delivery_attempts(db_path: &Path, message_id: &str) -> i64 {
    let conn = team_agent::db::schema::open_db(db_path).unwrap();
    conn.query_row(
        "select delivery_attempts from messages where message_id = ?1",
        params![message_id],
        |row| row.get::<_, i64>(0),
    )
    .unwrap()
}

fn read_events(workspace: &Path) -> String {
    std::fs::read_to_string(workspace.join(".team").join("logs").join("events.jsonl"))
        .unwrap_or_default()
}

fn count_event(events: &str, event_name: &str) -> usize {
    events
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|event| event["event"] == event_name)
        .count()
}

#[derive(Debug, Default)]
struct TransportState {
    captures: Vec<String>,
    capture_idx: usize,
    inject_count: usize,
    session_present: bool,
    windows: Vec<WindowName>,
    spawn_argv: Vec<Vec<String>>,
    carrier: Option<u32>,
}

#[derive(Debug, Clone)]
struct RecordingTransport {
    state: std::sync::Arc<Mutex<TransportState>>,
}

impl RecordingTransport {
    fn new(captures: Vec<String>) -> Self {
        Self {
            state: std::sync::Arc::new(Mutex::new(TransportState {
                captures,
                ..TransportState::default()
            })),
        }
    }

    fn with_session_present(self, present: bool) -> Self {
        self.state.lock().unwrap().session_present = present;
        self
    }

    fn with_windows(self, windows: Vec<WindowName>) -> Self {
        self.state.lock().unwrap().windows = windows;
        self
    }

    fn inject_count(&self) -> usize {
        self.state.lock().unwrap().inject_count
    }

    fn spawn_argv(&self) -> Vec<String> {
        self.state
            .lock()
            .unwrap()
            .spawn_argv
            .first()
            .cloned()
            .unwrap_or_default()
    }

    fn spawn_result(
        &self,
        session: &SessionName,
        window: &WindowName,
        argv: &[String],
        pane_prefix: &str,
    ) -> SpawnResult {
        let mut state = self.state.lock().unwrap();
        state.spawn_argv.push(argv.to_vec());
        SpawnResult {
            pane_id: PaneId::new(format!("%{}{}", pane_prefix, state.spawn_argv.len())),
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
        _env: &BTreeMap<String, String>,
    ) -> Result<SpawnResult, TransportError> {
        Ok(self.spawn_result(session, window, argv, "first"))
    }

    fn spawn_into(
        &self,
        session: &SessionName,
        window: &WindowName,
        argv: &[String],
        _cwd: &Path,
        _env: &BTreeMap<String, String>,
    ) -> Result<SpawnResult, TransportError> {
        Ok(self.spawn_result(session, window, argv, "into"))
    }

    fn inject(
        &self,
        _target: &Target,
        _payload: &InjectPayload,
        _submit: Key,
        _bracketed: bool,
    ) -> Result<InjectReport, TransportError> {
        self.state.lock().unwrap().inject_count += 1;
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
        let mut state = self.state.lock().unwrap();
        let text = state
            .captures
            .get(state.capture_idx)
            .or_else(|| state.captures.last())
            .cloned()
            .unwrap_or_default();
        state.capture_idx = state.capture_idx.saturating_add(1);
        Ok(CapturedText { text, range })
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
        Ok(self.state.lock().unwrap().carrier.map(|carrier| PaneInfo {
            pane_id: PaneId::new("%7"),
            session: SessionName::new("team-ta-subroot-SUB-PROBE-2-send-turn-idle"),
            window_index: None,
            window_name: Some(WindowName::new("w1")),
            pane_index: None,
            tty: None,
            current_command: None,
            current_path: None,
            active: true,
            pane_pid: Some(carrier),
            leader_env: BTreeMap::new(),
        }).into_iter().collect())
    }

    fn has_session(&self, _session: &SessionName) -> Result<bool, TransportError> {
        Ok(self.state.lock().unwrap().session_present)
    }

    fn list_windows(&self, _session: &SessionName) -> Result<Vec<WindowName>, TransportError> {
        Ok(self.state.lock().unwrap().windows.clone())
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
