//! Bug 4 RED: runtime.dangerous_auto_approve must reach real Codex worker argv.
//!
//! The provider adapter already knows how to emit the bypass flag. Fresh launch and
//! restart must pass the same resolved safety/tool context into that adapter instead
//! of falling back to restricted `--ask-for-approval on-request`.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use serde_json::json;
use team_agent::lifecycle::{launch_with_transport, restart_with_transport};
use team_agent::provider::{get_adapter, AuthMode, Provider};
use team_agent::transport::{
    AttachOutcome, BackendKind, CaptureRange, CapturedText, InjectPayload, InjectReport,
    InjectStage, InjectVerification, Key, PaneField, PaneId, PaneInfo, SessionName,
    SetEnvOutcome, SpawnResult, SubmitVerification, Target, Transport, TransportError,
    TurnVerification, WindowName,
};

const CODEX_BYPASS: &str = "--dangerously-bypass-approvals-and-sandbox";

#[test]
fn codex_bypass_argv_is_consistent_across_adapter_launch_and_restart() {
    let adapter = get_adapter(Provider::Codex);
    let adapter_argv = adapter
        .build_command_with_tools(
            AuthMode::Subscription,
            None,
            Some("Worker"),
            Some("gpt-5.5"),
            &["mcp_team", "dangerous_auto_approve"],
        )
        .expect("adapter command");
    let mut failures = bypass_argv_failures(&adapter_argv, "provider adapter");

    let team = compiled_team_dir("launch-dangerous");
    let launch_transport = RecordingTransport::new();
    launch_with_transport(
        &team.join("team.spec.yaml"),
        false,
        true,
        true,
        &launch_transport,
    )
    .expect("launch fixture should spawn worker");
    failures.extend(bypass_argv_failures(
        &launch_transport.only_spawn_argv("fresh launch"),
        "fresh launch",
    ));

    let restart_ws = restart_workspace("restart-dangerous");
    let restart_transport = RecordingTransport::new();
    restart_with_transport(&restart_ws, true, None, &restart_transport)
        .expect("restart fixture should spawn worker");
    failures.extend(bypass_argv_failures(
        &restart_transport.only_spawn_argv("restart"),
        "restart",
    ));

    assert!(
        failures.is_empty(),
        "dangerous Codex argv contract failed:\n{}",
        failures.join("\n")
    );
}

fn bypass_argv_failures(argv: &[String], label: &str) -> Vec<String> {
    let mut failures = Vec::new();
    if !argv.iter().any(|arg| arg == CODEX_BYPASS) {
        failures.push(format!(
            "{label}: Codex argv must contain {CODEX_BYPASS}; argv={argv:?}"
        ));
    }
    if argv
        .iter()
        .any(|arg| arg == "--ask-for-approval" || arg == "--sandbox")
    {
        failures.push(format!(
            "{label}: bypass argv must not also contain restricted approval/sandbox flags; argv={argv:?}"
        ));
    }
    failures
}

fn compiled_team_dir(tag: &str) -> PathBuf {
    let team = tmp_dir(tag).join("teamdir");
    write_team_files(&team, "dangerteam");
    let spec = team_agent::compiler::compile_team(&team).unwrap();
    std::fs::write(
        team.join("team.spec.yaml"),
        team_agent::model::yaml::dumps(&spec),
    )
    .unwrap();
    team
}

fn restart_workspace(tag: &str) -> PathBuf {
    let ws = tmp_dir(tag);
    write_team_files(&ws, "dangerteam");
    let spec = team_agent::compiler::compile_team(&ws).unwrap();
    std::fs::write(
        ws.join("team.spec.yaml"),
        team_agent::model::yaml::dumps(&spec),
    )
    .unwrap();
    team_agent::state::persist::save_runtime_state(
        &ws,
        &json!({
            "active_team_key": "dangerteam",
            "session_name": "team-dangerteam",
            "runtime": {"dangerous_auto_approve": true},
            "agents": {
                "worker_a": {
                    "status": "running",
                    "provider": "codex",
                    "auth_mode": "subscription",
                    "role": "Worker A",
                    "model": "gpt-5.5",
                    "window": "worker_a",
                    "tools": ["mcp_team"]
                }
            }
        }),
    )
    .unwrap();
    seed_healthy_coordinator(&ws);
    ws
}

fn write_team_files(team: &Path, name: &str) {
    std::fs::create_dir_all(team.join("agents")).unwrap();
    std::fs::write(
        team.join("TEAM.md"),
        format!(
            "---\nname: {name}\nobjective: Dangerous argv contract.\nprovider: codex\ndangerous_auto_approve: true\n---\n\nTeam.\n"
        ),
    )
    .unwrap();
    std::fs::write(
        team.join("agents").join("worker_a.md"),
        "---\nname: worker_a\nrole: Worker A\nprovider: codex\nmodel: gpt-5.5\nauth_mode: subscription\ntools:\n  - mcp_team\n---\n\nWorker.\n",
    )
    .unwrap();
}

fn seed_healthy_coordinator(workspace: &Path) {
    let workspace = team_agent::coordinator::WorkspacePath::new(workspace.to_path_buf());
    std::fs::create_dir_all(team_agent::model::paths::runtime_dir(workspace.as_path())).unwrap();
    let _ = team_agent::message_store::MessageStore::open(workspace.as_path()).unwrap();
    let pid = team_agent::coordinator::Pid::new(std::process::id());
    team_agent::coordinator::write_coordinator_metadata(
        &workspace,
        pid,
        team_agent::coordinator::MetadataSource::Boot,
    )
    .unwrap();
    std::fs::write(
        team_agent::coordinator::coordinator_pid_path(&workspace),
        pid.to_string(),
    )
    .unwrap();
}

fn tmp_dir(tag: &str) -> PathBuf {
    static N: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "ta-rs-bug4-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::canonicalize(dir).unwrap()
}

#[derive(Debug, Default)]
struct RecordingTransport {
    spawns: Mutex<Vec<Vec<String>>>,
}

impl RecordingTransport {
    fn new() -> Self {
        Self::default()
    }

    fn only_spawn_argv(&self, label: &str) -> Vec<String> {
        let spawns = self.spawns.lock().unwrap();
        assert_eq!(
            spawns.len(),
            1,
            "{label}: fixture should record exactly one worker spawn; spawns={spawns:?}"
        );
        spawns[0].clone()
    }

    fn spawn_result(
        &self,
        session: &SessionName,
        window: &WindowName,
        argv: &[String],
    ) -> SpawnResult {
        let mut spawns = self.spawns.lock().unwrap();
        spawns.push(argv.to_vec());
        SpawnResult {
            pane_id: PaneId::new(format!("%{}", spawns.len())),
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
        argv: &[String],
        _cwd: &Path,
        _env: &BTreeMap<String, String>,
    ) -> Result<SpawnResult, TransportError> {
        Ok(self.spawn_result(session, window, argv))
    }

    fn spawn_into(
        &self,
        session: &SessionName,
        window: &WindowName,
        argv: &[String],
        _cwd: &Path,
        _env: &BTreeMap<String, String>,
    ) -> Result<SpawnResult, TransportError> {
        Ok(self.spawn_result(session, window, argv))
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
        range: CaptureRange,
    ) -> Result<CapturedText, TransportError> {
        Ok(CapturedText {
            text: String::new(),
            range,
        })
    }

    fn query(
        &self,
        _target: &Target,
        field: PaneField,
    ) -> Result<Option<String>, TransportError> {
        match field {
            PaneField::PaneWidth => Ok(Some("120".to_string())),
            _ => Ok(None),
        }
    }

    fn liveness(
        &self,
        _pane: &PaneId,
    ) -> Result<team_agent::transport::PaneLiveness, TransportError> {
        Ok(team_agent::transport::PaneLiveness::Live)
    }

    fn list_targets(&self) -> Result<Vec<PaneInfo>, TransportError> {
        Ok(Vec::new())
    }

    fn has_session(&self, _session: &SessionName) -> Result<bool, TransportError> {
        Ok(!self.spawns.lock().unwrap().is_empty())
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
