//! 0.5.44 debt-sweep B-car RED contracts.
//!
//! User-visible contracts:
//! - Bare claim/takeover uses the canonical target team, not stale caller env.
//! - Endpoint convergence remains distinct from physical team-session readiness.
//! - Wrapper-launched providers are classified from descendant provider argv,
//!   while unrelated node/bash wrappers stay unverifiable, not dead.
//! - The minimum bang gate is an executable private-socket add-agent harness,
//!   not another prose-only declaration.
//! - This car adds no new visible Team Agent commands.

#![cfg(unix)]
#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

#[path = "support/hermetic.rs"]
mod hermetic_guard;
#[allow(dead_code)]
fn _hermetic_boundary_marker(_: &hermetic_guard::HermeticTestEnv) {}

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{json, Value};
use serial_test::serial;
use team_agent::coordinator::{Coordinator, ErrorLists, ProviderRegistry, WorkspacePath};
use team_agent::provider::{get_adapter, Provider, ProviderAdapter};
use team_agent::transport::{
    AttachOutcome, BackendKind, CaptureRange, CapturedText, InjectPayload, InjectReport,
    InjectStage, InjectVerification, Key, PaneField, PaneId, PaneInfo, PaneLiveness, SessionName,
    SetEnvOutcome, SpawnResult, SubmitVerification, Target, Transport, TransportError,
    TurnVerification, WindowName,
};

const ACTIVE_TEAM: &str = "fleet";
const SIBLING_TEAM: &str = "current";
const WORKER: &str = "fetcher";
const CALLER_PANE: &str = "%0";
const STALE_OWNER_PANE: &str = "%9";
const LIVE_LEADER_PID: u32 = 14_663;
const STALE_OWNER_PID: u32 = 47_641;
const BASELINE_VISIBLE_COMMAND_COUNT: usize = 14;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_team-agent")
}

#[test]
#[serial(env)]
fn b1_bare_claim_uses_active_fleet_not_stale_caller_env_and_reports_session_absent() {
    let case = FleetClaimCase::new("bare-claim-active-fleet");
    let before_current = case.team_state(SIBLING_TEAM);

    let claim = case.run_ta(&[
        "claim-leader",
        "--workspace",
        case.workspace_str(),
        "--confirm",
        "--json",
    ]);
    let claim_json = json_output(&claim, "B1 bare claim-leader");
    assert_binding_ok(&claim, &claim_json, "B1 bare claim-leader");

    assert_checked_paths_target(&claim_json, ACTIVE_TEAM, "B1 bare claim-leader");
    assert_team_session_ready(&claim_json, Some(false), "B1 bare claim-leader");
    assert_convergence_event_target_and_readiness(&case, "claim-leader", ACTIVE_TEAM, Some(false));
    assert_root_and_team_converged(&case.read_state(), &case.new_socket, ACTIVE_TEAM);
    assert_eq!(
        case.team_state(SIBLING_TEAM),
        before_current,
        "B1: bare claim must not mutate preserved sibling team `{SIBLING_TEAM}`"
    );
}

#[test]
#[serial(env)]
fn b1_bare_takeover_uses_active_fleet_not_stale_caller_env_and_reports_session_absent() {
    let case = FleetClaimCase::new("bare-takeover-active-fleet");
    let before_current = case.team_state(SIBLING_TEAM);

    let takeover = case.run_ta(&[
        "takeover",
        "--workspace",
        case.workspace_str(),
        "--confirm",
        "--json",
    ]);
    let takeover_json = json_output(&takeover, "B1 bare takeover");
    assert_binding_ok(&takeover, &takeover_json, "B1 bare takeover");

    assert_checked_paths_target(&takeover_json, ACTIVE_TEAM, "B1 bare takeover");
    assert_team_session_ready(&takeover_json, Some(false), "B1 bare takeover");
    assert_convergence_event_target_and_readiness(&case, "takeover", ACTIVE_TEAM, Some(false));
    assert_root_and_team_converged(&case.read_state(), &case.new_socket, ACTIVE_TEAM);
    assert_eq!(
        case.team_state(SIBLING_TEAM),
        before_current,
        "B1: bare takeover must not mutate preserved sibling team `{SIBLING_TEAM}`"
    );
}

#[test]
#[serial(env)]
fn b1_explicit_current_still_targets_current_not_active_fleet() {
    let case = FleetClaimCase::new("explicit-current-stays-current");
    let before_fleet = case.team_state(ACTIVE_TEAM);

    let claim = case.run_ta(&[
        "claim-leader",
        "--workspace",
        case.workspace_str(),
        "--team",
        SIBLING_TEAM,
        "--confirm",
        "--json",
    ]);
    let claim_json = json_output(&claim, "B1 explicit current claim-leader");
    assert_binding_ok(&claim, &claim_json, "B1 explicit current claim-leader");

    assert_checked_paths_target(&claim_json, SIBLING_TEAM, "B1 explicit current");
    assert_team_session_ready(&claim_json, Some(false), "B1 explicit current");
    assert_team_converged(&case.read_state(), &case.new_socket, SIBLING_TEAM);
    assert_eq!(
        case.team_state(ACTIVE_TEAM),
        before_fleet,
        "B1: explicit --team current must not be swallowed by active_team_key=fleet"
    );
}

#[test]
#[serial(env)]
fn b2_wrapper_node_with_live_codex_descendant_is_alive_not_provider_dead() {
    let case = WrapperCase::new("node-descendant-provider", WrapperShape::CodexDescendant);

    case.tick();
    let watch = case.abnormal_watch();

    assert_eq!(
        watch.get("provider_process_dead").and_then(Value::as_bool),
        Some(false),
        "B2: node/bash wrapper with a live Codex descendant must not write provider_process_dead=true; watch={watch}"
    );
    assert_eq!(
        watch.get("last_liveness").and_then(Value::as_str),
        Some("alive"),
        "B2: descendant provider argv is positive provider liveness; watch={watch}"
    );
    assert!(
        !case.events().contains("\"worker.abnormal_exit\""),
        "B2: live descendant must not emit abnormal exit; events={}",
        case.events()
    );
}

#[test]
#[serial(env)]
fn b2_unrelated_node_wrapper_is_unverifiable_not_alive_or_dead() {
    let case = WrapperCase::new("node-generic", WrapperShape::GenericNode);

    case.tick();
    let watch = case.abnormal_watch();

    assert_eq!(
        watch.get("provider_process_dead").and_then(Value::as_bool),
        Some(false),
        "B2: unrelated node service must not be folded into Dead/provider_not_foreground; watch={watch}"
    );
    assert_eq!(
        watch.get("last_liveness").and_then(Value::as_str),
        Some("unverifiable"),
        "B2: unrelated node service is not provider-positive either; watch={watch}"
    );
    assert!(
        watch
            .get("last_liveness_detail")
            .and_then(Value::as_str)
            .is_none_or(|detail| !detail.contains("provider_not_foreground")),
        "B2: command mismatch alone must not be the death proof; watch={watch}"
    );
}

#[test]
fn b_car_adds_no_new_visible_team_agent_commands() {
    let output = Command::new(bin())
        .arg("--help")
        .output()
        .expect("run team-agent --help");
    assert!(
        output.status.success(),
        "help must run before checking command count; stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    let help = String::from_utf8_lossy(&output.stdout);
    let commands = visible_commands(&help);
    assert_eq!(
        commands.len(),
        BASELINE_VISIBLE_COMMAND_COUNT,
        "B car governance: expected new visible command count delta 0 from main@90f590b baseline {BASELINE_VISIBLE_COMMAND_COUNT}; visible commands={commands:?}"
    );
    for forbidden in [
        "repair-provider",
        "provider-diagnose",
        "bang-gate",
        "env-isolate",
    ] {
        assert!(
            !commands.iter().any(|command| command == forbidden),
            "B car must internalize fixes in existing paths, not add `{forbidden}`; commands={commands:?}"
        );
    }
}

#[test]
fn b4_minimum_bang_gate_is_promoted_from_declaration_to_executable_harness() {
    let candidates = candidate_gate_files();
    let required = [
        "TMUX_SERVER_DEATH_0544_BANG_ADD_AGENT_OUTCOMES",
        "#!",
        "--contract-check",
        "send-keys",
        "add-agent",
        "--role-file",
        "--workspace",
        "--team",
        "private",
        "PATH",
        "team-agent",
        "tmux",
        "success",
        "failure",
        "mcp.server_exit",
        "coordinator.session_missing",
    ];
    let matches = candidates
        .iter()
        .filter(|(path, text)| {
            path.starts_with("tools/gate-harness/")
                && !path.ends_with(".md")
                && required.iter().all(|needle| text.contains(needle))
        })
        .map(|(path, _)| path.clone())
        .collect::<Vec<_>>();
    let Some(path) = matches.first() else {
        panic!(
            "B4: 0.5.44 must promote the minimum bang/private-socket bare add-agent gate to an executable success+failure outcome harness under tools/gate-harness/. Markdown declarations are not enough. Missing marker TMUX_SERVER_DEATH_0544_BANG_ADD_AGENT_OUTCOMES; scanned {} files.",
            candidates.len()
        );
    };
    assert_executable_gate(path);

    let harness = repo_root().join(path);
    let output = Command::new(&harness)
        .arg("--contract-check")
        .output()
        .unwrap_or_else(|error| panic!("B4: run {path} --contract-check: {error}"));
    assert!(
        output.status.success(),
        "B4: {path} --contract-check must pass without a real provider; code={:?} stdout={} stderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

struct FleetClaimCase {
    workspace: PathBuf,
    fake_bin: PathBuf,
    new_socket: String,
}

impl FleetClaimCase {
    fn new(tag: &str) -> Self {
        let workspace = tmp_dir(&format!("claim-{tag}"));
        let run_id = next_id();
        let old_socket = format!("/private/tmp/tmux-501/ta-0544-old-{run_id}");
        let new_socket = format!("/private/tmp/tmux-501/ta-0544-new-{run_id}");
        let team_session = format!("team-0544-fleet-{run_id}");
        let leader_session = format!("leader-0544-fleet-{run_id}");
        std::fs::create_dir_all(workspace.join("home")).expect("create isolated home");
        let fake_bin = fake_tmux_bin(
            &workspace,
            &old_socket,
            &new_socket,
            &team_session,
            &leader_session,
        );
        seed_fleet_state(
            &workspace,
            &old_socket,
            &new_socket,
            &team_session,
            &leader_session,
        );
        write_minimal_team_spec(&workspace, &team_session);
        Self {
            workspace,
            fake_bin,
            new_socket,
        }
    }

    fn workspace_str(&self) -> &str {
        self.workspace
            .to_str()
            .expect("workspace path must be utf8")
    }

    fn read_state(&self) -> Value {
        serde_json::from_str(
            &std::fs::read_to_string(self.workspace.join(".team/runtime/state.json"))
                .expect("read state"),
        )
        .expect("parse state")
    }

    fn team_state(&self, team: &str) -> Value {
        self.read_state()
            .pointer(&format!("/teams/{team}"))
            .cloned()
            .unwrap_or_else(|| panic!("team state missing: {team}"))
    }

    fn events(&self) -> String {
        std::fs::read_to_string(self.workspace.join(".team/logs/events.jsonl")).unwrap_or_default()
    }

    fn run_ta(&self, args: &[&str]) -> Output {
        let mut command = Command::new(bin());
        command
            .args(args)
            .current_dir(&self.workspace)
            .env(
                "TEAM_AGENT_TEST_ENDPOINT_CONVERGENCE_HARNESS_SPEC_FALLBACK",
                "1",
            )
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    self.fake_bin.display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            .env("TMUX", format!("{},12345,0", self.new_socket))
            .env("TMUX_PANE", CALLER_PANE)
            .env("TEAM_AGENT_TEAM_ID", SIBLING_TEAM)
            .env("TEAM_AGENT_LEADER_PROVIDER", "codex")
            .env("TEAM_AGENT_MACHINE_FINGERPRINT", "machine-0544-b1")
            .env("HOME", self.workspace.join("home"))
            .env("USER", "te-red");
        for key in [
            "TEAM_AGENT_LEADER_PANE_ID",
            "TEAM_AGENT_LEADER_SESSION_UUID",
            "TEAM_AGENT_ID",
            "TEAM_AGENT_AGENT_ID",
            "TEAM_AGENT_WORKSPACE",
            "TEAM_AGENT_OWNER_TEAM_ID",
        ] {
            command.env_remove(key);
        }
        command.output().expect("run team-agent test binary")
    }
}

impl Drop for FleetClaimCase {
    fn drop(&mut self) {
        if std::env::var("TEAM_AGENT_KEEP_TEST_TMP").as_deref() != Ok("1") {
            let _ = std::fs::remove_dir_all(&self.workspace);
        }
    }
}

enum WrapperShape {
    CodexDescendant,
    GenericNode,
}

struct WrapperCase {
    workspace: PathBuf,
    _process: Option<ProcessTree>,
}

impl WrapperCase {
    fn new(tag: &str, shape: WrapperShape) -> Self {
        let workspace = tmp_dir(&format!("wrapper-{tag}"));
        let rollout = workspace.join("rollout-worker.jsonl");
        std::fs::write(&rollout, "{\"method\":\"turn/completed\",\"params\":{\"turn\":{\"id\":\"t0\",\"status\":\"completed\"}}}\n")
            .expect("write rollout");
        let process = match shape {
            WrapperShape::CodexDescendant => Some(ProcessTree::spawn_with_codex_child(&workspace)),
            WrapperShape::GenericNode => Some(ProcessTree::spawn_generic_wrapper()),
        };
        let pane_pid = process.as_ref().map(ProcessTree::pid);
        seed_wrapper_state(&workspace, &rollout, pane_pid);
        Self {
            workspace,
            _process: process,
        }
    }

    fn tick(&self) {
        let target = PaneInfo {
            pane_id: PaneId::new("%1"),
            session: SessionName::new("team-0544-wrapper"),
            window_index: None,
            window_name: Some(WindowName::new(WORKER)),
            pane_index: None,
            tty: None,
            current_command: Some("node".to_string()),
            current_path: Some(self.workspace.clone()),
            active: false,
            pane_pid: self._process.as_ref().map(ProcessTree::pid),
            leader_env: BTreeMap::new(),
        };
        let coord = Coordinator::new(
            WorkspacePath::new(self.workspace.clone()),
            Box::new(TestProviderRegistry),
            Box::new(WrapperTransport {
                targets: vec![target],
                capture_text: String::new(),
            }),
        );
        coord.tick().expect("coordinator tick");
    }

    fn abnormal_watch(&self) -> Value {
        self.read_state()
            .pointer("/coordinator/abnormal_exit_watch/fetcher")
            .cloned()
            .or_else(|| {
                self.read_state()
                    .pointer("/teams/team/coordinator/abnormal_exit_watch/fetcher")
                    .cloned()
            })
            .unwrap_or_else(|| panic!("missing abnormal watch; state={}", self.read_state()))
    }

    fn read_state(&self) -> Value {
        serde_json::from_str(
            &std::fs::read_to_string(self.workspace.join(".team/runtime/state.json"))
                .expect("read wrapper state"),
        )
        .expect("parse wrapper state")
    }

    fn events(&self) -> String {
        std::fs::read_to_string(self.workspace.join(".team/logs/events.jsonl")).unwrap_or_default()
    }
}

impl Drop for WrapperCase {
    fn drop(&mut self) {
        if std::env::var("TEAM_AGENT_KEEP_TEST_TMP").as_deref() != Ok("1") {
            let _ = std::fs::remove_dir_all(&self.workspace);
        }
    }
}

struct WrapperTransport {
    targets: Vec<PaneInfo>,
    capture_text: String,
}

impl Transport for WrapperTransport {
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
        Ok(SpawnResult {
            pane_id: PaneId::new("%spawn"),
            session: session.clone(),
            window: window.clone(),
            child_pid: None,
        })
    }

    fn spawn_into(
        &self,
        session: &SessionName,
        window: &WindowName,
        _argv: &[String],
        _cwd: &Path,
        _env: &BTreeMap<String, String>,
    ) -> Result<SpawnResult, TransportError> {
        Ok(SpawnResult {
            pane_id: PaneId::new("%spawn"),
            session: session.clone(),
            window: window.clone(),
            child_pid: None,
        })
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
            text: self.capture_text.clone(),
            range,
        })
    }

    fn query(&self, _target: &Target, field: PaneField) -> Result<Option<String>, TransportError> {
        match field {
            PaneField::PaneWidth => Ok(Some("120".to_string())),
            _ => Ok(None),
        }
    }

    fn liveness(&self, _pane: &PaneId) -> Result<PaneLiveness, TransportError> {
        Ok(PaneLiveness::Live)
    }

    fn list_targets(&self) -> Result<Vec<PaneInfo>, TransportError> {
        Ok(self.targets.clone())
    }

    fn has_session(&self, _session: &SessionName) -> Result<bool, TransportError> {
        Ok(true)
    }

    fn list_windows(&self, _session: &SessionName) -> Result<Vec<WindowName>, TransportError> {
        Ok(vec![WindowName::new(WORKER)])
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

struct TestProviderRegistry;

impl ProviderRegistry for TestProviderRegistry {
    fn adapter_for(&self, provider: Provider) -> Box<dyn ProviderAdapter> {
        get_adapter(provider)
    }

    fn error_lists(&self, _provider: Provider) -> ErrorLists {
        ErrorLists {
            whitelist: Vec::new(),
            blacklist: Vec::new(),
        }
    }
}

struct ProcessTree {
    child: Child,
    pgid: i32,
}

impl ProcessTree {
    fn spawn_with_codex_child(workspace: &Path) -> Self {
        use std::os::unix::process::CommandExt;

        let fake_bin = workspace.join("fake-provider-bin");
        std::fs::create_dir_all(&fake_bin).expect("create fake provider bin");
        let codex = fake_bin.join("codex");
        std::fs::write(&codex, "#!/bin/sh\nsleep 60\n").expect("write fake codex");
        set_executable(&codex);
        let wrapper = workspace.join("node-wrapper.sh");
        std::fs::write(&wrapper, "codex & wait\n").expect("write wrapper");
        set_executable(&wrapper);

        let mut command = Command::new("sh");
        command.arg(&wrapper);
        command.env_clear();
        command.env(
            "PATH",
            format!(
                "{}:{}",
                fake_bin.display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        );
        unsafe {
            command.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
        let child = command.spawn().expect("spawn wrapper process");
        Self {
            pgid: child.id() as i32,
            child,
        }
    }

    fn spawn_generic_wrapper() -> Self {
        use std::os::unix::process::CommandExt;

        let mut command = Command::new("sh");
        command.arg("-c").arg("sleep 60 & wait");
        command.env_clear();
        command.env("PATH", std::env::var("PATH").unwrap_or_default());
        unsafe {
            command.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
        let child = command.spawn().expect("spawn generic wrapper");
        Self {
            pgid: child.id() as i32,
            child,
        }
    }

    fn pid(&self) -> u32 {
        self.child.id()
    }
}

impl Drop for ProcessTree {
    fn drop(&mut self) {
        unsafe {
            libc::kill(-self.pgid, libc::SIGTERM);
        }
        let _ = self.child.wait();
        unsafe {
            libc::kill(-self.pgid, libc::SIGKILL);
        }
    }
}

fn seed_fleet_state(
    workspace: &Path,
    old_socket: &str,
    new_socket: &str,
    team_session: &str,
    leader_session: &str,
) {
    let worker = json!({
        "id": WORKER,
        "name": WORKER,
        "provider": "fake",
        "window": WORKER,
        "status": "stopped",
        "owner_team_id": ACTIVE_TEAM
    });
    let fleet_receiver = receiver_json(
        new_socket,
        leader_session,
        STALE_OWNER_PANE,
        STALE_OWNER_PID,
    );
    let fleet_owner = owner_json(new_socket, STALE_OWNER_PANE, STALE_OWNER_PID);
    let current_receiver = receiver_json(old_socket, "old-current-leader", "%44", 44_044);
    let current_owner = owner_json(old_socket, "%44", 44_044);
    team_agent::state::persist::save_runtime_state(
        workspace,
        &json!({
            "active_team_key": ACTIVE_TEAM,
            "session_name": team_session,
            "team_dir": workspace.to_string_lossy().to_string(),
            "spec_path": workspace.join("team.spec.yaml").to_string_lossy().to_string(),
            "tmux_endpoint": old_socket,
            "tmux_socket": old_socket,
            "agents": { WORKER: worker.clone() },
            "leader_receiver": fleet_receiver.clone(),
            "team_owner": fleet_owner.clone(),
            "teams": {
                ACTIVE_TEAM: {
                    "session_name": team_session,
                    "team_dir": workspace.to_string_lossy().to_string(),
                    "spec_path": workspace.join("team.spec.yaml").to_string_lossy().to_string(),
                    "tmux_endpoint": old_socket,
                    "tmux_socket": old_socket,
                    "agents": { WORKER: worker },
                    "leader_receiver": fleet_receiver,
                    "team_owner": fleet_owner
                },
                SIBLING_TEAM: {
                    "session_name": "team-0544-current-preserved",
                    "team_dir": workspace.to_string_lossy().to_string(),
                    "spec_path": workspace.join("team.spec.yaml").to_string_lossy().to_string(),
                    "tmux_endpoint": old_socket,
                    "tmux_socket": old_socket,
                    "agents": {},
                    "leader_receiver": current_receiver,
                    "team_owner": current_owner,
                    "preserved_sibling_marker": "must-remain-byte-identical"
                }
            }
        }),
    )
    .expect("seed fleet state");
}

fn receiver_json(socket: &str, leader_session: &str, pane: &str, pid: u32) -> Value {
    json!({
        "mode": "direct_tmux",
        "status": "attached",
        "provider": "codex",
        "pane_id": pane,
        "pane_pid": pid,
        "session_name": leader_session,
        "window_name": "claude_code",
        "tmux_socket": socket,
        "leader_session_uuid": "leader-0544",
        "owner_epoch": 7,
        "claimed_via": "claim-leader"
    })
}

fn owner_json(socket: &str, pane: &str, pid: u32) -> Value {
    json!({
        "pane_id": pane,
        "provider": "codex",
        "pane_pid": pid,
        "tmux_socket": socket,
        "leader_session_uuid": "leader-0544",
        "machine_fingerprint": "machine-0544-b1",
        "owner_epoch": 7,
        "claimed_via": "claim-leader"
    })
}

fn seed_wrapper_state(workspace: &Path, rollout: &Path, pane_pid: Option<u32>) {
    let mut agent = json!({
        "provider": "codex",
        "status": "running",
        "agent_id": WORKER,
        "window": WORKER,
        "pane_id": "%1",
        "rollout_path": rollout.to_string_lossy(),
        "spawn_cwd": workspace.to_string_lossy(),
        "spawn_epoch": 1,
        "owner_team_id": "team"
    });
    if let Some(pid) = pane_pid {
        agent["pane_pid"] = json!(pid);
    }
    team_agent::state::persist::save_runtime_state(
        workspace,
        &json!({
            "active_team_key": "team",
            "session_name": "team-0544-wrapper",
            "team_dir": workspace.to_string_lossy().to_string(),
            "spec_path": workspace.join("team.spec.yaml").to_string_lossy().to_string(),
            "agents": { WORKER: agent },
        }),
    )
    .expect("seed wrapper state");
}

fn write_minimal_team_spec(workspace: &Path, team_session: &str) {
    let spec = format!(
        r#"version: 1
team:
  name: "{team}"
  mode: "supervisor_worker"
  objective: "0.5.44 canonical target contract"
  workspace: "{workspace}"
leader:
  id: "leader"
  role: "leader"
  provider: "codex"
  model: null
  tools: []
agents:
  - id: "{worker}"
    role: "fetcher"
    provider: "fake"
    model: "fake"
    auth_mode: "subscription"
    working_directory: "{workspace}"
    system_prompt:
      inline: "fake worker"
      file: null
    tools: []
    permission_mode: "restricted"
    preferred_for: []
    avoid_for: []
    output_contract:
      format: "result_envelope_v1"
      required_fields: []
routing:
  default_assignee: "{worker}"
  rules: []
communication:
  protocol: "mcp_inbox"
  topology: "leader_centered"
  worker_to_worker: true
  ack_timeout_sec: 60
  result_format: "result_envelope_v1"
  message_store:
    sqlite: ".team/runtime/team.db"
    mirror_files: ".team/messages"
runtime:
  backend: "tmux"
  display_backend: "none"
  session_name: "{session}"
  auto_launch: true
  require_user_approval_before_launch: false
  max_active_agents: 1
  startup_order:
    - "{worker}"
  dangerous_auto_approve: false
  fast: false
  tick_interval_sec: 2
  push_min_interval_sec: 60
  stuck_timeout_sec: 300
context:
  state_file: "team_state.md"
  artifact_dir: ".team/artifacts"
  log_dir: ".team/logs"
  summarization:
    worker_full_logs: "retain_outside_leader_context"
    state_update: "after_each_result"
tasks: []
"#,
        team = ACTIVE_TEAM,
        worker = WORKER,
        session = team_session,
        workspace = workspace.display()
    );
    std::fs::write(workspace.join("team.spec.yaml"), &spec).expect("write team.spec.yaml");
    for team in [ACTIVE_TEAM, SIBLING_TEAM] {
        let runtime_spec_dir = workspace.join(".team/runtime").join(team);
        std::fs::create_dir_all(&runtime_spec_dir).expect("create runtime spec dir");
        std::fs::write(runtime_spec_dir.join("team.spec.yaml"), &spec)
            .expect("write runtime team.spec.yaml");
    }
}

fn fake_tmux_bin(
    workspace: &Path,
    old_socket: &str,
    new_socket: &str,
    team_session: &str,
    leader_session: &str,
) -> PathBuf {
    let bin_dir = workspace.join("fake-bin");
    std::fs::create_dir_all(&bin_dir).expect("create fake bin dir");
    let tmux = bin_dir.join("tmux");
    let log_path = workspace.join("fake-tmux.log");
    let new_line = pane_line(
        CALLER_PANE,
        leader_session,
        "claude_code",
        "codex",
        workspace,
        LIVE_LEADER_PID,
    );
    let script = format!(
        r#"#!/bin/sh
endpoint="default"
previous=""
target=""
for arg in "$@"; do
  if [ "$previous" = "-S" ]; then endpoint="$arg"; fi
  if [ "$previous" = "-t" ]; then target="$arg"; fi
  previous="$arg"
done
printf '%s\t%s\n' "$endpoint" "$*" >> '{log_path}'
case "$endpoint" in
  "{old_socket}")
    echo "no server running on {old_socket}" >&2
    exit 1
    ;;
  "{new_socket}"|"default")
    case " $* " in
      *" list-panes "*) printf '%s' '{new_line}'; exit 0 ;;
      *" list-sessions "*) printf '%s\n' '{leader_session}: 1 windows'; exit 0 ;;
      *" display-message "*) printf '%s\n' '{leader_pid}'; exit 0 ;;
      *" has-session "*)
        if [ "$target" = "{team_session}" ]; then exit 1; fi
        exit 0
        ;;
      *) exit 0 ;;
    esac
    ;;
  *)
    echo "unknown endpoint $endpoint" >&2
    exit 1
    ;;
esac
"#,
        log_path = shell_single_quoted_payload(&log_path.to_string_lossy()),
        old_socket = old_socket,
        new_socket = new_socket,
        new_line = shell_single_quoted_payload(&new_line),
        leader_session = leader_session,
        leader_pid = LIVE_LEADER_PID,
        team_session = team_session,
    );
    std::fs::write(&tmux, script).expect("write fake tmux");
    set_executable(&tmux);
    bin_dir
}

fn pane_line(
    pane: &str,
    session: &str,
    window: &str,
    command: &str,
    cwd: &Path,
    pid: u32,
) -> String {
    format!(
        "{pane}\t{session}\t0\t{window}\t0\t/dev/ttys0544\t{command}\t1\t{}\t1\t0\t{pid}\n",
        cwd.display()
    )
}

fn assert_binding_ok(output: &Output, value: &Value, label: &str) {
    assert!(
        output.status.success() && value.get("ok").and_then(Value::as_bool) == Some(true),
        "{label}: expected ok:true; code={:?} json={value} stdout={} stderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn assert_checked_paths_target(value: &Value, team: &str, label: &str) {
    let checked = checked_paths(value);
    assert!(
        checked
            .iter()
            .any(|path| path.contains(&format!("/teams/{team}/"))),
        "{label}: checked_paths must name canonical target team `{team}`; checked_paths={checked:?}; json={value}"
    );
    let other = if team == ACTIVE_TEAM {
        SIBLING_TEAM
    } else {
        ACTIVE_TEAM
    };
    assert!(
        checked
            .iter()
            .all(|path| !path.contains(&format!("/teams/{other}/"))),
        "{label}: checked_paths must not drift to sibling `{other}`; checked_paths={checked:?}; json={value}"
    );
}

fn checked_paths(value: &Value) -> Vec<String> {
    value
        .pointer("/topology_convergence/checked_paths")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect()
}

fn assert_team_session_ready(value: &Value, expected: Option<bool>, label: &str) {
    let actual = value.get("team_session_ready");
    match expected {
        Some(expected) => assert_eq!(
            actual.and_then(Value::as_bool),
            Some(expected),
            "{label}: claim/takeover JSON must carry additive top-level team_session_ready={expected}; json={value}"
        ),
        None => assert!(
            actual == Some(&Value::Null),
            "{label}: expected team_session_ready:null; json={value}"
        ),
    }
}

fn assert_convergence_event_target_and_readiness(
    case: &FleetClaimCase,
    source: &str,
    team: &str,
    ready: Option<bool>,
) {
    let found = case
        .events()
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .any(|event| {
            event.get("event").and_then(Value::as_str)
                == Some("leader_receiver.tmux_endpoint_converged")
                && event.get("source").and_then(Value::as_str) == Some(source)
                && event
                    .get("checked_paths")
                    .and_then(Value::as_array)
                    .is_some_and(|paths| {
                        paths
                            .iter()
                            .filter_map(Value::as_str)
                            .any(|path| path.contains(&format!("/teams/{team}/")))
                    })
                && match ready {
                    Some(expected) => {
                        event.get("team_session_ready").and_then(Value::as_bool) == Some(expected)
                    }
                    None => event.get("team_session_ready") == Some(&Value::Null),
                }
        });
    assert!(
        found,
        "B1: convergence event must carry canonical team `{team}` and team_session_ready={ready:?}; events={}",
        case.events()
    );
}

fn assert_root_and_team_converged(state: &Value, socket: &str, team: &str) {
    assert_eq!(
        state.get("tmux_endpoint").and_then(Value::as_str),
        Some(socket),
        "root tmux_endpoint must converge to caller endpoint; state={state}"
    );
    assert_team_converged(state, socket, team);
}

fn assert_team_converged(state: &Value, socket: &str, team: &str) {
    assert_eq!(
        state
            .pointer(&format!("/teams/{team}/tmux_endpoint"))
            .and_then(Value::as_str),
        Some(socket),
        "teams.{team}.tmux_endpoint must converge to caller endpoint; state={state}"
    );
    assert_eq!(
        state
            .pointer(&format!("/teams/{team}/tmux_socket"))
            .and_then(Value::as_str),
        Some(socket),
        "teams.{team}.tmux_socket must converge to caller endpoint; state={state}"
    );
}

fn json_output(output: &Output, label: &str) -> Value {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let start = stdout.find('{').unwrap_or_else(|| {
        panic!(
            "{label}: stdout must contain JSON object; code={:?} stdout={stdout:?} stderr={:?}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr)
        )
    });
    let end = stdout.rfind('}').expect("stdout JSON object end");
    serde_json::from_str(&stdout[start..=end]).unwrap_or_else(|error| {
        panic!(
            "{label}: parse JSON failed: {error}; stdout={stdout:?} stderr={:?}",
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

fn visible_commands(help: &str) -> Vec<String> {
    help.lines()
        .filter_map(|line| line.strip_prefix("  "))
        .filter(|line| !line.starts_with("team-agent "))
        .filter_map(|line| line.split_whitespace().next())
        .filter(|command| {
            command
                .chars()
                .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-')
        })
        .map(str::to_string)
        .collect()
}

fn candidate_gate_files() -> Vec<(String, String)> {
    let root = repo_root();
    let mut files = Vec::new();
    for relative_dir in [
        "crates/team-agent/tests",
        "crates/team-agent/tests/e2e/cases",
        "tools",
        ".team/artifacts/gate-harness",
    ] {
        let dir = root.join(relative_dir);
        if dir.exists() {
            collect_text_files(&root, &dir, &mut files);
        }
    }
    files
        .into_iter()
        .filter(|(path, _)| !path.ends_with("debt_sweep_0544_contract.rs"))
        .collect()
}

fn assert_executable_gate(path: &str) {
    use std::os::unix::fs::PermissionsExt;

    let full_path = repo_root().join(path);
    let mode = std::fs::metadata(&full_path)
        .unwrap_or_else(|error| panic!("B4: stat executable harness {path}: {error}"))
        .permissions()
        .mode();
    assert!(
        mode & 0o111 != 0,
        "B4: executable gate {path} must have an execute bit set, mode={mode:o}"
    );
}

fn collect_text_files(root: &Path, dir: &Path, out: &mut Vec<(String, String)>) {
    for entry in std::fs::read_dir(dir).expect("read candidate dir") {
        let path = entry.expect("read candidate entry").path();
        if path.is_dir() {
            collect_text_files(root, &path, out);
            continue;
        }
        let Some(ext) = path.extension().and_then(|ext| ext.to_str()) else {
            continue;
        };
        if !matches!(
            ext,
            "rs" | "md" | "sh" | "py" | "json" | "toml" | "yaml" | "yml"
        ) {
            continue;
        }
        if let Ok(text) = std::fs::read_to_string(&path) {
            out.push((relative_path(root, &path), text));
        }
    }
}

fn relative_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .to_string()
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("team-agent crate should live under crates/team-agent")
        .to_path_buf()
}

fn shell_single_quoted_payload(text: &str) -> String {
    text.replace('\'', "'\\''")
}

fn set_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
        .expect("chmod executable");
}

fn tmp_dir(tag: &str) -> PathBuf {
    let root = std::env::var_os("TEAM_AGENT_TEST_TMP")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    std::fs::create_dir_all(&root).expect("create test tmp root");
    let dir = root.join(format!(
        "ta-0544-{tag}-{}-{}",
        std::process::id(),
        next_id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create temp workspace");
    std::fs::canonicalize(dir).expect("canonicalize temp workspace")
}

fn next_id() -> u64 {
    static N: AtomicU64 = AtomicU64::new(0);
    N.fetch_add(1, Ordering::Relaxed)
}
