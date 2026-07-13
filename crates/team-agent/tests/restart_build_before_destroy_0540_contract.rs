//! 0.5.40 RED contract: restart builds replacement workers before destroying
//! the live worker session.
//!
//! Reference: `.team/artifacts/tmux-server-death-locate.md` §7 Slice 3.

#![cfg(unix)]
#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

#[path = "support/hermetic.rs"]
mod hermetic_guard;
#[allow(dead_code)]
fn _hermetic_boundary_marker(_: &hermetic_guard::HermeticTestEnv) {}

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use serial_test::serial;
use team_agent::coordinator::{MetadataSource, Pid, WorkspacePath};
use team_agent::event_log::EventLog;
use team_agent::lifecycle::{
    restart_with_transport, restart_with_transport_with_readiness_deadline, RestartReport,
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
const TMUX_ENDPOINT: &str = "/Volumes/nvme/tmp/ta-0540-build-before-destroy.sock";

#[test]
#[serial(env)]
fn recording_restart_failure_does_not_kill_live_session_before_replacement_is_viable() {
    let case = RestartCase::new(
        "recording-no-teardown-first",
        worker_ids(3),
        ProviderShape::Fake,
    );
    let transport = BuildBeforeDestroyTransport::recording(worker_ids(3))
        .with_spawn_failure("w1", "injected replacement spawn failure");

    let report = restart_with_transport(&case.workspace, true, Some(TEAM), &transport)
        .expect("restart should return a typed report");

    assert_restart_did_not_succeed(&report);
    assert_no_original_session_kill(
        &transport.ops(),
        "R1: restart must not destroy the live worker session before a replacement cohort is minimally viable",
    );
}

#[test]
#[serial(env)]
fn real_tmux_restart_failure_keeps_original_session_windows_and_panes_injectable() {
    let case =
        RestartCase::new_unseeded("real-tmux-preserve-old", worker_ids(3), ProviderShape::Fake);
    let tmux = RealTmuxTeam::start(case.env.root(), worker_ids(3));
    case.seed_state_with_panes(ProviderShape::Fake, &tmux.panes_by_worker());
    let transport = BuildBeforeDestroyTransport::real_tmux(tmux.socket.clone(), worker_ids(3))
        .with_spawn_failure("w1", "injected replacement spawn failure");

    let result = restart_with_transport_with_readiness_deadline(
        &case.workspace,
        true,
        Some(TEAM),
        &transport,
        Some(1),
    );
    if let Ok(report) = result.as_ref() {
        assert_restart_did_not_succeed(report);
    }
    tmux.assert_original_windows_present(&["w1", "w2", "w3"]);
    tmux.assert_original_panes_injectable("TA0540_ORIGINAL_PANE_STILL_INJECTABLE");
    assert_no_original_session_kill(
        &transport.ops(),
        "R2: even with real tmux, a failed replacement spawn must leave the original session untouched",
    );
    assert!(
        result.is_ok(),
        "R2: failed replacement build should return a typed failed/refused report after preserving the original session, not an unstructured readiness error; result={result:?}"
    );
}

#[test]
#[serial(env)]
fn server_exited_during_replacement_spawn_does_not_cascade_session_disappeared_against_old_team() {
    let case = RestartCase::new(
        "case-b-no-second-cascade",
        worker_ids(2),
        ProviderShape::CodexResumable,
    );
    let transport =
        BuildBeforeDestroyTransport::recording(worker_ids(2)).with_server_exit_after_first_spawn();

    let report = restart_with_transport(&case.workspace, false, Some(TEAM), &transport)
        .expect("restart should return a typed report");

    assert_restart_did_not_succeed(&report);
    let report_text = format!("{report:?}");
    let events = case.events_text();
    assert!(
        !report_text.contains("session_disappeared_after_spawn")
            && !events.contains("session_disappeared_after_spawn"),
        "R3: server_exited during replacement build should return failed/refused directly, not a second session_disappeared_after_spawn cascade; report={report_text} events={events}"
    );
    assert_no_original_session_kill(
        &transport.ops(),
        "R3: server_exited during replacement build must not first destroy the original session",
    );
    let state = case.read_state();
    assert_eq!(
        state.pointer("/agents/w1/status").and_then(Value::as_str),
        Some("running"),
        "R3: failed replacement build must leave old worker state authoritative; state={state}"
    );
    assert_eq!(
        state.pointer("/agents/w1/pane_id").and_then(Value::as_str),
        Some("%old-w1"),
        "R3: failed replacement build must not overwrite the old worker pane binding; state={state}"
    );
}

struct RestartCase {
    env: hermetic_guard::HermeticTestEnv,
    workspace: PathBuf,
    workers: Vec<String>,
}

impl RestartCase {
    fn new(tag: &str, workers: Vec<String>, provider: ProviderShape) -> Self {
        let case = Self::new_unseeded(tag, workers, provider);
        let panes = case
            .workers
            .iter()
            .map(|worker| (worker.clone(), format!("%old-{worker}")))
            .collect::<BTreeMap<_, _>>();
        case.seed_state_with_panes(provider, &panes);
        case
    }

    fn new_unseeded(tag: &str, workers: Vec<String>, provider: ProviderShape) -> Self {
        let env = hermetic_guard::HermeticTestEnv::enter(tag);
        env.scrub_tmux();
        env.assert_no_real_tmux();
        let workspace = env.workspace(tag);
        write_team_docs(&workspace, &workers, provider);
        write_runtime_spec(&workspace, &workspace);
        Self {
            env,
            workspace,
            workers,
        }
    }

    fn seed_state_with_panes(&self, provider: ProviderShape, panes: &BTreeMap<String, String>) {
        seed_restart_state(&self.workspace, &self.workers, provider, panes);
        seed_healthy_coordinator(&self.workspace);
    }

    fn read_state(&self) -> Value {
        load_runtime_state(&self.workspace).expect("load runtime state")
    }

    fn events_text(&self) -> String {
        EventLog::new(&self.workspace)
            .tail(0)
            .expect("read events")
            .into_iter()
            .map(|event| event.to_string())
            .collect::<Vec<_>>()
            .join("\n")
    }
}

#[derive(Clone, Copy)]
enum ProviderShape {
    Fake,
    CodexResumable,
}

impl ProviderShape {
    fn provider(self) -> &'static str {
        match self {
            Self::Fake => "fake",
            Self::CodexResumable => "codex",
        }
    }

    fn model(self) -> &'static str {
        match self {
            Self::Fake => "fake",
            Self::CodexResumable => "gpt-5.5",
        }
    }
}

fn write_team_docs(team_dir: &Path, workers: &[String], provider: ProviderShape) {
    fs::create_dir_all(team_dir.join("agents")).expect("create agents dir");
    fs::write(
        team_dir.join("TEAM.md"),
        format!(
            "---\nname: {TEAM}\nobjective: Restart build-before-destroy contract.\nprovider: {}\n---\n\nTeam.\n",
            provider.provider()
        ),
    )
    .expect("write TEAM.md");
    for worker in workers {
        fs::write(
            team_dir.join("agents").join(format!("{worker}.md")),
            format!(
                "---\nname: {worker}\nrole: Worker {worker}\nprovider: {}\nmodel: {}\nauth_mode: subscription\ntools:\n  - mcp_team\n---\n\n{worker}.\n",
                provider.provider(),
                provider.model()
            ),
        )
        .expect("write role doc");
    }
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

fn seed_restart_state(
    workspace: &Path,
    workers: &[String],
    provider: ProviderShape,
    panes: &BTreeMap<String, String>,
) {
    let agents = workers
        .iter()
        .map(|worker| {
            let mut row = json!({
                "status": "running",
                "provider": provider.provider(),
                "agent_id": worker,
                "role": format!("Worker {worker}"),
                "model": provider.model(),
                "auth_mode": "subscription",
                "tools": ["mcp_team"],
                "window": worker,
                "pane_id": panes.get(worker).cloned().unwrap_or_else(|| format!("%old-{worker}")),
                "owner_team_id": TEAM,
                "spawn_epoch": 0,
                "spawn_cwd": workspace.to_string_lossy(),
                "spawned_at": "2026-07-14T00:00:00+00:00",
            });
            if matches!(provider, ProviderShape::CodexResumable) {
                let rollout = workspace.join(format!("{worker}.jsonl"));
                fs::write(&rollout, "{}\n").expect("write rollout fixture");
                let row_obj = row.as_object_mut().expect("agent row object");
                row_obj.insert("session_id".to_string(), json!(format!("session-{worker}")));
                row_obj.insert(
                    "rollout_path".to_string(),
                    json!(rollout.to_string_lossy().to_string()),
                );
                row_obj.insert(
                    "first_send_at".to_string(),
                    json!("2026-07-14T00:01:00+00:00"),
                );
            }
            (worker.clone(), row)
        })
        .collect::<serde_json::Map<String, Value>>();
    let spec_path = runtime_spec_path(workspace, TEAM);
    save_runtime_state(
        workspace,
        &json!({
            "active_team_key": TEAM,
            "team_key": TEAM,
            "workspace": workspace.to_string_lossy(),
            "team_dir": workspace.to_string_lossy(),
            "spec_path": spec_path.to_string_lossy(),
            "session_name": TEAM_SESSION,
            "tmux_endpoint": TMUX_ENDPOINT,
            "tmux_socket": TMUX_ENDPOINT,
            "agents": Value::Object(agents.clone()),
            "teams": {
                TEAM: {
                    "team_key": TEAM,
                    "workspace": workspace.to_string_lossy(),
                    "team_dir": workspace.to_string_lossy(),
                    "spec_path": spec_path.to_string_lossy(),
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

fn worker_ids(count: usize) -> Vec<String> {
    (1..=count).map(|index| format!("w{index}")).collect()
}

fn assert_restart_did_not_succeed(report: &RestartReport) {
    assert!(
        !matches!(report, RestartReport::Restarted { .. }),
        "fixture injects replacement failure; restart must not report success: {report:?}"
    );
}

fn assert_no_original_session_kill(ops: &[String], context: &str) {
    let forbidden = format!("kill_session:{TEAM_SESSION}");
    assert!(
        !ops.iter().any(|op| op == &forbidden),
        "{context}; ops={ops:?}"
    );
}

#[derive(Clone)]
struct BuildBeforeDestroyTransport {
    state: Arc<Mutex<TransportState>>,
    real_socket: Option<PathBuf>,
    fail_agent: Option<String>,
    server_exit_after_first_spawn: bool,
}

#[derive(Default)]
struct TransportState {
    session_present: bool,
    ops: Vec<String>,
    panes: BTreeMap<String, PaneInfo>,
    next_pane: usize,
    spawns: usize,
}

impl BuildBeforeDestroyTransport {
    fn recording(workers: Vec<String>) -> Self {
        Self {
            state: Arc::new(Mutex::new(TransportState {
                session_present: true,
                panes: pane_infos_from_ids(
                    workers
                        .into_iter()
                        .map(|worker| (worker.clone(), format!("%old-{worker}")))
                        .collect(),
                ),
                next_pane: 1,
                ..TransportState::default()
            })),
            real_socket: None,
            fail_agent: None,
            server_exit_after_first_spawn: false,
        }
    }

    fn real_tmux(socket: PathBuf, workers: Vec<String>) -> Self {
        Self {
            state: Arc::new(Mutex::new(TransportState {
                session_present: true,
                panes: pane_infos_from_ids(
                    workers
                        .into_iter()
                        .map(|worker| (worker.clone(), format!("%old-{worker}")))
                        .collect(),
                ),
                next_pane: 1,
                ..TransportState::default()
            })),
            real_socket: Some(socket),
            fail_agent: None,
            server_exit_after_first_spawn: false,
        }
    }

    fn with_spawn_failure(mut self, agent: &str, message: &str) -> Self {
        self.fail_agent = Some(format!("{agent}:{message}"));
        self
    }

    fn with_server_exit_after_first_spawn(mut self) -> Self {
        self.server_exit_after_first_spawn = true;
        self
    }

    fn ops(&self) -> Vec<String> {
        self.state.lock().unwrap().ops.clone()
    }

    fn spawn(
        &self,
        kind: &'static str,
        session: &SessionName,
        window: &WindowName,
    ) -> Result<SpawnResult, TransportError> {
        let mut state = self.state.lock().unwrap();
        state
            .ops
            .push(format!("{kind}:{}:{}", session.as_str(), window.as_str()));
        state.spawns += 1;
        if let Some(failure) = self.fail_agent.as_deref() {
            if failure.starts_with(&format!("{}:", window.as_str())) {
                return Err(TransportError::Subprocess {
                    argv: vec!["tmux".to_string(), kind.to_string()],
                    code: Some(1),
                    stderr: failure.to_string(),
                });
            }
        }
        let pane_id = PaneId::new(format!("%new-{}", state.next_pane));
        state.next_pane += 1;
        state.session_present = true;
        state.panes.insert(
            pane_id.as_str().to_string(),
            pane_info(session.clone(), window.clone(), pane_id.clone()),
        );
        if self.server_exit_after_first_spawn && state.spawns == 1 {
            state.session_present = false;
        }
        Ok(SpawnResult {
            pane_id,
            session: session.clone(),
            window: window.clone(),
            child_pid: None,
        })
    }

    fn real_tmux_has_session(&self, session: &SessionName) -> Result<bool, TransportError> {
        let Some(socket) = self.real_socket.as_ref() else {
            return Ok(self.state.lock().unwrap().session_present);
        };
        Ok(Command::new("tmux")
            .args([
                "-S",
                socket.to_str().unwrap(),
                "has-session",
                "-t",
                session.as_str(),
            ])
            .output()
            .map_err(|error| TransportError::MuxUnavailable {
                backend: BackendKind::Tmux,
                detail: error.to_string(),
            })?
            .status
            .success())
    }

    fn real_tmux_list_panes(&self) -> Result<Vec<PaneInfo>, TransportError> {
        let Some(socket) = self.real_socket.as_ref() else {
            return Ok(self.state.lock().unwrap().panes.values().cloned().collect());
        };
        let output = Command::new("tmux")
            .args([
                "-S",
                socket.to_str().unwrap(),
                "list-panes",
                "-a",
                "-F",
                "#{session_name}\t#{window_name}\t#{pane_id}\t#{pane_pid}",
            ])
            .output()
            .map_err(|error| TransportError::MuxUnavailable {
                backend: BackendKind::Tmux,
                detail: error.to_string(),
            })?;
        if !output.status.success() {
            return Ok(Vec::new());
        }
        let text = String::from_utf8_lossy(&output.stdout);
        Ok(text
            .lines()
            .filter_map(|line| {
                let mut fields = line.split('\t');
                let session = fields.next()?;
                let window = fields.next()?;
                let pane = fields.next()?;
                let pid = fields.next().and_then(|raw| raw.parse::<u32>().ok());
                Some(PaneInfo {
                    pane_id: PaneId::new(pane),
                    session: SessionName::new(session),
                    window_index: None,
                    window_name: Some(WindowName::new(window)),
                    pane_index: None,
                    tty: None,
                    current_command: None,
                    current_path: None,
                    active: true,
                    pane_pid: pid,
                    leader_env: BTreeMap::new(),
                })
            })
            .collect())
    }
}

impl Transport for BuildBeforeDestroyTransport {
    fn kind(&self) -> BackendKind {
        BackendKind::Tmux
    }

    fn tmux_endpoint(&self) -> Option<String> {
        self.real_socket
            .as_ref()
            .map(|socket| socket.to_string_lossy().to_string())
            .or_else(|| Some(TMUX_ENDPOINT.to_string()))
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
        Ok(inject_report())
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
        Ok(match field {
            PaneField::PaneId => Some("%new-1".to_string()),
            PaneField::SessionName => Some(TEAM_SESSION.to_string()),
            _ => None,
        })
    }

    fn liveness(&self, pane: &PaneId) -> Result<PaneLiveness, TransportError> {
        Ok(
            if self
                .real_tmux_list_panes()?
                .iter()
                .any(|info| info.pane_id.as_str() == pane.as_str())
                || self.state.lock().unwrap().panes.contains_key(pane.as_str())
            {
                PaneLiveness::Live
            } else {
                PaneLiveness::Dead
            },
        )
    }

    fn has_pane(&self, pane: &PaneId) -> Result<Option<bool>, TransportError> {
        Ok(Some(
            self.real_tmux_list_panes()?
                .iter()
                .any(|info| info.pane_id.as_str() == pane.as_str())
                || self.state.lock().unwrap().panes.contains_key(pane.as_str()),
        ))
    }

    fn list_targets(&self) -> Result<Vec<PaneInfo>, TransportError> {
        self.real_tmux_list_panes()
    }

    fn has_session(&self, session: &SessionName) -> Result<bool, TransportError> {
        let live = self.real_tmux_has_session(session)?;
        self.state
            .lock()
            .unwrap()
            .ops
            .push(format!("has_session:{}={live}", session.as_str()));
        Ok(live)
    }

    fn list_windows(&self, session: &SessionName) -> Result<Vec<WindowName>, TransportError> {
        if self.real_socket.is_some() {
            let mut windows = BTreeSet::new();
            for pane in self.real_tmux_list_panes()? {
                if pane.session.as_str() == session.as_str() {
                    if let Some(window) = pane.window_name {
                        windows.insert(window);
                    }
                }
            }
            return Ok(windows.into_iter().collect());
        }
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

    fn kill_session(&self, session: &SessionName) -> Result<(), TransportError> {
        self.state
            .lock()
            .unwrap()
            .ops
            .push(format!("kill_session:{}", session.as_str()));
        if session.as_str() == TEAM_SESSION {
            self.state.lock().unwrap().session_present = false;
            self.state.lock().unwrap().panes.clear();
            if let Some(socket) = self.real_socket.as_ref() {
                let status = Command::new("tmux")
                    .args([
                        "-S",
                        socket.to_str().unwrap(),
                        "kill-session",
                        "-t",
                        session.as_str(),
                    ])
                    .status()
                    .map_err(|error| TransportError::MuxUnavailable {
                        backend: BackendKind::Tmux,
                        detail: error.to_string(),
                    })?;
                if !status.success() {
                    return Err(TransportError::Subprocess {
                        argv: vec![
                            "tmux".to_string(),
                            "-S".to_string(),
                            socket.to_string_lossy().to_string(),
                            "kill-session".to_string(),
                            "-t".to_string(),
                            session.as_str().to_string(),
                        ],
                        code: status.code(),
                        stderr: "kill-session failed".to_string(),
                    });
                }
            }
        }
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

fn pane_infos_from_ids(ids: BTreeMap<String, String>) -> BTreeMap<String, PaneInfo> {
    ids.into_iter()
        .map(|(worker, pane)| {
            let pane_id = PaneId::new(pane);
            (
                pane_id.as_str().to_string(),
                pane_info(
                    SessionName::new(TEAM_SESSION),
                    WindowName::new(worker),
                    pane_id,
                ),
            )
        })
        .collect()
}

fn pane_info(session: SessionName, window: WindowName, pane_id: PaneId) -> PaneInfo {
    PaneInfo {
        pane_id,
        session,
        window_index: None,
        window_name: Some(window),
        pane_index: None,
        tty: None,
        current_command: Some("team-agent-worker".to_string()),
        current_path: None,
        active: true,
        pane_pid: Some(10_000),
        leader_env: BTreeMap::new(),
    }
}

fn inject_report() -> InjectReport {
    InjectReport {
        stage_reached: InjectStage::Submit,
        inject_verification: InjectVerification::NoToken,
        submit_verification: SubmitVerification::EnterSentWithoutPlaceholderCheck,
        turn_verification: TurnVerification::NotRequired,
        attempts: 1,
        submit_diagnostics: None,
    }
}

struct RealTmuxTeam {
    socket: PathBuf,
    panes: BTreeMap<String, String>,
}

impl RealTmuxTeam {
    fn start(root: &Path, workers: Vec<String>) -> Self {
        let socket = root.join("ta-0540-real-tmux.sock");
        let _ = Command::new("tmux")
            .args(["-S", socket.to_str().unwrap(), "kill-server"])
            .output();
        let mut iter = workers.into_iter();
        let first = iter.next().expect("at least one worker");
        run_tmux(
            &socket,
            &[
                "new-session",
                "-d",
                "-s",
                TEAM_SESSION,
                "-n",
                &first,
                "while :; do sleep 60; done",
            ],
        );
        for worker in iter {
            run_tmux(
                &socket,
                &[
                    "new-window",
                    "-t",
                    TEAM_SESSION,
                    "-n",
                    &worker,
                    "while :; do sleep 60; done",
                ],
            );
        }
        let panes = list_tmux_panes_by_worker(&socket);
        Self { socket, panes }
    }

    fn panes_by_worker(&self) -> BTreeMap<String, String> {
        self.panes.clone()
    }

    fn assert_original_windows_present(&self, expected: &[&str]) {
        let output = Command::new("tmux")
            .args([
                "-S",
                self.socket.to_str().unwrap(),
                "list-windows",
                "-t",
                TEAM_SESSION,
                "-F",
                "#{window_name}",
            ])
            .output()
            .expect("list real tmux windows");
        assert!(
            output.status.success(),
            "original tmux session must still exist after failed restart; stderr={}",
            String::from_utf8_lossy(&output.stderr)
        );
        let actual = String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(str::to_string)
            .collect::<BTreeSet<_>>();
        for window in expected {
            assert!(
                actual.contains(*window),
                "original tmux window `{window}` missing after failed restart; actual={actual:?}"
            );
        }
    }

    fn assert_original_panes_injectable(&self, token: &str) {
        for (worker, pane) in &self.panes {
            let status = Command::new("tmux")
                .args([
                    "-S",
                    self.socket.to_str().unwrap(),
                    "send-keys",
                    "-t",
                    pane,
                    &format!("printf {token}_{worker}"),
                    "Enter",
                ])
                .status()
                .expect("send keys to original pane");
            assert!(
                status.success(),
                "original pane {pane} for {worker} must remain addressable/injectable"
            );
        }
    }
}

impl Drop for RealTmuxTeam {
    fn drop(&mut self) {
        let _ = Command::new("tmux")
            .args(["-S", self.socket.to_str().unwrap(), "kill-server"])
            .output();
        let _ = fs::remove_file(&self.socket);
    }
}

fn run_tmux(socket: &Path, args: &[&str]) {
    let output = Command::new("tmux")
        .arg("-S")
        .arg(socket)
        .args(args)
        .output()
        .expect("run tmux");
    assert!(
        output.status.success(),
        "tmux {:?} failed: stdout={} stderr={}",
        args,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn list_tmux_panes_by_worker(socket: &Path) -> BTreeMap<String, String> {
    let output = Command::new("tmux")
        .args([
            "-S",
            socket.to_str().unwrap(),
            "list-panes",
            "-a",
            "-F",
            "#{window_name}\t#{pane_id}",
        ])
        .output()
        .expect("list tmux panes");
    assert!(
        output.status.success(),
        "list-panes failed: stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| {
            let mut fields = line.split('\t');
            Some((fields.next()?.to_string(), fields.next()?.to_string()))
        })
        .collect()
}
