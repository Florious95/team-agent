//! Restart must preserve provider session context before destructive teardown.
//!
//! #240 root cause: `restart`/`shutdown` decide or kill from runtime state before
//! force-refreshing missing provider `session_id`s. A real provider session can exist under the
//! provider home while `state.agents[*].session_id` is still null; destructive lifecycle must capture
//! it first and resume from that durable id.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use serde_json::json;
use serial_test::serial;
use team_agent::lifecycle::{
    classify_restart_plan, restart_with_transport, RestartReport, ResumeDecision, StartMode,
};
use team_agent::model::enums::Provider;
use team_agent::provider::get_adapter;
use team_agent::state::persist::{load_runtime_state, save_runtime_state};
use team_agent::transport::{
    AttachOutcome, BackendKind, CaptureRange, CapturedText, InjectPayload, InjectReport,
    InjectStage, InjectVerification, Key, PaneField, PaneId, PaneInfo, SessionName,
    SetEnvOutcome, SpawnResult, SubmitVerification, Target, Transport, TransportError,
    TurnVerification, WindowName,
};

#[test]
#[serial(env)]
fn provider_home_codex_rollout_session_meta_is_captured_for_restart_resume() {
    let fixture = RestartFixture::new("codex-home-capture", "codex");
    let _home = HomeGuard::with_home(fixture.root.join("home"));
    let codex_session = "codex-sess-240";
    seed_codex_home_rollout(&fixture.home(), &fixture.spawn_cwd, codex_session);
    seed_running_state_without_session(&fixture.team, "codex", &fixture.spawn_cwd);

    let adapter = get_adapter(Provider::Codex);
    let captured = adapter
        .capture_session_id("worker_a", &fixture.spawn_cwd, 0)
        .expect("Codex provider-home capture should not error")
        .expect("Codex rollout under HOME/.codex/sessions must be discoverable before restart");

    assert_eq!(
        captured.session_id.as_ref().map(|id| id.as_str()),
        Some(codex_session),
        "Codex capture must extract session_meta.payload.id from provider-home rollout JSONL"
    );
    assert!(
        captured
            .rollout_path
            .as_ref()
            .is_some_and(|path| path.as_path().starts_with(fixture.home().join(".codex/sessions"))),
        "Codex capture must scan provider home, not only spawn_cwd; captured={captured:?}"
    );
}

#[test]
#[serial(env)]
fn provider_home_codex_rollout_matches_workspace_spawn_cwd_to_team_subdir_across_symlink() {
    let fixture = RestartFixture::new_symlinked_workspace("codex-workspace-team-cwd", "codex");
    let _home = HomeGuard::with_home(fixture.root.join("home"));
    let codex_session = "codex-sess-workspace-team-subdir";
    seed_codex_home_rollout_with_cwd(&fixture.home(), &fixture.team, codex_session);
    seed_running_state_without_session(&fixture.team, "codex", &fixture.spawn_cwd);

    let adapter = get_adapter(Provider::Codex);
    let captured = adapter
        .capture_session_id("worker_a", &fixture.spawn_cwd, 0)
        .expect("Codex provider-home capture should not error")
        .expect(
            "Codex rollout cwd at the team subdir must match a worker spawn_cwd at the parent workspace, even through symlink-equivalent paths",
        );

    assert_eq!(
        captured.session_id.as_ref().map(|id| id.as_str()),
        Some(codex_session),
        "Codex capture must accept workspace↔team-subdir cwd granularity when the team dir belongs to this worker workspace"
    );
}

#[test]
#[serial(env)]
fn restart_force_capture_matches_workspace_spawn_cwd_to_team_subdir_across_symlink() {
    let fixture = RestartFixture::new_symlinked_workspace("restart-workspace-team-cwd", "codex");
    let _home = HomeGuard::with_home(fixture.root.join("home"));
    let codex_session = "codex-sess-resume-team-subdir";
    seed_codex_home_rollout_with_cwd(&fixture.home(), &fixture.team, codex_session);
    seed_running_state_without_session(&fixture.team, "codex", &fixture.spawn_cwd);
    let transport = RecordingTransport::new().with_session_present(true);

    let report = restart_with_transport(&fixture.team, false, None, &transport)
        .expect("restart should succeed after forced session capture from team-subdir rollout cwd");

    assert_restarted_with_resume(&report, codex_session);
    assert_state_session_id(&fixture.team, codex_session);
    assert_events_contain_resume_decision(&fixture.team, "resume", codex_session);
}

#[test]
#[serial(env)]
fn provider_home_codex_rollout_from_foreign_workspace_is_not_captured() {
    let fixture = RestartFixture::new_symlinked_workspace("codex-foreign-workspace", "codex");
    let _home = HomeGuard::with_home(fixture.root.join("home"));
    let foreign = fixture.root.join("foreign-workspace").join("teamdir");
    std::fs::create_dir_all(&foreign).unwrap();
    seed_codex_home_rollout_with_cwd(&fixture.home(), &foreign, "codex-sess-foreign");
    seed_running_state_without_session(&fixture.team, "codex", &fixture.spawn_cwd);

    let adapter = get_adapter(Provider::Codex);
    let captured = adapter
        .capture_session_id("worker_a", &fixture.spawn_cwd, 0)
        .expect("Codex provider-home capture should not error");

    assert!(
        captured.is_none(),
        "workspace↔team-subdir matching must stay scoped to this worker workspace; foreign rollout cwd must not be captured: {captured:?}"
    );
}

#[test]
#[serial(env)]
fn provider_home_claude_session_is_captured_for_restart_resume() {
    let fixture = RestartFixture::new("claude-home-capture", "claude");
    let _home = HomeGuard::with_home(fixture.root.join("home"));
    let claude_session = "claude-sess-240";
    seed_claude_home_session(&fixture.home(), &fixture.spawn_cwd, claude_session);
    seed_running_state_without_session(&fixture.team, "claude", &fixture.spawn_cwd);

    let adapter = get_adapter(Provider::Claude);
    let captured = adapter
        .capture_session_id("worker_a", &fixture.spawn_cwd, 0)
        .expect("Claude provider-home capture should not error")
        .expect("Claude session under HOME/.claude/sessions must be discoverable before restart");

    assert_eq!(
        captured.session_id.as_ref().map(|id| id.as_str()),
        Some(claude_session),
        "Claude capture must recover sessionId from provider-home session JSON"
    );
    assert!(
        captured
            .rollout_path
            .as_ref()
            .is_some_and(|path| path.as_path().starts_with(fixture.home().join(".claude/sessions"))),
        "Claude capture must scan provider home, not only spawn_cwd; captured={captured:?}"
    );
}

#[test]
#[serial(env)]
fn restart_force_captures_real_codex_session_meta_before_teardown() {
    let fixture = RestartFixture::new("restart-codex-force-capture", "codex");
    let _home = HomeGuard::with_home(fixture.root.join("home"));
    let codex_session = "codex-sess-before-kill";
    seed_codex_home_rollout(&fixture.home(), &fixture.spawn_cwd, codex_session);
    seed_running_state_without_session(&fixture.team, "codex", &fixture.spawn_cwd);
    let transport = RecordingTransport::new().with_session_present(true);

    let report = restart_with_transport(&fixture.team, false, None, &transport)
        .expect("restart should succeed after forced session capture");

    assert_restarted_with_resume(&report, codex_session);
    assert_spawn_contains_ordered(
        &transport.single_spawn_argv(),
        &["resume", codex_session],
        "Codex restart must spawn `codex ... resume <session_id>`",
    );
    assert_state_session_id(&fixture.team, codex_session);
    assert_events_contain_resume_decision(&fixture.team, "resume", codex_session);
    assert!(
        transport.kill_count() <= 1,
        "transport should not need multiple destructive teardowns; kill_count={}",
        transport.kill_count()
    );
}

#[test]
#[serial(env)]
fn restart_force_captures_claude_session_before_teardown() {
    let fixture = RestartFixture::new("restart-claude-force-capture", "claude");
    let _home = HomeGuard::with_home(fixture.root.join("home"));
    let claude_session = "claude-sess-before-kill";
    seed_claude_home_session(&fixture.home(), &fixture.spawn_cwd, claude_session);
    seed_running_state_without_session(&fixture.team, "claude", &fixture.spawn_cwd);
    let transport = RecordingTransport::new().with_session_present(true);

    let report = restart_with_transport(&fixture.team, false, None, &transport)
        .expect("restart should succeed after forced session capture");

    assert_restarted_with_resume(&report, claude_session);
    assert_spawn_contains_adjacent(
        &transport.single_spawn_argv(),
        &["--resume", claude_session],
        "Claude restart must spawn `claude ... --resume <session_id>`",
    );
    assert_state_session_id(&fixture.team, claude_session);
    assert_events_contain_resume_decision(&fixture.team, "resume", claude_session);
}

#[test]
#[serial(env)]
fn shutdown_force_captures_missing_provider_session_before_kill() {
    let fixture = RestartFixture::new("shutdown-force-capture", "codex");
    let _home = HomeGuard::with_home(fixture.root.join("home"));
    let codex_session = "codex-sess-before-shutdown";
    seed_codex_home_rollout(&fixture.home(), &fixture.spawn_cwd, codex_session);
    seed_running_state_without_session_no_coordinator(&fixture.team, "codex", &fixture.spawn_cwd);
    let transport = RecordingTransport::new().with_session_present(true);

    let result = team_agent::cli::lifecycle_port::shutdown_with_transport(
        &fixture.team,
        true,
        None,
        &transport,
    )
    .expect("shutdown should complete after pre-kill session capture");

    assert_eq!(result.get("ok").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(transport.kill_count(), 1, "shutdown fixture should kill the team session once");
    assert_state_session_id(&fixture.team, codex_session);
}

#[test]
fn persisted_session_id_is_the_resume_authority_for_restart_plan_and_spawn_argv() {
    let codex = RestartFixture::new("codex-resume-authority", "codex");
    seed_running_state_with_session(&codex.team, "codex", &codex.spawn_cwd, "codex-known-session");
    let codex_state = load_runtime_state(&codex.team).unwrap();
    let codex_plan = classify_restart_plan(&codex_state, false).unwrap();
    assert_eq!(codex_plan.decisions.len(), 1);
    assert_eq!(codex_plan.decisions[0].decision, ResumeDecision::Resume);
    assert_eq!(codex_plan.decisions[0].restart_mode, StartMode::Resumed);

    let codex_transport = RecordingTransport::new().with_session_present(true);
    let codex_report = restart_with_transport(&codex.team, false, None, &codex_transport)
        .expect("persisted Codex session_id must be resumable");
    assert_restarted_with_resume(&codex_report, "codex-known-session");
    assert_spawn_contains_ordered(
        &codex_transport.single_spawn_argv(),
        &["resume", "codex-known-session"],
        "Codex restart must use the persisted session id",
    );

    let claude = RestartFixture::new("claude-resume-authority", "claude");
    seed_running_state_with_session(&claude.team, "claude", &claude.spawn_cwd, "claude-known-session");
    let claude_transport = RecordingTransport::new().with_session_present(true);
    let claude_report = restart_with_transport(&claude.team, false, None, &claude_transport)
        .expect("persisted Claude session_id must be resumable");
    assert_restarted_with_resume(&claude_report, "claude-known-session");
    assert_spawn_contains_adjacent(
        &claude_transport.single_spawn_argv(),
        &["--resume", "claude-known-session"],
        "Claude restart must use the persisted session id",
    );
}

fn assert_restarted_with_resume(report: &RestartReport, expected_session: &str) {
    match report {
        RestartReport::Restarted { agents, .. } => {
            assert_eq!(agents.len(), 1, "fixture has exactly one worker; agents={agents:?}");
            assert_eq!(agents[0].decision, ResumeDecision::Resume);
            assert_eq!(agents[0].restart_mode, StartMode::Resumed);
            assert_eq!(
                agents[0].session_id.as_ref().map(|id| id.as_str()),
                Some(expected_session)
            );
        }
        other => panic!("restart must resume after pre-kill capture, got {other:?}"),
    }
}

fn assert_spawn_contains_adjacent(argv: &[String], expected: &[&str], message: &str) {
    assert!(
        argv.windows(expected.len())
            .any(|window| window.iter().map(String::as_str).eq(expected.iter().copied())),
        "{message}; argv={argv:?}"
    );
}

fn assert_spawn_contains_ordered(argv: &[String], expected: &[&str], message: &str) {
    let mut index = 0;
    for arg in argv {
        if expected.get(index).is_some_and(|needle| arg == needle) {
            index += 1;
        }
    }
    assert_eq!(
        index,
        expected.len(),
        "{message}; expected ordered tokens {expected:?}; argv={argv:?}"
    );
}

fn assert_state_session_id(team: &Path, expected_session: &str) {
    let state = load_runtime_state(team).unwrap();
    assert_eq!(
        state
            .get("agents")
            .and_then(|agents| agents.get("worker_a"))
            .and_then(|agent| agent.get("session_id"))
            .and_then(|value| value.as_str()),
        Some(expected_session),
        "destructive lifecycle must persist captured session_id as resume authority before teardown; state={state}"
    );
}

fn assert_events_contain_resume_decision(team: &Path, expected_decision: &str, expected_session: &str) {
    let events = std::fs::read_to_string(team.join(".team/logs/events.jsonl")).unwrap();
    let needle = format!(
        r#""event":"restart.resume_decision".*"decision":"{expected_decision}".*"has_session_id":true.*"session_id":"{expected_session}""#
    );
    assert!(
        events.lines().any(|line| {
            line.contains(r#""event":"restart.resume_decision""#)
                && line.contains(&format!(r#""decision":"{expected_decision}""#))
                && line.contains(r#""has_session_id":true"#)
                && line.contains(&format!(r#""session_id":"{expected_session}""#))
        }),
        "restart must audit a resume decision with captured session_id; missing pattern {needle}; events={events}"
    );
}

fn seed_codex_home_rollout(home: &Path, spawn_cwd: &Path, session_id: &str) {
    seed_codex_home_rollout_with_cwd(home, spawn_cwd, session_id);
}

fn seed_codex_home_rollout_with_cwd(home: &Path, rollout_cwd: &Path, session_id: &str) {
    let dir = home.join(".codex/sessions/2026/06/07");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join(format!("rollout-worker-a-{session_id}.jsonl")),
        format!(
            "{}\n{}\n",
            json!({
                "type": "session_meta",
                "payload": {
                    "id": session_id,
                    "cwd": rollout_cwd.to_string_lossy().to_string()
                }
            }),
            json!({"type":"event","payload":{"message":"later record"}})
        ),
    )
    .unwrap();
}

fn seed_claude_home_session(home: &Path, spawn_cwd: &Path, session_id: &str) {
    let dir = home.join(".claude/sessions");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("worker-a-session.json"),
        serde_json::to_string(&json!({
            "sessionId": session_id,
            "cwd": spawn_cwd.to_string_lossy().to_string()
        }))
        .unwrap(),
    )
    .unwrap();
}

fn seed_running_state_without_session(team: &Path, provider: &str, spawn_cwd: &Path) {
    seed_running_state(team, provider, spawn_cwd, None);
    seed_healthy_coordinator(team);
}

fn seed_running_state_without_session_no_coordinator(team: &Path, provider: &str, spawn_cwd: &Path) {
    seed_running_state(team, provider, spawn_cwd, None);
}

fn seed_running_state_with_session(team: &Path, provider: &str, spawn_cwd: &Path, session_id: &str) {
    seed_running_state(team, provider, spawn_cwd, Some(session_id));
    seed_healthy_coordinator(team);
}

fn seed_running_state(team: &Path, provider: &str, spawn_cwd: &Path, session_id: Option<&str>) {
    save_runtime_state(
        team,
        &json!({
            "active_team_key": "ctxteam",
            "team_dir": team.to_string_lossy().to_string(),
            "spec_path": team.join("team.spec.yaml").to_string_lossy().to_string(),
            "session_name": "team-ctxteam",
            "agents": {
                "worker_a": {
                    "status": "running",
                    "provider": provider,
                    "role": "Worker",
                    "tools": ["mcp_team"],
                    "window": "worker_a",
                    "owner_team_id": "ctxteam",
                    "session_id": session_id,
                    "rollout_path": null,
                    "spawn_cwd": spawn_cwd.to_string_lossy().to_string(),
                    "spawned_at": "2026-06-07T00:00:00+00:00",
                    "first_send_at": "2026-06-07T00:01:00+00:00"
                }
            }
        }),
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

struct RestartFixture {
    root: PathBuf,
    team: PathBuf,
    spawn_cwd: PathBuf,
}

impl RestartFixture {
    fn new(label: &str, provider: &str) -> Self {
        let root = tmp_dir(label);
        let team = root.join("teamdir");
        std::fs::create_dir_all(team.join("agents")).unwrap();
        std::fs::write(
            team.join("TEAM.md"),
            "---\nname: ctxteam\nobjective: Restart session context contract.\nprovider: codex\n---\n\nTeam.\n",
        )
        .unwrap();
        std::fs::write(
            team.join("agents/worker_a.md"),
            format!(
                "---\nname: worker_a\nrole: Worker\nprovider: {provider}\nmodel: {}\nauth_mode: subscription\ntools:\n  - mcp_team\n---\n\nWorker.\n",
                if provider == "claude" {
                    "claude-sonnet-4-6"
                } else {
                    "gpt-5.5"
                }
            ),
        )
        .unwrap();
        let spec = team_agent::compiler::compile_team(&team).unwrap();
        std::fs::write(
            team.join("team.spec.yaml"),
            team_agent::model::yaml::dumps(&spec),
        )
        .unwrap();
        let spawn_cwd = root.join("spawn-cwd");
        std::fs::create_dir_all(&spawn_cwd).unwrap();
        Self {
            root,
            team,
            spawn_cwd,
        }
    }

    fn new_symlinked_workspace(label: &str, provider: &str) -> Self {
        let root = tmp_dir(label);
        let workspace = root.join("real-workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let team = workspace.join("teamdir");
        std::fs::create_dir_all(team.join("agents")).unwrap();
        std::fs::write(
            team.join("TEAM.md"),
            "---\nname: ctxteam\nobjective: Restart session context contract.\nprovider: codex\n---\n\nTeam.\n",
        )
        .unwrap();
        std::fs::write(
            team.join("agents/worker_a.md"),
            format!(
                "---\nname: worker_a\nrole: Worker\nprovider: {provider}\nmodel: {}\nauth_mode: subscription\ntools:\n  - mcp_team\n---\n\nWorker.\n",
                if provider == "claude" {
                    "claude-sonnet-4-6"
                } else {
                    "gpt-5.5"
                }
            ),
        )
        .unwrap();
        let spec = team_agent::compiler::compile_team(&team).unwrap();
        std::fs::write(
            team.join("team.spec.yaml"),
            team_agent::model::yaml::dumps(&spec),
        )
        .unwrap();
        let spawn_cwd = root.join("workspace-symlink");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&workspace, &spawn_cwd).unwrap();
        #[cfg(not(unix))]
        {
            std::fs::create_dir_all(&spawn_cwd).unwrap();
        }
        Self {
            root,
            team,
            spawn_cwd,
        }
    }

    fn home(&self) -> PathBuf {
        self.root.join("home")
    }
}

fn tmp_dir(tag: &str) -> PathBuf {
    static N: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "ta-rs-restart-session-capture-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::canonicalize(dir).unwrap()
}

struct HomeGuard {
    previous_home: Option<String>,
}

impl HomeGuard {
    fn with_home(home: PathBuf) -> Self {
        std::fs::create_dir_all(&home).unwrap();
        let previous_home = std::env::var("HOME").ok();
        unsafe {
            std::env::set_var("HOME", home);
        }
        Self { previous_home }
    }
}

impl Drop for HomeGuard {
    fn drop(&mut self) {
        unsafe {
            if let Some(value) = self.previous_home.take() {
                std::env::set_var("HOME", value);
            } else {
                std::env::remove_var("HOME");
            }
        }
    }
}

#[derive(Debug, Clone)]
struct RecordedSpawn {
    argv: Vec<String>,
}

#[derive(Debug, Default)]
struct RecordingTransport {
    spawns: Mutex<Vec<RecordedSpawn>>,
    kills: Mutex<u32>,
    session_present: bool,
}

impl RecordingTransport {
    fn new() -> Self {
        Self::default()
    }

    fn with_session_present(mut self, present: bool) -> Self {
        self.session_present = present;
        self
    }

    fn single_spawn_argv(&self) -> Vec<String> {
        let spawns = self.spawns.lock().unwrap();
        assert_eq!(
            spawns.len(),
            1,
            "fixture should record exactly one worker spawn; spawns={spawns:?}"
        );
        spawns[0].argv.clone()
    }

    fn kill_count(&self) -> u32 {
        *self.kills.lock().unwrap()
    }

    fn spawn_result(
        &self,
        session: &SessionName,
        window: &WindowName,
        argv: &[String],
    ) -> SpawnResult {
        let mut spawns = self.spawns.lock().unwrap();
        spawns.push(RecordedSpawn {
            argv: argv.to_vec(),
        });
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
        Ok(self.session_present)
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
        *self.kills.lock().unwrap() += 1;
        Ok(())
    }

    fn kill_window(&self, _target: &Target) -> Result<(), TransportError> {
        Ok(())
    }

    fn attach_session(&self, _session: &SessionName) -> Result<AttachOutcome, TransportError> {
        Ok(AttachOutcome::Attached)
    }
}
