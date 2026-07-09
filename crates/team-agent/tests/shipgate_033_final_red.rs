//! 0.3.3 pre-ship real-machine anchored RED contracts.
//!
//! These contracts enter through the public `team-agent` binary and real local
//! tmux/fake-worker/coordinator surfaces. JSON `ok:true` is never sufficient:
//! assertions use tmux sessions/panes, process snapshots, DB rows, and state.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, dead_code)]

#[path = "support/hermetic.rs"]
mod hermetic_guard;
#[allow(dead_code)]
fn _hermetic_boundary_marker(_: &hermetic_guard::HermeticTestEnv) {}

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use rusqlite::params;
use serde_json::{json, Value};
use serial_test::file_serial;
use team_agent::db::schema::open_db;
use team_agent::message_store::MessageStore;
use team_agent::state::persist::load_runtime_state;
use team_agent::tmux_backend::TmuxBackend;
use team_agent::transport::{CaptureRange, PaneId, SessionName, Target, Transport};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_team-agent")
}

#[test]
#[ignore = "real-machine: needs real tmux/coordinator/binary"]
#[file_serial(tmux)]
fn tit16_scoped_child_shutdown_reaps_only_child_and_parent_still_delivers() {
    let case = RealCase::new("tit16-scoped-shutdown");
    let parent = case.unique_team("parent");
    let child = case.unique_team("child");
    write_fake_team(case.root(), &parent, "sublead", &[]);
    write_fake_team(case.root(), &child, "cw1", &[]);

    let parent_out = case.quick_start(&parent, "sublead", &[]);
    assert_success_json("parent quick-start fixture", &parent_out);
    let parent_state = state_value(case.root());
    let parent_pane = agent_pane(&parent_state, &parent, "sublead")
        .unwrap_or_else(|| panic!("parent sublead pane_id must be persisted; state={parent_state}"));
    assert!(
        case.has_session(&format!("team-{parent}")),
        "fixture must prove parent tmux session exists after quick-start"
    );
    assert!(
        processes(case.root()).has_fake_worker("sublead"),
        "fixture must prove parent fake-worker sublead process exists before child launch; processes={:?}",
        processes(case.root()).lines
    );

    let child_env = [
        ("TEAM_AGENT_OWNER_TEAM_ID", parent.as_str()),
        ("TEAM_AGENT_TEAM_ID", parent.as_str()),
        ("TEAM_AGENT_ID", "sublead"),
    ];
    let child_out = case.quick_start(&child, "cw1", &child_env);
    assert_success_json("child quick-start from parent sublead env", &child_out);
    assert!(
        case.has_session(&format!("team-{child}")),
        "fixture must prove child tmux session exists before scoped shutdown"
    );
    assert!(
        processes(case.root()).has_fake_worker("cw1"),
        "fixture must prove child fake-worker process exists before scoped shutdown; processes={:?}",
        processes(case.root()).lines
    );

    let shutdown = case.run_cli(
        &[
            "shutdown",
            "--workspace",
            case.root_str().as_str(),
            "--team",
            child.as_str(),
            "--json",
        ],
        &[],
        Duration::from_secs(12),
    );
    let shutdown_json = parse_json_or_null(&shutdown.stdout);
    assert!(
        !shutdown.timed_out,
        "scoped child shutdown must return; stdout={} stderr={}",
        shutdown.stdout,
        shutdown.stderr
    );
    assert!(
        shutdown.code == Some(0) && shutdown_json["ok"] == json!(true),
        "fixture requires child shutdown command itself to complete before scope assertions; rc={:?} stdout={} stderr={}",
        shutdown.code,
        shutdown.stdout,
        shutdown.stderr
    );

    let after_shutdown_processes = processes(case.root());
    assert!(
        !case.has_session(&format!("team-{child}")),
        "shutdown --team child must remove only the child tmux session; child session still live"
    );
    assert!(
        !after_shutdown_processes.has_fake_worker("cw1"),
        "shutdown --team child must reap child fake-worker cw1; processes={:?}",
        after_shutdown_processes.lines
    );
    assert!(
        case.has_session(&format!("team-{parent}")),
        "TIT-16: shutdown --team child must not kill parent tmux session; shutdown={shutdown_json} processes={:?}",
        after_shutdown_processes.lines
    );
    assert!(
        after_shutdown_processes.has_coordinator(),
        "TIT-16: shutdown --team child must not kill the parent/workspace coordinator needed for parent delivery; processes={:?}",
        after_shutdown_processes.lines
    );
    assert!(
        after_shutdown_processes.has_fake_worker("sublead"),
        "TIT-16: shutdown --team child must not reap parent fake-worker sublead; processes={:?}",
        after_shutdown_processes.lines
    );

    let token = format!("TIT16_PARENT_AFTER_CHILD_SHUTDOWN_{}", case.id);
    let send = case.run_cli(
        &[
            "send",
            "sublead",
            token.as_str(),
            "--workspace",
            case.root_str().as_str(),
            "--team",
            parent.as_str(),
            "--sender",
            "leader",
            "--json",
            "--no-wait",
            "--no-ack",
        ],
        &[],
        Duration::from_secs(8),
    );
    assert!(
        !send.timed_out && send.code == Some(0),
        "parent send after child shutdown must enqueue; rc={:?} stdout={} stderr={}",
        send.code,
        send.stdout,
        send.stderr
    );
    for _ in 0..10 {
        let _ = case.run_cli(
            &["coordinator", "--workspace", case.root_str().as_str(), "--once"],
            &[],
            Duration::from_secs(4),
        );
        if delivered_message(case.root(), &parent, &token)
            && case.capture_pane(&parent_pane).contains(&token)
        {
            return;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    let rows = db_messages(case.root());
    let pane = case.capture_pane(&parent_pane);
    let state = maybe_state_value(case.root()).unwrap_or(Value::Null);
    panic!(
        "TIT-16: after scoped child shutdown, parent send must still be delivered and visible in parent sublead pane; token={token} rows={rows:?} pane={pane:?} state={state} processes={:?}",
        processes(case.root()).lines
    );
}

#[test]
#[ignore = "real-machine: needs real tmux/coordinator/binary"]
#[file_serial(tmux)]
fn collect_stored_unknown_task_result_is_nonfatal_but_marks_invalid() {
    let case = RealCase::new("collect-stored-manual");
    let team = case.unique_team("current");
    write_fake_team(case.root(), &team, "worker_a", &[]);
    let quick = case.quick_start(&team, "worker_a", &[]);
    assert_success_json("quick-start fixture", &quick);
    seed_stored_result(case.root(), &team, "res_manual_stray", "manual", "worker_a");

    let collect = case.run_cli(
        &["collect", "--workspace", case.root_str().as_str(), "--json"],
        &[],
        Duration::from_secs(8),
    );
    let out = parse_json_or_null(&collect.stdout);

    assert_eq!(
        collect.code,
        Some(0),
        "collect must exit rc0 for stored stray unknown-task rows; they are warnings, not command failures. stdout={} stderr={}",
        collect.stdout,
        collect.stderr
    );
    assert_eq!(
        out["ok"],
        json!(true),
        "collect JSON must remain ok:true when it successfully marks a stored unknown-task row invalid; out={out}"
    );
    assert!(
        out["invalid_results"].as_array().is_some_and(|rows| {
            rows.iter().any(|row| {
                row["result_id"] == json!("res_manual_stray")
                    && row["task_id"] == json!("manual")
                    && row["error"].as_str().is_some_and(|e| e.contains("unknown task id: manual"))
            })
        }),
        "collect must surface the stored unknown-task row in invalid_results; out={out}"
    );
    assert_eq!(
        out["results"]["invalid"],
        json!(1),
        "collect must increment results.invalid for the skipped stored row; out={out}"
    );
    assert!(
        !out["collected_results"].as_array().unwrap_or(&Vec::new()).iter().any(|row| {
            row["result_id"] == json!("res_manual_stray")
        }),
        "collect must not fabricate a collected task result for an unknown-task row; out={out}"
    );
}

#[test]
#[ignore = "real-machine: needs real tmux/coordinator/binary"]
#[file_serial(tmux)]
fn collect_explicit_invalid_result_file_remains_fatal() {
    let case = RealCase::new("collect-invalid-file");
    let team = case.unique_team("current");
    write_fake_team(case.root(), &team, "worker_a", &[]);
    let quick = case.quick_start(&team, "worker_a", &[]);
    assert_success_json("quick-start fixture", &quick);
    let bad = case.root().join("bad-result.json");
    std::fs::write(
        &bad,
        r#"{"schema_version":"result_envelope_v1","task_id":"manual","agent_id":"worker_a"}"#,
    )
    .unwrap();

    let collect = case.run_cli(
        &[
            "collect",
            "--workspace",
            case.root_str().as_str(),
            "--result-file",
            bad.to_string_lossy().as_ref(),
            "--json",
        ],
        &[],
        Duration::from_secs(8),
    );
    assert!(
        collect.code != Some(0),
        "explicit invalid --result-file ingestion must remain fatal/rc1; stdout={} stderr={}",
        collect.stdout,
        collect.stderr
    );
}

struct RealCase {
    root: PathBuf,
    backend: TmuxBackend,
    id: u64,
}

impl RealCase {
    fn new(tag: &str) -> Self {
        let id = next_id();
        let root = tmp_dir(tag, id);
        let backend = TmuxBackend::for_workspace(&root);
        backend.kill_server();
        Self { root, backend, id }
    }

    fn root(&self) -> &Path {
        &self.root
    }

    fn root_str(&self) -> String {
        self.root.to_string_lossy().to_string()
    }

    fn unique_team(&self, prefix: &str) -> String {
        format!("{prefix}{:x}", self.id)
    }

    fn quick_start(&self, team_key: &str, agent: &str, env: &[(&str, &str)]) -> CliRun {
        self.run_cli(
            &[
                "quick-start",
                self.root.join(team_key).to_string_lossy().as_ref(),
                "--workspace",
                self.root_str().as_str(),
                "--team-id",
                team_key,
                "--name",
                team_key,
                "--yes",
                "--json",
            ],
            env,
            Duration::from_secs(12),
        )
        .tap(|_| {
            let _ = agent;
        })
    }

    fn run_cli(&self, args: &[&str], env: &[(&str, &str)], timeout: Duration) -> CliRun {
        run_cli_at(&self.root, args, env, timeout)
    }

    fn has_session(&self, session: &str) -> bool {
        self.backend
            .has_session(&SessionName::new(session))
            .unwrap_or(false)
    }

    fn capture_pane(&self, pane: &str) -> String {
        if pane.is_empty() {
            return String::new();
        }
        self.backend
            .capture(&Target::Pane(PaneId::new(pane)), CaptureRange::Tail(200))
            .map(|captured| captured.text)
            .unwrap_or_default()
    }
}

impl Drop for RealCase {
    fn drop(&mut self) {
        self.backend.kill_server();
        kill_default_sessions_with_id(self.id);
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[derive(Debug)]
struct CliRun {
    code: Option<i32>,
    stdout: String,
    stderr: String,
    timed_out: bool,
}

fn run_cli_at(cwd: &Path, args: &[&str], env: &[(&str, &str)], timeout: Duration) -> CliRun {
    let mut child = Command::new(bin())
        .args(args)
        .current_dir(cwd)
        .env_remove("TMUX")
        .env_remove("TMUX_PANE")
        .env_remove("TEAM_AGENT_OWNER_TEAM_ID")
        .env_remove("TEAM_AGENT_TEAM_ID")
        .env_remove("TEAM_AGENT_ID")
        .envs(env.iter().copied())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|e| panic!("spawn team-agent {args:?}: {e}"));
    wait_child(&mut child, args, timeout)
}

fn wait_child(child: &mut Child, args: &[&str], timeout: Duration) -> CliRun {
    let started = Instant::now();
    let mut timed_out = false;
    let status = loop {
        if let Some(status) = child.try_wait().expect("poll child") {
            break status;
        }
        if started.elapsed() >= timeout {
            timed_out = true;
            let _ = child.kill();
            break child.wait().expect("wait killed child");
        }
        std::thread::sleep(Duration::from_millis(25));
    };
    let mut stdout = String::new();
    let mut stderr = String::new();
    if let Some(mut pipe) = child.stdout.take() {
        let _ = pipe.read_to_string(&mut stdout);
    }
    if let Some(mut pipe) = child.stderr.take() {
        let _ = pipe.read_to_string(&mut stderr);
    }
    if timed_out {
        eprintln!("timed out team-agent {args:?}");
    }
    CliRun {
        code: status.code(),
        stdout,
        stderr,
        timed_out,
    }
}

fn write_fake_team(root: &Path, team_key: &str, agent: &str, tasks: &[(&str, &str)]) {
    let team = root.join(team_key);
    std::fs::create_dir_all(team.join("agents")).unwrap();
    let tasks_yaml = if tasks.is_empty() {
        String::new()
    } else {
        let mut out = String::from("tasks:\n");
        for (task_id, assignee) in tasks {
            out.push_str(&format!(
                "  - id: {task_id}\n    title: {task_id}\n    type: implementation\n    assignee: {assignee}\n    deps: []\n    acceptance:\n      - done\n    status: pending\n    requires_tools:\n      - mcp_team\n"
            ));
        }
        out
    };
    std::fs::write(
        team.join("TEAM.md"),
        format!(
            "---\nname: {team_key}\nobjective: 0.3.3 shipgate real RED fixture.\nprovider: fake\n{tasks_yaml}---\n\nTeam.\n"
        ),
    )
    .unwrap();
    std::fs::write(
        team.join("agents").join(format!("{agent}.md")),
        format!(
            "---\nname: {agent}\nrole: Shipgate fake worker\nprovider: fake\nmodel: fake\nauth_mode: subscription\ntools:\n  - mcp_team\n---\n\nWorker.\n"
        ),
    )
    .unwrap();
}

fn assert_success_json(label: &str, run: &CliRun) {
    let body = parse_json_or_null(&run.stdout);
    assert!(
        !run.timed_out && run.code == Some(0) && body["ok"] == json!(true),
        "{label}: expected rc=0 JSON ok=true; rc={:?} timed_out={} stdout={} stderr={}",
        run.code,
        run.timed_out,
        run.stdout,
        run.stderr
    );
}

fn parse_json_or_null(stdout: &str) -> Value {
    serde_json::from_str(stdout).unwrap_or(Value::Null)
}

fn state_value(root: &Path) -> Value {
    load_runtime_state(root)
        .unwrap_or_else(|e| panic!("runtime state must exist and parse: {e}; root={}", root.display()))
}

fn maybe_state_value(root: &Path) -> Option<Value> {
    load_runtime_state(root).ok()
}

fn agent_pane(state: &Value, team: &str, agent: &str) -> Option<String> {
    state
        .pointer(&format!("/teams/{team}/agents/{agent}/pane_id"))
        .and_then(Value::as_str)
        .filter(|pane| !pane.is_empty())
        .map(ToString::to_string)
}

#[derive(Debug, Clone)]
struct MessageRow {
    message_id: String,
    owner_team_id: String,
    sender: String,
    recipient: String,
    status: String,
    content: String,
}

fn db_messages(root: &Path) -> Vec<MessageRow> {
    let path = root.join(".team").join("runtime").join("team.db");
    if !path.exists() {
        return Vec::new();
    }
    let _ = MessageStore::open(root);
    let conn = open_db(&path).expect("open team.db");
    conn.prepare(
        "select message_id, coalesce(owner_team_id,''), sender, recipient, status, content
         from messages
         order by created_at, message_id",
    )
    .and_then(|mut stmt| {
        stmt.query_map([], |row| {
            Ok(MessageRow {
                message_id: row.get(0)?,
                owner_team_id: row.get(1)?,
                sender: row.get(2)?,
                recipient: row.get(3)?,
                status: row.get(4)?,
                content: row.get(5)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()
    })
    .unwrap_or_default()
}

fn delivered_message(root: &Path, owner_team_id: &str, token: &str) -> bool {
    db_messages(root).iter().any(|row| {
        row.owner_team_id == owner_team_id && row.content.contains(token) && row.status == "delivered"
    })
}

fn seed_stored_result(root: &Path, owner_team_id: &str, result_id: &str, task_id: &str, agent_id: &str) {
    let _ = MessageStore::open(root).expect("open message store");
    let db_path = root.join(".team").join("runtime").join("team.db");
    let conn = open_db(&db_path).expect("open team.db");
    let envelope = json!({
        "schema_version": "result_envelope_v1",
        "task_id": task_id,
        "agent_id": agent_id,
        "status": "success",
        "summary": "stored stray manual result",
        "artifacts": [],
        "changes": [],
        "tests": [],
        "risks": [],
        "next_actions": []
    });
    conn.execute(
        "insert into results(result_id, owner_team_id, task_id, agent_id, envelope, status, created_at)
         values (?1, ?2, ?3, ?4, ?5, 'success', ?6)",
        params![
            result_id,
            owner_team_id,
            task_id,
            agent_id,
            envelope.to_string(),
            now_isoish()
        ],
    )
    .expect("insert stray result");
}

#[derive(Debug)]
struct ProcessSnapshot {
    lines: Vec<String>,
}

impl ProcessSnapshot {
    fn has_fake_worker(&self, agent_id: &str) -> bool {
        self.lines
            .iter()
            .any(|line| line.contains("team-agent fake-worker") && line.contains(agent_id))
    }

    fn has_coordinator(&self) -> bool {
        self.lines
            .iter()
            .any(|line| line.contains("team-agent coordinator"))
    }
}

fn processes(root: &Path) -> ProcessSnapshot {
    let needle = root.to_string_lossy();
    let output = Command::new("ps")
        .args(["-axo", "pid=,command="])
        .output()
        .expect("ps");
    let lines = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|line| line.contains(needle.as_ref()))
        .filter(|line| line.contains("team-agent coordinator") || line.contains("team-agent fake-worker"))
        .map(ToString::to_string)
        .collect();
    ProcessSnapshot { lines }
}

fn tmp_dir(tag: &str, id: u64) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "ta-033-shipgate-{tag}-{}-{id}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::canonicalize(dir).unwrap()
}

fn kill_default_sessions_with_id(id: u64) {
    let marker = format!("{id:x}");
    let output = Command::new("tmux")
        .args(["list-sessions", "-F", "#{session_name}"])
        .output();
    let Ok(output) = output else {
        return;
    };
    for name in String::from_utf8_lossy(&output.stdout).lines() {
        if name.starts_with("team-") && name.contains(&marker) {
            let _ = Command::new("tmux").args(["kill-session", "-t", name]).status();
        }
    }
}

fn next_id() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64;
    (nanos & 0xffff_ffff).saturating_add(NEXT.fetch_add(1, Ordering::Relaxed))
}

fn now_isoish() -> String {
    format!(
        "{}.{:09}Z",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .subsec_nanos()
    )
}

trait Tap: Sized {
    fn tap(self, f: impl FnOnce(&Self)) -> Self {
        f(&self);
        self
    }
}

impl<T> Tap for T {}
