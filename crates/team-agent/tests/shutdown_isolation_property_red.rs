//! P0 shutdown-isolation property red suite.
//!
//! These tests intentionally exercise the public shutdown decision path with a
//! hermetic recording transport.  They are based only on the shutdown contract:
//! foreign sessions, panes, processes, sockets, and coordinator instances are
//! never destructive targets of Team A's shutdown.
//!
//! The suite uses a deterministic bounded generator instead of an external
//! property-testing crate so the red suite stays dependency-free.  Each seed is
//! printed in its assertion context and is reproducible by test name.

#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::{BTreeMap, BTreeSet};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use team_agent::state::persist::save_runtime_state;
use team_agent::transport::{
    AttachOutcome, BackendKind, CaptureRange, CapturedText, InjectPayload, InjectReport,
    InjectStage, InjectVerification, Key, PaneField, PaneId, PaneInfo, PaneLiveness, SessionName,
    SessionOwner, SetEnvOutcome, SpawnResult, SubmitVerification, Target, Transport, TransportError,
    TurnVerification, WindowName,
};

static CASE_ID: AtomicU64 = AtomicU64::new(0);

#[test]
fn p1_same_socket_shutdown_a_never_touches_team_b_for_bounded_generated_topologies() {
    for seed in 0..8u64 {
        let case = Case::new(&format!("p1-{seed}"));
        let socket = case
            .path
            .join(format!("socket-{seed}"))
            .display()
            .to_string();
        let mut targets = vec![pane("%a", "team-a", "worker-a", 10 + seed as u32)];
        if seed % 2 == 0 {
            targets.push(pane("%a2", "team-a", "worker-a-2", 11 + seed as u32));
        }
        targets.push(pane("%b", "team-b", "worker-b", 20 + seed as u32));
        if seed % 3 == 0 {
            targets.push(pane("%b2", "team-b", "worker-b-2", 21 + seed as u32));
        }
        if seed % 2 == 0 {
            targets.reverse();
        }
        save_state(
            &case,
            json!({
                "schema_version": 1,
                "active_team_key": "team-a",
                "team_key": "team-a",
                "session_name": "team-a",
                "tmux_endpoint": socket,
                "tmux_socket": socket,
                "tmux_socket_source": if seed % 2 == 0 { "workspace" } else { "leader_env" },
                "is_external_leader": seed % 2 != 0,
                "teams": {
                    "team-a": { "team_key": "team-a", "session_name": "team-a", "tmux_socket": socket, "agents": {} },
                    "team-b": { "team_key": "team-b", "session_name": "team-b", "tmux_socket": socket, "agents": {} }
                },
                "agents": {}
            }),
        );
        let transport = RecordingTransport::with_targets(targets);
        let out = shutdown(&case, Some("team-a"), &transport);
        let killed_sessions = transport.killed_sessions();
        let remaining = transport.sessions();

        assert!(
            killed_sessions.iter().all(|name| name == "team-a"),
            "P1 seed={seed}: only Team A may be killed; actions={killed_sessions:?}, out={out}"
        );
        assert!(
            remaining.iter().any(|name| name == "team-b"),
            "P1 seed={seed}: Team B must remain on the shared socket; remaining={remaining:?}, out={out}"
        );
        assert!(
            !transport.kill_server_called(),
            "P1 seed={seed}: shared socket must not receive kill-server; out={out}"
        );
    }
}

#[test]
fn p2_shared_pgid_foreign_coordinator_survives_team_a_shutdown() {
    let case = Case::new("p2-shared-pgid");
    let mut a = spawn_group_member(None);
    let a_pgid = a.id();
    let mut foreign = spawn_group_member(Some(a_pgid));
    let foreign_pid = foreign.id();

    save_state(
        &case,
        json!({
            "schema_version": 1,
            "team_key": "team-a",
            "session_name": "team-a",
            "agents": {
                "worker-a": { "status": "running", "provider": "fake", "pid": a.id() }
            }
        }),
    );
    let transport = RecordingTransport::with_targets(vec![pane("%a", "team-a", "worker-a", 1)]);
    let out = shutdown(&case, Some("team-a"), &transport);
    let foreign_alive = child_alive(&mut foreign);

    terminate_child(&mut a);
    terminate_child(&mut foreign);

    assert!(
        foreign_alive,
        "P2 foreign coordinator pid={foreign_pid} shared with Team A pgid must survive; out={out}"
    );
}

#[test]
fn p3_nested_tmux_host_endpoint_is_never_a_shutdown_authority() {
    for seed in 0..4u64 {
        let case = Case::new(&format!("p3-nested-{seed}"));
        let host_socket = case.path.join("host.sock").display().to_string();
        let worker_socket = case.path.join("worker.sock").display().to_string();
        let targets = vec![
            pane("%a", "team-a", "worker-a", 10),
            pane("%host", "host-team", "shell", 11),
            pane("%b", "team-b", "worker-b", 12),
        ];
        save_state(
            &case,
            json!({
                "schema_version": 1,
                "team_key": "team-a",
                "session_name": "team-a",
                "tmux_endpoint": worker_socket,
                "tmux_socket": worker_socket,
                "tmux_socket_source": "workspace",
                "is_external_leader": false,
                "leader_receiver": { "pane_id": "%host", "tmux_socket": host_socket },
                "teams": {
                    "team-a": {
                        "team_key": "team-a", "session_name": "team-a",
                        "tmux_socket": worker_socket,
                        "agents": {}
                    },
                    "team-b": {
                        "team_key": "team-b", "session_name": "team-b",
                        "tmux_socket": host_socket,
                        "agents": {}
                    }
                },
                "agents": {}
            }),
        );
        let transport = RecordingTransport::with_targets(targets);
        let out = shutdown(&case, Some("team-a"), &transport);
        let remaining = transport.sessions();
        assert!(
            remaining.contains("host-team") && remaining.contains("team-b"),
            "P3 seed={seed}: host and sibling sessions must remain; remaining={remaining:?}, out={out}"
        );
        assert!(
            !transport.kill_server_called(),
            "P3 seed={seed}: nested TMUX must never authorize host kill-server; out={out}"
        );
    }
}

#[test]
fn p4_destructive_actions_require_exact_positive_identity_and_no_pgid_broadcast() {
    let source = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/cli/mod.rs"))
        .expect("read shutdown implementation for the public no-broadcast contract");
    assert!(
        !source.contains("reap_process_groups(")
            && !source.contains("terminate_group("),
        "P4 shutdown must not use process-group broadcast termination"
    );

    let case = Case::new("p4-exact-target");
    save_state(
        &case,
        json!({
            "schema_version": 1,
            "team_key": "team-a",
            "session_name": "team-a",
            "tmux_socket": case.path.join("a.sock").display().to_string(),
            "agents": {}
        }),
    );
    let transport = RecordingTransport::with_targets(vec![
        pane("%a", "team-a", "worker", 1),
        pane("%ab", "team-ab", "worker", 2),
        pane("%prefix", "team-agent-leader-team-a", "leader", 3),
    ]);
    let out = shutdown(&case, Some("team-a"), &transport);
    assert_eq!(
        transport.killed_sessions(),
        vec!["team-a"],
        "P4 prefix/similar names are not exact targets; out={out}"
    );
    assert!(
        transport.sessions().contains("team-ab"),
        "P4 foreign prefix collision must remain alive; out={out}"
    );
}

#[test]
fn p5_probe_failure_is_refusal_not_empty_success_or_destructive_cleanup() {
    let case = Case::new("p5-probe-failure");
    save_state(
        &case,
        json!({
            "schema_version": 1,
            "team_key": "team-a",
            "session_name": "team-a",
            "tmux_socket": case.path.join("a.sock").display().to_string(),
            "agents": {}
        }),
    );
    let transport = RecordingTransport::with_targets(vec![pane("%a", "team-a", "worker", 1)])
        .with_list_targets_failure();
    let out = shutdown(&case, Some("team-a"), &transport);
    assert!(
        out["ok"] == json!(false) || out["status"] != json!("ok"),
        "P5 list/probe failure must be explicit refusal/partial, not success; out={out}"
    );
    assert!(
        transport.killed_sessions().is_empty() && !transport.kill_server_called(),
        "P5 failed observation must authorize no destructive action; out={out}"
    );
}

#[test]
fn p6_protected_leader_pane_survives_same_session_worker_shutdown() {
    let case = Case::new("p6-protected-leader");
    save_state(
        &case,
        json!({
            "schema_version": 1,
            "team_key": "team-a",
            "session_name": "team-a",
            "tmux_socket": case.path.join("a.sock").display().to_string(),
            "team_owner": { "pane_id": "%leader" },
            "agents": {
                "worker": { "status": "running", "provider": "fake", "window": "worker", "pane_id": "%worker" }
            }
        }),
    );
    let transport = RecordingTransport::with_targets(vec![
        pane("%leader", "team-a", "leader", 1),
        pane("%worker", "team-a", "worker", 2),
    ]);
    let out = shutdown(&case, Some("team-a"), &transport);
    assert!(
        transport.killed_windows().iter().any(|target| target == "pane:%worker"),
        "P6 worker pane must be the precise target; actions={:?}, out={out}",
        transport.killed_windows()
    );
    assert!(
        !transport.killed_windows().iter().any(|target| target == "pane:%leader")
            && !transport.killed_sessions().contains(&"team-a".to_string()),
        "P6 protected leader must survive without session-wide kill; actions={:?}, out={out}",
        transport.killed_windows()
    );
}

#[test]
fn p7_missing_or_unbound_coordinator_metadata_is_fail_closed() {
    let case = Case::new("p7-coordinator-metadata");
    let mut foreign = spawn_group_member(None);
    let foreign_pid = foreign.id();
    let runtime = case.path.join(".team/runtime");
    std::fs::create_dir_all(&runtime).unwrap();
    std::fs::write(runtime.join("coordinator.pid"), format!("{foreign_pid}\n")).unwrap();
    save_state(
        &case,
        json!({
            "schema_version": 1,
            "team_key": "team-a",
            "session_name": "team-a",
            "agents": {}
        }),
    );
    let transport = RecordingTransport::with_targets(vec![pane("%a", "team-a", "worker", 1)]);
    let out = shutdown(&case, Some("team-a"), &transport);
    let foreign_alive = child_alive(&mut foreign);
    terminate_child(&mut foreign);

    assert!(
        foreign_alive,
        "P7 pid-only coordinator discovery must not terminate an unbound pid={foreign_pid}; out={out}"
    );
}

#[test]
fn p8_scoped_sibling_and_generation_are_preserved() {
    for seed in 0..6u64 {
        let case = Case::new(&format!("p8-sibling-{seed}"));
        let socket = case.path.join("shared.sock").display().to_string();
        let mut targets = vec![pane("%a", "team-a", "worker", 1), pane("%b", "team-b", "worker", 2)];
        if seed % 2 == 0 {
            targets.push(pane("%b2", "team-b", "worker-2", 3));
        }
        save_state(
            &case,
            json!({
                "schema_version": 1,
                "active_team_key": "team-a",
                "teams": {
                    "team-a": { "team_key": "team-a", "session_name": "team-a", "tmux_socket": socket, "generation": seed + 1, "agents": {} },
                    "team-b": { "team_key": "team-b", "session_name": "team-b", "tmux_socket": socket, "generation": seed + 2, "agents": {} }
                },
                "team_key": "team-a",
                "session_name": "team-a",
                "tmux_socket": socket,
                "agents": {}
            }),
        );
        let transport = RecordingTransport::with_targets(targets);
        let out = shutdown(&case, Some("team-a"), &transport);
        assert!(
            transport.sessions().contains("team-b"),
            "P8 seed={seed}: sibling and its generation must survive; out={out}"
        );
        assert!(
            !transport.kill_server_called(),
            "P8 seed={seed}: shared sibling socket must not be globally killed; out={out}"
        );
    }
}

#[test]
fn p9_normal_owned_team_is_not_a_noop_and_second_shutdown_is_idempotent() {
    let case = Case::new("p9-idempotent");
    save_state(
        &case,
        json!({
            "schema_version": 1,
            "team_key": "team-a",
            "session_name": "team-a",
            "tmux_socket": case.path.join("a.sock").display().to_string(),
            "agents": {}
        }),
    );
    let transport = RecordingTransport::with_targets(vec![pane("%a", "team-a", "worker", 1)]);
    let first = shutdown(&case, Some("team-a"), &transport);
    assert_eq!(
        transport.killed_sessions(),
        vec!["team-a"],
        "P9 first shutdown must actually stop owned A; out={first}"
    );
    let second = shutdown(&case, Some("team-a"), &transport);
    assert!(
        second["ok"] == json!(true) || second["status"] == json!("ok"),
        "P9 second shutdown must be explicit idempotent success; out={second}"
    );
}

#[test]
fn p10_kill_set_is_monotone_under_foreign_population_and_enumeration_order() {
    let mut expected: Option<BTreeSet<String>> = None;
    for seed in 0..10u64 {
        let case = Case::new(&format!("p10-scale-{seed}"));
        let mut targets = vec![pane("%a", "team-a", "worker", 1)];
        for idx in 0..(seed % 5) {
            targets.push(pane(
                &format!("%foreign-{idx}"),
                &format!("foreign-{idx}"),
                "worker",
                idx as u32 + 10,
            ));
        }
        if seed % 2 == 1 {
            targets.reverse();
        }
        save_state(
            &case,
            json!({
                "schema_version": 1,
                "team_key": "team-a",
                "session_name": "team-a",
                "tmux_socket": case.path.join("a.sock").display().to_string(),
                "agents": {}
            }),
        );
        let transport = RecordingTransport::with_targets(targets);
        let out = shutdown(&case, Some("team-a"), &transport);
        let killed = transport.killed_sessions().into_iter().collect::<BTreeSet<_>>();
        if let Some(previous) = &expected {
            assert_eq!(
                &killed, previous,
                "P10 seed={seed}: foreign population/order changed kill set; out={out}"
            );
        } else {
            expected = Some(killed);
        }
        let foreign_expected = seed % 5 != 0;
        assert_eq!(
            transport.sessions().iter().any(|name| name.starts_with("foreign-")),
            foreign_expected,
            "P10 seed={seed}: foreign sessions must remain when generated; out={out}"
        );
    }
}

fn shutdown(case: &Case, team: Option<&str>, transport: &RecordingTransport) -> Value {
    transport.bind_workspace(&case.path);
    team_agent::cli::lifecycle_port::shutdown_with_transport(&case.path, true, team, transport)
        .unwrap_or_else(|error| panic!("shutdown returned an unexpected API error: {error}"))
}

fn save_state(case: &Case, mut value: Value) {
    add_fixture_generations(&mut value);
    save_runtime_state(&case.path, &value).expect("save fixture runtime state");
}

fn add_fixture_generations(value: &mut Value) {
    let Some(object) = value.as_object_mut() else {
        return;
    };
    let session = object
        .get("session_name")
        .and_then(Value::as_str)
        .map(str::to_string);
    if let Some(session) = session {
        object
            .entry("generation")
            .or_insert(Value::String(session));
    }
    if let Some(teams) = object.get_mut("teams").and_then(Value::as_object_mut) {
        for team in teams.values_mut() {
            add_fixture_generations(team);
        }
    }
}

fn pane(id: &str, session: &str, window: &str, _pid: u32) -> PaneInfo {
    PaneInfo {
        pane_id: PaneId::new(id),
        session: SessionName::new(session),
        window_index: Some(0),
        window_name: Some(WindowName::new(window)),
        pane_index: Some(0),
        tty: None,
        current_command: Some("fake-worker".to_string()),
        current_path: None,
        active: true,
        pane_pid: None,
        leader_env: BTreeMap::new(),
    }
}

struct Case {
    path: PathBuf,
}

impl Case {
    fn new(tag: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "ta-shutdown-isolation-{tag}-{}-{}",
            std::process::id(),
            CASE_ID.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(path.join(".team/runtime")).expect("create fixture workspace");
        Self { path }
    }
}

impl Drop for Case {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

#[derive(Clone, Default)]
struct RecordingTransport {
    targets: Arc<Mutex<Vec<PaneInfo>>>,
    killed_sessions: Arc<Mutex<Vec<String>>>,
    killed_windows: Arc<Mutex<Vec<String>>>,
    kill_server: Arc<Mutex<bool>>,
    owner_workspace: Arc<Mutex<Option<PathBuf>>>,
    list_targets_failure: bool,
}

impl RecordingTransport {
    fn with_targets(targets: Vec<PaneInfo>) -> Self {
        Self {
            targets: Arc::new(Mutex::new(targets)),
            ..Self::default()
        }
    }

    fn with_list_targets_failure(mut self) -> Self {
        self.list_targets_failure = true;
        self
    }

    fn bind_workspace(&self, workspace: &Path) {
        *self.owner_workspace.lock().unwrap() = Some(
            workspace
                .canonicalize()
                .unwrap_or_else(|_| workspace.to_path_buf()),
        );
    }

    fn sessions(&self) -> BTreeSet<String> {
        self.targets
            .lock()
            .unwrap()
            .iter()
            .map(|pane| pane.session.as_str().to_string())
            .collect()
    }

    fn killed_sessions(&self) -> Vec<String> {
        self.killed_sessions.lock().unwrap().clone()
    }

    fn killed_windows(&self) -> Vec<String> {
        self.killed_windows.lock().unwrap().clone()
    }

    fn kill_server_called(&self) -> bool {
        *self.kill_server.lock().unwrap()
    }

    fn spawn_result(&self, session: &SessionName, window: &WindowName) -> SpawnResult {
        SpawnResult {
            pane_id: PaneId::new("%spawned"),
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
        _argv: &[String],
        _cwd: &Path,
        _env: &BTreeMap<String, String>,
    ) -> Result<SpawnResult, TransportError> {
        Ok(self.spawn_result(session, window))
    }

    fn spawn_into(
        &self,
        session: &SessionName,
        window: &WindowName,
        _argv: &[String],
        _cwd: &Path,
        _env: &BTreeMap<String, String>,
    ) -> Result<SpawnResult, TransportError> {
        Ok(self.spawn_result(session, window))
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
        Ok(CapturedText { text: String::new(), range })
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
        if self.list_targets_failure {
            return Err(TransportError::Subprocess {
                argv: vec!["list-targets".to_string()],
                code: Some(1),
                stderr: "injected probe failure".to_string(),
            });
        }
        let workspace = self.owner_workspace.lock().unwrap().clone();
        Ok(self
            .targets
            .lock()
            .unwrap()
            .iter()
            .cloned()
            .map(|mut pane| {
                if let Some(workspace) = workspace.as_ref() {
                    pane.current_path = Some(workspace.clone());
                }
                pane
            })
            .collect())
    }

    fn session_owner(
        &self,
        session: &SessionName,
    ) -> Result<Option<SessionOwner>, TransportError> {
        let Some(workspace) = self.owner_workspace.lock().unwrap().clone() else {
            return Ok(None);
        };
        Ok(Some(SessionOwner {
            workspace: workspace.to_string_lossy().into_owned(),
            team: session.as_str().to_string(),
            generation: session.as_str().to_string(),
        }))
    }

    fn has_session(&self, session: &SessionName) -> Result<bool, TransportError> {
        Ok(self
            .targets
            .lock()
            .unwrap()
            .iter()
            .any(|pane| pane.session == *session))
    }

    fn list_windows(&self, session: &SessionName) -> Result<Vec<WindowName>, TransportError> {
        Ok(self
            .targets
            .lock()
            .unwrap()
            .iter()
            .filter(|pane| pane.session == *session)
            .filter_map(|pane| pane.window_name.clone())
            .collect())
    }

    fn set_session_env(
        &self,
        _session: &SessionName,
        _key: &str,
        _value: &str,
    ) -> Result<SetEnvOutcome, TransportError> {
        Ok(SetEnvOutcome::Applied)
    }

    fn kill_server(&self) -> Result<(), TransportError> {
        *self.kill_server.lock().unwrap() = true;
        self.targets.lock().unwrap().clear();
        Ok(())
    }

    fn kill_session(&self, session: &SessionName) -> Result<(), TransportError> {
        self.killed_sessions
            .lock()
            .unwrap()
            .push(session.as_str().to_string());
        self.targets
            .lock()
            .unwrap()
            .retain(|pane| pane.session != *session);
        Ok(())
    }

    fn kill_window(&self, target: &Target) -> Result<(), TransportError> {
        let label = match target {
            Target::Pane(pane) => format!("pane:{}", pane.as_str()),
            Target::SessionWindow { session, window } => {
                format!("window:{}:{}", session.as_str(), window.as_str())
            }
        };
        self.killed_windows.lock().unwrap().push(label);
        let mut targets = self.targets.lock().unwrap();
        match target {
            Target::Pane(pane) => targets.retain(|entry| entry.pane_id != *pane),
            Target::SessionWindow { session, window } => targets.retain(|entry| {
                !(entry.session == *session && entry.window_name.as_ref() == Some(window))
            }),
        }
        Ok(())
    }

    fn attach_session(&self, _session: &SessionName) -> Result<AttachOutcome, TransportError> {
        Ok(AttachOutcome::Attached)
    }
}

fn spawn_group_member(pgid: Option<u32>) -> Child {
    let mut command = Command::new("/bin/sh");
    command.args(["-c", "trap ':' TERM; while :; do sleep 1; done"]);
    unsafe {
        command.pre_exec(move || {
            let target = pgid.map_or(0, |value| value as libc::pid_t);
            if libc::setpgid(0, target) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    command.spawn().expect("spawn fixture process-group member")
}

fn child_alive(child: &mut Child) -> bool {
    matches!(child.try_wait(), Ok(None))
}

fn terminate_child(child: &mut Child) {
    if child_alive(child) {
        let _ = child.kill();
    }
    let _ = child.wait();
}

#[allow(dead_code)]
fn _output_status(output: &Output) -> String {
    format!(
        "status={:?} stdout={} stderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

#[allow(dead_code)]
fn _value_string(value: &Value) -> String {
    value.to_string()
}

#[allow(dead_code)]
fn _workspace_marker(case: &Case) -> &Path {
    &case.path
}

#[allow(dead_code)]
fn _session_name(name: &str) -> SessionName {
    SessionName::new(name)
}

#[allow(dead_code)]
fn _map(entries: &[(&str, &str)]) -> BTreeMap<String, String> {
    entries
        .iter()
        .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
        .collect()
}

// Independent PR #218 review reproductions. Hermetic transport; only F3
// creates a test-owned sleep process. Execute on isolated Linux CI/Grok.
#[test]
fn pr218_review_f1_bare_shutdown_must_preserve_managed_leader() {
    let case = Case::new("pr218-review-f1");
    save_state(&case, json!({
        "team_key": "team-a", "active_team_key": "team-a",
        "session_name": "team-a", "is_external_leader": false,
        "tmux_socket": case.path.join("review.sock").display().to_string(),
        "team_owner": {"pane_id": "%leader"},
        "leader_receiver": {"pane_id": "%leader"}, "agents": {}
    }));
    let transport = RecordingTransport::with_targets(vec![
        pane("%leader", "team-a", "leader", 1),
    ]);
    let out = shutdown(&case, None, &transport);
    eprintln!("F1 shutdown={out}; killed_sessions={:?}; surviving_sessions={:?}",
        transport.killed_sessions(), transport.sessions());
    assert!(transport.killed_sessions().is_empty() && transport.sessions().contains("team-a"),
        "F1: bare socket cleanup killed the managed leader after main path spared it; out={out}");
}

#[test]
fn pr218_review_f2_stale_pane_id_must_not_kill_other_session() {
    let case = Case::new("pr218-review-f2");
    save_state(&case, json!({
        "team_key": "team-a", "active_team_key": "team-a",
        "session_name": "team-a", "is_external_leader": false,
        "tmux_socket": case.path.join("review.sock").display().to_string(),
        "team_owner": {"pane_id": "%leader"},
        "leader_receiver": {"pane_id": "%leader"},
        "agents": {"worker-a": {"status": "running", "provider": "fake",
            "window": "worker-a", "pane_id": "%foreign"}}
    }));
    let transport = RecordingTransport::with_targets(vec![
        pane("%leader", "team-a", "leader", 1),
        pane("%foreign", "foreign-b", "worker-b", 2),
    ]);
    let out = shutdown(&case, Some("team-a"), &transport);
    eprintln!("F2 shutdown={out}; killed_windows={:?}; surviving_sessions={:?}",
        transport.killed_windows(), transport.sessions());
    assert!(!transport.killed_windows().iter().any(|v| v == "pane:%foreign")
            && transport.sessions().contains("foreign-b"),
        "F2: stale pane ID bypassed expected session/team gate; out={out}");
}

struct Pr218ReviewCanary(Child);
impl Drop for Pr218ReviewCanary {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
        }
        let _ = self.0.wait();
    }
}

#[test]
fn pr218_review_f3_stale_pid_and_shared_cwd_do_not_prove_process_ownership() {
    let case = Case::new("pr218-review-f3");
    let mut command = Command::new("/bin/sleep");
    command.arg("120").current_dir(&case.path);
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                Err(std::io::Error::last_os_error())
            } else {
                Ok(())
            }
        });
    }
    let mut canary = Pr218ReviewCanary(command.spawn().expect("spawn isolated owned canary"));
    assert!(canary.0.try_wait().expect("canary preflight").is_none());
    let pid = canary.0.id();
    save_state(&case, json!({
        "team_key": "team-a", "active_team_key": "team-a",
        "session_name": "team-a", "is_external_leader": true,
        "tmux_socket": case.path.join("review.sock").display().to_string(),
        "agents": {"stale-worker": {"status": "running", "provider": "fake",
            "provider_pid": pid, "spawn_cwd": case.path.display().to_string()}}
    }));
    let transport = RecordingTransport::with_targets(vec![
        pane("%owned-a", "team-a", "worker-a", 1),
    ]);
    let out = shutdown(&case, Some("team-a"), &transport);
    let observed = canary.0.try_wait();
    eprintln!("F3 shutdown={out}; canary_pid={pid}; canary_wait={observed:?}");
    assert!(matches!(observed, Ok(None)),
        "F3: stale PID plus same cwd authorized killing an unrelated process; out={out}; observed={observed:?}");
}
