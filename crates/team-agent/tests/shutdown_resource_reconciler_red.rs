//! #248 RED: shutdown must reconcile tmux sessions and pane process trees before
//! reporting success.
//!
//! User-facing contract:
//! - `kill-session` success is not enough; shutdown must verify the session is
//!   gone and pane process trees are reaped before `session_killed=true`.
//! - stale/live runtimes may carry stored tmux endpoints, so shutdown must honor
//!   that stored endpoint instead of deriving a fresh socket only from workspace.
//! - a stale coordinator pid file must not become a false `kill_failed` when the
//!   process is already gone.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use serial_test::serial;
use team_agent::cli::lifecycle_port::shutdown_with_transport;
use team_agent::coordinator::{coordinator_pid_path, WorkspacePath};
use team_agent::state::persist::save_runtime_state;
use team_agent::transport::{
    AttachOutcome, BackendKind, CaptureRange, CapturedText, InjectPayload, InjectReport,
    InjectStage, InjectVerification, Key, PaneField, PaneId, PaneInfo, PaneLiveness,
    SessionName, SetEnvOutcome, SpawnResult, SubmitVerification, Target, Transport,
    TransportError, TurnVerification, WindowName,
};

#[test]
#[serial(env)]
fn shutdown_reports_partial_when_kill_session_ok_but_session_still_present() {
    let workspace = tmp_dir("session-still-present");
    let session = SessionName::new("team-resource-still-present");
    seed_state(&workspace, session.as_str());
    let transport = ShutdownTransport::new([session.as_str()], false, Vec::new());

    let report = shutdown_with_transport(&workspace, true, None, &transport)
        .expect("shutdown should return a typed report, not throw away residue evidence");

    assert!(
        transport.called("kill_session"),
        "shutdown must attempt the session kill before reconciliation; calls={:?}",
        transport.calls()
    );
    assert!(
        transport.called("has_session") || transport.called("list_targets"),
        "shutdown must verify session/process residue after kill_session Ok; calls={:?} report={report}",
        transport.calls()
    );
    assert_ne!(
        report.get("session_killed").and_then(Value::as_bool),
        Some(true),
        "shutdown must not report session_killed=true while the target session is still present; report={report}"
    );
    assert!(
        report.get("ok").and_then(Value::as_bool) != Some(true),
        "shutdown residue must be honest partial/failed rather than ok=true; report={report}"
    );
    assert!(
        report.pointer("/residuals/sessions").is_some(),
        "shutdown partial/failed reports must include a residual session list; report={report}"
    );
}

#[test]
#[serial(env)]
fn shutdown_reaps_pane_pid_process_tree_before_reporting_session_killed() {
    let workspace = tmp_dir("pane-process-tree");
    let session = SessionName::new("team-resource-process-tree");
    seed_state(&workspace, session.as_str());

    let process_tree = ProcessTree::spawn();
    let child_pid = process_tree.wait_for_child();
    let pane = pane_info("%1", &session, "w1", Some(process_tree.pid()));
    let transport = ShutdownTransport::new([session.as_str()], true, vec![pane]);

    let report = shutdown_with_transport(&workspace, true, None, &transport)
        .expect("shutdown should complete after reconciling pane process trees");
    let pane_alive = pid_is_alive(process_tree.pid());
    let child_alive = pid_is_alive(child_pid);

    assert_eq!(
        report.get("session_killed").and_then(Value::as_bool),
        Some(true),
        "shutdown may report session_killed=true only after session and pane process tree are gone; report={report}"
    );
    assert!(
        !pane_alive && !child_alive,
        "shutdown must recursively reap the pane pid process tree before claiming success; \
         pane_pid={} pane_alive={pane_alive} child_pid={child_pid} child_alive={child_alive} report={report}",
        process_tree.pid()
    );
}

#[test]
fn shutdown_entrypoint_uses_stored_tmux_endpoint_for_legacy_live_runtime() {
    let source = include_str!("../src/cli/mod.rs");
    let shutdown_body = slice_between(
        source,
        "pub fn shutdown(workspace: &Path, keep_logs: bool, team: Option<&str>)",
        "pub fn shutdown_with_transport",
    );

    assert!(
        shutdown_body.contains("for_tmux_endpoint")
            || shutdown_body.contains("stored")
            || shutdown_body.contains("tmux_socket"),
        "shutdown must select its tmux backend from the stored legacy endpoint when one exists; \
         deriving only TmuxBackend::for_workspace(&run_ws) misses old live teams. body:\n{shutdown_body}"
    );
    assert!(
        !shutdown_body.contains("TmuxBackend::for_workspace(&run_ws)")
            || shutdown_body.contains("tmux_socket"),
        "shutdown must not blindly derive the backend from workspace before consulting stored tmux_socket/full endpoint; \
         body:\n{shutdown_body}"
    );
}

#[test]
#[serial(env)]
fn stale_coordinator_pid_file_does_not_create_false_kill_failed_when_process_is_gone() {
    let workspace = tmp_dir("stale-pid");
    seed_state(&workspace, "");
    let pid_path = coordinator_pid_path(&WorkspacePath::new(workspace.clone()));
    std::fs::create_dir_all(pid_path.parent().unwrap()).unwrap();
    let stale_pid = nonexistent_pid();
    std::fs::write(&pid_path, format!("{stale_pid}\n")).unwrap();

    let transport = ShutdownTransport::new([], true, Vec::new());
    let report = shutdown_with_transport(&workspace, true, None, &transport)
        .expect("shutdown should surface typed coordinator status");

    assert_ne!(
        report.pointer("/coordinator/status").and_then(Value::as_str),
        Some("kill_failed"),
        "W1 guard: a stale pid file whose process is already gone must be verified as not-live, \
         not reported as kill_failed; stale_pid={stale_pid} report={report}"
    );
    assert_eq!(
        report.get("ok").and_then(Value::as_bool),
        Some(true),
        "W1 guard: shutdown should not fail only because SIGTERM returned non-zero for an already-gone pid; report={report}"
    );
}

#[derive(Debug)]
struct ShutdownTransport {
    sessions: Mutex<HashSet<String>>,
    remove_session_on_kill: bool,
    targets: Mutex<Vec<PaneInfo>>,
    calls: Mutex<Vec<&'static str>>,
}

impl ShutdownTransport {
    fn new<const N: usize>(
        sessions: [&str; N],
        remove_session_on_kill: bool,
        targets: Vec<PaneInfo>,
    ) -> Self {
        Self {
            sessions: Mutex::new(sessions.into_iter().map(str::to_string).collect()),
            remove_session_on_kill,
            targets: Mutex::new(targets),
            calls: Mutex::new(Vec::new()),
        }
    }

    fn calls(&self) -> Vec<&'static str> {
        self.calls.lock().unwrap().clone()
    }

    fn called(&self, name: &'static str) -> bool {
        self.calls.lock().unwrap().contains(&name)
    }

    fn record(&self, name: &'static str) {
        self.calls.lock().unwrap().push(name);
    }

    fn spawn_result(session: &SessionName, window: &WindowName) -> SpawnResult {
        SpawnResult {
            pane_id: PaneId::new("%fixture"),
            session: session.clone(),
            window: window.clone(),
            child_pid: None,
        }
    }
}

impl Transport for ShutdownTransport {
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
        Ok(Self::spawn_result(session, window))
    }

    fn spawn_into(
        &self,
        session: &SessionName,
        window: &WindowName,
        _argv: &[String],
        _cwd: &Path,
        _env: &BTreeMap<String, String>,
    ) -> Result<SpawnResult, TransportError> {
        Ok(Self::spawn_result(session, window))
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
        })
    }

    fn send_keys(&self, _target: &Target, _keys: &[Key]) -> Result<(), TransportError> {
        Ok(())
    }

    fn capture(&self, _target: &Target, range: CaptureRange) -> Result<CapturedText, TransportError> {
        Ok(CapturedText { text: String::new(), range })
    }

    fn query(&self, _target: &Target, _field: PaneField) -> Result<Option<String>, TransportError> {
        Ok(None)
    }

    fn liveness(&self, _pane: &PaneId) -> Result<PaneLiveness, TransportError> {
        Ok(PaneLiveness::Live)
    }

    fn list_targets(&self) -> Result<Vec<PaneInfo>, TransportError> {
        self.record("list_targets");
        Ok(self.targets.lock().unwrap().clone())
    }

    fn has_session(&self, session: &SessionName) -> Result<bool, TransportError> {
        self.record("has_session");
        Ok(self.sessions.lock().unwrap().contains(session.as_str()))
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

    fn kill_session(&self, session: &SessionName) -> Result<(), TransportError> {
        self.record("kill_session");
        if self.remove_session_on_kill {
            self.sessions.lock().unwrap().remove(session.as_str());
        }
        Ok(())
    }

    fn kill_window(&self, _target: &Target) -> Result<(), TransportError> {
        Ok(())
    }

    fn attach_session(&self, _session: &SessionName) -> Result<AttachOutcome, TransportError> {
        Ok(AttachOutcome::Attached)
    }
}

struct ProcessTree {
    child: Child,
}

impl ProcessTree {
    fn spawn() -> Self {
        let child = Command::new("/bin/sh")
            .args(["-lc", "sleep 300 & wait"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn disposable pane process tree");
        Self { child }
    }

    fn pid(&self) -> u32 {
        self.child.id()
    }

    fn wait_for_child(&self) -> u32 {
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            if let Some(pid) = child_pid_of(self.pid()) {
                return pid;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        panic!("pane process {} did not spawn a child process", self.pid());
    }
}

impl Drop for ProcessTree {
    fn drop(&mut self) {
        reap_process_tree(self.pid());
        let _ = self.child.wait();
    }
}

fn pane_info(pane_id: &str, session: &SessionName, window: &str, pane_pid: Option<u32>) -> PaneInfo {
    PaneInfo {
        pane_id: PaneId::new(pane_id),
        session: session.clone(),
        window_index: Some(0),
        window_name: Some(WindowName::new(window)),
        pane_index: Some(0),
        tty: None,
        current_command: Some("sh".to_string()),
        current_path: None,
        active: true,
        pane_pid,
        leader_env: BTreeMap::new(),
    }
}

fn seed_state(workspace: &Path, session_name: &str) {
    std::fs::create_dir_all(team_agent::model::paths::runtime_dir(workspace)).unwrap();
    save_runtime_state(
        workspace,
        &json!({
            "session_name": session_name,
            "agents": {
                "w1": {
                    "agent_id": "w1",
                    "status": "running",
                    "provider": "fake",
                    "window": "w1",
                    "pane_id": "%1"
                }
            },
            "tasks": []
        }),
    )
    .unwrap();
}

fn tmp_dir(tag: &str) -> PathBuf {
    static N: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "ta-rs-shutdown-resource-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::canonicalize(dir).unwrap()
}

fn child_pid_of(parent: u32) -> Option<u32> {
    let output = Command::new("pgrep")
        .args(["-P", &parent.to_string()])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .find_map(|line| line.trim().parse::<u32>().ok())
}

fn pid_is_alive(pid: u32) -> bool {
    let Ok(pid_t) = libc::pid_t::try_from(pid) else {
        return false;
    };
    unsafe { libc::kill(pid_t, 0) == 0 }
}

fn nonexistent_pid() -> u32 {
    (900_000..999_999)
        .find(|pid| !pid_is_alive(*pid))
        .expect("test host should have at least one non-live high pid")
}

fn reap_process_tree(pid: u32) {
    let pid_arg = pid.to_string();
    let _ = Command::new("pkill").args(["-TERM", "-P", &pid_arg]).status();
    let _ = Command::new("kill").args(["-TERM", &pid_arg]).status();
    std::thread::sleep(Duration::from_millis(50));
    let _ = Command::new("pkill").args(["-KILL", "-P", &pid_arg]).status();
    let _ = Command::new("kill").args(["-KILL", &pid_arg]).status();
}

fn slice_between<'a>(text: &'a str, start: &str, end: &str) -> &'a str {
    let start_index = text.find(start).unwrap_or_else(|| panic!("missing start marker {start:?}"));
    let tail = &text[start_index..];
    let end_index = tail.find(end).unwrap_or_else(|| panic!("missing end marker {end:?}"));
    &tail[..end_index]
}
