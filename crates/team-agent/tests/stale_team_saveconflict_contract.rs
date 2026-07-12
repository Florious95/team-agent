//! 0.5.26 RED contract: stale team lifecycle must not poison live-team saves.
//!
//! References:
//! - `.team/artifacts/stale-team-saveconflict-locate.md` §8 RED1-RED6.
//!
//! User story: after a team is shut down with logs kept, its retained state is
//! visible as down/diagnostic history, but it is not an alive candidate and its
//! stopped agents' old pane fields cannot block writes to another live team.

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

#[path = "support/hermetic.rs"]
mod hermetic_guard;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::{json, Value};
use serial_test::serial;
use team_agent::lifecycle::add_agent_with_transport;
use team_agent::model::ids::AgentId;
use team_agent::model::paths::runtime_spec_path;
use team_agent::state::paths::CommandScope;
use team_agent::state::persist::{load_runtime_state, save_runtime_state};
use team_agent::state::projection::{
    project_top_level_view, save_team_scoped_state, team_state_candidates,
};
use team_agent::transport::{
    AttachOutcome, BackendKind, CaptureRange, CapturedText, InjectPayload, InjectReport,
    InjectStage, InjectVerification, Key, PaneField, PaneId, PaneInfo, PaneLiveness, SessionName,
    SetEnvOutcome, SpawnResult, SubmitVerification, Target, Transport, TransportError,
    TurnVerification, WindowName,
};

const CURRENT: &str = "current";
const RESEARCH: &str = "research";
const CURRENT_SESSION: &str = "team-supermarket-suite";
const RESEARCH_SESSION: &str = "team-supermarket-research";

#[test]
fn explicit_shutdown_status_is_non_alive_selector_guard() {
    let state = json!({"teams": {
        CURRENT: {"status": "shutdown", "session_name": CURRENT_SESSION, "agents": {"adminweb": agent("adminweb", "stopped", true)}},
        RESEARCH: {"status": "alive", "session_name": RESEARCH_SESSION, "agents": {"researcher": agent("researcher", "running", true)}}
    }});
    assert_eq!(
        keys(&team_state_candidates(&state)),
        vec![RESEARCH],
        "RED1 guard: explicit shutdown team status must be non-alive; state={state}"
    );
    // 0.5.27 locate showed the former source-text scan here was a false green:
    // it never executed scoped shutdown nor reloaded disk state. The behavioral
    // shutdown roundtrip contract lives in `src/cli/tests/shutdown_kill_plan.rs`.
}

#[test]
#[serial(env)]
fn legacy_all_stopped_team_is_not_alive_but_empty_bootstrap_remains_alive() {
    let bootstrap = json!({"teams": {
        "bootstrap": {"session_name": "legacy-bootstrap"},
        RESEARCH: {"status": "alive", "session_name": RESEARCH_SESSION}
    }});
    assert!(
        team_state_candidates(&bootstrap).contains_key("bootstrap"),
        "RED2 compatibility guard: missing status remains alive for an empty legacy/bootstrap team"
    );

    let case = Case::new("legacy-stopped-selector");
    case.write_state(case.root_with_current(Value::Null, "stopped", true));
    let alive = team_state_candidates(&load_runtime_state(&case.workspace).unwrap());
    assert_eq!(
        keys(&alive),
        vec![RESEARCH],
        "RED2: status:null plus all agents stopped is a legacy shutdown residue and must not remain an alive candidate; alive={alive:?}"
    );
    assert!(
        matches!(CommandScope::resolve(&case.workspace, None), CommandScope::Resolved(team) if team == RESEARCH),
        "RED2: bare command scope must resolve to the only live research team, not refuse as ambiguous because current is a stopped residue"
    );
}

#[test]
#[serial(env)]
fn dead_sibling_stale_topology_does_not_block_live_team_save_but_running_still_conflicts() {
    let running = Case::new("running-sibling-conflict");
    running.write_state(running.root_with_current(json!("alive"), "running", true));
    let err = save_team_scoped_state(
        &running.workspace,
        &running.incoming_research_without_current_topology(),
    )
    .expect_err("RED3 guard: running sibling with mismatched topology must still SaveConflict");
    assert!(
        err.to_string().contains("SaveConflict") || err.to_string().contains("save conflict"),
        "RED3 guard: live/running topology conflicts must remain protected; err={err}"
    );

    let stopped = Case::new("stopped-sibling-no-conflict");
    stopped.write_state(stopped.root_with_current(Value::Null, "stopped", true));
    save_team_scoped_state(
        &stopped.workspace,
        &stopped.incoming_research_without_current_topology(),
    )
    .expect("RED3: stopped/dead sibling stale topology must not block a write to live research");
    let saved = load_runtime_state(&stopped.workspace).unwrap();
    assert_eq!(
        saved.pointer("/teams/current/agents/adminweb/status").and_then(Value::as_str),
        Some("stopped"),
        "RED3: dead sibling team must remain as stopped/down diagnostic history after live-team save; saved={saved}"
    );
    assert!(
        saved.pointer("/teams/research/agents/standards").is_some(),
        "RED3: live research update must persist; saved={saved}"
    );
}

#[test]
#[serial(env)]
fn add_agent_downstream_failure_rolls_back_spec_and_state_so_retry_is_clean() {
    let case = Case::new("add-agent-rollback");
    case.write_team_docs();
    case.write_state(case.root_with_current(Value::Null, "stopped", true));
    let role = case.write_role_doc("standards");
    let transport = FailingSpawnTransport::new();

    let first = add_agent_with_transport(
        &case.workspace,
        &AgentId::new("standards"),
        &role,
        false,
        Some(RESEARCH),
        &transport,
    );
    assert!(
        first.is_err(),
        "RED4 setup: fake transport must fail downstream start; first={first:?}"
    );
    assert!(
        transport.spawn_attempts() > 0,
        "RED4 setup: failure must happen after add-agent reaches downstream start/spawn, not before runtime upsert"
    );
    assert_no_standards(&case);

    let second = add_agent_with_transport(
        &case.workspace,
        &AgentId::new("standards"),
        &role,
        false,
        Some(RESEARCH),
        &transport,
    );
    let second_text = format!("{second:?}");
    assert!(
        !second_text.contains("agent id already exists"),
        "RED4: rollback must remove half-registered state/spec rows so retry is not blocked as already-exists; first={first:?} second={second:?}"
    );
}

#[test]
#[serial(env)]
fn purge_agent_help_and_dispatch_are_consistent() {
    let case = Case::new("purge-agent");
    let help = output_text(&case.run(["--help"]));
    let command =
        output_text(&case.run(["purge-agent", "ghost", "--workspace", case.ws(), "--json"]));
    if help.contains("purge-agent") {
        assert!(
            !command.contains("invalid choice") && !command.contains("unknown subcommand") && !command.contains("Commands:"),
            "RED6: help exposes purge-agent, so dispatch must be real or the command must be removed from help; help={help} command={command}"
        );
    } else {
        assert!(
            command.contains("unknown subcommand") || command.contains("invalid choice"),
            "RED6: if purge-agent is not implemented, help and dispatch must agree it is absent; help={help} command={command}"
        );
    }
}

struct Case {
    env: hermetic_guard::HermeticTestEnv,
    workspace: PathBuf,
    workspace_s: String,
}

impl Case {
    fn new(tag: &str) -> Self {
        let env = hermetic_guard::HermeticTestEnv::enter(tag);
        let workspace = env.workspace(tag);
        let workspace_s = workspace.to_string_lossy().to_string();
        Self {
            env,
            workspace,
            workspace_s,
        }
    }

    fn ws(&self) -> &str {
        &self.workspace_s
    }

    fn write_state(&self, state: Value) {
        save_runtime_state(&self.workspace, &state).unwrap();
    }

    fn root_with_current(
        &self,
        current_status: Value,
        current_agent_status: &str,
        current_topology: bool,
    ) -> Value {
        let research_agent = agent("researcher", "running", true);
        let current = json!({
            "team_key": CURRENT,
            "session_name": CURRENT_SESSION,
            "team_dir": self.workspace_s,
            "status": current_status,
            "agents": {"adminweb": agent("adminweb", current_agent_status, current_topology)}
        });
        let research = json!({
            "team_key": RESEARCH,
            "session_name": RESEARCH_SESSION,
            "team_dir": self.workspace_s,
            "status": "alive",
            "agents": {"researcher": research_agent.clone()},
            "tasks": [{"id": "T", "assignee": "researcher", "status": "pending"}]
        });
        json!({
            "schema_version": 1,
            "active_team_key": RESEARCH,
            "team_key": RESEARCH,
            "session_name": RESEARCH_SESSION,
            "team_dir": self.workspace_s,
            "status": "alive",
            "agents": {"researcher": research_agent},
            "tasks": [{"id": "T", "assignee": "researcher", "status": "pending"}],
            "teams": {CURRENT: current, RESEARCH: research}
        })
    }

    fn incoming_research_without_current_topology(&self) -> Value {
        let mut state =
            project_top_level_view(&load_runtime_state(&self.workspace).unwrap(), RESEARCH);
        state["agents"]["standards"] = agent("standards", "starting", false);
        state["teams"][RESEARCH]["agents"]["standards"] = agent("standards", "starting", false);
        state["teams"][CURRENT]["agents"]["adminweb"] = agent("adminweb", "stopped", false);
        state
    }

    fn write_team_docs(&self) {
        std::fs::create_dir_all(self.workspace.join("agents")).unwrap();
        std::fs::write(
            self.workspace.join("TEAM.md"),
            "---\nname: research\nobjective: Supermarket research.\nprovider: fake\n---\n\nResearch team.\n",
        )
        .unwrap();
        std::fs::write(
            self.workspace.join("agents/researcher.md"),
            role_doc("researcher"),
        )
        .unwrap();
    }

    fn write_role_doc(&self, id: &str) -> PathBuf {
        let path = self.workspace.join(format!("{id}.md"));
        std::fs::write(&path, role_doc(id)).unwrap();
        path
    }

    fn write_runtime_spec(&self) {
        let spec = team_agent::compiler::compile_team(&self.workspace).unwrap();
        let path = runtime_spec_path(&self.workspace, RESEARCH);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, team_agent::model::yaml::dumps(&spec)).unwrap();
    }

    fn run<const N: usize>(&self, args: [&str; N]) -> std::process::Output {
        self.env.run_cli(&self.workspace, &args)
    }
}

fn agent(id: &str, status: &str, topology: bool) -> Value {
    let mut agent = json!({"agent_id": id, "id": id, "provider": "fake", "auth_mode": "subscription", "status": status});
    if topology {
        agent["window"] = json!(id);
        agent["pane_id"] = json!("%1003");
        agent["pane_pid"] = json!(59756);
        agent["spawned_at"] = json!("2026-07-10T16:43:35.802946+00:00");
        agent["spawn_epoch"] = json!(1);
    }
    agent
}

fn role_doc(id: &str) -> String {
    format!("---\nname: {id}\nrole: {id}\nprovider: fake\nmodel: fake\nauth_mode: subscription\ntools:\n  - mcp_team\n---\n\n{id}.\n")
}

fn assert_no_standards(case: &Case) {
    let state = load_runtime_state(&case.workspace).unwrap();
    assert!(
        state.pointer("/agents/standards").is_none()
            && state.pointer("/teams/research/agents/standards").is_none(),
        "RED4: failed add-agent rollback must remove standards from runtime state; state={state}"
    );
    let spec = runtime_spec_path(&case.workspace, RESEARCH);
    if let Ok(text) = std::fs::read_to_string(&spec) {
        assert!(
            !text.contains("standards"),
            "RED4: failed add-agent rollback must restore spec without standards; spec={text}"
        );
    }
}

struct FailingSpawnTransport {
    attempts: AtomicUsize,
}

impl FailingSpawnTransport {
    fn new() -> Self {
        Self {
            attempts: AtomicUsize::new(0),
        }
    }
    fn spawn_attempts(&self) -> usize {
        self.attempts.load(Ordering::SeqCst)
    }
    fn fail_spawn(&self) -> Result<SpawnResult, TransportError> {
        self.attempts.fetch_add(1, Ordering::SeqCst);
        Err(TransportError::Spawn {
            backend: BackendKind::Tmux,
            source: std::io::Error::other("forced add-agent downstream start failure"),
        })
    }
}

impl Transport for FailingSpawnTransport {
    fn kind(&self) -> BackendKind {
        BackendKind::Tmux
    }
    fn spawn_first(
        &self,
        _: &SessionName,
        _: &WindowName,
        _: &[String],
        _: &Path,
        _: &BTreeMap<String, String>,
    ) -> Result<SpawnResult, TransportError> {
        self.fail_spawn()
    }
    fn spawn_into(
        &self,
        _: &SessionName,
        _: &WindowName,
        _: &[String],
        _: &Path,
        _: &BTreeMap<String, String>,
    ) -> Result<SpawnResult, TransportError> {
        self.fail_spawn()
    }
    fn inject(
        &self,
        _: &Target,
        _: &InjectPayload,
        _: Key,
        _: bool,
    ) -> Result<InjectReport, TransportError> {
        Ok(InjectReport {
            stage_reached: InjectStage::Submit,
            inject_verification: InjectVerification::EmptyTextSendKeys,
            submit_verification: SubmitVerification::EnterSentWithoutPlaceholderCheck,
            turn_verification: TurnVerification::NotRequired,
            attempts: 1,
            submit_diagnostics: None,
        })
    }
    fn send_keys(&self, _: &Target, _: &[Key]) -> Result<(), TransportError> {
        Ok(())
    }
    fn capture(&self, _: &Target, range: CaptureRange) -> Result<CapturedText, TransportError> {
        Ok(CapturedText {
            text: String::new(),
            range,
        })
    }
    fn query(&self, _: &Target, _: PaneField) -> Result<Option<String>, TransportError> {
        Ok(None)
    }
    fn liveness(&self, _: &PaneId) -> Result<PaneLiveness, TransportError> {
        Ok(PaneLiveness::Live)
    }
    fn list_targets(&self) -> Result<Vec<PaneInfo>, TransportError> {
        Ok(Vec::new())
    }
    fn has_session(&self, _: &SessionName) -> Result<bool, TransportError> {
        Ok(true)
    }
    fn list_windows(&self, _: &SessionName) -> Result<Vec<WindowName>, TransportError> {
        Ok(vec![WindowName::new("researcher")])
    }
    fn set_session_env(
        &self,
        _: &SessionName,
        _: &str,
        _: &str,
    ) -> Result<SetEnvOutcome, TransportError> {
        Ok(SetEnvOutcome::Applied)
    }
    fn kill_session(&self, _: &SessionName) -> Result<(), TransportError> {
        Ok(())
    }
    fn kill_window(&self, _: &Target) -> Result<(), TransportError> {
        Ok(())
    }
    fn attach_session(&self, _: &SessionName) -> Result<AttachOutcome, TransportError> {
        Ok(AttachOutcome::Attached)
    }
}

fn keys(map: &serde_json::Map<String, Value>) -> Vec<&str> {
    map.keys().map(String::as_str).collect()
}

fn output_text(output: &std::process::Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}
