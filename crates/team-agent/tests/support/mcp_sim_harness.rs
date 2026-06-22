#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

use rusqlite::OptionalExtension;
use serde_json::{json, Value};
use team_agent::event_log::EventLog;
use team_agent::message_store::MessageStore;
use team_agent::messaging::{deliver_pending_messages, fire_due_scheduled_events};
use team_agent::tmux_backend::TmuxBackend;
use team_agent::transport::{CaptureRange, PaneId, SessionName, Target, Transport, WindowName};

const EXPECTED_TOOLS: &[&str] = &[
    "assign_task",
    "send_message",
    "report_result",
    "update_state",
    "get_team_status",
    "stop_agent",
    "reset_agent",
    "add_agent",
    "fork_agent",
    "request_human",
    "stuck_list",
    "stuck_cancel",
];

pub struct McpSimHarness {
    workspace: PathBuf,
    backend: TmuxBackend,
    session: SessionName,
    panes: BTreeMap<String, PaneId>,
}

impl McpSimHarness {
    pub fn new() -> Self {
        static N: AtomicU64 = AtomicU64::new(0);
        let workspace = std::env::temp_dir().join(format!(
            "ta-rs-mcp-sim-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&workspace);
        std::fs::create_dir_all(&workspace).unwrap();
        let workspace = std::fs::canonicalize(workspace).unwrap();
        let backend = TmuxBackend::for_workspace(&workspace);
        let session = SessionName::new(format!(
            "team-mcp-sim-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let mut harness = Self {
            workspace,
            backend,
            session,
            panes: BTreeMap::new(),
        };
        harness.spawn_pane("leader", true);
        harness.spawn_pane("worker_a", false);
        harness.spawn_pane("worker_b", false);
        harness.spawn_pane("worker_c", false);
        harness.spawn_pane("team_b_leader", false);
        harness.seed_state();
        let _ = MessageStore::open(&harness.workspace).unwrap();
        harness
    }

    pub fn spawn_mcp_client(&self, worker_id: &str, owner_team_id: &str) -> McpClient {
        spawn_mcp_client(&self.workspace, worker_id, owner_team_id)
    }

    pub fn workspace_path(&self) -> &Path {
        &self.workspace
    }

    pub fn workspace_display(&self) -> String {
        self.workspace.to_string_lossy().to_string()
    }

    pub fn state_value(&self) -> Value {
        team_agent::state::persist::load_runtime_state(&self.workspace).unwrap()
    }

    pub fn drive_delivery_once(&self) {
        let store = MessageStore::open(&self.workspace).unwrap();
        let event_log = EventLog::new(&self.workspace);
        let _ = fire_due_scheduled_events(&self.workspace, &store, &self.backend, &event_log)
            .unwrap();
        let state = team_agent::state::persist::load_runtime_state(&self.workspace).unwrap();
        let _ = deliver_pending_messages(&self.workspace, &state, &self.backend, &event_log)
            .unwrap();
        std::thread::sleep(Duration::from_millis(150));
    }

    pub fn drive_delivery_twice(&self) {
        self.drive_delivery_once();
        self.drive_delivery_once();
    }

    pub fn pane_text(&self, name: &str) -> String {
        let pane = self.panes.get(name).unwrap_or_else(|| panic!("unknown pane {name}"));
        self.backend
            .capture(&Target::Pane(pane.clone()), CaptureRange::Full)
            .unwrap()
            .text
    }

    pub fn pane_contains_count(&self, name: &str, needle: &str) -> usize {
        self.pane_text(name).matches(needle).count()
    }

    pub fn events_text(&self) -> String {
        std::fs::read_to_string(self.workspace.join(".team").join("logs").join("events.jsonl"))
            .unwrap_or_default()
    }

    pub fn clear_leader_receiver_binding(&self) {
        let mut state = team_agent::state::persist::load_runtime_state(&self.workspace).unwrap();
        for key in ["leader_receiver", "team_owner"] {
            if let Some(obj) = state.get_mut(key).and_then(Value::as_object_mut) {
                obj.remove("pane_id");
            }
        }
        team_agent::state::persist::save_runtime_state(&self.workspace, &state).unwrap();
    }

    pub fn message_rows_containing(&self, needle: &str) -> Vec<MessageRow> {
        let store = MessageStore::open(&self.workspace).unwrap();
        let conn = team_agent::db::schema::open_db(store.db_path()).unwrap();
        let mut stmt = conn
            .prepare(
                "select message_id, owner_team_id, sender, recipient, status, content
                 from messages
                 order by created_at, message_id",
            )
            .unwrap();
        let rows = stmt
            .query_map([], |row| {
                Ok(MessageRow {
                    message_id: row.get(0)?,
                    owner_team_id: row.get(1)?,
                    sender: row.get(2)?,
                    recipient: row.get(3)?,
                    status: row.get(4)?,
                    content: row.get(5)?,
                })
            })
            .unwrap();
        rows.collect::<Result<Vec<_>, _>>()
            .unwrap()
            .into_iter()
            .filter(|row| row.content.contains(needle))
            .collect()
    }

    pub fn result_row(&self, result_id: &str) -> Option<ResultRow> {
        let store = MessageStore::open(&self.workspace).unwrap();
        let conn = team_agent::db::schema::open_db(store.db_path()).unwrap();
        conn.query_row(
            "select result_id, owner_team_id, task_id, agent_id, envelope, status
             from results where result_id = ?1",
            [result_id],
            |row| {
                Ok(ResultRow {
                    result_id: row.get(0)?,
                    owner_team_id: row.get(1)?,
                    task_id: row.get(2)?,
                    agent_id: row.get(3)?,
                    envelope: row.get(4)?,
                    status: row.get(5)?,
                })
            },
        )
        .optional()
        .unwrap()
    }

    pub fn scheduled_event_count(&self) -> i64 {
        let store = MessageStore::open(&self.workspace).unwrap();
        let conn = team_agent::db::schema::open_db(store.db_path()).unwrap();
        conn.query_row("select count(*) from scheduled_events", [], |row| row.get(0))
            .unwrap()
    }

    fn spawn_pane(&mut self, name: &str, first: bool) {
        let window = WindowName::new(name);
        // Disable pty input echo BEFORE exec'ing cat so the canary text isn't visible
        // twice in `tmux capture-pane` (once from terminal local echo on paste, once
        // from cat's stdout). Real Codex/Claude panes don't run with default
        // ICANON+ECHO; the contract is calibrated for "one rendered occurrence per
        // injected message". Plumbing-only — no contract assertions touched.
        let argv = vec![
            "sh".to_string(),
            "-lc".to_string(),
            "stty -echo 2>/dev/null; exec cat".to_string(),
        ];
        let env = BTreeMap::new();
        let result = if first {
            self.backend
                .spawn_first(&self.session, &window, &argv, &self.workspace, &env)
        } else {
            self.backend
                .spawn_into(&self.session, &window, &argv, &self.workspace, &env)
        }
        .unwrap();
        self.panes.insert(name.to_string(), result.pane_id);
    }

    fn seed_state(&self) {
        let leader_pane = self.panes["leader"].as_str();
        let worker_a = self.panes["worker_a"].as_str();
        let worker_b = self.panes["worker_b"].as_str();
        let worker_c = self.panes["worker_c"].as_str();
        // 0.4.0 refactor: lifecycle ops (stop_agent, restart, reset) now
        // run through `ensure_owner_allowed_for_state`. The MCP sim spawns
        // `worker_b` as the caller, so seed the team owner pane to worker_b
        // so the owner-gate's pane-equality bypass (`caller_pane ==
        // owner_pane → return None`) lets the lifecycle op proceed. Without
        // this, every stop/restart/reset call refuses with
        // `team_owner_mismatch`. The test contract is about the LIFECYCLE
        // side effect, not the owner gate (which has its own dedicated
        // tests in state/owner_gate.rs).
        team_agent::state::persist::save_runtime_state(
            &self.workspace,
            &json!({
                "active_team_key": "teamA",
                "session_name": self.session.as_str(),
                "leader": {"id": "leader"},
                "team_owner": {
                    "owner_epoch": 1,
                    "pane_id": worker_b,
                    "leader_session_uuid": "leader-session-team-a"
                },
                "leader_receiver": {
                    "mode": "direct_tmux",
                    "owner_epoch": 1,
                    "pane_id": leader_pane,
                    "provider": "codex",
                    "leader_session_uuid": "leader-session-team-a"
                },
                "agents": {
                    "worker_a": {
                        "provider": "fake",
                        "status": "running",
                        "window": "worker_a",
                        "pane_id": worker_a
                    },
                    "worker_b": {
                        "provider": "fake",
                        "status": "running",
                        "window": "worker_b",
                        "pane_id": worker_b
                    },
                    "worker_c": {
                        "provider": "fake",
                        "status": "running",
                        "window": "worker_c",
                        "pane_id": worker_c
                    }
                },
                "teams": {
                    "teamA": {
                        "agents": {
                            "worker_a": {"status": "running"},
                            "worker_b": {"status": "running"},
                            "worker_c": {"status": "running"}
                        },
                        "tasks": [
                            {"id": "task_mcp", "assignee": "worker_a", "status": "pending"}
                        ]
                    },
                    "teamB": {
                        "agents": {
                            "worker_x": {"status": "running"}
                        }
                    }
                },
                "tasks": [
                    {"id": "task_mcp", "assignee": "worker_a", "status": "pending"}
                ]
            }),
        )
        .unwrap();
    }
}

impl Drop for McpSimHarness {
    fn drop(&mut self) {
        let _ = self.backend.kill_session(&self.session);
        self.backend.kill_server();
        let _ = std::fs::remove_dir_all(&self.workspace);
    }
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct MessageRow {
    pub message_id: String,
    pub owner_team_id: Option<String>,
    pub sender: String,
    pub recipient: String,
    pub status: String,
    pub content: String,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct ResultRow {
    pub result_id: String,
    pub owner_team_id: Option<String>,
    pub task_id: String,
    pub agent_id: String,
    pub envelope: String,
    pub status: String,
}

pub struct McpClient {
    child: Child,
    stdin: ChildStdin,
    stdout_rx: Receiver<String>,
    next_id: i64,
    spawn_spec: McpSpawnSpec,
    trace_path: PathBuf,
}

pub fn spawn_mcp_client(workspace: &Path, worker_id: &str, owner_team_id: &str) -> McpClient {
    let program = env!("CARGO_BIN_EXE_team-agent").to_string();
    let args = vec![
        "mcp-server".to_string(),
        "--workspace".to_string(),
        workspace.to_string_lossy().to_string(),
    ];
    let env = BTreeMap::from([
        (
            "TEAM_AGENT_WORKSPACE".to_string(),
            workspace.to_string_lossy().to_string(),
        ),
        ("TEAM_AGENT_ID".to_string(), worker_id.to_string()),
        (
            "TEAM_AGENT_OWNER_TEAM_ID".to_string(),
            owner_team_id.to_string(),
        ),
        // 0.4.0 refactor: lifecycle ops (stop/restart/reset) check owner via
        // check_team_owner. The MCP child process has no TMUX_PANE so the
        // pane-equality bypass doesn't fire; provide the matching
        // leader_session_uuid so the same-uuid bypass allows the call.
        // Value must equal the harness's seeded team_owner.leader_session_uuid.
        (
            "TEAM_AGENT_LEADER_SESSION_UUID_OVERRIDE".to_string(),
            "leader-session-team-a".to_string(),
        ),
    ]);
    let trace_path = workspace
        .join(".team")
        .join("test-evidence")
        .join(format!("mcp-rpc-{worker_id}.jsonl"));
    let _ = std::fs::remove_file(&trace_path);
    let mut command = Command::new(&program);
    command.args(&args);
    for (key, value) in &env {
        command.env(key, value);
    }
    // 0.4.0 refactor: owner-gate's same-uuid bypass requires caller_pane to
    // be empty OR equal to owner_pane. The MCP child process must not inherit
    // a stray TMUX_PANE from the harness parent (which would set caller_pane
    // to the harness's own pane, breaking the bypass).
    command.env_remove("TMUX_PANE");
    command.env_remove("TMUX");
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn team-agent mcp-server");
    let stdin = child.stdin.take().expect("mcp stdin");
    let stdout = child.stdout.take().expect("mcp stdout");
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    let mut client = McpClient {
        child,
        stdin,
        stdout_rx: rx,
        next_id: 1,
        spawn_spec: McpSpawnSpec {
            program,
            args,
            env,
        },
        trace_path,
    };
    client.assert_initialize_and_tools();
    client
}

impl McpClient {
    pub fn spawn_spec(&self) -> &McpSpawnSpec {
        &self.spawn_spec
    }

    pub fn trace_path(&self) -> &Path {
        &self.trace_path
    }

    pub fn trace_entries(&self) -> Vec<Value> {
        std::fs::read_to_string(&self.trace_path)
            .unwrap_or_default()
            .lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect()
    }

    pub fn call_tool(&mut self, name: &str, arguments: Value) -> McpToolCall {
        let raw = self.rpc("tools/call", json!({"name": name, "arguments": arguments}));
        let result = raw
            .get("result")
            .unwrap_or_else(|| panic!("tools/call response missing result: {raw}"));
        let is_error = result
            .get("isError")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let text = result
            .get("content")
            .and_then(Value::as_array)
            .and_then(|items| items.first())
            .and_then(|item| item.get("text"))
            .and_then(Value::as_str)
            .unwrap_or("{}");
        let body = serde_json::from_str(text).unwrap_or_else(|_| json!({"raw_text": text}));
        McpToolCall {
            raw: raw.clone(),
            body,
            is_error,
        }
    }

    fn assert_initialize_and_tools(&mut self) {
        let init = self.rpc("initialize", json!({"protocolVersion": "2024-11-05"}));
        assert_eq!(
            init["result"]["serverInfo"]["name"],
            json!("team_orchestrator"),
            "MCP initialize must expose the Team Agent server identity; init={init}"
        );
        let listed = self.rpc("tools/list", json!({}));
        let tools = listed["result"]["tools"]
            .as_array()
            .unwrap_or_else(|| panic!("tools/list returned no tools array: {listed}"))
            .iter()
            .filter_map(|tool| tool.get("name").and_then(Value::as_str))
            .collect::<BTreeSet<_>>();
        let expected = EXPECTED_TOOLS.iter().copied().collect::<BTreeSet<_>>();
        assert_eq!(
            tools, expected,
            "MCP tools/list must expose the 12 canonical Team Agent tools; listed={listed}"
        );
    }

    fn rpc(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        let request = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params
        });
        writeln!(self.stdin, "{request}").expect("write json-rpc request");
        self.stdin.flush().expect("flush json-rpc request");
        // 0.3.28-final E55 + send false-negative hardening: strict
        // consumption gate retries 3× with ~2s Phase-1 timeout + a 1.2s
        // Phase-2 poll per attempt. report_result can synchronously retry
        // leader delivery, and broadcast paths may fan out serially. Keep the
        // MCP harness above that bare-shell fixture wall-clock.
        let line = self
            .stdout_rx
            .recv_timeout(Duration::from_secs(75))
            .unwrap_or_else(|_| panic!("timed out waiting for MCP response to {method}"));
        let value: Value = serde_json::from_str(&line).unwrap_or_else(|err| {
            panic!("invalid JSON-RPC response for {method}: {err}; line={line}")
        });
        assert!(
            value.get("error").is_none(),
            "JSON-RPC method {method} returned protocol error: {value}"
        );
        self.record_trace(&request, &value);
        value
    }

    fn record_trace(&self, request: &Value, response: &Value) {
        if let Some(parent) = self.trace_path.parent() {
            std::fs::create_dir_all(parent).expect("create mcp trace dir");
        }
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.trace_path)
            .expect("open mcp rpc trace");
        writeln!(
            file,
            "{}",
            json!({
                "request": request,
                "response": response,
            })
        )
        .expect("write mcp rpc trace");
    }
}

impl Drop for McpClient {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[derive(Debug, Clone)]
pub struct McpToolCall {
    pub raw: Value,
    pub body: Value,
    pub is_error: bool,
}

#[derive(Debug, Clone)]
pub struct McpSpawnSpec {
    pub program: String,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
}
