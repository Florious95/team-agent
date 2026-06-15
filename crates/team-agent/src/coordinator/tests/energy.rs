use super::*;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use crate::event_log::EventLog;
use crate::message_store::MessageStore;
use crate::state::persist::{load_runtime_state, save_runtime_state};
use crate::transport::{
    AttachOutcome, BackendKind, CaptureRange, CapturedText, InjectPayload, InjectReport,
    InjectStage, InjectVerification, Key, PaneField, PaneId, PaneInfo, PaneLiveness, SessionName,
    SetEnvOutcome, SpawnResult, SubmitVerification, Target, Transport, TransportError,
    TurnVerification, WindowName,
};

#[test]
fn slice1_stopped_or_missing_agents_do_not_probe_or_emit_capture_failures() {
    let workspace = temp_ws("stopped-missing");
    save_runtime_state(
        &workspace,
        &json!({
            "session_name": "team-energy",
            "agents": {
                "stopped": {
                    "provider": "codex",
                    "status": "stopped",
                    "window": "stopped",
                    "pane_id": "%stopped"
                },
                "missing": {
                    "provider": "codex",
                    "status": "running",
                    "window": "missing",
                    "pane_id": "%missing"
                }
            }
        }),
    )
    .unwrap();
    let transport = EnergyTransport::new()
        .with_session_present(true)
        .with_windows(vec![WindowName::new("live")])
        .with_capture_error("pane missing");
    let calls = transport.calls.clone();
    let coord = coordinator(&workspace, transport);

    coord.tick().expect("tick");

    assert_eq!(
        count_calls(&calls, "capture"),
        0,
        "stopped and absent-window agents must not be capture-probed"
    );
    let events = read_event_log_dir(&workspace);
    assert!(
        !events
            .iter()
            .any(|event| event_name(event).is_some_and(|name| {
                name == "provider.startup_prompt_failed"
                    || name == "runtime_approval.capture_failed"
                    || name == "coordinator.agent_capture_failed"
            })),
        "ineligible agents must not create repeated capture failure events: {events:?}"
    );
}

#[test]
fn slice1_startup_probe_disables_for_epoch_after_grace_and_reopens_on_new_epoch() {
    let workspace = temp_ws("startup-epoch");
    save_runtime_state(
        &workspace,
        &json!({
            "session_name": "team-energy",
            "agents": {
                "w1": running_agent(json!({
                    "spawned_at": old_rfc3339(),
                    "pane_pid": 1010,
                    "activity": {"status": "idle"},
                    "coordinator_idle_capture_next_at": future_rfc3339(),
                    "runtime_approval_probe": {
                        "backoff_secs": 30,
                        "next_probe_at": future_rfc3339()
                    }
                }))
            }
        }),
    )
    .unwrap();
    let transport = EnergyTransport::new()
        .with_session_present(true)
        .with_windows(vec![WindowName::new("w1")])
        .with_targets(vec![pane("%1", "w1")])
        .with_capture_text("startup trust prompt would be here");
    let calls = transport.calls.clone();
    let coord = coordinator(&workspace, transport);

    coord.tick().expect("first tick");
    assert_eq!(
        count_calls(&calls, "capture"),
        0,
        "startup probing after grace must disable without capture; calls={:?}",
        calls.lock().unwrap()
    );
    let disabled_state = load_runtime_state(&workspace).unwrap();
    let disabled_epoch = disabled_state
        .pointer("/agents/w1/startup_prompt_probe_epoch")
        .and_then(Value::as_str)
        .expect("disabled epoch stored")
        .to_string();
    assert_eq!(
        disabled_state
            .pointer("/agents/w1/startup_prompt_status")
            .and_then(Value::as_str),
        Some("disabled_for_epoch")
    );

    let reopened_workspace = temp_ws("startup-epoch-reopen");
    save_runtime_state(
        &reopened_workspace,
        &json!({
            "session_name": "team-energy",
            "agents": {
                "w1": running_agent(json!({
                    "spawned_at": chrono::Utc::now().to_rfc3339(),
                    "pane_pid": 2020,
                    "startup_prompts": "disabled_for_epoch",
                    "startup_prompt_status": "disabled_for_epoch",
                    "startup_prompt_probe_epoch": disabled_epoch,
                    "startup_prompt_probe_disabled_at": old_rfc3339(),
                    "activity": {"status": "idle"},
                    "coordinator_idle_capture_next_at": future_rfc3339(),
                    "runtime_approval_probe": {
                        "backoff_secs": 30,
                        "next_probe_at": future_rfc3339()
                    }
                }))
            }
        }),
    )
    .unwrap();
    let reopened_transport = EnergyTransport::new()
        .with_session_present(true)
        .with_windows(vec![WindowName::new("w1")])
        .with_targets(vec![pane("%1", "w1")])
        .with_capture_text("ordinary ready output");
    let reopened = coordinator(&reopened_workspace, reopened_transport);
    reopened.tick().expect("second tick");
    let after = load_runtime_state(&reopened_workspace).unwrap();

    assert_ne!(
        after
            .pointer("/agents/w1/startup_prompt_status")
            .and_then(Value::as_str),
        Some("disabled_for_epoch"),
        "new process epoch must not remain disabled under the old epoch marker"
    );
}

#[test]
fn slice1_runtime_approval_probe_backs_off_but_remains_recurring() {
    let workspace = temp_ws("runtime-backoff");
    save_runtime_state(
        &workspace,
        &json!({
            "session_name": "team-energy",
            "agents": {
                "w1": running_agent(json!({
                    "startup_prompts": "handled",
                    "startup_prompt_status": "handled",
                    "activity": {"status": "idle"},
                    "coordinator_idle_capture_next_at": future_rfc3339()
                }))
            }
        }),
    )
    .unwrap();
    let transport = EnergyTransport::new()
        .with_session_present(true)
        .with_windows(vec![WindowName::new("w1")])
        .with_targets(vec![pane("%1", "w1")])
        .with_capture_text("ordinary output, no approval prompt");
    let calls = transport.calls.clone();
    let coord = coordinator(&workspace, transport);

    coord.tick().expect("first tick");
    let first_capture_count = count_calls(&calls, "capture");
    assert!(
        first_capture_count > 0,
        "first eligible runtime approval probe must capture"
    );
    let first = load_runtime_state(&workspace).unwrap();
    assert_eq!(
        first
            .pointer("/agents/w1/runtime_approval_probe/backoff_secs")
            .and_then(Value::as_i64),
        Some(30),
        "first empty runtime approval probe starts at 30s backoff"
    );
    assert!(
        first
            .pointer("/agents/w1/runtime_approval_probe/next_probe_at")
            .and_then(Value::as_str)
            .is_some(),
        "runtime approval stores a next probe time instead of disabling forever"
    );

    coord.tick().expect("second tick");
    assert_eq!(
        count_calls(&calls, "capture"),
        first_capture_count,
        "second tick before next_probe_at must not capture again; calls={:?}",
        calls.lock().unwrap()
    );
}

#[test]
fn slice1_idle_tick_skips_per_agent_capture_when_no_work_is_pending() {
    let workspace = temp_ws("idle-zero-capture");
    save_runtime_state(
        &workspace,
        &json!({
            "session_name": "team-energy",
            "agents": {
                "w1": running_agent(json!({
                    "startup_prompts": "handled",
                    "startup_prompt_status": "handled",
                    "activity": {"status": "idle"},
                    "coordinator_idle_capture_next_at": future_rfc3339(),
                    "runtime_approval_probe": {
                        "backoff_secs": 30,
                        "next_probe_at": future_rfc3339()
                    }
                }))
            }
        }),
    )
    .unwrap();
    let transport = EnergyTransport::new()
        .with_session_present(true)
        .with_windows(vec![WindowName::new("w1")])
        .with_targets(vec![pane("%1", "w1")])
        .with_capture_error("idle capture should not happen");
    let calls = transport.calls.clone();
    let coord = coordinator(&workspace, transport);

    coord.tick().expect("tick");

    assert_eq!(
        count_calls(&calls, "capture"),
        0,
        "warm idle tick with no work must perform zero per-agent capture-pane calls; calls={:?}",
        calls.lock().unwrap()
    );
}

#[test]
fn slice1_idle_tick_captures_when_delivery_work_is_pending() {
    let workspace = temp_ws("idle-with-work");
    save_runtime_state(
        &workspace,
        &json!({
            "session_name": "team-energy",
            "agents": {
                "w1": running_agent(json!({
                    "startup_prompts": "handled",
                    "startup_prompt_status": "handled",
                    "activity": {"status": "idle"},
                    "coordinator_idle_capture_next_at": future_rfc3339()
                }))
            }
        }),
    )
    .unwrap();
    let store = MessageStore::open(&workspace).unwrap();
    store
        .create_message(None, "leader", "w1", "hello", None, false, None)
        .unwrap();
    let transport = EnergyTransport::new()
        .with_session_present(true)
        .with_windows(vec![WindowName::new("w1")])
        .with_targets(vec![pane("%1", "w1")])
        .with_capture_text("❯\n");
    let calls = transport.calls.clone();
    let coord = coordinator(&workspace, transport);

    coord.tick().expect("tick");

    assert!(
        count_calls(&calls, "capture") > 0,
        "pending delivery work must override idle capture suppression"
    );
}

fn coordinator(workspace: &Path, transport: EnergyTransport) -> Coordinator {
    Coordinator::for_test(
        WorkspacePath::new(workspace.to_path_buf()),
        Box::new(MockRegistry::new(&[], &[])),
        Box::new(transport),
        None,
        None,
    )
}

fn running_agent(extra: Value) -> Value {
    let mut agent = json!({
        "provider": "codex",
        "status": "running",
        "window": "w1",
        "pane_id": "%1",
        "pane_pid": 1001,
        "spawned_at": chrono::Utc::now().to_rfc3339()
    });
    merge_object(&mut agent, extra);
    agent
}

fn merge_object(dst: &mut Value, src: Value) {
    let Some(dst) = dst.as_object_mut() else {
        return;
    };
    let Some(src) = src.as_object() else {
        return;
    };
    for (key, value) in src {
        dst.insert(key.clone(), value.clone());
    }
}

fn pane(pane_id: &str, window: &str) -> PaneInfo {
    PaneInfo {
        pane_id: PaneId::new(pane_id),
        session: SessionName::new("team-energy"),
        window_index: Some(0),
        window_name: Some(WindowName::new(window)),
        pane_index: Some(0),
        tty: None,
        current_command: Some("codex".to_string()),
        current_path: None,
        active: true,
        pane_pid: Some(1001),
        leader_env: BTreeMap::new(),
    }
}

fn count_calls(calls: &Arc<Mutex<Vec<&'static str>>>, name: &str) -> usize {
    calls
        .lock()
        .unwrap()
        .iter()
        .filter(|call| **call == name)
        .count()
}

fn event_name(event: &Value) -> Option<&str> {
    event.get("event").and_then(Value::as_str)
}

fn temp_ws(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "ta-rs-slice1-{tag}-{}-{}",
        std::process::id(),
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::canonicalize(dir).unwrap()
}

fn old_rfc3339() -> String {
    (chrono::Utc::now() - chrono::Duration::minutes(10)).to_rfc3339()
}

fn future_rfc3339() -> String {
    (chrono::Utc::now() + chrono::Duration::minutes(10)).to_rfc3339()
}

#[derive(Clone)]
struct EnergyTransport {
    calls: Arc<Mutex<Vec<&'static str>>>,
    session_present: bool,
    windows: Vec<WindowName>,
    targets: Vec<PaneInfo>,
    capture_text: String,
    capture_error: Option<String>,
}

impl EnergyTransport {
    fn new() -> Self {
        Self {
            calls: Arc::new(Mutex::new(Vec::new())),
            session_present: false,
            windows: Vec::new(),
            targets: Vec::new(),
            capture_text: String::new(),
            capture_error: None,
        }
    }

    fn with_session_present(mut self, present: bool) -> Self {
        self.session_present = present;
        self
    }

    fn with_windows(mut self, windows: Vec<WindowName>) -> Self {
        self.windows = windows;
        self
    }

    fn with_targets(mut self, targets: Vec<PaneInfo>) -> Self {
        self.targets = targets;
        self
    }

    fn with_capture_text(mut self, text: &str) -> Self {
        self.capture_text = text.to_string();
        self.capture_error = None;
        self
    }

    fn with_capture_error(mut self, error: &str) -> Self {
        self.capture_error = Some(error.to_string());
        self
    }

    fn record(&self, call: &'static str) {
        self.calls.lock().unwrap().push(call);
    }
}

impl Transport for EnergyTransport {
    fn kind(&self) -> BackendKind {
        BackendKind::Tmux
    }

    fn spawn_first(
        &self,
        _session: &SessionName,
        _window: &WindowName,
        _argv: &[String],
        _cwd: &Path,
        _env: &BTreeMap<String, String>,
    ) -> Result<SpawnResult, TransportError> {
        unreachable!("energy tests do not spawn")
    }

    fn spawn_into(
        &self,
        _session: &SessionName,
        _window: &WindowName,
        _argv: &[String],
        _cwd: &Path,
        _env: &BTreeMap<String, String>,
    ) -> Result<SpawnResult, TransportError> {
        unreachable!("energy tests do not spawn")
    }

    fn inject(
        &self,
        _target: &Target,
        _payload: &InjectPayload,
        _submit: Key,
        _bracketed: bool,
    ) -> Result<InjectReport, TransportError> {
        self.record("inject");
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
        self.record("send_keys");
        Ok(())
    }

    fn capture(
        &self,
        _target: &Target,
        range: CaptureRange,
    ) -> Result<CapturedText, TransportError> {
        self.record("capture");
        if let Some(error) = &self.capture_error {
            return Err(TransportError::Capture {
                source: std::io::Error::other(error.clone()),
            });
        }
        Ok(CapturedText {
            text: self.capture_text.clone(),
            range,
        })
    }

    fn query(&self, _target: &Target, _field: PaneField) -> Result<Option<String>, TransportError> {
        self.record("query");
        Ok(None)
    }

    fn liveness(&self, _pane: &PaneId) -> Result<PaneLiveness, TransportError> {
        self.record("liveness");
        Ok(PaneLiveness::Live)
    }

    fn list_targets(&self) -> Result<Vec<PaneInfo>, TransportError> {
        self.record("list_targets");
        Ok(self.targets.clone())
    }

    fn has_session(&self, _session: &SessionName) -> Result<bool, TransportError> {
        self.record("has_session");
        Ok(self.session_present)
    }

    fn list_windows(&self, _session: &SessionName) -> Result<Vec<WindowName>, TransportError> {
        self.record("list_windows");
        Ok(self.windows.clone())
    }

    fn set_session_env(
        &self,
        _session: &SessionName,
        _key: &str,
        _value: &str,
    ) -> Result<SetEnvOutcome, TransportError> {
        self.record("set_session_env");
        Ok(SetEnvOutcome::Applied)
    }

    fn kill_session(&self, _session: &SessionName) -> Result<(), TransportError> {
        self.record("kill_session");
        Ok(())
    }

    fn kill_window(&self, _target: &Target) -> Result<(), TransportError> {
        self.record("kill_window");
        Ok(())
    }

    fn attach_session(&self, _session: &SessionName) -> Result<AttachOutcome, TransportError> {
        self.record("attach_session");
        Ok(AttachOutcome::Attached)
    }
}
