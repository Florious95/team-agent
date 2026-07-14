//! #252 restart/rebind hotfix contracts.
//!
//! Local cargo/black-box contracts for the 0.3.3 hotfix surface. These avoid
//! real provider subscriptions and pin the externally visible behavior around
//! restart cwd, MCP owner scope, team-scoped coordinator writeback, pane identity,
//! unbound leader delivery, SQLite contention, and coordinator shutdown.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[path = "support/hermetic.rs"]
mod hermetic_guard;
#[allow(dead_code)]
fn _hermetic_boundary_marker(_: &hermetic_guard::HermeticTestEnv) {}

use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use serde_json::{json, Value};
use serial_test::serial;
use team_agent::coordinator::{Coordinator, ErrorLists, ProviderRegistry, WorkspacePath};
use team_agent::db::schema::{initialize_schema, open_db};
use team_agent::event_log::EventLog;
use team_agent::lifecycle::{launch_with_transport_in_workspace, restart_with_transport};
use team_agent::message_store::MessageStore;
use team_agent::messaging::delivery::deliver_pending_message;
use team_agent::model::enums::{AuthMode, Provider};
use team_agent::provider::{
    AuthHintStatus, CaptureVia, CapturedSession, Confidence, HandledPrompt, McpConfig,
    ProviderAdapter, ProviderCaps, ProviderError, RolloutPath, SessionId, StatusPatterns,
};
use team_agent::state::persist::{load_runtime_state, save_runtime_state};
use team_agent::transport::{
    AttachOutcome, BackendKind, CaptureRange, CapturedText, InjectPayload, InjectReport,
    InjectStage, InjectVerification, Key, PaneField, PaneId, PaneInfo, PaneLiveness, SessionName,
    SetEnvOutcome, SpawnResult, SubmitVerification, Target, Transport, TransportError,
    TurnVerification, WindowName,
};

const CURRENT: &str = "current";
const SUB: &str = "sub";

#[test]
#[ignore = "real-machine: restart/rebind lifecycle gate"]
#[serial(env)]
fn restart_resume_spawns_worker_with_team_dir_cwd_matching_launch() {
    let case = HotfixCase::new("restart-cwd");
    case.write_team(CURRENT, "greeter", Provider::Codex);
    case.seed_restart_state(CURRENT, "greeter", "sess-current", "handled");
    case.seed_healthy_coordinator();
    let transport = RecordingTransport::new().with_session_present(false);

    restart_with_transport(&case.team_dir(CURRENT), false, Some(CURRENT), &transport)
        .expect("restart should resume the worker");

    let spawn = transport.single_spawn();
    assert_eq!(
        spawn.cwd,
        case.team_dir(CURRENT),
        "RC0: restart/resume must spawn the worker in the same team directory cwd used by first launch. \
         Passing the run workspace root makes Codex detect cwd != session cwd and show the \
         'Choose working directory to resume this session' prompt. spawn={spawn:?}"
    );
}

#[test]
#[ignore = "real-machine: restart/rebind lifecycle gate"]
#[serial(env)]
fn restart_resets_startup_prompt_state_for_respawn() {
    let case = HotfixCase::new("restart-startup-state");
    case.write_team(CURRENT, "greeter", Provider::Codex);
    case.seed_restart_state(CURRENT, "greeter", "sess-current", "handled");
    case.seed_healthy_coordinator();
    let transport = RecordingTransport::new().with_session_present(false);

    restart_with_transport(&case.team_dir(CURRENT), false, Some(CURRENT), &transport)
        .expect("restart should resume the worker");
    let state = load_runtime_state(&case.workspace).expect("state after restart");
    let agent = state
        .pointer("/teams/current/agents/greeter")
        .or_else(|| state.pointer("/agents/greeter"))
        .expect("greeter state exists");
    let status = agent.get("startup_prompts").and_then(Value::as_str);

    assert!(
        !matches!(status, Some("handled" | "complete")),
        "RC0 defense: a respawned worker is a new pane and must be eligible for startup prompt detection. \
         Restart must clear/version stale startup_prompts=handled instead of carrying it into the new process; state={state}"
    );
}

#[test]
#[ignore = "real-machine: spawns team-agent mcp-server process"]
#[serial(env)]
fn report_result_respects_mcp_owner_team_env_when_active_team_differs() {
    let case = HotfixCase::new("mcp-report-owner");
    case.seed_dual_team_state(SUB, false);
    let output = run_mcp_report_result(
        &case.workspace,
        "greeter",
        CURRENT,
        "res-current-env",
        "current team result",
    );
    assert!(
        !output["result"]["isError"].as_bool().unwrap_or(true),
        "RC4 fixture sanity: real MCP tools/call report_result must not be an MCP error; frame={output}"
    );

    let conn = open_db(&case.db_path()).unwrap();
    let result_owner: Option<String> = conn
        .query_row(
            "select owner_team_id from results where task_id='task-current' and agent_id='greeter' order by created_at desc limit 1",
            [],
            |row| row.get(0),
        )
        .unwrap_or_else(|error| {
            let rows = db_rows(&conn, "results", "result_id, owner_team_id");
            panic!("RC4 fixture: report_result did not persist any greeter/task-current result row: error={error} frame={output} rows={rows:?}");
        });
    let leader_message_owner: Option<String> = conn
        .query_row(
            "select owner_team_id from messages where sender='greeter' and recipient='leader' order by created_at desc limit 1",
            [],
            |row| row.get(0),
        )
        .unwrap_or_else(|error| {
            let rows = db_rows(&conn, "messages", "message_id, owner_team_id, sender, recipient, status");
            panic!("RC4 fixture: report_result did not create expected leader message row: error={error} frame={output} rows={rows:?}");
        });

    assert_eq!(
        result_owner.as_deref(),
        Some(CURRENT),
        "RC4: MCP report_result must use TEAM_AGENT_OWNER_TEAM_ID=current even when raw state active_team_key=sub; frame={output}"
    );
    assert_eq!(
        leader_message_owner.as_deref(),
        Some(CURRENT),
        "RC4: report_result leader notification message must be queued under current, not active sub"
    );
}

#[test]
#[serial(env)]
fn coordinator_tick_saves_captured_session_into_nested_active_team() {
    let case = HotfixCase::new("tick-nested-session");
    case.seed_dual_team_state(SUB, false);
    let rollout = case.workspace.join("sub-rollout.jsonl");
    std::fs::write(&rollout, "{}").unwrap();
    let registry = StaticCaptureRegistry {
        session_id: "sess-sub-captured".to_string(),
        rollout_path: rollout,
    };
    let transport = RecordingTransport::new()
        .with_session_present(true)
        .with_windows(vec![WindowName::new("subgreeter")]);
    let coord = Coordinator::new(
        WorkspacePath::new(case.workspace.clone()),
        Box::new(registry),
        Box::new(transport),
    );

    coord.tick().expect("coordinator tick");
    let state = load_runtime_state(&case.workspace).expect("state after tick");

    assert_eq!(
        state.pointer("/teams/sub/agents/subgreeter/session_id").and_then(Value::as_str),
        Some("sess-sub-captured"),
        "RC1: coordinator tick must write captured provider session into the nested active team, not only the top-level projection; state={state}"
    );
}

#[test]
#[serial(env)]
fn launch_persists_agent_pane_id_and_pid() {
    let case = HotfixCase::new("launch-pane-id");
    case.write_team(CURRENT, "greeter", Provider::Fake);
    // 0.3.28 Step 4b: workers live in their own window named after the
    // agent_id, not the adaptive `team-w1` anchor.
    let transport = RecordingTransport::new()
        .with_targets(vec![pane_info("%7", "team-current", "greeter", Some(4242))])
        .with_windows(vec![WindowName::new("greeter")])
        .with_default_liveness(PaneLiveness::Live);

    launch_with_transport_in_workspace(
        &case.workspace,
        &case.team_dir(CURRENT).join("team.spec.yaml"),
        false,
        true,
        true,
        &transport,
    )
    .expect("launch should spawn fake worker");
    let state = load_runtime_state(&case.workspace).expect("state after launch");
    let agent = state
        .pointer("/teams/current/agents/greeter")
        .or_else(|| state.pointer("/agents/greeter"))
        .expect("greeter state");

    assert_eq!(
        agent.get("pane_id").and_then(Value::as_str),
        Some("%7"),
        "RC2: launch must persist the concrete tmux %pane_id returned by spawn/list_targets; state={state}"
    );
    assert_eq!(
        agent.get("pane_pid").and_then(Value::as_u64),
        Some(4242),
        "RC2: launch must continue persisting pane_pid alongside pane_id; state={state}"
    );
}

#[test]
#[serial(env)]
fn unbound_leader_receiver_is_not_attached_and_delivery_never_injects_sentinel() {
    let case = HotfixCase::new("unbound-leader");
    case.seed_unbound_leader_state();
    let store = MessageStore::open(&case.workspace).unwrap();
    let message_id = store
        .create_message(
            None,
            "subgreeter",
            "leader",
            "sentinel canary",
            None,
            false,
            Some(SUB),
        )
        .unwrap();
    let state = load_runtime_state(&case.workspace).unwrap();
    let transport = RecordingTransport::new().with_default_liveness(PaneLiveness::Live);

    let out = deliver_pending_message(
        &case.workspace,
        &store,
        &transport,
        &message_id,
        &EventLog::new(&case.workspace),
        &state,
    )
    .expect("delivery should return an explicit rebind outcome");

    assert!(
        !out.ok && out.channel.as_deref() == Some("rebind_required") && transport.inject_targets().is_empty(),
        "RC3: __team_agent_unbound__ is not an attached leader receiver; delivery must return rebind_required and never inject; outcome={out:?} targets={:?}",
        transport.inject_targets()
    );
    assert!(
        transport.inject_targets().is_empty(),
        "RC3: delivery must never physically inject/paste to the sentinel __team_agent_unbound__; outcome={out:?} targets={:?}",
        transport.inject_targets()
    );
}

#[test]
#[serial(env)]
fn sqlite_open_retries_under_concurrent_writer() {
    let case = HotfixCase::new("sqlite-lock");
    std::fs::create_dir_all(case.db_path().parent().unwrap()).unwrap();
    let holder = rusqlite::Connection::open(case.db_path()).unwrap();
    initialize_schema(&holder, Some(&case.db_path())).unwrap();
    holder.execute_batch("BEGIN EXCLUSIVE;").unwrap();

    let start = Instant::now();
    let result = MessageStore::open(&case.workspace);
    let elapsed = start.elapsed();
    holder.execute_batch("ROLLBACK;").unwrap();

    assert!(
        result.is_ok(),
        "RC5: opening/initializing team.db under a concurrent writer must retry/idempotently survive instead of surfacing database-is-locked. elapsed={elapsed:?} error={:?}",
        result.err()
    );
}

#[test]
#[ignore = "real-machine: coordinator pidfile SIGTERM process gate"]
#[serial(env)]
fn shutdown_pidfile_sigterm_waits_and_normalizes() {
    let case = HotfixCase::new("shutdown-pidfile");
    let mut child = Command::new("sh")
        .arg("-c")
        .arg("trap 'sleep 2; exit 0' TERM; while true; do sleep 1; done")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn delayed SIGTERM child");
    case.write_coordinator_pid(child.id());

    let report =
        team_agent::coordinator::stop_coordinator(&WorkspacePath::new(case.workspace.clone()))
            .expect("stop coordinator");
    let still_running = process_running(child.id());
    if still_running {
        let _ = child.kill();
    }
    let _ = child.wait();

    assert!(
        report.ok && !still_running,
        "RC6: pidfile coordinator stop must wait until the SIGTERM target really exits before reporting ok/stopped and removing metadata. report={report:?} still_running={still_running}"
    );
}

#[derive(Debug)]
struct HotfixCase {
    workspace: PathBuf,
}

impl HotfixCase {
    fn new(tag: &str) -> Self {
        let root = tmp_dir(tag);
        let workspace = root.join("ws");
        std::fs::create_dir_all(workspace.join(".team/runtime")).unwrap();
        Self { workspace }
    }

    fn team_dir(&self, team: &str) -> PathBuf {
        self.workspace.join(".team").join(team)
    }

    fn db_path(&self) -> PathBuf {
        self.workspace.join(".team/runtime/team.db")
    }

    fn write_team(&self, team: &str, agent_id: &str, provider: Provider) {
        let dir = self.team_dir(team);
        std::fs::create_dir_all(dir.join("agents")).unwrap();
        std::fs::write(
            dir.join("TEAM.md"),
            format!(
                "---\nname: {team}\nobjective: hotfix\nprovider: {}\n---\n\nTeam.\n",
                provider_name(provider)
            ),
        )
        .unwrap();
        std::fs::write(
            dir.join("agents").join(format!("{agent_id}.md")),
            format!(
                "---\nname: {agent_id}\nrole: Greeter\nprovider: {}\nmodel: test\nauth_mode: subscription\ntools:\n  - mcp_team\n---\n\nWorker.\n",
                provider_name(provider)
            ),
        )
        .unwrap();
        let spec = team_agent::compiler::compile_team(&dir).unwrap();
        std::fs::write(
            dir.join("team.spec.yaml"),
            team_agent::model::yaml::dumps(&spec),
        )
        .unwrap();
    }

    fn seed_restart_state(
        &self,
        team: &str,
        agent_id: &str,
        session_id: &str,
        startup_prompts: &str,
    ) {
        let team_dir = self.team_dir(team);
        let state = json!({
            "active_team_key": team,
            "team_dir": team_dir.to_string_lossy(),
            "spec_path": team_dir.join("team.spec.yaml").to_string_lossy(),
            "session_name": format!("team-{team}"),
            "agents": {
                agent_id: restart_agent(agent_id, team, &team_dir, session_id, startup_prompts)
            },
            "tasks": [],
            "teams": {
                team: {
                    "status": "alive",
                    "team_dir": team_dir.to_string_lossy(),
                    "spec_path": team_dir.join("team.spec.yaml").to_string_lossy(),
                    "session_name": format!("team-{team}"),
                    "agents": {
                        agent_id: restart_agent(agent_id, team, &team_dir, session_id, startup_prompts)
                    },
                    "tasks": []
                }
            }
        });
        save_runtime_state(&self.workspace, &state).unwrap();
    }

    fn seed_dual_team_state(&self, active: &str, with_leader_receivers: bool) {
        let current_dir = self.team_dir(CURRENT);
        let sub_dir = self.team_dir(SUB);
        std::fs::create_dir_all(&current_dir).unwrap();
        std::fs::create_dir_all(&sub_dir).unwrap();
        let current = team_state(CURRENT, "greeter", &current_dir, with_leader_receivers);
        let sub = team_state(SUB, "subgreeter", &sub_dir, with_leader_receivers);
        let mut state = if active == CURRENT {
            current.clone()
        } else {
            sub.clone()
        };
        let obj = state.as_object_mut().unwrap();
        obj.insert("active_team_key".to_string(), json!(active));
        obj.insert("teams".to_string(), json!({CURRENT: current, SUB: sub}));
        save_runtime_state(&self.workspace, &state).unwrap();
        let _ = MessageStore::open(&self.workspace).unwrap();
    }

    fn seed_unbound_leader_state(&self) {
        let sub_dir = self.team_dir(SUB);
        std::fs::create_dir_all(&sub_dir).unwrap();
        save_runtime_state(
            &self.workspace,
            &json!({
                "active_team_key": SUB,
                "session_name": "team-sub",
                "team_dir": sub_dir.to_string_lossy(),
                "spec_path": sub_dir.join("team.spec.yaml").to_string_lossy(),
                "leader_receiver": {
                    "mode": "direct_tmux",
                    "status": "attached",
                    "pane_id": "__team_agent_unbound__"
                },
                "team_owner": {
                    "status": "attached",
                    "pane_id": "__team_agent_unbound__"
                },
                "agents": {
                    "subgreeter": {"status": "running", "provider": "fake", "window": "subgreeter", "owner_team_id": SUB}
                },
                "teams": {
                    SUB: {
                        "status": "alive",
                        "session_name": "team-sub",
                        "team_dir": sub_dir.to_string_lossy(),
                        "spec_path": sub_dir.join("team.spec.yaml").to_string_lossy(),
                        "leader_receiver": {
                            "mode": "direct_tmux",
                            "status": "attached",
                            "pane_id": "__team_agent_unbound__"
                        },
                        "team_owner": {
                            "status": "attached",
                            "pane_id": "__team_agent_unbound__"
                        },
                        "agents": {
                            "subgreeter": {"status": "running", "provider": "fake", "window": "subgreeter", "owner_team_id": SUB}
                        }
                    }
                }
            }),
        )
        .unwrap();
    }

    fn seed_healthy_coordinator(&self) {
        let workspace = WorkspacePath::new(self.workspace.clone());
        let _ = MessageStore::open(workspace.as_path()).unwrap();
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

    fn write_coordinator_pid(&self, pid: u32) {
        let workspace = WorkspacePath::new(self.workspace.clone());
        std::fs::create_dir_all(self.workspace.join(".team/runtime")).unwrap();
        std::fs::write(
            team_agent::coordinator::coordinator_pid_path(&workspace),
            pid.to_string(),
        )
        .unwrap();
        team_agent::coordinator::write_coordinator_metadata(
            &workspace,
            team_agent::coordinator::Pid::new(pid),
            team_agent::coordinator::MetadataSource::Start,
        )
        .unwrap();
    }
}

fn restart_agent(
    agent_id: &str,
    team: &str,
    team_dir: &Path,
    session_id: &str,
    startup_prompts: &str,
) -> Value {
    json!({
        "status": "running",
        "agent_id": agent_id,
        "provider": "codex",
        "role": "Greeter",
        "tools": ["mcp_team"],
        "window": agent_id,
        "owner_team_id": team,
        "session_id": session_id,
        "first_send_at": "2026-06-07T01:00:00+00:00",
        "spawn_cwd": team_dir.to_string_lossy(),
        "startup_prompts": startup_prompts,
        "startup_prompt_status": startup_prompts
    })
}

fn team_state(team: &str, agent_id: &str, team_dir: &Path, with_leader_receiver: bool) -> Value {
    let mut state = json!({
        "status": "alive",
        "team_dir": team_dir.to_string_lossy(),
        "spec_path": team_dir.join("team.spec.yaml").to_string_lossy(),
        "session_name": format!("team-{team}"),
        "agents": {
            agent_id: {
                "status": "running",
                "provider": "codex",
                "window": agent_id,
                "owner_team_id": team,
                "spawn_cwd": team_dir.to_string_lossy(),
                "session_id": null
            }
        },
        "tasks": [{"id": format!("task-{team}"), "assignee": agent_id, "status": "pending"}]
    });
    if with_leader_receiver {
        let obj = state.as_object_mut().unwrap();
        obj.insert(
            "leader_receiver".to_string(),
            json!({"mode": "direct_tmux", "status": "attached", "pane_id": format!("%leader-{team}")}),
        );
        obj.insert(
            "team_owner".to_string(),
            json!({"status": "attached", "pane_id": format!("%leader-{team}")}),
        );
    }
    state
}

fn run_mcp_report_result(
    workspace: &Path,
    agent_id: &str,
    owner_team_id: &str,
    result_id: &str,
    summary: &str,
) -> Value {
    let exe = env!("CARGO_BIN_EXE_team-agent");
    let mut child = Command::new(exe)
        .arg("mcp-server")
        .arg("--workspace")
        .arg(workspace)
        .env("TEAM_AGENT_WORKSPACE", workspace)
        .env("TEAM_AGENT_ID", agent_id)
        .env("TEAM_AGENT_OWNER_TEAM_ID", owner_team_id)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn mcp-server");
    {
        let stdin = child.stdin.as_mut().expect("stdin");
        writeln!(stdin, r#"{{"jsonrpc":"2.0","id":1,"method":"initialize","params":{{"protocolVersion":"2024-11-05"}}}}"#).unwrap();
        writeln!(
            stdin,
            "{}",
            json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "tools/call",
                "params": {
                    "name": "report_result",
                    "arguments": {
                        "envelope": {
                            "result_id": result_id,
                            "task_id": "task-current",
                            "agent_id": agent_id,
                            "status": "success",
                            "summary": summary,
                            "changes": [], "tests": [], "risks": [], "artifacts": [], "next_actions": []
                        }
                    }
                }
            })
        )
        .unwrap();
    }
    let output = child.wait_with_output().expect("wait mcp-server");
    assert!(
        output.status.success(),
        "mcp-server exited unsuccessfully: status={:?} stderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    stdout
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .find(|frame| frame["id"] == json!(2))
        .unwrap_or_else(|| panic!("missing tools/call response; stdout={stdout}"))
}

#[derive(Debug, Clone)]
struct RecordedSpawn {
    argv: Vec<String>,
    cwd: PathBuf,
    env: BTreeMap<String, String>,
    pane_id: PaneId,
}

#[derive(Debug)]
struct TransportState {
    spawns: Vec<RecordedSpawn>,
    injections: Vec<Target>,
    keys: Vec<Vec<Key>>,
    captures: Vec<String>,
    session_present: bool,
    windows: Vec<WindowName>,
    targets: Vec<PaneInfo>,
    default_liveness: PaneLiveness,
    liveness: BTreeMap<String, PaneLiveness>,
}

impl Default for TransportState {
    fn default() -> Self {
        Self {
            spawns: Vec::new(),
            injections: Vec::new(),
            keys: Vec::new(),
            captures: Vec::new(),
            session_present: false,
            windows: Vec::new(),
            targets: Vec::new(),
            default_liveness: PaneLiveness::Dead,
            liveness: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, Default)]
struct RecordingTransport {
    state: Arc<Mutex<TransportState>>,
}

impl RecordingTransport {
    fn new() -> Self {
        Self::default()
    }

    fn with_session_present(self, present: bool) -> Self {
        self.state.lock().unwrap().session_present = present;
        self
    }

    fn with_windows(self, windows: Vec<WindowName>) -> Self {
        self.state.lock().unwrap().windows = windows;
        self
    }

    fn with_targets(self, targets: Vec<PaneInfo>) -> Self {
        self.state.lock().unwrap().targets = targets;
        self
    }

    fn with_default_liveness(self, liveness: PaneLiveness) -> Self {
        self.state.lock().unwrap().default_liveness = liveness;
        self
    }

    fn single_spawn(&self) -> RecordedSpawn {
        let state = self.state.lock().unwrap();
        assert_eq!(
            state.spawns.len(),
            1,
            "expected one spawn; spawns={:?}",
            state.spawns
        );
        state.spawns[0].clone()
    }

    fn inject_targets(&self) -> Vec<Target> {
        self.state.lock().unwrap().injections.clone()
    }

    fn spawn_result(
        &self,
        session: &SessionName,
        window: &WindowName,
        argv: &[String],
        cwd: &Path,
        env: &BTreeMap<String, String>,
    ) -> SpawnResult {
        let mut state = self.state.lock().unwrap();
        let pane_id = state
            .targets
            .iter()
            .find(|pane| pane.session == *session && pane.window_name.as_ref() == Some(window))
            .or_else(|| {
                state
                    .targets
                    .iter()
                    .find(|pane| pane.window_name.as_ref() == Some(window))
            })
            .map(|pane| pane.pane_id.clone())
            .unwrap_or_else(|| PaneId::new(format!("%{}", state.spawns.len() + 1)));
        let child_pid = state
            .targets
            .iter()
            .find(|pane| pane.pane_id == pane_id)
            .and_then(|pane| pane.pane_pid);
        state.spawns.push(RecordedSpawn {
            argv: argv.to_vec(),
            cwd: cwd.to_path_buf(),
            env: env.clone(),
            pane_id: pane_id.clone(),
        });
        SpawnResult {
            pane_id,
            session: session.clone(),
            window: window.clone(),
            child_pid,
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
        cwd: &Path,
        env: &BTreeMap<String, String>,
    ) -> Result<SpawnResult, TransportError> {
        Ok(self.spawn_result(session, window, argv, cwd, env))
    }

    fn spawn_into(
        &self,
        session: &SessionName,
        window: &WindowName,
        argv: &[String],
        cwd: &Path,
        env: &BTreeMap<String, String>,
    ) -> Result<SpawnResult, TransportError> {
        Ok(self.spawn_result(session, window, argv, cwd, env))
    }

    fn inject(
        &self,
        target: &Target,
        _payload: &InjectPayload,
        _submit: Key,
        _bracketed: bool,
    ) -> Result<InjectReport, TransportError> {
        self.state.lock().unwrap().injections.push(target.clone());
        Ok(InjectReport {
            stage_reached: InjectStage::Submit,
            inject_verification: InjectVerification::CaptureContainsToken,
            submit_verification: SubmitVerification::EnterSentWithoutPlaceholderCheck,
            turn_verification: TurnVerification::NotYetObserved,
            attempts: 1,
            submit_diagnostics: None,
        })
    }

    fn send_keys(&self, _target: &Target, keys: &[Key]) -> Result<(), TransportError> {
        self.state.lock().unwrap().keys.push(keys.to_vec());
        Ok(())
    }

    fn capture(
        &self,
        _target: &Target,
        range: CaptureRange,
    ) -> Result<CapturedText, TransportError> {
        let text = self
            .state
            .lock()
            .unwrap()
            .captures
            .first()
            .cloned()
            .unwrap_or_default();
        Ok(CapturedText { text, range })
    }

    fn query(&self, _target: &Target, field: PaneField) -> Result<Option<String>, TransportError> {
        match field {
            PaneField::PaneWidth => Ok(Some("120".to_string())),
            _ => Ok(None),
        }
    }

    fn liveness(&self, pane: &PaneId) -> Result<PaneLiveness, TransportError> {
        let state = self.state.lock().unwrap();
        Ok(state
            .liveness
            .get(pane.as_str())
            .copied()
            .unwrap_or(state.default_liveness))
    }

    fn list_targets(&self) -> Result<Vec<PaneInfo>, TransportError> {
        Ok(self.state.lock().unwrap().targets.clone())
    }

    fn has_session(&self, _session: &SessionName) -> Result<bool, TransportError> {
        Ok(self.state.lock().unwrap().session_present)
    }

    fn list_windows(&self, _session: &SessionName) -> Result<Vec<WindowName>, TransportError> {
        Ok(self.state.lock().unwrap().windows.clone())
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

struct StaticCaptureRegistry {
    session_id: String,
    rollout_path: PathBuf,
}

impl ProviderRegistry for StaticCaptureRegistry {
    fn adapter_for(&self, provider: Provider) -> Box<dyn ProviderAdapter> {
        Box::new(StaticCaptureAdapter {
            provider,
            session_id: self.session_id.clone(),
            rollout_path: self.rollout_path.clone(),
        })
    }

    fn error_lists(&self, _provider: Provider) -> ErrorLists {
        ErrorLists::default()
    }
}

struct StaticCaptureAdapter {
    provider: Provider,
    session_id: String,
    rollout_path: PathBuf,
}

impl ProviderAdapter for StaticCaptureAdapter {
    fn provider(&self) -> Provider {
        self.provider
    }

    fn caps(&self) -> ProviderCaps {
        ProviderCaps {
            resume: true,
            fork: false,
            native_mcp_config: false,
            writes_global_settings: false,
        }
    }

    fn is_installed(&self) -> bool {
        true
    }

    fn version(&self) -> Result<String, ProviderError> {
        Ok("test".to_string())
    }

    fn auth_hint(&self, _auth_mode: AuthMode) -> AuthHintStatus {
        AuthHintStatus::Unknown
    }

    fn build_command(
        &self,
        _auth_mode: AuthMode,
        _mcp_config: Option<&McpConfig>,
        _system_prompt: Option<&str>,
        _model: Option<&str>,
    ) -> Result<Vec<String>, ProviderError> {
        Ok(vec!["fake".to_string()])
    }

    fn build_command_with_tools(
        &self,
        auth_mode: AuthMode,
        mcp_config: Option<&McpConfig>,
        system_prompt: Option<&str>,
        model: Option<&str>,
        _tools: &[&str],
    ) -> Result<Vec<String>, ProviderError> {
        self.build_command(auth_mode, mcp_config, system_prompt, model)
    }

    fn capture_session_id(
        &self,
        _agent_id: &str,
        spawn_cwd: &Path,
        _timeout_s: u64,
    ) -> Result<Option<CapturedSession>, ProviderError> {
        Ok(Some(CapturedSession {
            session_id: Some(SessionId::new(self.session_id.clone())),
            rollout_path: Some(RolloutPath::new(self.rollout_path.clone())),
            captured_via: CaptureVia::FsWatch,
            attribution_confidence: Confidence::High,
            spawn_cwd: spawn_cwd.to_path_buf(),
        }))
    }

    fn recover_session_id(
        &self,
        _agent_id: &str,
        _spawn_cwd: &Path,
    ) -> Result<Option<SessionId>, ProviderError> {
        Ok(Some(SessionId::new(self.session_id.clone())))
    }

    fn session_is_resumable(
        &self,
        _session_id: Option<&SessionId>,
        _auth_mode: AuthMode,
    ) -> Result<bool, ProviderError> {
        Ok(true)
    }

    fn build_resume_command(
        &self,
        _session_id: Option<&SessionId>,
        _auth_mode: AuthMode,
        _mcp_config: Option<&McpConfig>,
    ) -> Result<Vec<String>, ProviderError> {
        Ok(vec!["fake".to_string(), "resume".to_string()])
    }

    fn build_resume_command_with_context(
        &self,
        session_id: Option<&SessionId>,
        auth_mode: AuthMode,
        mcp_config: Option<&McpConfig>,
        _system_prompt: Option<&str>,
        _model: Option<&str>,
        _tools: &[&str],
    ) -> Result<Vec<String>, ProviderError> {
        self.build_resume_command(session_id, auth_mode, mcp_config)
    }

    fn fork(
        &self,
        _session_id: Option<&SessionId>,
        _auth_mode: AuthMode,
        _mcp_config: Option<&McpConfig>,
    ) -> Result<Vec<String>, ProviderError> {
        Ok(vec!["fake".to_string(), "fork".to_string()])
    }

    fn fork_with_context(
        &self,
        session_id: Option<&SessionId>,
        auth_mode: AuthMode,
        mcp_config: Option<&McpConfig>,
        _system_prompt: Option<&str>,
        _model: Option<&str>,
        _tools: &[&str],
    ) -> Result<Vec<String>, ProviderError> {
        self.fork(session_id, auth_mode, mcp_config)
    }

    fn mcp_config(&self, _auth_mode: AuthMode) -> Result<McpConfig, ProviderError> {
        Ok(McpConfig { raw: json!({}) })
    }

    fn install_mcp(&self, _config: &McpConfig) -> Result<(), ProviderError> {
        Ok(())
    }

    fn status_patterns(&self) -> Result<StatusPatterns, ProviderError> {
        Ok(StatusPatterns {
            idle: regex::Regex::new("idle").unwrap(),
            processing: regex::Regex::new("processing").unwrap(),
            error: regex::Regex::new("error").unwrap(),
        })
    }

    fn validate_model(&self, _model: &str) -> Result<bool, ProviderError> {
        Ok(true)
    }

    fn handle_startup_prompts(
        &self,
        _transport: &dyn Transport,
        _target: &Target,
        _checks: usize,
        _sleep_s: f64,
    ) -> Vec<HandledPrompt> {
        Vec::new()
    }
}

fn pane_info(pane_id: &str, session: &str, window: &str, pane_pid: Option<u32>) -> PaneInfo {
    PaneInfo {
        pane_id: PaneId::new(pane_id),
        session: SessionName::new(session),
        window_index: Some(0),
        window_name: Some(WindowName::new(window)),
        pane_index: Some(0),
        tty: None,
        current_command: Some("fake".to_string()),
        current_path: None,
        active: true,
        pane_pid,
        leader_env: BTreeMap::new(),
    }
}

fn process_running(pid: u32) -> bool {
    // 0.5.x Windows portability Batch 5: route through
    // `platform::process::pid_is_alive` so the helper compiles on
    // both platforms. Unix behavior byte-equivalent (kill(pid, 0)
    // via platform primitive).
    team_agent::platform::process::pid_is_alive(pid)
}

fn db_rows(conn: &rusqlite::Connection, table: &str, columns: &str) -> Vec<String> {
    let mut stmt = conn
        .prepare(&format!("select {columns} from {table}"))
        .unwrap();
    let column_count = stmt.column_count();
    stmt.query_map([], |row| {
        let values = (0..column_count)
            .map(|idx| {
                row.get::<_, Option<String>>(idx)
                    .unwrap_or(None)
                    .unwrap_or_else(|| "NULL".to_string())
            })
            .collect::<Vec<_>>();
        Ok(values.join("|"))
    })
    .unwrap()
    .collect::<Result<Vec<_>, _>>()
    .unwrap()
}

fn provider_name(provider: Provider) -> &'static str {
    match provider {
        Provider::Claude | Provider::ClaudeCode => "claude",
        Provider::Codex => "codex",
        Provider::Copilot => "copilot",
        Provider::GeminiCli => "gemini",
        Provider::Fake => "fake",
    }
}

fn tmp_dir(tag: &str) -> PathBuf {
    static N: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "ta-rs-252-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::canonicalize(dir).unwrap()
}
