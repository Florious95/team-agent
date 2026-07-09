#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[path = "support/hermetic.rs"]
mod hermetic_guard;
#[allow(dead_code)]
fn _hermetic_boundary_marker(_: &hermetic_guard::HermeticTestEnv) {}

use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use serial_test::file_serial;
use team_agent::lifecycle::{launch_with_transport, restart_with_transport};
use team_agent::state::persist::{load_runtime_state, save_runtime_state};
use team_agent::tmux_backend::TmuxBackend;
use team_agent::transport::{
    AttachOutcome, BackendKind, CaptureRange, CapturedText, InjectPayload, InjectReport,
    InjectStage, InjectVerification, Key, PaneField, PaneId, PaneInfo, PaneLiveness, SessionName,
    SetEnvOutcome, SpawnResult, SubmitVerification, Target, Transport, TransportError,
    TurnVerification, WindowName,
};

#[test]
#[ignore = "real-machine: restart/session liveness lifecycle gate"]
fn restart_rechecks_session_liveness_before_spawning_second_worker() {
    let fixture = RestartFixture::new("restart-liveness-recording", &["alpha", "bravo"]);
    fixture.seed_resumable_state_with_stale_receiver();
    let transport = RestartLivenessTransport::new();

    let result = restart_with_transport(&fixture.team, false, None, &transport);
    let records = transport.spawn_records();

    assert_eq!(
        records.len(),
        1,
        "after alpha spawn_first reports success but the recreated session disappears, restart must stop before spawning bravo; result={result:?} records={records:?}"
    );
    assert_eq!(records[0].kind, "spawn_first", "alpha must be the only spawn and must recreate the session; records={records:?}");
    assert!(
        records.iter().all(|record| record.kind != "spawn_into"),
        "restart must not call spawn_into/new-window for bravo against a dead session; result={result:?} records={records:?}"
    );
    let text = format!("{result:?}");
    assert!(
        text.contains("session_disappeared_after_spawn") || text.contains("provider_resume_exited"),
        "restart must fail with an explicit first-agent/session liveness error, not a later tmux no-server error; result={result:?} records={records:?}"
    );
    assert!(
        text.contains("alpha"),
        "restart error must name the first resumed agent whose session disappeared; result={result:?}"
    );
}

#[test]
#[ignore = "real-machine: restart/session liveness lifecycle gate"]
fn restart_partial_failure_clears_stale_receiver_and_does_not_claim_running_without_pane() {
    let fixture = RestartFixture::new("restart-liveness-state", &["alpha", "bravo"]);
    fixture.seed_resumable_state_with_stale_receiver();
    let transport = RestartLivenessTransport::new();

    let _ = restart_with_transport(&fixture.team, false, None, &transport);
    let state = load_runtime_state(&fixture.team).unwrap();

    assert!(
        state.pointer("/leader_receiver/status").and_then(Value::as_str) != Some("attached"),
        "after restart kills the session and rebuild fails, state must not leave leader_receiver.status=attached to a dead pane; state={state}"
    );
    for agent in ["alpha", "bravo"] {
        let row = state
            .pointer(&format!("/agents/{agent}"))
            .unwrap_or_else(|| panic!("agent row exists for {agent}; state={state}"));
        let has_session = row.get("session_id").and_then(Value::as_str).is_some();
        let running_without_pane = row.get("status").and_then(Value::as_str) == Some("running")
            && row.get("pane_id").and_then(Value::as_str).is_none();
        assert!(
            !(has_session && running_without_pane),
            "restart must not treat session_id without a live pane binding as successful restart; agent={agent} row={row} state={state}"
        );
    }
}

#[test]
#[ignore = "real-machine: launch pane identity lifecycle gate"]
fn launch_persists_real_pane_id_even_when_pane_pid_is_unavailable() {
    let fixture = RestartFixture::new("restart-liveness-pane-id", &["alpha"]);
    let transport = LaunchPaneIdTransport::new();

    launch_with_transport(
        &fixture.team.join("team.spec.yaml"),
        false,
        true,
        true,
        &transport,
    )
    .expect("launch should reach fake transport spawn");

    let state = load_runtime_state(&fixture.root).unwrap();
    assert_eq!(
        state.pointer("/agents/alpha/pane_id").and_then(Value::as_str),
        Some("%0"),
        "launch must persist the real %pane_id returned by spawn even when pane_pid/list_targets is unavailable; state={state}"
    );
}

#[test]
#[ignore = "real-machine: needs real tmux/binary; fake codex exits first resumed worker to reproduce no-server restart"]
#[file_serial(tmux)]
fn real_tmux_restart_names_first_resume_agent_when_first_resume_exits_before_second_spawn() {
    let fixture = RestartFixture::new("restart-liveness-real-tmux", &["alpha", "bravo"]);
    fixture.seed_resumable_state_with_stale_receiver();
    let _cleanup = TmuxCleanup::new(&fixture.team);
    let fake_bin = fixture.install_fake_codex_that_exits_alpha_resume();

    let output = Command::new(env!("CARGO_BIN_EXE_team-agent"))
        .args([
            "restart",
            "--workspace",
            fixture.team.to_str().unwrap(),
            "--json",
        ])
        .env(
            "PATH",
            format!("{}:{}", fake_bin.display(), std::env::var("PATH").unwrap_or_default()),
        )
        .env("CODEX_FAKE_LOG", fixture.root.join("fake-codex.log"))
        .output()
        .expect("run team-agent restart");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let combined = format!("{stdout}\n{stderr}");

    assert!(
        !output.status.success(),
        "fixture expects first resumed codex process to exit and restart to fail honestly; stdout={stdout} stderr={stderr}"
    );
    assert!(
        combined.contains("alpha")
            && (combined.contains("session_disappeared_after_spawn")
                || combined.contains("provider_resume_exited")),
        "real tmux restart must name first resumed agent/session disappearance, not only a later transport failure; stdout={stdout} stderr={stderr}"
    );
    assert!(
        !(combined.contains("bravo") && combined.contains("no server running")),
        "restart must not continue to bravo new-window against a dead server; stdout={stdout} stderr={stderr}"
    );
}

#[derive(Clone, Debug)]
#[allow(dead_code)]
struct SpawnRecord {
    kind: String,
    window: String,
    argv: Vec<String>,
}

#[derive(Clone, Default)]
struct RestartLivenessTransport {
    inner: Arc<Mutex<RestartTransportState>>,
}

#[derive(Default)]
struct RestartTransportState {
    has_session_answers: VecDeque<bool>,
    spawns: Vec<SpawnRecord>,
}

impl RestartLivenessTransport {
    fn new() -> Self {
        let mut has_session_answers = VecDeque::new();
        has_session_answers.push_back(true);
        Self {
            inner: Arc::new(Mutex::new(RestartTransportState {
                has_session_answers,
                spawns: Vec::new(),
            })),
        }
    }

    fn spawn_records(&self) -> Vec<SpawnRecord> {
        self.inner.lock().unwrap().spawns.clone()
    }

    fn record_spawn(
        &self,
        kind: &str,
        session: &SessionName,
        window: &WindowName,
        argv: &[String],
    ) -> Result<SpawnResult, TransportError> {
        let mut inner = self.inner.lock().unwrap();
        inner.spawns.push(SpawnRecord {
            kind: kind.to_string(),
            window: window.as_str().to_string(),
            argv: argv.to_vec(),
        });
        if kind == "spawn_into" {
            return Err(TransportError::MuxUnavailable {
                backend: BackendKind::Tmux,
                detail: format!(
                    "session disappeared after alpha resume; attempted new-window for {}",
                    window.as_str()
                ),
            });
        }
        Ok(SpawnResult {
            pane_id: PaneId::new("%0"),
            session: session.clone(),
            window: window.clone(),
            child_pid: None,
        })
    }
}

impl Transport for RestartLivenessTransport {
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
        self.record_spawn("spawn_first", session, window, argv)
    }

    fn spawn_into(
        &self,
        session: &SessionName,
        window: &WindowName,
        argv: &[String],
        _cwd: &Path,
        _env: &BTreeMap<String, String>,
    ) -> Result<SpawnResult, TransportError> {
        self.record_spawn("spawn_into", session, window, argv)
    }

    fn has_session(&self, _session: &SessionName) -> Result<bool, TransportError> {
        Ok(self
            .inner
            .lock()
            .unwrap()
            .has_session_answers
            .pop_front()
            .unwrap_or(false))
    }

    fn kill_session(&self, _session: &SessionName) -> Result<(), TransportError> {
        Ok(())
    }

    fn list_targets(&self) -> Result<Vec<PaneInfo>, TransportError> {
        Ok(Vec::new())
    }

    fn list_windows(&self, _session: &SessionName) -> Result<Vec<WindowName>, TransportError> {
        Ok(Vec::new())
    }

    fn query(&self, _target: &Target, field: PaneField) -> Result<Option<String>, TransportError> {
        Ok(match field {
            PaneField::PaneId => Some("%0".to_string()),
            PaneField::SessionName => Some("team-ctxteam".to_string()),
            _ => None,
        })
    }

    fn liveness(&self, _pane: &PaneId) -> Result<PaneLiveness, TransportError> {
        Ok(PaneLiveness::Live)
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

    fn capture(&self, _target: &Target, range: CaptureRange) -> Result<CapturedText, TransportError> {
        Ok(CapturedText {
            text: String::new(),
            range,
        })
    }

    fn set_session_env(
        &self,
        _session: &SessionName,
        _key: &str,
        _value: &str,
    ) -> Result<SetEnvOutcome, TransportError> {
        Ok(SetEnvOutcome::Applied)
    }

    fn kill_window(&self, _target: &Target) -> Result<(), TransportError> {
        Ok(())
    }

    fn attach_session(&self, _session: &SessionName) -> Result<AttachOutcome, TransportError> {
        Ok(AttachOutcome::Attached)
    }
}

#[derive(Clone, Default)]
struct LaunchPaneIdTransport {
    spawns: Arc<Mutex<Vec<SpawnRecord>>>,
}

impl LaunchPaneIdTransport {
    fn new() -> Self {
        Self::default()
    }
}

impl Transport for LaunchPaneIdTransport {
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
        self.spawns.lock().unwrap().push(SpawnRecord {
            kind: "spawn_first".to_string(),
            window: window.as_str().to_string(),
            argv: argv.to_vec(),
        });
        Ok(SpawnResult {
            pane_id: PaneId::new("%0"),
            session: session.clone(),
            window: window.clone(),
            child_pid: None,
        })
    }

    fn spawn_into(
        &self,
        session: &SessionName,
        window: &WindowName,
        argv: &[String],
        _cwd: &Path,
        _env: &BTreeMap<String, String>,
    ) -> Result<SpawnResult, TransportError> {
        self.spawns.lock().unwrap().push(SpawnRecord {
            kind: "spawn_into".to_string(),
            window: window.as_str().to_string(),
            argv: argv.to_vec(),
        });
        Ok(SpawnResult {
            pane_id: PaneId::new("%1"),
            session: session.clone(),
            window: window.clone(),
            child_pid: None,
        })
    }

    fn has_session(&self, _session: &SessionName) -> Result<bool, TransportError> {
        Ok(false)
    }

    fn list_targets(&self) -> Result<Vec<PaneInfo>, TransportError> {
        Ok(Vec::new())
    }

    fn list_windows(&self, session: &SessionName) -> Result<Vec<WindowName>, TransportError> {
        let _ = session;
        Ok(self
            .spawns
            .lock()
            .unwrap()
            .iter()
            .map(|record| WindowName::new(record.window.as_str()))
            .collect())
    }

    fn query(&self, _target: &Target, field: PaneField) -> Result<Option<String>, TransportError> {
        Ok(match field {
            PaneField::PaneId => Some("%0".to_string()),
            PaneField::SessionName => Some("team-ctxteam".to_string()),
            _ => None,
        })
    }

    fn liveness(&self, _pane: &PaneId) -> Result<PaneLiveness, TransportError> {
        Ok(PaneLiveness::Live)
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

    fn capture(&self, _target: &Target, range: CaptureRange) -> Result<CapturedText, TransportError> {
        Ok(CapturedText {
            text: String::new(),
            range,
        })
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

struct RestartFixture {
    root: PathBuf,
    team: PathBuf,
    agents: Vec<String>,
}

impl RestartFixture {
    fn new(tag: &str, agents: &[&str]) -> Self {
        let root = tmp_dir(tag);
        let team = root.join("teamdir");
        std::fs::create_dir_all(team.join("agents")).unwrap();
        std::fs::write(
            team.join("TEAM.md"),
            "---\nname: ctxteam\nobjective: Restart liveness contract.\nprovider: codex\n---\n\nTeam.\n",
        )
        .unwrap();
        for agent in agents {
            std::fs::write(
                team.join("agents").join(format!("{agent}.md")),
                format!(
                    "---\nname: {agent}\nrole: Worker {agent}\nprovider: codex\nmodel: gpt-5.5\nauth_mode: subscription\ntools:\n  - mcp_team\n---\n\nWorker.\n"
                ),
            )
            .unwrap();
        }
        let spec = team_agent::compiler::compile_team(&team).unwrap();
        std::fs::write(team.join("team.spec.yaml"), team_agent::model::yaml::dumps(&spec)).unwrap();
        Self {
            root,
            team,
            agents: agents.iter().map(|agent| (*agent).to_string()).collect(),
        }
    }

    fn seed_resumable_state_with_stale_receiver(&self) {
        let agents = self
            .agents
            .iter()
            .map(|agent| {
                (
                    agent.clone(),
                    json!({
                        "status": "running",
                        "provider": "codex",
                        "role": format!("Worker {agent}"),
                        "tools": ["mcp_team"],
                        "window": agent,
                        "owner_team_id": "ctxteam",
                        "session_id": format!("session-{agent}"),
                        "rollout_path": self.root.join(format!("{agent}.jsonl")).to_string_lossy().to_string(),
                        "spawn_cwd": self.team.to_string_lossy().to_string(),
                        "spawned_at": "2026-06-08T00:00:00+00:00",
                        "first_send_at": "2026-06-08T00:01:00+00:00"
                    }),
                )
            })
            .collect::<serde_json::Map<_, _>>();
        for agent in &self.agents {
            std::fs::write(self.root.join(format!("{agent}.jsonl")), "{}\n").unwrap();
        }
        save_runtime_state(
            &self.team,
            &json!({
                "active_team_key": "ctxteam",
                "team_dir": self.team.to_string_lossy().to_string(),
                "spec_path": self.team.join("team.spec.yaml").to_string_lossy().to_string(),
                "session_name": "team-ctxteam",
                "leader_receiver": {
                    "mode": "direct_tmux",
                    "status": "attached",
                    "pane_id": "%leader",
                    "provider": "claude",
                    "owner_epoch": 7
                },
                "agents": agents
            }),
        )
        .unwrap();
        seed_healthy_coordinator(&self.team);
    }

    fn install_fake_codex_that_exits_alpha_resume(&self) -> PathBuf {
        let bin = self.root.join("fake-bin");
        std::fs::create_dir_all(&bin).unwrap();
        let script = bin.join("codex");
        std::fs::write(
            &script,
            r#"#!/bin/sh
echo "$@" >> "${CODEX_FAKE_LOG:-/tmp/team-agent-fake-codex.log}"
case "$*" in
  *session-alpha*) exit 0 ;;
  *) sleep 30 ;;
esac
"#,
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        bin
    }
}

struct TmuxCleanup {
    workspace: PathBuf,
}

impl TmuxCleanup {
    fn new(workspace: &Path) -> Self {
        Self {
            workspace: workspace.to_path_buf(),
        }
    }
}

impl Drop for TmuxCleanup {
    fn drop(&mut self) {
        TmuxBackend::for_workspace(&self.workspace).kill_server();
    }
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
        "ta-rs-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::canonicalize(dir).unwrap()
}
