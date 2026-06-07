//! Team-in-team scoped command contracts.
//!
//! User-visible contract (#241):
//! - `status --team <team>` and `collect --team <team>` select that team's runtime
//!   projection and filter DB rows by `owner_team_id`.
//! - `shutdown --team <team>` stops only that team's tmux session/state and must not
//!   tear down the shared workspace tmux server/coordinator used by sibling teams.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use rusqlite::params;
use serde_json::{json, Value};
use serial_test::serial;
use team_agent::message_store::MessageStore;
use team_agent::state::persist::{load_runtime_state, save_runtime_state};
use team_agent::transport::{
    AttachOutcome, BackendKind, CaptureRange, CapturedText, InjectPayload, InjectReport,
    InjectStage, InjectVerification, Key, PaneField, PaneId, PaneInfo, PaneLiveness,
    SessionName, SetEnvOutcome, SpawnResult, SubmitVerification, Target, Transport,
    TransportError, TurnVerification, WindowName,
};

#[test]
#[serial(env)]
fn status_and_collect_advertise_team_selector_on_the_command_surface() {
    let status_help = cli_text(["status", "--help"]);
    assert!(
        status_help.contains("--team TEAM"),
        "`team-agent status --help` must expose --team because status is a selected-team command; help={status_help:?}"
    );

    let collect_help = cli_text(["collect", "--help"]);
    assert!(
        collect_help.contains("--team TEAM"),
        "`team-agent collect --help` must expose --team because collect is a selected-team command; help={collect_help:?}"
    );
}

#[test]
#[serial(env)]
fn status_team_selector_filters_agents_tasks_messages_and_results_to_that_team() {
    let _env = EnvGuard::unset();
    let fixture = MultiTeamFixture::new("status-scope");

    let output = run_cli([
        "status",
        "--workspace",
        fixture.root_str(),
        "--team",
        "teamA",
        "--json",
    ]);
    assert_success(&output, "status --team teamA --json");
    let status = stdout_json(&output);

    assert!(
        status.pointer("/agents/worker_a").is_some(),
        "status --team teamA must show teamA's worker; status={status}"
    );
    assert!(
        status.pointer("/agents/worker_b").is_none(),
        "status --team teamA must not leak teamB's worker; status={status}"
    );
    assert!(
        task_ids(&status).contains(&"task_a".to_string()),
        "status --team teamA must show teamA tasks; status={status}"
    );
    assert!(
        !task_ids(&status).contains(&"task_b".to_string()),
        "status --team teamA must not collapse to active teamB tasks; status={status}"
    );
    assert_eq!(
        status.pointer("/messages/accepted").and_then(Value::as_i64),
        Some(1),
        "status --team teamA message counts must filter owner_team_id=teamA, not count sibling rows; status={status}"
    );
    assert_eq!(
        status.pointer("/results/total").and_then(Value::as_i64),
        Some(1),
        "status --team teamA result counts must filter owner_team_id=teamA, not count sibling rows; status={status}"
    );
    assert!(
        status.pointer("/agent_health/worker_a").is_some()
            && status.pointer("/agent_health/worker_b").is_none(),
        "status --team teamA agent_health must be owner_team_id scoped; status={status}"
    );
}

#[test]
#[serial(env)]
fn collect_team_selector_collects_only_that_team_and_writes_back_that_team_state() {
    let _env = EnvGuard::unset();
    let fixture = MultiTeamFixture::new("collect-scope");

    let output = run_cli([
        "collect",
        "--workspace",
        fixture.root_str(),
        "--team",
        "teamA",
        "--json",
    ]);
    assert_success(&output, "collect --team teamA --json");
    let collected = stdout_json(&output);

    assert_eq!(
        collected.get("ok").and_then(Value::as_bool),
        Some(true),
        "collect --team teamA should validate and collect only teamA rows; collected={collected}"
    );
    let result_ids = collected_result_ids(&collected);
    assert_eq!(
        result_ids,
        vec!["res_team_a".to_string()],
        "collect --team teamA must not collect or invalidate sibling teamB results; collected={collected}"
    );

    let conn = db_conn(&fixture.root);
    assert_eq!(result_status(&conn, "res_team_a").as_deref(), Some("collected"));
    assert_eq!(
        result_status(&conn, "res_team_b").as_deref(),
        Some("success"),
        "collect --team teamA must leave sibling teamB result uncollected"
    );

    let state = load_runtime_state(&fixture.root).expect("state after collect");
    assert_eq!(
        task_status(&state, "teamA", "task_a").as_deref(),
        Some("done"),
        "collect --team teamA must write task completion into state.teams.teamA; state={state}"
    );
    assert_eq!(
        task_status(&state, "teamB", "task_b").as_deref(),
        Some("running"),
        "collect --team teamA must not mutate sibling teamB tasks; state={state}"
    );
}

#[test]
#[serial(env)]
fn scoped_shutdown_kills_only_selected_team_session_and_preserves_sibling_state() {
    let _env = EnvGuard::unset();
    let fixture = MultiTeamFixture::new("shutdown-scope");
    let transport = ShutdownRecordingTransport::new(["team-teamA", "team-teamB"]);

    let report = team_agent::cli::lifecycle_port::shutdown_with_transport(
        &fixture.root,
        true,
        Some("teamA"),
        &transport,
    )
    .expect("scoped shutdown should return a typed report");

    assert_eq!(
        report.get("team").and_then(Value::as_str),
        Some("teamA"),
        "scoped shutdown report should preserve the selected team; report={report}"
    );
    assert_eq!(
        transport.killed_sessions(),
        vec!["team-teamA".to_string()],
        "shutdown --team teamA must kill only the selected team's session; active/top-level teamB must remain alive"
    );
    let state = load_runtime_state(&fixture.root).expect("state after scoped shutdown");
    assert_eq!(
        agent_status(&state, "teamA", "worker_a").as_deref(),
        Some("stopped"),
        "shutdown --team teamA must save team-scoped stopped state for teamA; state={state}"
    );
    assert_eq!(
        agent_status(&state, "teamB", "worker_b").as_deref(),
        Some("running"),
        "shutdown --team teamA must not mark sibling teamB agents stopped; state={state}"
    );
}

#[test]
#[serial(env)]
fn global_shutdown_without_team_keeps_existing_global_session_contract() {
    let _env = EnvGuard::unset();
    let fixture = MultiTeamFixture::new("shutdown-global");
    let transport = ShutdownRecordingTransport::new(["team-teamA", "team-teamB"]);

    let report = team_agent::cli::lifecycle_port::shutdown_with_transport(
        &fixture.root,
        true,
        None,
        &transport,
    )
    .expect("global shutdown should still return a typed report");

    assert_eq!(
        report.get("team"),
        Some(&Value::Null),
        "global shutdown must remain the no-team path; report={report}"
    );
    assert_eq!(
        transport.killed_sessions(),
        vec!["team-teamB".to_string()],
        "no-team shutdown keeps the existing top-level/global session behavior; report={report}"
    );
}

#[test]
fn scoped_shutdown_source_guard_does_not_unconditionally_kill_shared_server() {
    let source = std::fs::read_to_string("src/cli/mod.rs").expect("read cli/mod.rs");
    let shutdown = function_body(&source, "pub fn shutdown(workspace:");
    assert!(
        !shutdown.contains("\n        transport.kill_server();\n        result"),
        "shutdown --team must not unconditionally call kill_server after shutdown_with_transport; kill_server is reserved for the no-team/global shutdown path"
    );
}

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_team-agent")
}

fn cli_text<const N: usize>(args: [&str; N]) -> String {
    let output = Command::new(bin()).args(args).output().expect("run team-agent");
    assert!(
        output.status.success(),
        "command should succeed; stdout={:?} stderr={:?}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).to_string()
}

fn run_cli<const N: usize>(args: [&str; N]) -> Output {
    Command::new(bin()).args(args).output().expect("run team-agent")
}

fn assert_success(output: &Output, label: &str) {
    assert!(
        output.status.success(),
        "{label} should exit 0; stdout={:?} stderr={:?}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn stdout_json(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "stdout must be JSON: {error}; stdout={:?} stderr={:?}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

fn task_ids(status: &Value) -> Vec<String> {
    status
        .get("tasks")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|task| task.get("id").and_then(Value::as_str).map(str::to_string))
        .collect()
}

fn collected_result_ids(value: &Value) -> Vec<String> {
    value
        .get("collected_results")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|result| result.get("result_id").and_then(Value::as_str).map(str::to_string))
        .collect()
}

fn function_body<'a>(source: &'a str, signature: &str) -> &'a str {
    let start = source.find(signature).expect("function signature exists");
    let rest = &source[start..];
    let end = rest
        .find("\n    pub fn ")
        .filter(|idx| *idx > 0)
        .unwrap_or(rest.len());
    &rest[..end]
}

struct MultiTeamFixture {
    root: PathBuf,
    root_str: String,
}

impl MultiTeamFixture {
    fn new(tag: &str) -> Self {
        let root = tmp_dir(tag);
        let team_a = write_team_dir(&root, "teamA", "worker_a");
        let team_b = write_team_dir(&root, "teamB", "worker_b");
        let state = json!({
            "active_team_key": "teamB",
            "session_name": "team-teamB",
            "team_dir": team_b.to_string_lossy().to_string(),
            "spec_path": team_b.join("team.spec.yaml").to_string_lossy().to_string(),
            "leader": {"id": "leader"},
            "leader_receiver": receiver("teamB", "%20"),
            "team_owner": owner("teamB", "%20", 2),
            "owner_epoch": 2,
            "agents": agents("teamB", "worker_b", "running"),
            "tasks": [task("task_b", "worker_b", "running")],
            "teams": {
                "teamA": team_state("teamA", &team_a, "worker_a", "task_a", "%10", 1),
                "teamB": team_state("teamB", &team_b, "worker_b", "task_b", "%20", 2)
            }
        });
        save_runtime_state(&root, &state).expect("seed state");
        seed_db(&root);
        let root_str = root.to_string_lossy().to_string();
        Self { root, root_str }
    }

    fn root_str(&self) -> &str {
        &self.root_str
    }
}

fn team_state(team: &str, team_dir: &Path, agent: &str, task_id: &str, pane: &str, epoch: i64) -> Value {
    json!({
        "status": "alive",
        "session_name": format!("team-{team}"),
        "team_dir": team_dir.to_string_lossy().to_string(),
        "spec_path": team_dir.join("team.spec.yaml").to_string_lossy().to_string(),
        "leader": {"id": "leader"},
        "leader_receiver": receiver(team, pane),
        "team_owner": owner(team, pane, epoch),
        "owner_epoch": epoch,
        "agents": agents(team, agent, "running"),
        "tasks": [task(task_id, agent, "running")]
    })
}

fn agents(team: &str, agent: &str, status: &str) -> Value {
    json!({
        agent: {
            "agent_id": agent,
            "owner_team_id": team,
            "status": status,
            "provider": "codex",
            "window": agent,
            "model": "gpt-5"
        }
    })
}

fn task(task_id: &str, agent: &str, status: &str) -> Value {
    json!({
        "id": task_id,
        "title": format!("{task_id} title"),
        "assignee": agent,
        "status": status,
        "type": "task"
    })
}

fn receiver(team: &str, pane: &str) -> Value {
    json!({
        "status": "attached",
        "mode": "direct_tmux",
        "provider": "codex",
        "pane_id": pane,
        "owner_team_id": team,
        "session_name": format!("team-{team}"),
        "window_name": "leader"
    })
}

fn owner(team: &str, pane: &str, epoch: i64) -> Value {
    json!({
        "team_id": team,
        "owner_team_id": team,
        "pane_id": pane,
        "provider": "codex",
        "owner_epoch": epoch,
        "liveness": "live"
    })
}

fn write_team_dir(root: &Path, team: &str, agent: &str) -> PathBuf {
    let dir = root.join(team);
    std::fs::create_dir_all(dir.join("agents")).unwrap();
    std::fs::write(
        dir.join("TEAM.md"),
        format!("---\nname: {team}\nobjective: Scoped command fixture.\nprovider: codex\n---\n\n{team}.\n"),
    )
    .unwrap();
    std::fs::write(
        dir.join("team.spec.yaml"),
        format!(
            "team:\n  name: {team}\n  objective: Scoped command fixture.\nleader:\n  provider: codex\nagents:\n  - id: {agent}\n    provider: codex\n    role: Worker\n"
        ),
    )
    .unwrap();
    std::fs::write(
        dir.join("agents").join(format!("{agent}.md")),
        format!("---\nname: {agent}\nrole: Worker\nprovider: codex\n---\n\nWorker.\n"),
    )
    .unwrap();
    dir
}

fn seed_db(root: &Path) {
    let store = MessageStore::open(root).expect("open team.db");
    store
        .create_message(Some("task_a"), "leader", "worker_a", "teamA message", None, true, Some("teamA"))
        .unwrap();
    store
        .create_message(Some("task_b"), "leader", "worker_b", "teamB message", None, true, Some("teamB"))
        .unwrap();
    let conn = db_conn(root);
    insert_result(&conn, "res_team_a", "teamA", "task_a", "worker_a", "team A done", "2026-06-07T00:00:00Z");
    insert_result(&conn, "res_team_b", "teamB", "task_b", "worker_b", "team B done", "2026-06-07T00:00:01Z");
    conn.execute(
        "insert into agent_health(owner_team_id, agent_id, status, updated_at)
         values (?1, ?2, 'idle', ?3), (?4, ?5, 'idle', ?6)",
        params!["teamA", "worker_a", "2026-06-07T00:00:00Z", "teamB", "worker_b", "2026-06-07T00:00:00Z"],
    )
    .unwrap();
}

fn insert_result(
    conn: &rusqlite::Connection,
    result_id: &str,
    team: &str,
    task_id: &str,
    agent: &str,
    summary: &str,
    created_at: &str,
) {
    let envelope = json!({
        "schema_version": "result_envelope_v1",
        "result_id": result_id,
        "task_id": task_id,
        "agent_id": agent,
        "status": "success",
        "summary": summary,
        "artifacts": [],
        "changes": [],
        "tests": [],
        "risks": [],
        "next_actions": []
    });
    conn.execute(
        "insert into results(result_id, owner_team_id, task_id, agent_id, envelope, status, created_at)
         values (?1, ?2, ?3, ?4, ?5, 'success', ?6)",
        params![result_id, team, task_id, agent, envelope.to_string(), created_at],
    )
    .unwrap();
}

fn db_conn(root: &Path) -> rusqlite::Connection {
    let store = MessageStore::open(root).expect("open team.db");
    team_agent::db::schema::open_db(store.db_path()).expect("open sqlite")
}

fn result_status(conn: &rusqlite::Connection, result_id: &str) -> Option<String> {
    conn.query_row(
        "select status from results where result_id = ?1",
        params![result_id],
        |row| row.get(0),
    )
    .ok()
}

fn task_status(state: &Value, team: &str, task_id: &str) -> Option<String> {
    state
        .pointer(&format!("/teams/{team}/tasks"))
        .and_then(Value::as_array)?
        .iter()
        .find(|task| task.get("id").and_then(Value::as_str) == Some(task_id))?
        .get("status")
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn agent_status(state: &Value, team: &str, agent_id: &str) -> Option<String> {
    state
        .pointer(&format!("/teams/{team}/agents/{agent_id}/status"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn tmp_dir(tag: &str) -> PathBuf {
    static N: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "ta-rs-team-in-team-scope-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::canonicalize(dir).unwrap()
}

struct EnvGuard {
    previous: Vec<(&'static str, Option<String>)>,
}

impl EnvGuard {
    fn unset() -> Self {
        let keys = [
            "TMUX",
            "TMUX_PANE",
            "TEAM_AGENT_ID",
            "TEAM_AGENT_OWNER_TEAM_ID",
            "TEAM_AGENT_WORKSPACE",
            "TEAM_AGENT_LEADER_PANE_ID",
        ];
        let previous = keys
            .iter()
            .map(|key| (*key, std::env::var(key).ok()))
            .collect::<Vec<_>>();
        for key in keys {
            unsafe {
                std::env::remove_var(key);
            }
        }
        Self { previous }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (key, value) in self.previous.drain(..).rev() {
            unsafe {
                if let Some(value) = value {
                    std::env::set_var(key, value);
                } else {
                    std::env::remove_var(key);
                }
            }
        }
    }
}

#[derive(Debug)]
struct ShutdownRecordingTransport {
    sessions: Mutex<HashSet<String>>,
    killed: Mutex<Vec<String>>,
}

impl ShutdownRecordingTransport {
    fn new<const N: usize>(sessions: [&str; N]) -> Self {
        Self {
            sessions: Mutex::new(sessions.into_iter().map(str::to_string).collect()),
            killed: Mutex::new(Vec::new()),
        }
    }

    fn killed_sessions(&self) -> Vec<String> {
        self.killed.lock().unwrap().clone()
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

impl Transport for ShutdownRecordingTransport {
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
        Ok(Vec::new())
    }

    fn has_session(&self, session: &SessionName) -> Result<bool, TransportError> {
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
        self.killed.lock().unwrap().push(session.as_str().to_string());
        self.sessions.lock().unwrap().remove(session.as_str());
        Ok(())
    }

    fn kill_window(&self, _target: &Target) -> Result<(), TransportError> {
        Ok(())
    }

    fn attach_session(&self, _session: &SessionName) -> Result<AttachOutcome, TransportError> {
        Ok(AttachOutcome::Attached)
    }
}
