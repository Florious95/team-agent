//! F032 regression contract: startup prompt handling is best-effort at lifecycle boundaries.
//!
//! Launch/restart may spawn a pane and then ask the provider adapter to handle startup prompts.
//! A provider-specific prompt handler panic must not unwind out of the user command and leave a
//! half-initialized runtime. The command should still complete once the pane has been spawned.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;

use serde_json::json;
use team_agent::lifecycle::{launch_with_transport, restart_with_transport, RestartReport};
use team_agent::transport::{
    AttachOutcome, BackendKind, CaptureRange, CapturedText, InjectPayload, InjectReport,
    InjectStage, InjectVerification, Key, PaneField, PaneId, PaneInfo, SessionName,
    SetEnvOutcome, SpawnResult, SubmitVerification, Target, Transport, TransportError,
    TurnVerification, WindowName,
};

static SEQ: AtomicU32 = AtomicU32::new(0);

const TEAM_MD: &str =
    "---\nname: f032team\nobjective: F032 lifecycle prompt fault probe.\nprovider: codex\n---\n\nF032.\n";
const ROLE_MD: &str = "---\nname: implementer\nrole: Implementation Engineer\nprovider: codex\nmodel: gpt-5.5\nauth_mode: subscription\ntools:\n  - mcp_team\n---\n\nImplement.\n";

#[test]
fn f032_launch_spawn_path_treats_startup_prompt_panic_as_best_effort() {
    let team = compiled_team_dir("launch");
    let spec_path = team.join("team.spec.yaml");
    let transport = PanicOnStartupPromptTransport::new();

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        launch_with_transport(&spec_path, false, true, true, &transport)
    }));

    assert_eq!(
        transport.spawn_count(),
        1,
        "test fixture must prove the pane was spawned before the startup prompt handler faulted"
    );
    let report = assert_lifecycle_result_ok(result, "launch");
    assert!(
        !report.started.is_empty(),
        "launch must keep the spawned pane in LaunchReport.started even if startup prompt handling panics"
    );
}

#[test]
fn f032_restart_spawn_path_treats_startup_prompt_panic_as_best_effort() {
    let workspace = restart_workspace("restart");
    let transport = PanicOnStartupPromptTransport::new();

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        restart_with_transport(&workspace, false, None, &transport)
    }));

    assert_eq!(
        transport.spawn_count(),
        1,
        "test fixture must prove restart spawned a pane before the startup prompt handler faulted"
    );
    let report = assert_lifecycle_result_ok(result, "restart");
    assert!(
        matches!(report, RestartReport::Restarted { .. }),
        "restart must complete after degrading startup prompt handling best-effort; got {report:?}"
    );
}

fn assert_lifecycle_result_ok<T>(
    result: std::thread::Result<Result<T, team_agent::lifecycle::LifecycleError>>,
    command: &str,
) -> T {
    match result {
        Ok(Ok(report)) => report,
        Ok(Err(err)) => panic!(
            "{command} must not fail because startup prompt handling is best-effort; got error {err:?}"
        ),
        Err(_) => panic!(
            "{command} must catch provider startup prompt panics at or below the lifecycle boundary"
        ),
    }
}

fn compiled_team_dir(label: &str) -> PathBuf {
    let team = temp_ws(label).join("teamdir");
    std::fs::create_dir_all(team.join("agents")).expect("create agents dir");
    std::fs::write(team.join("TEAM.md"), TEAM_MD).expect("write TEAM.md");
    std::fs::write(team.join("agents").join("implementer.md"), ROLE_MD).expect("write role");
    let spec = team_agent::compiler::compile_team(&team).expect("compile team");
    std::fs::write(
        team.join("team.spec.yaml"),
        team_agent::model::yaml::dumps(&spec),
    )
    .expect("write compiled spec");
    team
}

fn restart_workspace(label: &str) -> PathBuf {
    let ws = compiled_team_dir(label);
    let rollout = ws.join("implementer-rollout.jsonl");
    std::fs::write(&rollout, "{}\n").expect("seed rollout backing");
    team_agent::state::persist::save_runtime_state(
        &ws,
        &json!({
            "session_name": "team-f032team",
            "agents": {
                "implementer": {
                    "status": "running",
                    "provider": "codex",
                    "session_id": "sess-impl",
                    "rollout_path": rollout.to_string_lossy(),
                    "first_send_at": "2026-05-27T10:00:00+00:00"
                }
            }
        }),
    )
    .expect("seed runtime state");
    seed_healthy_coordinator(&ws);
    ws
}

fn seed_healthy_coordinator(workspace: &Path) {
    let workspace = team_agent::coordinator::WorkspacePath::new(workspace.to_path_buf());
    std::fs::create_dir_all(team_agent::model::paths::runtime_dir(workspace.as_path()))
        .expect("create runtime dir");
    let _ = team_agent::message_store::MessageStore::open(workspace.as_path())
        .expect("create message store schema");
    let pid = team_agent::coordinator::Pid::new(std::process::id());
    team_agent::coordinator::write_coordinator_metadata(
        &workspace,
        pid,
        team_agent::coordinator::MetadataSource::Boot,
    )
    .expect("write coordinator metadata");
    std::fs::write(
        team_agent::coordinator::coordinator_pid_path(&workspace),
        pid.to_string(),
    )
    .expect("write coordinator pid");
}

fn temp_ws(label: &str) -> PathBuf {
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let ws = std::env::temp_dir().join(format!(
        "ta_rs_f032_prompt_best_effort_{}_{}_{}",
        std::process::id(),
        label,
        n
    ));
    std::fs::create_dir_all(&ws).expect("create temp workspace");
    ws
}

#[derive(Debug, Default)]
struct PanicOnStartupPromptTransport {
    spawns: Mutex<Vec<String>>,
}

impl PanicOnStartupPromptTransport {
    fn new() -> Self {
        Self::default()
    }

    fn spawn_count(&self) -> usize {
        self.spawns.lock().expect("spawns lock").len()
    }

    fn spawn_result(
        &self,
        kind: &'static str,
        session: &SessionName,
        window: &WindowName,
    ) -> SpawnResult {
        let mut spawns = self.spawns.lock().expect("spawns lock");
        let pane_index = spawns.len();
        spawns.push(kind.to_string());
        SpawnResult {
            pane_id: PaneId::new(format!("%{pane_index}")),
            session: session.clone(),
            window: window.clone(),
            child_pid: None,
        }
    }
}

impl Transport for PanicOnStartupPromptTransport {
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
        Ok(self.spawn_result("spawn_first", session, window))
    }

    fn spawn_into(
        &self,
        session: &SessionName,
        window: &WindowName,
        _argv: &[String],
        _cwd: &Path,
        _env: &BTreeMap<String, String>,
    ) -> Result<SpawnResult, TransportError> {
        Ok(self.spawn_result("spawn_into", session, window))
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

    fn capture(
        &self,
        _target: &Target,
        _range: CaptureRange,
    ) -> Result<CapturedText, TransportError> {
        panic!("fault adapter: startup prompt capture panicked after spawn");
    }

    fn query(
        &self,
        _target: &Target,
        _field: PaneField,
    ) -> Result<Option<String>, TransportError> {
        Ok(None)
    }

    fn liveness(
        &self,
        _pane: &PaneId,
    ) -> Result<team_agent::transport::PaneLiveness, TransportError> {
        Ok(team_agent::transport::PaneLiveness::Live)
    }

    fn has_pane(&self, pane: &PaneId) -> Result<Option<bool>, TransportError> {
        let spawns = self.spawns.lock().expect("spawns lock");
        Ok(Some(spawns.iter().enumerate().any(|(idx, _)| {
            pane.as_str() == format!("%{idx}")
        })))
    }

    fn list_targets(&self) -> Result<Vec<PaneInfo>, TransportError> {
        Ok(Vec::new())
    }

    fn has_session(&self, _session: &SessionName) -> Result<bool, TransportError> {
        Ok(!self.spawns.lock().expect("spawns lock").is_empty())
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
