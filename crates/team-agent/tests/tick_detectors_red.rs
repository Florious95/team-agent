#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{
    atomic::{AtomicU32, AtomicU64, Ordering},
    Arc,
};

use serde_json::{json, Value};
use team_agent::coordinator::{Coordinator, ErrorLists, ProviderRegistry, WorkspacePath};
use team_agent::event_log::EventLog;
use team_agent::provider::{Provider, ProviderAdapter};
use team_agent::state::persist::{load_runtime_state, save_runtime_state};
use team_agent::transport::{
    AttachOutcome, BackendKind, CaptureRange, CapturedText, InjectPayload, InjectReport,
    InjectStage, InjectVerification, Key, PaneField, PaneId, PaneInfo, PaneLiveness, SessionName,
    SetEnvOutcome, SpawnResult, SubmitVerification, Target, Transport, TransportError,
    TurnVerification, WindowName,
};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_team-agent")
}

#[test]
fn tick_reports_compaction_from_real_worker_capture_not_empty_placeholder() {
    let worker_capture = "\
        previous output\n\
        /compact\n\
        Context compacted. Compaction completed; continue from the summary.\n\
        ❯\n";
    let fixture = TickFixture::new("compaction", worker_capture, "");
    let report = fixture.coord.tick().expect("tick should complete");
    let events = fixture.events();

    assert!(
        !report.compaction.is_empty(),
        "TickReport.compaction must be populated from the captured worker pane when compaction \
         markers are visible; base_tick_report must not fabricate an empty vec. report={report:?} events={events:?}"
    );
    assert!(
        events.iter().any(|event| {
            event_name(event).is_some_and(|name| {
                name == "coordinator.compaction_observed"
                    || name == "coordinator.compaction_threshold"
                    || name.contains("compaction")
            })
        }),
        "tick must audit the observed compaction/threshold event from pane capture; events={events:?}"
    );
    assert_eq!(
        fixture.registry.adapter_calls.load(Ordering::SeqCst),
        0,
        "compaction detector is read-only over capture/state and must not request provider adapters"
    );
}

#[test]
fn tick_marks_codex_session_drift_and_send_refuses_drifted_worker() {
    let worker_capture = "Codex resumed. Switched to thread S2-drifted-thread\n❯\n";
    let fixture = TickFixture::new("drift", worker_capture, "");
    let report = fixture.coord.tick().expect("tick should complete");
    let state = load_runtime_state(&fixture.workspace).expect("load post-tick state");

    assert!(
        !report.session_drift.is_empty(),
        "TickReport.session_drift must include the codex worker whose captured thread id differs \
         from stored session_id=S1; report={report:?} state={state}"
    );
    assert_eq!(
        state.pointer("/agents/w1/status").and_then(Value::as_str),
        Some("session_drift"),
        "tick must persist agent status=session_drift when capture shows a different thread; state={state}"
    );
    assert_eq!(
        state.pointer("/agents/w1/session_drift/stored_session_id").and_then(Value::as_str),
        Some("S1"),
        "session_drift payload must retain stored_session_id for operator diagnosis; state={state}"
    );

    let out = Command::new(bin())
        .args([
            "send",
            "--workspace",
            fixture.workspace.to_str().unwrap(),
            "--to",
            "w1",
            "--json",
            "hello",
        ])
        .output()
        .expect("run send after drift");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let value = serde_json::from_slice::<Value>(&out.stdout).unwrap_or(Value::Null);
    assert!(
        !out.status.success()
            && value.get("ok").and_then(Value::as_bool) == Some(false)
            && serde_json::to_string(&value).unwrap_or_default().contains("session_drift"),
        "after tick marks drift, `team-agent send --to w1` must refuse with session_drift instead \
         of injecting into the wrong provider thread. code={:?} stdout={stdout:?} stderr={stderr:?}",
        out.status.code()
    );
}

#[test]
fn tick_records_leader_api_error_once_per_fingerprint_from_leader_capture() {
    let leader_capture = "\
        Working...\n\
        API Error: Overloaded\n\
        529 too many requests, please retry later\n";
    let fixture = TickFixture::new("api-error", "❯\n", leader_capture);

    let first = fixture.coord.tick().expect("first tick should complete");
    let events_after_first = fixture.events();
    assert!(
        !first.api_errors.is_empty(),
        "TickReport.api_errors must contain the provider-neutral leader API error fact from leader \
         pane capture; report={first:?} events={events_after_first:?}"
    );
    let first_count = leader_api_error_count(&events_after_first);
    assert_eq!(
        first_count, 1,
        "first tick must emit exactly one leader.api_error for the captured overloaded fingerprint; \
         events={events_after_first:?}"
    );

    let second = fixture.coord.tick().expect("second tick should complete");
    let events_after_second = fixture.events();
    let second_count = leader_api_error_count(&events_after_second);
    assert_eq!(
        second_count, first_count,
        "same leader API error fingerprint must be deduped on the next tick; report={second:?} \
         events={events_after_second:?}"
    );
    assert_eq!(
        fixture.registry.adapter_calls.load(Ordering::SeqCst),
        0,
        "api-error detector reads leader capture/state only and must not request provider adapters"
    );
}

struct TickFixture {
    workspace: PathBuf,
    coord: Coordinator,
    registry: Arc<CountingRegistry>,
}

impl TickFixture {
    fn new(tag: &str, worker_capture: &str, leader_capture: &str) -> Self {
        let workspace = tmp_dir(tag);
        seed_state(&workspace);
        let registry = Arc::new(CountingRegistry::default());
        let transport = DetectorTransport {
            worker_capture: worker_capture.to_string(),
            leader_capture: leader_capture.to_string(),
        };
        let coord = Coordinator::new(
            WorkspacePath::new(workspace.clone()),
            Box::new(RegistryHandle(Arc::clone(&registry))),
            Box::new(transport),
        );
        Self { workspace, coord, registry }
    }

    fn events(&self) -> Vec<Value> {
        EventLog::new(&self.workspace).tail(100).expect("read events")
    }
}

impl Drop for TickFixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.workspace);
    }
}

#[derive(Default)]
struct CountingRegistry {
    adapter_calls: AtomicU32,
}

struct RegistryHandle(Arc<CountingRegistry>);

impl ProviderRegistry for RegistryHandle {
    fn adapter_for(&self, provider: Provider) -> Box<dyn ProviderAdapter> {
        self.0.adapter_calls.fetch_add(1, Ordering::SeqCst);
        team_agent::provider::get_adapter(provider)
    }

    fn error_lists(&self, _provider: Provider) -> ErrorLists {
        ErrorLists::default()
    }
}

struct DetectorTransport {
    worker_capture: String,
    leader_capture: String,
}

impl Transport for DetectorTransport {
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
        unreachable!("tick detectors are read-only and must not spawn providers")
    }

    fn spawn_into(
        &self,
        _session: &SessionName,
        _window: &WindowName,
        _argv: &[String],
        _cwd: &Path,
        _env: &BTreeMap<String, String>,
    ) -> Result<SpawnResult, TransportError> {
        unreachable!("tick detectors are read-only and must not spawn providers")
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

    fn capture(&self, target: &Target, range: CaptureRange) -> Result<CapturedText, TransportError> {
        let text = match target {
            Target::Pane(pane) if pane.as_str() == "%leader" => self.leader_capture.clone(),
            _ => self.worker_capture.clone(),
        };
        Ok(CapturedText { text, range })
    }

    fn query(&self, _target: &Target, field: PaneField) -> Result<Option<String>, TransportError> {
        Ok(match field {
            PaneField::PaneCurrentCommand => Some("codex".to_string()),
            PaneField::PaneCurrentPath => Some("/tmp".to_string()),
            _ => None,
        })
    }

    fn liveness(&self, _pane: &PaneId) -> Result<PaneLiveness, TransportError> {
        Ok(PaneLiveness::Live)
    }

    fn list_targets(&self) -> Result<Vec<PaneInfo>, TransportError> {
        Ok(vec![
            pane_info("%leader", "leader"),
            pane_info("%w1", "w1"),
        ])
    }

    fn has_session(&self, _session: &SessionName) -> Result<bool, TransportError> {
        Ok(true)
    }

    fn list_windows(&self, _session: &SessionName) -> Result<Vec<WindowName>, TransportError> {
        Ok(vec![WindowName::new("leader"), WindowName::new("w1")])
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

fn seed_state(workspace: &Path) {
    save_runtime_state(
        workspace,
        &json!({
            "active_team_key": "current",
            "session_name": "team-current",
            "leader": {"id": "leader", "provider": "codex"},
            "leader_receiver": {
                "mode": "direct_tmux",
                "status": "attached",
                "provider": "codex",
                "pane_id": "%leader",
                "owner_epoch": 1
            },
            "team_owner": {
                "provider": "codex",
                "pane_id": "%leader",
                "owner_epoch": 1
            },
            "agents": {
                "w1": {
                    "provider": "codex",
                    "status": "running",
                    "session_id": "S1",
                    "window": "w1",
                    "pane_id": "%w1",
                    "startup_prompts": "handled"
                }
            },
            "teams": {
                "current": {
                    "session_name": "team-current",
                    "leader_receiver": {
                        "mode": "direct_tmux",
                        "status": "attached",
                        "provider": "codex",
                        "pane_id": "%leader",
                        "owner_epoch": 1
                    },
                    "team_owner": {
                        "provider": "codex",
                        "pane_id": "%leader",
                        "owner_epoch": 1
                    },
                    "agents": {
                        "w1": {
                            "provider": "codex",
                            "status": "running",
                            "session_id": "S1",
                            "window": "w1",
                            "pane_id": "%w1",
                            "startup_prompts": "handled"
                        }
                    }
                }
            }
        }),
    )
    .expect("seed runtime state");
}

fn pane_info(pane_id: &str, window: &str) -> PaneInfo {
    PaneInfo {
        pane_id: PaneId::new(pane_id),
        session: SessionName::new("team-current"),
        window_index: Some(if window == "leader" { 0 } else { 1 }),
        window_name: Some(WindowName::new(window)),
        pane_index: Some(0),
        tty: None,
        current_command: Some("codex".to_string()),
        current_path: None,
        active: true,
        pane_pid: Some(std::process::id()),
        leader_env: BTreeMap::new(),
    }
}

fn event_name(event: &Value) -> Option<&str> {
    event.get("event").and_then(Value::as_str)
}

fn leader_api_error_count(events: &[Value]) -> usize {
    events
        .iter()
        .filter(|event| event_name(event) == Some("leader.api_error"))
        .count()
}

fn tmp_dir(tag: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "ta-rs-tick-detectors-{tag}-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::canonicalize(dir).unwrap()
}
