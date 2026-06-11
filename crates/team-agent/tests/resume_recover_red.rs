#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{json, Value};
use serial_test::serial;
use team_agent::coordinator::{
    coordinator_pid_path, write_coordinator_metadata, MetadataSource, Pid, WorkspacePath,
};
use team_agent::event_log::EventLog;
use team_agent::lifecycle::{restart_with_transport, RestartReport, ResumeDecision};
use team_agent::message_store::MessageStore;
use team_agent::state::persist::{load_runtime_state, save_runtime_state};
use team_agent::transport::{
    AttachOutcome, BackendKind, CaptureRange, CapturedText, InjectPayload, InjectReport,
    InjectStage, InjectVerification, Key, PaneField, PaneId, PaneInfo, PaneLiveness, SessionName,
    SetEnvOutcome, SpawnResult, SubmitVerification, Target, Transport, TransportError,
    TurnVerification, WindowName,
};

#[test]
#[serial(env)]
fn restart_repairs_missing_resume_session_from_events_before_resume_decision() {
    let fixture = ResumeFixture::new("event-repair-success");
    fixture.seed_missing_session_state();
    let rollout = fixture.root.join("rollouts").join("worker-a.jsonl");
    std::fs::create_dir_all(rollout.parent().unwrap()).unwrap();
    std::fs::write(&rollout, "{}\n").unwrap();
    EventLog::new(&fixture.team)
        .write(
            "session.captured",
            json!({
                "agent_id": "worker_a",
                "provider": "codex",
                "session_id": "sess-from-events",
                "rollout_path": rollout.to_string_lossy().to_string(),
                "attribution_confidence": "high"
            }),
        )
        .unwrap();
    let _env = fixture.bounded_empty_provider_home_env();
    let transport = ResumeTransport::new();

    let report = restart_with_transport(&fixture.team, false, None, &transport)
        .expect("restart should repair missing session from events and continue to resume");

    let state = load_runtime_state(&fixture.team).unwrap();
    let agent = state
        .pointer("/agents/worker_a")
        .unwrap_or_else(|| panic!("worker_a missing; state={state}"));
    assert_eq!(
        agent.get("session_id").and_then(Value::as_str),
        Some("sess-from-events"),
        "restart must restore session_id from the newest valid session.captured event before classification; state={state}"
    );
    assert_eq!(
        agent.get("captured_via").and_then(Value::as_str),
        Some("event_log_repair"),
        "event-log repair must mark captured_via=event_log_repair for auditability; state={state}"
    );
    assert_eq!(
        agent.get("attribution_confidence").and_then(Value::as_str),
        Some("high"),
        "event-log repair must preserve attribution confidence; state={state}"
    );
    assert!(
        matches!(
            report,
            RestartReport::Restarted { ref agents, .. }
                if agents.iter().any(|decision| {
                    decision.agent_id.as_str() == "worker_a"
                        && decision.decision == ResumeDecision::Resume
                        && decision.session_id.as_ref().is_some_and(|id| id.as_str() == "sess-from-events")
                })
        ),
        "restart must choose resume after event-log repair, not fresh/refuse; report={report:?} state={state}"
    );
}

#[test]
#[serial(env)]
fn restart_does_not_resurrect_tombstoned_event_session_and_refuses_without_allow_fresh() {
    let fixture = ResumeFixture::new("event-repair-tombstone");
    fixture.seed_missing_session_state();
    let rollout = fixture
        .root
        .join("rollouts")
        .join("worker-a-tombstoned.jsonl");
    std::fs::create_dir_all(rollout.parent().unwrap()).unwrap();
    std::fs::write(&rollout, "{}\n").unwrap();
    EventLog::new(&fixture.team)
        .write(
            "session.captured",
            json!({
                "agent_id": "worker_a",
                "provider": "codex",
                "session_id": "sess-tombstoned",
                "rollout_path": rollout.to_string_lossy().to_string(),
                "attribution_confidence": "high"
            }),
        )
        .unwrap();
    EventLog::new(&fixture.team)
        .write(
            "discard.session_tombstone",
            json!({ "agent_id": "worker_a", "session_id": "sess-tombstoned" }),
        )
        .unwrap();
    let _env = fixture.bounded_empty_provider_home_env();
    let transport = ResumeTransport::new();

    let report = restart_with_transport(&fixture.team, false, None, &transport)
        .expect("tombstoned event recovery should produce an honest typed refusal, not an error");
    let state = load_runtime_state(&fixture.team).unwrap();

    assert!(
        matches!(report, RestartReport::RefusedResumeNotReady { .. } | RestartReport::RefusedResumeAtomicity { .. }),
        "a newer discard.session_tombstone must block event-log resurrection and keep allow_fresh=false hard refusal; report={report:?} state={state}"
    );
    assert!(
        state
            .pointer("/agents/worker_a/session_id")
            .and_then(Value::as_str)
            .is_none(),
        "tombstoned session must not be restored into state; state={state}"
    );
}

struct ResumeFixture {
    root: PathBuf,
    team: PathBuf,
}

impl ResumeFixture {
    fn new(tag: &str) -> Self {
        let root = tmp_dir(tag);
        let team = root.join("team");
        std::fs::create_dir_all(team.join("agents")).unwrap();
        std::fs::write(
            team.join("TEAM.md"),
            "---\nname: resumeteam\nobjective: Resume recovery contract.\nprovider: codex\n---\n\nTeam.\n",
        )
        .unwrap();
        std::fs::write(
            team.join("agents").join("worker_a.md"),
            "---\nname: worker_a\nrole: Worker A\nprovider: codex\nmodel: gpt-5.5\nauth_mode: subscription\ntools:\n  - mcp_team\n---\n\nWorker.\n",
        )
        .unwrap();
        let spec = team_agent::compiler::compile_team(&team).unwrap();
        std::fs::write(
            team.join("team.spec.yaml"),
            team_agent::model::yaml::dumps(&spec),
        )
        .unwrap();
        Self { root, team }
    }

    fn seed_missing_session_state(&self) {
        save_runtime_state(
            &self.team,
            &json!({
                "active_team_key": "resumeteam",
                "team_dir": self.team.to_string_lossy().to_string(),
                "spec_path": self.team.join("team.spec.yaml").to_string_lossy().to_string(),
                "session_name": "team-resumeteam",
                "agents": {
                    "worker_a": {
                        "status": "running",
                        "provider": "codex",
                        "auth_mode": "subscription",
                        "role": "Worker A",
                        "tools": ["mcp_team"],
                        "window": "worker_a",
                        "pane_id": "%1",
                        "pane_pid": 4242,
                        "spawn_cwd": self.team.to_string_lossy().to_string(),
                        "spawned_at": "2026-06-09T00:00:00+00:00",
                        "first_send_at": "2026-06-09T00:01:00+00:00"
                    }
                }
            }),
        )
        .unwrap();
        self.seed_healthy_coordinator();
    }

    fn seed_healthy_coordinator(&self) {
        let workspace = WorkspacePath::new(self.team.clone());
        std::fs::create_dir_all(team_agent::model::paths::runtime_dir(workspace.as_path()))
            .unwrap();
        let _ = MessageStore::open(workspace.as_path()).unwrap();
        let pid = Pid::new(std::process::id());
        write_coordinator_metadata(&workspace, pid, MetadataSource::Boot).unwrap();
        std::fs::write(coordinator_pid_path(&workspace), pid.to_string()).unwrap();
    }

    fn bounded_empty_provider_home_env(&self) -> EnvGuard {
        let home = self.root.join("empty-home");
        std::fs::create_dir_all(&home).unwrap();
        EnvGuard::set_many(vec![
            ("HOME", home.to_string_lossy().to_string()),
            (
                "TEAM_AGENT_RESTART_SESSION_CAPTURE_DEADLINE_MS",
                "25".to_string(),
            ),
            (
                "TEAM_AGENT_RESTART_SESSION_CAPTURE_POLL_MS",
                "5".to_string(),
            ),
        ])
    }
}

impl Drop for ResumeFixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

struct EnvGuard {
    old: Vec<(&'static str, Option<String>)>,
}

impl EnvGuard {
    fn set_many(pairs: Vec<(&'static str, String)>) -> Self {
        let old = pairs
            .iter()
            .map(|(key, _)| (*key, std::env::var(key).ok()))
            .collect::<Vec<_>>();
        for (key, value) in pairs {
            std::env::set_var(key, value);
        }
        Self { old }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (key, old) in self.old.drain(..) {
            match old {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
    }
}

#[derive(Default)]
struct ResumeTransport;

impl ResumeTransport {
    fn new() -> Self {
        Self
    }
}

impl Transport for ResumeTransport {
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
            pane_id: PaneId::new("%10"),
            session: session.clone(),
            window: window.clone(),
            child_pid: Some(10_010),
        })
    }

    fn spawn_into(
        &self,
        session: &SessionName,
        window: &WindowName,
        argv: &[String],
        cwd: &Path,
        env: &BTreeMap<String, String>,
    ) -> Result<SpawnResult, TransportError> {
        self.spawn_first(session, window, argv, cwd, env)
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

    fn query(&self, _target: &Target, field: PaneField) -> Result<Option<String>, TransportError> {
        Ok(match field {
            PaneField::PaneId => Some("%10".to_string()),
            PaneField::SessionName => Some("team-resumeteam".to_string()),
            _ => None,
        })
    }

    fn liveness(&self, _pane: &PaneId) -> Result<PaneLiveness, TransportError> {
        Ok(PaneLiveness::Live)
    }

    fn list_targets(&self) -> Result<Vec<PaneInfo>, TransportError> {
        Ok(Vec::new())
    }

    fn has_session(&self, _session: &SessionName) -> Result<bool, TransportError> {
        Ok(true)
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

fn tmp_dir(tag: &str) -> PathBuf {
    static N: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "ta-rs-resume-recover-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::canonicalize(dir).unwrap()
}
