//! RS 0.3.1 dogfood BUG-2/BUG-4 window consistency contracts.
//!
//! Real-machine evidence:
//! - state/status reported `clauder` running, but tmux only listed the `codexer` window.
//! - worker->leader delivery queued, then the coordinator pasted to `team-verify031:leader`,
//!   but that window did not exist; state had a bound `leader_receiver.pane_id`.
//!
//! These are command-substitute contracts: no real provider or subscription is used, but the
//! observable invariant is the same as the Mac mini run.

#![allow(clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde_json::json;
use serial_test::{file_serial, serial};
use team_agent::event_log::EventLog;
use team_agent::lifecycle::{launch_with_transport, quick_start_with_transport, QuickStartReport};
use team_agent::messaging::{ MessageTarget, TrustedSender };
use team_agent::model::ids::AgentId;
use team_agent::state::persist::{load_runtime_state, save_runtime_state};
use team_agent::transport::{ BackendKind, CaptureRange, CapturedText, InjectStage, Key, PaneField, PaneId, PaneInfo, PaneLiveness, SessionName, SpawnResult, Target, Transport, TransportError, WindowName };

#[path = "../../../tests/support/hermetic.rs"]
mod hermetic;
use hermetic::HermeticTestEnv;

const TEAM_MD: &str =
    "---\nname: verify031\nobjective: Verify mixed provider windows.\nprovider: codex\n---\n\nTeam.\n";

const FAKE_ROLE: &str = "---\nname: faker\nrole: Fake Worker\nprovider: fake\nmodel: fake\nauth_mode: subscription\ndangerously_skip_permissions: false\ntools:\n  - mcp_team\n---\n\nFake worker.\n";

const CLAUDE_ROLE: &str = "---\nname: clauder\nrole: Claude Worker\nprovider: claude\nmodel: claude-sonnet-4-6\nauth_mode: subscription\ndangerously_skip_permissions: false\ntools:\n  - mcp_team\n---\n\nClaude worker.\n";

#[test]
#[serial(env)]
fn worker_mcp_owner_team_scope_must_match_runtime_team_key_not_spec_name() {
    let hermetic = HermeticTestEnv::enter_with_leader("rs031-mcp-owner-scope");
    let team = mixed_provider_team_dir(&hermetic, "mcp-owner-scope");
    super::launch_spawn::seed_healthy_coordinator(team.parent().unwrap());
    let transport = RecordingTransport::new()
        .with_windows(vec![WindowName::new("clauder"), WindowName::new("faker")]);
    let launch = ready_launch(
        quick_start_with_transport(&team, None, true, None, &transport)
            .expect("quick-start should reach worker spawn path"),
    );
    assert_eq!(
        launch.started.len(),
        2,
        "fixture precondition: both workers spawn"
    );

    let workspace = team.parent().expect("teamdir has parent workspace");
    let state = load_runtime_state(workspace).expect("runtime state written by launch");
    let runtime_team_key = team_agent::state::projection::team_state_key(&state);
    // quickstart-team-key-divergence fix: quick-start now unifies the runtime
    // key on the compiled `TEAM.md name:` (`verify031`), no longer the team
    // directory basename. The invariant below (worker MCP owner scope must
    // equal the runtime key, whatever it is) is unchanged.
    assert_eq!(
        runtime_team_key, "verify031",
        "fixture precondition: runtime state key is the compiled team identity"
    );

    let bad_scopes = transport
        .spawn_envs()
        .into_iter()
        .filter_map(|env| env.get("TEAM_AGENT_OWNER_TEAM_ID").cloned())
        .filter(|scope| scope.as_str() != runtime_team_key)
        .collect::<Vec<_>>();
    let bad_mcp_config_scopes = transport
        .spawn_argvs()
        .into_iter()
        .filter(|argv| {
            argv.iter().any(|arg| {
                arg.contains("TEAM_AGENT_OWNER_TEAM_ID")
                    && !arg.contains(&format!("TEAM_AGENT_OWNER_TEAM_ID=\"{runtime_team_key}\""))
                    && !arg.contains(&format!("TEAM_AGENT_OWNER_TEAM_ID={runtime_team_key}"))
            })
        })
        .collect::<Vec<_>>();
    assert!(
        bad_scopes.is_empty() && bad_mcp_config_scopes.is_empty(),
        "BUG-4 contract: worker MCP TEAM_AGENT_OWNER_TEAM_ID must match the runtime team key used by state. \
         Mac mini saw coordinator notifications under `teamdir` but worker MCP scope_resolved under the spec name, \
         so send_message(to=leader) looked in the wrong team and reported leader_not_attached. \
         expected_scope={runtime_team_key:?} bad_scopes={bad_scopes:?} bad_mcp_config_scopes={bad_mcp_config_scopes:?} state={state}"
    );
}

#[test]
#[serial(env)]
fn quick_start_preserves_external_leader_receiver_when_worker_pane_id_collides_across_tmux_sockets()
{
    let hermetic = HermeticTestEnv::enter("rs031-external-pane-collision");
    let caller_endpoint = "/tmp/default-tmux-socket";
    let _tmux = hermetic.with_env("TMUX", &format!("{caller_endpoint},123,0"));
    let _pane = hermetic.with_env("TMUX_PANE", "%0");
    let _provider = hermetic.with_env("TEAM_AGENT_LEADER_PROVIDER", "codex");
    let team = fake_only_team_dir(&hermetic, "external-pane-collision");
    let workspace = team.parent().expect("teamdir has parent workspace");
    // c28a675f / c8639440: distinct endpoints and an actual live external carrier.
    let carrier = hermetic::spawn_owned_coordinator(workspace);
    assert!(crate::lifecycle::carrier::birth_identity(carrier.id()).is_some());
    let mut caller = PaneInfo {
        pane_id: PaneId::new("%0"),
        session: SessionName::new("leader"),
        window_index: Some(0),
        window_name: Some(WindowName::new("leader")),
        pane_index: Some(0),
        tty: None,
        current_command: Some("codex".to_string()),
        current_path: Some(workspace.to_path_buf()),
        active: true,
        pane_pid: Some(carrier.id()),
        leader_env: Default::default(),
    };
    // R-RS031 / 37928560: both observations carry the same already-published live nonce.
    caller.leader_env.insert(
        crate::tmux_backend::PANE_BINDING_NONCE_METADATA_KEY.to_string(),
        "rs031-caller-nonce".to_string(),
    );
    let worker_endpoint = workspace.join("worker.sock").to_string_lossy().into_owned();
    let transport = RecordingTransport::new()
        .with_windows(vec![WindowName::new("faker")])
        .with_tmux_endpoint(worker_endpoint.clone())
        .with_targets(vec![caller.clone()]);
    let live = crate::transport::test_support::OfflineTransport::default()
        .with_tmux_endpoint(caller_endpoint)
        .with_targets(vec![caller]);
    let launch = ready_launch(
        crate::transport_factory::with_leader_endpoint_transport(caller_endpoint, live, || {
            quick_start_with_transport(&team, None, true, None, &transport)
        })
        .expect("quick-start should reach worker spawn path"),
    );
    assert_eq!(launch.tmux_endpoint.as_deref(), Some(worker_endpoint.as_str()));
    let worker_pane = launch
        .started
        .iter()
        .find(|agent| agent.agent_id.as_str() == "faker")
        .map(|agent| agent.target.as_str())
        .expect("fixture precondition: faker spawned");
    assert_eq!(
        worker_pane, "%0",
        "fixture precondition: first worker in the product -L socket receives bare pane id %0"
    );

    let state = load_runtime_state(workspace).expect("runtime state written by launch");
    let receiver = state.pointer("/teams/verify031/leader_receiver").expect("canonical receiver");
    assert_eq!(receiver["binding_nonce"], "rs031-caller-nonce");
    assert_eq!(receiver["tmux_socket"], caller_endpoint);
    assert_eq!(receiver["scope_authority"], "fresh_caller");
    assert_eq!(receiver["status"], "attached");
    assert_eq!(
        PathBuf::from(receiver["authorized_team_workspace"].as_str().expect("authorized workspace")).canonicalize().unwrap(),
        workspace.canonicalize().unwrap(),
    );
    assert_ne!(launch.tmux_endpoint.as_deref(), receiver["tmux_socket"].as_str());
    let top_receiver_pane = state
        .get("leader_receiver")
        .and_then(|receiver| receiver.get("pane_id"))
        .and_then(serde_json::Value::as_str);
    let receiver_pane = state
        .pointer("/teams/verify031/leader_receiver/pane_id")
        .and_then(serde_json::Value::as_str);
    let top_owner_pane = state
        .get("team_owner")
        .and_then(|owner| owner.get("pane_id"))
        .and_then(serde_json::Value::as_str);
    let owner_pane = state
        .pointer("/teams/verify031/team_owner/pane_id")
        .and_then(serde_json::Value::as_str);

    assert_eq!(
        receiver_pane,
        Some("%0"),
        "BUG-4 socket contract: quick-start may run from a real leader pane on the user's/default tmux socket \
         whose bare id is `%0`, while the first worker in the product per-team -L socket is also `%0`. \
         Different sockets are not a worker/leader collision: the launched leader_receiver must remain bound \
         to the caller pane instead of being cleared to unbound/leader_not_attached. \
         receiver_pane={receiver_pane:?} top_receiver_pane={top_receiver_pane:?} owner_pane={owner_pane:?} \
         top_owner_pane={top_owner_pane:?} \
         worker_pane={worker_pane:?} state={state}"
    );
    assert_eq!(
        owner_pane,
        Some("%0"),
        "BUG-4 socket contract: team_owner must not be replaced with __team_agent_unbound__ for a different-socket bare id collision; state={state}"
    );
    assert!(
        top_receiver_pane.is_none() && top_owner_pane.is_none(),
        "Stage 3d state saves strip legacy top-level owner copies; state={state}"
    );
}

#[test]
#[serial(env)]
fn running_worker_state_must_only_be_written_for_windows_that_exist_after_launch() {
    let hermetic = HermeticTestEnv::enter("rs031-state-window");
    let team = mixed_provider_team_dir(&hermetic, "state-window");
    let spec = team_agent::compiler::compile_team(&team).expect("compile mixed provider team");
    let spec_path = team.join("team.spec.yaml");
    std::fs::write(&spec_path, team_agent::model::yaml::dumps(&spec)).expect("write spec");

    // Simulates the Mac mini final capture: the fake worker's window exists, the claude window does not.
    // launch_with_transport currently never verifies the post-spawn window set before persisting
    // `status=running`, so this fixture pins the state/physical invariant directly.
    let transport = RecordingTransport::new().with_windows(vec![WindowName::new("faker")]);
    let launch = launch_with_transport(&spec_path, false, true, true, &transport)
        .expect("launch should reach the real spawn/state path");
    assert_eq!(
        launch.started.len(),
        2,
        "fixture precondition: both spawn calls returned"
    );

    let workspace = team.parent().expect("teamdir has parent workspace");
    let state = load_runtime_state(workspace).expect("runtime state written by launch");
    let live_windows = transport
        .list_windows(&launch.session_name)
        .expect("recording transport list_windows");

    let mut violations = Vec::new();
    for (agent_id, agent) in state
        .get("agents")
        .and_then(serde_json::Value::as_object)
        .expect("state agents object")
    {
        let status = agent.get("status").and_then(serde_json::Value::as_str);
        if status != Some("running") {
            continue;
        }
        let window = agent
            .get("window")
            .and_then(serde_json::Value::as_str)
            .unwrap_or(agent_id);
        if !live_windows.iter().any(|live| live.as_str() == window) {
            violations.push(format!(
                "{agent_id} status=running window={window} missing from tmux list"
            ));
        }
    }

    assert!(
        violations.is_empty(),
        "BUG-2 contract: state/status must not mark an agent running unless its tmux window exists. \
         Mac mini saw state clauder=running while product_windows only had codexer. violations={violations:?} state={state}"
    );
}




#[test]
fn send_message_entrypoint_has_no_pre_enqueue_leader_liveness_gate() {
    let src = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/station/send.rs"
    ))
    .expect("read current station send.rs");
    let body = src
        .split("pub fn execute(")
        .nth(1)
        .and_then(|tail| tail.split("fn send_one(").next())
        .expect("current station admission body");

    for forbidden in [
        "leader_pane_bound_but_not_live",
        "rebind_required_outcome(None)",
        "\"leader_receiver.delivery_blocked\"",
        "\"leader_not_attached\"",
    ] {
        assert!(
            !body.contains(forbidden),
            "BUG-4 true-entry contract: station::send::execute must not have a pre-enqueue \
             leader liveness gate ({forbidden}); it must create a message_id and let \
             the current durable delivery path decide endpoint/liveness/submission"
        );
    }
}

#[test]
fn send_to_leader_receiver_entrypoint_has_no_private_attached_precheck() {
    let src = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/station/send.rs"
    ))
    .expect("read current station send.rs");
    let body = src
        .split("fn send_one(")
        .nth(1)
        .and_then(|tail| tail.split("fn remember_task(").next())
        .expect("current durable delivery body");

    let stored = body.find("crate::inbox_store::store(").expect("durable append");
    let submission = body.find("Submission::lock(").expect("physical submission boundary");
    assert!(stored < submission, "durable append must precede any physical submission attempt");

    for forbidden in [
        "leader_pane_is_live",
        "pane_attached",
        "leader_receiver.delivery_blocked",
        "leader_not_attached",
    ] {
        assert!(
            !body.contains(forbidden),
            "BUG-4 single-authority contract: current durable delivery must not carry a private \
             leader-attached precheck ({forbidden}); endpoint/liveness/blocking belongs only to \
             the current delivery router"
        );
    }
}

fn mixed_provider_team_dir(hermetic: &HermeticTestEnv, tag: &str) -> PathBuf {
    let team = hermetic.workspace(tag).join("teamdir");
    std::fs::create_dir_all(team.join("agents")).expect("create agents dir");
    std::fs::write(team.join("TEAM.md"), TEAM_MD).expect("write TEAM.md");
    std::fs::write(team.join("agents").join("clauder.md"), CLAUDE_ROLE)
        .expect("write clauder role");
    std::fs::write(team.join("agents").join("faker.md"), FAKE_ROLE).expect("write faker role");
    team
}



fn pane_info(pane_id: &str, session: &str, window: &str) -> PaneInfo {
    PaneInfo {
        pane_id: PaneId::new(pane_id),
        session: SessionName::new(session),
        window_index: Some(0),
        window_name: Some(WindowName::new(window)),
        pane_index: Some(0),
        tty: None,
        current_command: Some("bash".to_string()),
        current_path: None,
        active: true,
        pane_pid: None,
        leader_env: BTreeMap::new(),
    }
}

#[derive(Clone, Default)]
struct RecordingTransport {
    inner: Arc<Mutex<RecordingState>>,
}

#[derive(Default)]
struct RecordingState {
    windows: Vec<WindowName>,
    spawns: usize,
    inject_targets: Vec<Target>,
    spawn_envs: Vec<BTreeMap<String, String>>,
    spawn_argvs: Vec<Vec<String>>,
    liveness: BTreeMap<String, PaneLiveness>,
    targets: Vec<PaneInfo>,
    tmux_endpoint: Option<String>,
}

impl RecordingTransport {
    fn new() -> Self {
        Self::default()
    }

    fn with_windows(self, windows: Vec<WindowName>) -> Self {
        self.with_state(|state| state.windows = windows);
        self
    }

    fn with_liveness(self, entries: Vec<(String, PaneLiveness)>) -> Self {
        self.with_state(|state| {
            state.liveness = entries.into_iter().collect();
        });
        self
    }

    fn with_targets(self, targets: Vec<PaneInfo>) -> Self {
        self.with_state(|state| state.targets = targets);
        self
    }

    fn with_tmux_endpoint(self, endpoint: impl Into<String>) -> Self {
        self.with_state(|state| state.tmux_endpoint = Some(endpoint.into()));
        self
    }

    fn inject_targets(&self) -> Vec<Target> {
        self.with_state(|state| state.inject_targets.clone())
    }

    fn spawn_envs(&self) -> Vec<BTreeMap<String, String>> {
        self.with_state(|state| state.spawn_envs.clone())
    }

    fn spawn_argvs(&self) -> Vec<Vec<String>> {
        self.with_state(|state| state.spawn_argvs.clone())
    }

    fn with_state<T>(&self, f: impl FnOnce(&mut RecordingState) -> T) -> T {
        let mut guard = self.inner.lock().expect("recording transport lock");
        f(&mut guard)
    }
}

impl Transport for RecordingTransport {
    fn kind(&self) -> BackendKind {
        BackendKind::Tmux
    }

    fn tmux_endpoint(&self) -> Option<String> {
        self.with_state(|state| state.tmux_endpoint.clone())
    }

    fn spawn_first(
        &self,
        session: &SessionName,
        window: &WindowName,
        argv: &[String],
        _cwd: &std::path::Path,
        env: &BTreeMap<String, String>,
    ) -> Result<SpawnResult, TransportError> {
        self.with_state(|state| {
            state.spawn_envs.push(env.clone());
            state.spawn_argvs.push(argv.to_vec());
        });
        Ok(self.spawn_result(session, window))
    }

    fn spawn_into(
        &self,
        session: &SessionName,
        window: &WindowName,
        argv: &[String],
        _cwd: &std::path::Path,
        env: &BTreeMap<String, String>,
    ) -> Result<SpawnResult, TransportError> {
        self.with_state(|state| {
            state.spawn_envs.push(env.clone());
            state.spawn_argvs.push(argv.to_vec());
        });
        Ok(self.spawn_result(session, window))
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
            text: "OpenAI Codex".to_string(),
            range,
        })
    }

    fn query(&self, _target: &Target, field: PaneField) -> Result<Option<String>, TransportError> {
        match field {
            PaneField::PaneCurrentCommand => Ok(Some("codex".to_string())),
            _ => Ok(None),
        }
    }

    fn liveness(&self, pane: &PaneId) -> Result<PaneLiveness, TransportError> {
        Ok(self
            .with_state(|state| state.liveness.get(pane.as_str()).copied())
            .unwrap_or(PaneLiveness::Unknown))
    }

    fn list_targets(&self) -> Result<Vec<PaneInfo>, TransportError> {
        Ok(self.with_state(|state| state.targets.clone()))
    }

    fn has_session(&self, _session: &SessionName) -> Result<bool, TransportError> {
        Ok(false)
    }

    fn list_windows(&self, _session: &SessionName) -> Result<Vec<WindowName>, TransportError> {
        Ok(self.with_state(|state| state.windows.clone()))
    }


    fn kill_session(&self, _session: &SessionName) -> Result<(), TransportError> {
        Ok(())
    }

    fn kill_window(&self, _target: &Target) -> Result<(), TransportError> {
        Ok(())
    }

}

fn fake_only_team_dir(hermetic: &HermeticTestEnv, tag: &str) -> PathBuf {
    let team = hermetic.workspace(tag).join("teamdir");
    std::fs::create_dir_all(team.join("agents")).expect("create agents dir");
    std::fs::write(team.join("TEAM.md"), TEAM_MD).expect("write TEAM.md");
    std::fs::write(team.join("agents").join("faker.md"), FAKE_ROLE).expect("write fake role");
    team
}

fn ready_launch(report: QuickStartReport) -> team_agent::lifecycle::LaunchReport {
    match report {
        QuickStartReport::Ready { launch, .. } => *launch,
        other => panic!("fixture expected quick-start ready report, got {other:?}"),
    }
}

impl RecordingTransport {
    fn spawn_result(&self, session: &SessionName, window: &WindowName) -> SpawnResult {
        let pane_index = self.with_state(|state| {
            let idx = state.spawns;
            state.spawns = state.spawns.saturating_add(1);
            idx
        });
        SpawnResult {
            pane_id: PaneId::new(format!("%{pane_index}")),
            session: session.clone(),
            window: window.clone(),
            child_pid: None,
        }
    }
}
