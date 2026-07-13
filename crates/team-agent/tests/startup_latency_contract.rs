//! 0.5.38 RED contract: startup latency instrumentation and bounded worker spawn.
//!
//! References:
//! - `.team/artifacts/startup-latency-locate.md` §5 / §8.
//!
//! User-visible contract:
//! - Restart/launch expose structured phase timing before optimizing latency.
//! - Restart may spawn independent workers concurrently, but persisted topology
//!   stays deterministic and failure aggregation remains equivalent to serial.
//! - Readiness still waits on session, worker panes, and coordinator truth.

#![cfg(unix)]
#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

#[path = "support/hermetic.rs"]
mod hermetic_guard;
#[allow(dead_code)]
fn _hermetic_boundary_marker(_: &hermetic_guard::HermeticTestEnv) {}

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use serial_test::serial;
use team_agent::coordinator::{MetadataSource, Pid, WorkspacePath};
use team_agent::event_log::EventLog;
use team_agent::lifecycle::{
    quick_start_with_transport_in_workspace_with_display,
    restart_with_transport_with_readiness_deadline, QuickStartReport, RestartReport,
};
use team_agent::message_store::MessageStore;
use team_agent::model::paths::{runtime_dir, runtime_spec_path};
use team_agent::state::persist::{load_runtime_state, save_runtime_state};
use team_agent::transport::{
    AttachOutcome, BackendKind, CaptureRange, CapturedText, InjectPayload, InjectReport,
    InjectStage, InjectVerification, Key, PaneField, PaneId, PaneInfo, PaneLiveness, SessionName,
    SetEnvOutcome, SpawnResult, SubmitVerification, Target, Transport, TransportError,
    TurnVerification, WindowName,
};

const TEAM: &str = "current";
const TEAM_SESSION: &str = "team-current";
const TMUX_ENDPOINT: &str = "/Volumes/nvme/tmp/ta-0538-startup-latency.sock";

#[test]
#[serial(env)]
fn restart_records_parallel_spawn_overlap_and_deterministic_state() {
    let case = RestartLatencyCase::new("r1-parallel-state", 8);
    let transport = StartupLatencyTransport::new().with_spawn_delay(Duration::from_millis(80));

    let report = restart_with_transport_with_readiness_deadline(
        &case.workspace,
        true,
        Some(TEAM),
        &transport,
        Some(2_000),
    )
    .expect("R1 setup: restart should complete against recording transport");
    assert!(
        matches!(report, RestartReport::Restarted { .. }),
        "R1 setup: restart must reach Restarted before checking latency contract; report={report:?}"
    );

    let calls = transport.spawn_calls();
    let new_windows = calls
        .iter()
        .filter(|call| call.kind == "spawn_into")
        .collect::<Vec<_>>();
    let state = case.read_state();
    let mut violations = Vec::new();
    if !has_overlap(&new_windows) {
        violations.push(format!(
            "expected at least two new-window spawns to overlap after the initial session creator; calls={calls:?}"
        ));
    }
    let agent_keys = state
        .pointer("/agents")
        .and_then(Value::as_object)
        .map(|agents| agents.keys().cloned().collect::<Vec<_>>())
        .unwrap_or_default();
    let expected_agents = worker_ids(8);
    if agent_keys != expected_agents {
        violations.push(format!(
            "persisted agents must stay sorted/stable in plan order; got={agent_keys:?}"
        ));
    }
    for (index, worker) in expected_agents.iter().enumerate() {
        let agent = state
            .pointer(&format!("/agents/{worker}"))
            .unwrap_or_else(|| panic!("R1 setup: missing {worker}; state={state}"));
        if state.pointer("/session_name").and_then(Value::as_str) != Some(TEAM_SESSION) {
            violations.push(format!("{worker}: root session_name not {TEAM_SESSION}"));
        }
        if agent.get("window").and_then(Value::as_str) != Some(worker.as_str()) {
            violations.push(format!(
                "{worker}: persisted window mismatch; agent={agent}"
            ));
        }
        if agent.get("pane_id").and_then(Value::as_str) != Some(format!("%{}", index + 1).as_str())
        {
            violations.push(format!(
                "{worker}: persisted pane_id mismatch; agent={agent}"
            ));
        }
        if agent.get("spawn_epoch").and_then(Value::as_u64) != Some(1) {
            violations.push(format!(
                "{worker}: respawn must persist spawn_epoch=1, not leave the old/missing value; agent={agent}"
            ));
        }
    }
    assert!(
        violations.is_empty(),
        "R1: parallel restart must overlap independent new-window spawns while keeping deterministic persisted topology:\n{}",
        violations.join("\n")
    );
}

#[test]
#[serial(env)]
fn restart_failure_aggregation_matches_serial_semantics_without_early_abort() {
    let case = RestartLatencyCase::new("r2-failure-aggregation", 8);
    let transport =
        StartupLatencyTransport::new().with_spawn_failure("w3", "injected w3 spawn failure");

    let report = restart_with_transport_with_readiness_deadline(
        &case.workspace,
        true,
        Some(TEAM),
        &transport,
        Some(2_000),
    )
    .expect("R2 setup: restart should return a typed report");
    let RestartReport::Partial {
        agents,
        failed_agents,
        ..
    } = report
    else {
        panic!(
            "R2: one worker spawn failure must produce Partial, not early abort; report={report:?}"
        );
    };

    assert_eq!(
        failed_agents
            .iter()
            .map(|agent| agent.agent_id.as_str())
            .collect::<Vec<_>>(),
        vec!["w3"],
        "R2: only the injected worker should fail; failed_agents={failed_agents:?}"
    );
    assert_eq!(
        agents
            .iter()
            .map(|agent| agent.agent_id.as_str().to_string())
            .collect::<Vec<_>>(),
        ["w1", "w2", "w4", "w5", "w6", "w7", "w8"],
        "R2: successful workers after w3 must still be spawned and reported"
    );
    assert_eq!(
        transport
            .spawn_calls()
            .iter()
            .map(|call| call.window.as_str())
            .collect::<Vec<_>>(),
        worker_ids(8),
        "R2: failure aggregation must attempt the full plan; no early return after w3"
    );

    let state = case.read_state();
    assert_eq!(
        state.pointer("/agents/w3/status").and_then(Value::as_str),
        Some("failed"),
        "R2: failed worker must be persisted as failed; state={state}"
    );
    for worker in ["w1", "w2", "w4", "w5", "w6", "w7", "w8"] {
        assert_eq!(
            state
                .pointer(&format!("/agents/{worker}/status"))
                .and_then(Value::as_str),
            Some("running"),
            "R2: successful worker {worker} must be persisted running; state={state}"
        );
    }
    assert_phase_events(
        &case.events(),
        "restart.phase",
        &[
            "plan_classification",
            "spawn_all",
            "save_state",
            "completed",
        ],
    );
}

#[test]
#[serial(env)]
fn restart_readiness_timeout_keeps_three_truth_booleans_and_no_false_restarted() {
    let case = RestartLatencyCase::new("r3-readiness-timeout", 1);
    let transport = StartupLatencyTransport::new().with_session_missing_for_readiness();

    let result = restart_with_transport_with_readiness_deadline(
        &case.workspace,
        true,
        Some(TEAM),
        &transport,
        Some(1),
    );
    assert!(
        result.is_err(),
        "R3: readiness timeout must return Err, not a false Restarted report; result={result:?}"
    );
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("restart not ready within") && err.contains("tmux session created"),
        "R3: readiness error must name the missing truth gates; err={err}"
    );

    let events = case.events();
    let timeout = events
        .iter()
        .find(|event| event_name(event) == Some("restart.readiness_timeout"))
        .unwrap_or_else(|| {
            panic!("R3: restart.readiness_timeout event missing; events={events:?}")
        });
    assert_eq!(
        timeout.get("tmux_session_created").and_then(Value::as_bool),
        Some(false),
        "R3: timeout must expose tmux_session_created=false; event={timeout}"
    );
    assert_eq!(
        timeout
            .get("worker_pane_addressable")
            .and_then(Value::as_bool),
        Some(true),
        "R3: timeout must expose worker_pane_addressable=true for the spawned pane; event={timeout}"
    );
    assert_eq!(
        timeout.get("coordinator_alive").and_then(Value::as_bool),
        Some(false),
        "R3: coordinator_alive must be false when the team session is not live; event={timeout}"
    );
    assert!(
        !events.iter().any(|event| {
            event_name(event) == Some("restart.completed")
                && event.get("rc").and_then(Value::as_str) == Some("ok")
        }),
        "R3: readiness timeout must not emit a false ok restart.completed; events={events:?}"
    );
    assert_phase_events(&events, "restart.phase", &["readiness_wait"]);
}

#[test]
#[serial(env)]
fn eight_worker_restart_with_100ms_spawn_delay_beats_serial_baseline() {
    let case = RestartLatencyCase::new("r4-timing-smoke", 8);
    let delay = Duration::from_millis(100);
    let transport = StartupLatencyTransport::new().with_spawn_delay(delay);

    let started = Instant::now();
    let report = restart_with_transport_with_readiness_deadline(
        &case.workspace,
        true,
        Some(TEAM),
        &transport,
        Some(2_000),
    )
    .expect("R4 setup: restart should complete against delayed transport");
    let elapsed = started.elapsed();
    assert!(
        matches!(report, RestartReport::Restarted { .. }),
        "R4 setup: restart must reach Restarted; report={report:?}"
    );

    let calls = transport.spawn_calls();
    assert_eq!(
        calls.len(),
        8,
        "R4 setup: timing smoke must exercise all 8 workers; calls={calls:?}"
    );
    let serial_baseline = delay * calls.len() as u32;
    assert!(
        serial_baseline >= Duration::from_millis(800),
        "R4 setup: serial baseline must represent 8 workers * 100ms; baseline={serial_baseline:?}"
    );
    assert!(
        elapsed < serial_baseline - Duration::from_millis(150),
        "R4: bounded-concurrency restart should complete below the serial spawn sum; elapsed={elapsed:?} serial_baseline={serial_baseline:?} calls={calls:?}"
    );
    assert_phase_events(&case.events(), "restart.phase", &["spawn_all", "completed"]);
}

#[test]
#[serial(env)]
fn restart_and_launch_emit_structured_latency_events_without_fake_ready() {
    let restart_case = RestartLatencyCase::new("events-restart", 2);
    let restart_transport = StartupLatencyTransport::new();
    let restart = restart_with_transport_with_readiness_deadline(
        &restart_case.workspace,
        true,
        Some(TEAM),
        &restart_transport,
        Some(1_000),
    )
    .expect("event setup: restart should complete");
    assert!(
        matches!(restart, RestartReport::Restarted { .. }),
        "event setup: restart failed unexpectedly; report={restart:?}"
    );
    let restart_events = restart_case.events();
    assert_phase_events(
        &restart_events,
        "restart.phase",
        &[
            "resolve_context",
            "compile_spec",
            "plan_classification",
            "teardown",
            "spawn_all",
            "save_state",
            "coordinator_start",
            "readiness_wait",
            "completed",
        ],
    );
    assert_worker_spawn_timing_events(&restart_events, &worker_ids(2), "restart");
    assert_no_fake_ready_events(&restart_events);

    let launch_case = LaunchLatencyCase::new("events-launch", 2);
    let launch_transport = StartupLatencyTransport::new();
    let quick_start = quick_start_with_transport_in_workspace_with_display(
        &launch_case.workspace,
        &launch_case.team_dir,
        None,
        true,
        Some(TEAM),
        &launch_transport,
        false,
    )
    .expect("event setup: quick-start should complete");
    assert!(
        matches!(quick_start, QuickStartReport::Ready { .. }),
        "event setup: quick-start failed unexpectedly; report={quick_start:?}"
    );
    let launch_events = launch_case.events();
    assert_phase_events(
        &launch_events,
        "launch.phase",
        &[
            "compile_spec",
            "spawn_all",
            "coordinator_start",
            "readiness_wait",
            "completed",
        ],
    );
    assert_worker_spawn_timing_events(&launch_events, &worker_ids(2), "launch");
    assert_no_fake_ready_events(&launch_events);
}

struct RestartLatencyCase {
    _env: hermetic_guard::HermeticTestEnv,
    workspace: PathBuf,
}

impl RestartLatencyCase {
    fn new(tag: &str, workers: usize) -> Self {
        let env = hermetic_guard::HermeticTestEnv::enter(tag);
        env.scrub_tmux();
        env.assert_no_real_tmux();
        let workspace = env.workspace(tag);
        write_team_docs(&workspace, workers);
        write_runtime_spec(&workspace, &workspace);
        seed_restart_state(&workspace, workers);
        seed_healthy_coordinator(&workspace);
        Self {
            _env: env,
            workspace,
        }
    }

    fn read_state(&self) -> Value {
        load_runtime_state(&self.workspace).expect("load runtime state")
    }

    fn events(&self) -> Vec<Value> {
        EventLog::new(&self.workspace).tail(0).expect("read events")
    }
}

struct LaunchLatencyCase {
    _env: hermetic_guard::HermeticTestEnv,
    workspace: PathBuf,
    team_dir: PathBuf,
}

impl LaunchLatencyCase {
    fn new(tag: &str, workers: usize) -> Self {
        let env = hermetic_guard::HermeticTestEnv::enter(tag);
        env.scrub_tmux();
        env.assert_no_real_tmux();
        let workspace = env.workspace(tag);
        let team_dir = workspace.join("teamdir");
        fs::create_dir_all(&team_dir).expect("create launch team dir");
        write_team_docs(&team_dir, workers);
        seed_healthy_coordinator(&workspace);
        Self {
            _env: env,
            workspace,
            team_dir,
        }
    }

    fn events(&self) -> Vec<Value> {
        EventLog::new(&self.workspace)
            .tail(0)
            .expect("read launch events")
    }
}

fn write_team_docs(team_dir: &Path, workers: usize) {
    fs::create_dir_all(team_dir.join("agents")).expect("create agents dir");
    fs::write(
        team_dir.join("TEAM.md"),
        "---\nname: current\nobjective: Startup latency contract.\nprovider: fake\n---\n\nTeam.\n",
    )
    .expect("write TEAM.md");
    for worker in worker_ids(workers) {
        fs::write(
            team_dir.join("agents").join(format!("{worker}.md")),
            role_doc(&worker),
        )
        .expect("write role doc");
    }
}

fn role_doc(worker: &str) -> String {
    format!(
        "---\nname: {worker}\nrole: Worker {worker}\nprovider: fake\nmodel: fake\nauth_mode: subscription\ntools:\n  - mcp_team\n---\n\n{worker}.\n"
    )
}

fn write_runtime_spec(workspace: &Path, team_dir: &Path) {
    let spec = team_agent::compiler::compile_team(team_dir).expect("compile team");
    let spec_text = team_agent::model::yaml::dumps(&spec);
    fs::write(team_dir.join("team.spec.yaml"), &spec_text).expect("write legacy spec");
    let runtime_spec = runtime_spec_path(workspace, TEAM);
    fs::create_dir_all(runtime_spec.parent().expect("runtime spec parent"))
        .expect("create runtime spec dir");
    fs::write(runtime_spec, spec_text).expect("write runtime spec");
}

fn seed_restart_state(workspace: &Path, workers: usize) {
    let agents = worker_ids(workers)
        .into_iter()
        .map(|worker| {
            (
                worker.clone(),
                json!({
                    "status": "running",
                    "provider": "fake",
                    "agent_id": worker,
                    "model": "fake",
                    "auth_mode": "subscription",
                    "window": worker,
                    "pane_id": format!("%old-{worker}"),
                    "spawn_epoch": 0,
                    "spawn_cwd": workspace.to_string_lossy(),
                }),
            )
        })
        .collect::<serde_json::Map<String, Value>>();
    save_runtime_state(
        workspace,
        &json!({
            "active_team_key": TEAM,
            "team_key": TEAM,
            "workspace": workspace.to_string_lossy(),
            "team_dir": workspace.to_string_lossy(),
            "spec_path": runtime_spec_path(workspace, TEAM).to_string_lossy(),
            "session_name": TEAM_SESSION,
            "tmux_endpoint": TMUX_ENDPOINT,
            "tmux_socket": TMUX_ENDPOINT,
            "agents": Value::Object(agents.clone()),
            "teams": {
                TEAM: {
                    "team_key": TEAM,
                    "workspace": workspace.to_string_lossy(),
                    "team_dir": workspace.to_string_lossy(),
                    "spec_path": runtime_spec_path(workspace, TEAM).to_string_lossy(),
                    "session_name": TEAM_SESSION,
                    "tmux_endpoint": TMUX_ENDPOINT,
                    "tmux_socket": TMUX_ENDPOINT,
                    "agents": Value::Object(agents),
                }
            }
        }),
    )
    .expect("seed runtime state");
}

fn seed_healthy_coordinator(workspace: &Path) {
    fs::create_dir_all(runtime_dir(workspace)).expect("create runtime dir");
    let _store = MessageStore::open(workspace).expect("create message store schema");
    let wp = WorkspacePath::new(workspace.to_path_buf());
    let pid = Pid::new(std::process::id());
    team_agent::coordinator::write_coordinator_metadata(&wp, pid, MetadataSource::Boot)
        .expect("write coordinator metadata");
    fs::write(
        team_agent::coordinator::coordinator_pid_path(&wp),
        pid.to_string(),
    )
    .expect("write coordinator pid");
}

fn worker_ids(workers: usize) -> Vec<String> {
    (1..=workers).map(|i| format!("w{i}")).collect()
}

#[derive(Clone, Debug)]
struct SpawnCall {
    kind: &'static str,
    window: String,
    start: Instant,
    end: Instant,
}

#[derive(Default)]
struct TransportState {
    session_present: bool,
    calls: Vec<SpawnCall>,
    panes: BTreeMap<String, PaneInfo>,
    next_pane: usize,
}

#[derive(Clone)]
struct StartupLatencyTransport {
    state: Arc<Mutex<TransportState>>,
    spawn_delay: Duration,
    fail_agent: Option<String>,
    session_missing_for_readiness: bool,
}

impl StartupLatencyTransport {
    fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(TransportState {
                next_pane: 1,
                ..TransportState::default()
            })),
            spawn_delay: Duration::ZERO,
            fail_agent: None,
            session_missing_for_readiness: false,
        }
    }

    fn with_spawn_delay(mut self, delay: Duration) -> Self {
        self.spawn_delay = delay;
        self
    }

    fn with_spawn_failure(mut self, agent: &str, message: &str) -> Self {
        self.fail_agent = Some(format!("{agent}:{message}"));
        self
    }

    fn with_session_missing_for_readiness(mut self) -> Self {
        self.session_missing_for_readiness = true;
        self
    }

    fn spawn_calls(&self) -> Vec<SpawnCall> {
        self.state.lock().unwrap().calls.clone()
    }

    fn spawn(
        &self,
        kind: &'static str,
        session: &SessionName,
        window: &WindowName,
    ) -> Result<SpawnResult, TransportError> {
        let start = Instant::now();
        if !self.spawn_delay.is_zero() {
            std::thread::sleep(self.spawn_delay);
        }
        let end = Instant::now();
        let mut state = self.state.lock().unwrap();
        let pane_id = PaneId::new(format!("%{}", state.next_pane));
        state.next_pane += 1;
        state.calls.push(SpawnCall {
            kind,
            window: window.as_str().to_string(),
            start,
            end,
        });
        if let Some(failure) = self.fail_agent.as_deref() {
            if failure.starts_with(&format!("{}:", window.as_str())) {
                return Err(TransportError::Subprocess {
                    argv: vec!["tmux".to_string(), kind.to_string()],
                    code: Some(1),
                    stderr: failure.to_string(),
                });
            }
        }
        state.session_present = true;
        let pane = PaneInfo {
            pane_id: pane_id.clone(),
            session: session.clone(),
            window_index: None,
            window_name: Some(window.clone()),
            pane_index: None,
            tty: None,
            current_command: Some("team-agent-fake-worker".to_string()),
            current_path: None,
            active: true,
            pane_pid: Some(10_000 + state.next_pane as u32),
            leader_env: BTreeMap::new(),
        };
        state.panes.insert(pane_id.as_str().to_string(), pane);
        Ok(SpawnResult {
            pane_id,
            session: session.clone(),
            window: window.clone(),
            child_pid: Some(10_000 + state.next_pane as u32),
        })
    }
}

impl Transport for StartupLatencyTransport {
    fn kind(&self) -> BackendKind {
        BackendKind::Tmux
    }

    fn tmux_endpoint(&self) -> Option<String> {
        Some(TMUX_ENDPOINT.to_string())
    }

    fn spawn_first(
        &self,
        session: &SessionName,
        window: &WindowName,
        _argv: &[String],
        _cwd: &Path,
        _env: &BTreeMap<String, String>,
    ) -> Result<SpawnResult, TransportError> {
        self.spawn("spawn_first", session, window)
    }

    fn spawn_into(
        &self,
        session: &SessionName,
        window: &WindowName,
        _argv: &[String],
        _cwd: &Path,
        _env: &BTreeMap<String, String>,
    ) -> Result<SpawnResult, TransportError> {
        self.spawn("spawn_into", session, window)
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
            inject_verification: InjectVerification::NoToken,
            submit_verification: SubmitVerification::EnterSentWithoutPlaceholderCheck,
            turn_verification: TurnVerification::NotRequired,
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

    fn query(&self, _target: &Target, _field: PaneField) -> Result<Option<String>, TransportError> {
        Ok(None)
    }

    fn liveness(&self, pane: &PaneId) -> Result<PaneLiveness, TransportError> {
        if self.state.lock().unwrap().panes.contains_key(pane.as_str()) {
            Ok(PaneLiveness::Live)
        } else {
            Ok(PaneLiveness::Dead)
        }
    }

    fn has_pane(&self, pane: &PaneId) -> Result<Option<bool>, TransportError> {
        Ok(Some(
            self.state.lock().unwrap().panes.contains_key(pane.as_str()),
        ))
    }

    fn list_targets(&self) -> Result<Vec<PaneInfo>, TransportError> {
        Ok(self.state.lock().unwrap().panes.values().cloned().collect())
    }

    fn has_session(&self, _session: &SessionName) -> Result<bool, TransportError> {
        if self.session_missing_for_readiness {
            return Ok(false);
        }
        Ok(self.state.lock().unwrap().session_present)
    }

    fn list_windows(&self, _session: &SessionName) -> Result<Vec<WindowName>, TransportError> {
        let state = self.state.lock().unwrap();
        if !state.session_present {
            return Ok(Vec::new());
        }
        let mut windows = BTreeSet::new();
        for pane in state.panes.values() {
            if let Some(window) = pane.window_name.as_ref() {
                windows.insert(window.clone());
            }
        }
        Ok(windows.into_iter().collect())
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
        let mut state = self.state.lock().unwrap();
        state.session_present = false;
        state.panes.clear();
        Ok(())
    }

    fn kill_window(&self, target: &Target) -> Result<(), TransportError> {
        if let Target::Pane(pane) = target {
            self.state.lock().unwrap().panes.remove(pane.as_str());
        }
        Ok(())
    }

    fn attach_session(&self, _session: &SessionName) -> Result<AttachOutcome, TransportError> {
        Ok(AttachOutcome::Attached)
    }
}

fn has_overlap(calls: &[&SpawnCall]) -> bool {
    calls.iter().enumerate().any(|(index, left)| {
        calls
            .iter()
            .skip(index + 1)
            .any(|right| left.start < right.end && right.start < left.end)
    })
}

fn assert_phase_events(events: &[Value], event_kind: &str, expected_phases: &[&str]) {
    let phases = events
        .iter()
        .filter(|event| event_name(event) == Some(event_kind))
        .collect::<Vec<_>>();
    for phase in expected_phases {
        assert!(
            phases
                .iter()
                .any(|event| event.get("phase").and_then(Value::as_str) == Some(*phase)),
            "{event_kind} missing phase `{phase}`; events={events:?}"
        );
    }
    let mut last = 0;
    for event in phases {
        let elapsed = event
            .get("elapsed_ms")
            .and_then(Value::as_u64)
            .unwrap_or_else(|| panic!("{event_kind} event missing numeric elapsed_ms: {event}"));
        assert!(
            elapsed >= last,
            "{event_kind} elapsed_ms must be monotonic; previous={last} event={event}"
        );
        last = elapsed;
    }
}

fn assert_worker_spawn_timing_events(events: &[Value], workers: &[String], source: &str) {
    for worker in workers {
        let event = events
            .iter()
            .find(|event| {
                event_name(event) == Some("worker.spawn_timing")
                    && event.get("agent_id").and_then(Value::as_str) == Some(worker.as_str())
                    && event.get("source").and_then(Value::as_str) == Some(source)
            })
            .unwrap_or_else(|| {
                panic!("worker.spawn_timing missing for {source}:{worker}; events={events:?}")
            });
        for field in [
            "elapsed_ms",
            "command_plan_ms",
            "transport_spawn_ms",
            "pane_verify_ms",
            "startup_prompt_handler_ms",
        ] {
            assert!(
                event.get(field).and_then(Value::as_u64).is_some(),
                "worker.spawn_timing {source}:{worker} missing numeric {field}; event={event}"
            );
        }
        assert!(
            event
                .get("tmux_start_mode")
                .and_then(Value::as_str)
                .is_some(),
            "worker.spawn_timing {source}:{worker} must carry tmux_start_mode; event={event}"
        );
    }
}

fn assert_no_fake_ready_events(events: &[Value]) {
    let forbidden = events
        .iter()
        .filter_map(event_name)
        .filter(|name| {
            matches!(
                *name,
                "provider.ready" | "provider.prompt_ready" | "provider.fake_ready"
            )
        })
        .collect::<Vec<_>>();
    assert!(
        forbidden.is_empty(),
        "latency instrumentation must not invent provider ready/fake-ready events for recording transport; forbidden={forbidden:?} events={events:?}"
    );
}

fn event_name(event: &Value) -> Option<&str> {
    event.get("event").and_then(Value::as_str)
}
