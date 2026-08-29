//! Car-C successor TARGET-INVARIANT contract (verifier-frozen): the send path
//! persists before any recovery concern, through ONE primitive, at every
//! entry point — and coordinator availability is a delivery blocker, never a
//! pre-persist refusal.
//!
//! Category: target invariant (tests/ admissible). Fixtures are canonical
//! only (five-check clean): real quick-start teams, real tmux, fake provider,
//! real MCP server process against the real workspace; the ONLY fault used is
//! a real `kill` of the coordinator process — explicitly listed by MUST-15 as
//! a canonical trigger. No state/row/transcript synthesis anywhere.
//!
//! Invariants (aligned with runtime-owner's wire proposal, msg_01d41fc37b88):
//!  1. persist-before-recovery: a canonically resolved worker send with the
//!     coordinator DOWN still persists exactly one `msg_*` row, reports
//!     honestly (ok = persisted, delivered = false) and self-heals LOUDLY
//!     (coordinator auto-restart surfaced in the response). The
//!     `queued_coordinator_unavailable` durable-blocker wire (owner proposal
//!     msg_01d41fc37b88) applies only when the ensure itself fails — no
//!     canonical trigger exists for that state, so it is DEFERRED to unit
//!     territory per MUST-15.
//!  2. all-entrypoint parity: positional TO, `--to-name` alias and the MCP
//!     `send_message` tool all land in the same persisted fingerprint
//!     (recipient/sender/team/one-row-per-recipient).
//!  3. pre-persist refusal boundary: unknown recipient / unresolvable name /
//!     unbound leader refuse with ZERO DB side effects — availability is
//!     never mapped to a refusal, and refusals never persist.
//!  4. recovery same-row: once the coordinator is back (lazily ensured by the
//!     next canonical command), the SAME message_id advances out of the
//!     blocker; still exactly one row, no replacement row.
//!  5. fanout independence: each comma-list recipient gets its own row; one
//!     recipient's delivery blocker must not erase or block the other's.
//!
//! ---
//! arch:
//!   allowed_dependencies: [rusqlite, serde_json, serial_test, std, team_agent]
//!   read_closure: [messaging, coordinator]
//!   unresolved_disposition: inherited_incomplete_unknown
//! boundary:
//!   - test-only fanout row independence
//!   - panic-safe exact-owned tmux/coordinator cleanup
//!   - log-visible C5 failure packet
//! ---

#![cfg(unix)]
#![allow(clippy::expect_used, clippy::panic)]

#[path = "support/hermetic.rs"]
mod hermetic_guard;
use hermetic_guard::HermeticTestEnv;
#[path = "support/mcp_sim_harness.rs"]
mod mcp_sim_harness;

use std::path::PathBuf;
use std::process::{Command, Output};

use serde_json::{json, Value};
use serial_test::serial;

const TEAM: &str = "carc";

struct SendPathCase {
    env: HermeticTestEnv,
    workspace: PathBuf,
    socket: PathBuf,
    kill_pane: Value,
    panes_after_kill: Value,
}

impl SendPathCase {
    fn start(tag: &str) -> Self {
        let env = HermeticTestEnv::enter(tag);
        env.scrub_tmux();
        let workspace = env.workspace(tag);
        std::fs::create_dir_all(workspace.join("agents")).expect("create agents dir");
        std::fs::write(
            workspace.join("TEAM.md"),
            format!("---\nname: {TEAM}\nobjective: car-c persist-first contract.\nprovider: fake\n---\n"),
        )
        .expect("write TEAM.md");
        for worker in ["w1", "w2"] {
            std::fs::write(
                workspace.join("agents").join(format!("{worker}.md")),
                format!(
                    "---\nname: {worker}\nrole: {worker}\nprovider: fake\nmodel: fake\nauth_mode: subscription\ndangerously_skip_permissions: false\ntools:\n  - mcp_team\n---\n\n{worker}.\n"
                ),
            )
            .expect("write worker role doc");
        }
        let mut case = Self {
            env,
            workspace,
            socket: PathBuf::new(),
            kill_pane: Value::Null,
            panes_after_kill: Value::Null,
        };
        let output = case.run_cli(&[
            "quick-start",
            "--workspace",
            case.workspace_str(),
            "--team-id",
            TEAM,
            "--yes",
            "--no-display",
            "--json",
        ]);
        if let Ok(state_raw) = std::fs::read_to_string(
            case.workspace
                .join(".team")
                .join("runtime")
                .join("state.json"),
        ) {
            if let Ok(state) = serde_json::from_str::<Value>(&state_raw) {
                if let Some(socket) = state.get("tmux_socket").and_then(Value::as_str) {
                    case.socket = PathBuf::from(socket);
                }
            }
        }
        let value = json_stdout(&output, "quick-start");
        assert!(
            value
                .get("worker_readiness")
                .and_then(|node| node.get("all_workers_spawned"))
                .and_then(Value::as_bool)
                == Some(true),
            "fixture: quick-start must spawn workers; stdout={}",
            String::from_utf8_lossy(&output.stdout)
        );
        assert!(
            !case.socket.as_os_str().is_empty(),
            "fixture: state.json must expose tmux_socket after quick-start"
        );
        case
    }

    fn workspace_str(&self) -> &str {
        self.workspace.to_str().expect("workspace utf8")
    }

    fn run_cli(&self, args: &[&str]) -> Output {
        self.env.run_cli(&self.workspace, args)
    }

    fn send_json(&self, args: &[&str]) -> Value {
        let mut full = vec!["send"];
        full.extend_from_slice(args);
        full.extend_from_slice(&["--workspace", self.workspace_str(), "--json"]);
        json_stdout(&self.run_cli(&full), "send")
    }

    /// Canonical fault (MUST-15-listed trigger): really kill the coordinator
    /// process and wait for it to exit.
    fn kill_coordinator(&self) {
        let pid = self
            .coordinator_pid()
            .expect("coordinator.pid present after quick-start");
        let _ = Command::new("kill")
            .args(["-TERM", &pid.to_string()])
            .output();
        for _ in 0..50 {
            let alive = Command::new("kill")
                .args(["-0", &pid.to_string()])
                .output()
                .map(|probe| probe.status.success())
                .unwrap_or(false);
            if !alive {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        panic!("coordinator pid {pid} did not exit within 5s of SIGTERM");
    }

    fn db_rows(&self, needle: &str) -> Vec<DbRow> {
        let db = self.workspace.join(".team").join("runtime").join("team.db");
        if !db.exists() {
            return Vec::new();
        }
        let connection = rusqlite::Connection::open(&db).expect("open team.db");
        let mut statement = connection
            .prepare(
                "SELECT message_id, sender, recipient, owner_team_id, status, \
                        delivery_attempts, error, delivered_at FROM messages \
                 WHERE content LIKE ?1 ORDER BY rowid",
            )
            .expect("prepare message query");
        statement
            .query_map([format!("%{needle}%")], |row| {
                Ok(DbRow {
                    message_id: row.get(0)?,
                    sender: row.get(1)?,
                    recipient: row.get(2)?,
                    owner_team_id: row.get(3)?,
                    status: row.get(4)?,
                    delivery_attempts: row.get(5)?,
                    error: row.get(6)?,
                    delivered_at: row.get(7)?,
                })
            })
            .expect("query messages")
            .filter_map(Result::ok)
            .collect()
    }

    fn event_count(&self, event_name: &str, message_id: &str) -> usize {
        std::fs::read_to_string(
            self.workspace
                .join(".team")
                .join("logs")
                .join("events.jsonl"),
        )
        .unwrap_or_default()
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|event| {
            event.get("event").and_then(Value::as_str) == Some(event_name)
                && event.get("message_id").and_then(Value::as_str) == Some(message_id)
        })
        .count()
    }

    fn coordinator_pid_path(&self) -> PathBuf {
        self.workspace
            .join(".team")
            .join("runtime")
            .join("coordinator.pid")
    }

    fn coordinator_pid(&self) -> Option<i32> {
        std::fs::read_to_string(self.coordinator_pid_path())
            .ok()
            .and_then(|raw| raw.trim().parse::<i32>().ok())
    }

    fn pid_running(pid: i32) -> bool {
        Command::new("kill")
            .args(["-0", &pid.to_string()])
            .output()
            .map(|probe| probe.status.success())
            .unwrap_or(false)
    }

    fn command_snapshot(output: Result<Output, std::io::Error>) -> Value {
        match output {
            Ok(output) => json!({
                "exit_code": output.status.code(),
                "success": output.status.success(),
                "stdout": String::from_utf8_lossy(&output.stdout),
                "stderr": String::from_utf8_lossy(&output.stderr),
            }),
            Err(error) => json!({"spawn_error": error.to_string()}),
        }
    }

    fn tmux(&self, args: &[&str]) -> Result<Output, std::io::Error> {
        let socket = self.socket.to_str().unwrap_or("");
        Command::new("tmux")
            .args(["-S", socket])
            .args(args)
            .output()
    }

    fn pane_tuple_snapshot(&self) -> Value {
        SendPathCase::command_snapshot(self.tmux(&[
            "list-panes",
            "-a",
            "-F",
            "#{session_name}__TA_FIELD__#{window_name}__TA_FIELD__#{pane_id}__TA_FIELD__#{pane_pid}__TA_FIELD__#{pane_dead}",
        ]))
    }

    fn read_runtime_json(&self, name: &str) -> Value {
        let path = self.workspace.join(".team").join("runtime").join(name);
        match std::fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text).unwrap_or_else(|_| {
                json!({"path": path, "unparsed": text})
            }),
            Err(_) => json!({"path": path, "missing_or_unreadable": true}),
        }
    }

    fn events_for(&self, message_id: &str) -> Value {
        let path = self
            .workspace
            .join(".team")
            .join("logs")
            .join("events.jsonl");
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        let events = text
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .filter(|event| {
                event.get("message_id").and_then(Value::as_str) == Some(message_id)
            })
            .collect::<Vec<_>>();
        json!({"path": path, "events": events})
    }

    fn socket_is_exact_owned(&self) -> bool {
        if self.socket.as_os_str().is_empty() || !self.socket.is_absolute() {
            return false;
        }
        let Some(name) = self.socket.file_name().and_then(|name| name.to_str()) else {
            return false;
        };
        if !name.starts_with("ta-") {
            return false;
        }
        let ambient = std::env::var_os("TMUX").and_then(|value| {
            let socket = value.to_str()?.split(',').next()?;
            (!socket.is_empty()).then(|| PathBuf::from(socket))
        });
        ambient.as_deref() != Some(self.socket.as_path())
    }

    fn kill_exact_owned(&self) {
        if let Some(pid) = self.coordinator_pid() {
            let _ = Command::new("kill")
                .args(["-TERM", &pid.to_string()])
                .output();
            let _ = Command::new("kill")
                .args(["-KILL", &pid.to_string()])
                .output();
        }
        if self.socket_is_exact_owned() {
            if let Some(socket) = self.socket.to_str() {
                let _ = Command::new("tmux")
                    .args(["-S", socket, "kill-server"])
                    .output();
            }
        }
    }

    fn c5_failure_packet(&self, extra: Value) -> Value {
        let rows = self.db_rows("carc c5 fanout");
        let row_packets = rows
            .iter()
            .map(|row| {
                json!({
                    "message_id": row.message_id,
                    "sender": row.sender,
                    "recipient": row.recipient,
                    "owner_team_id": row.owner_team_id,
                    "status": row.status,
                    "delivery_attempts": row.delivery_attempts,
                    "error": row.error,
                    "delivered_at": row.delivered_at,
                    "events": self.events_for(&row.message_id),
                })
            })
            .collect::<Vec<_>>();
        let pid = self.coordinator_pid();
        json!({
            "schema": "team-agent/f07-c5-failure-v1",
            "extra": extra,
            "workspace": self.workspace.display().to_string(),
            "home": self.env.home().display().to_string(),
            "socket": self.socket.display().to_string(),
            "socket_is_exact_owned": self.socket_is_exact_owned(),
            "kill_pane": self.kill_pane,
            "panes_after_kill": self.panes_after_kill,
            "panes_at_failure": self.pane_tuple_snapshot(),
            "rows": row_packets,
            "coordinator": {
                "pid": pid,
                "pid_running": pid.is_some_and(Self::pid_running),
                "meta": self.read_runtime_json("coordinator.json"),
                "heartbeat": self.read_runtime_json("coordinator_tick.json"),
                "drain": self.read_runtime_json("drain.json"),
            },
            "owned_child_exits": {
                "coordinator_pid": pid,
                "coordinator_alive": pid.is_some_and(Self::pid_running),
            },
        })
    }

    fn persist_c5_failure_packet(&self, extra: Value) -> String {
        let packet = self.c5_failure_packet(extra);
        let encoded = serde_json::to_string(&packet).unwrap_or_else(|_| {
            "{\"schema\":\"team-agent/f07-c5-failure-v1\",\"encode_error\":true}".to_string()
        });
        eprintln!("F07_C5_DIAGNOSTIC {encoded}");
        encoded
    }

    fn wait_status(&self, message_id: &str, wanted: &[&str], seconds: u64) -> String {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(seconds);
        loop {
            let db = self.workspace.join(".team").join("runtime").join("team.db");
            let status = rusqlite::Connection::open(&db)
                .ok()
                .and_then(|connection| {
                    connection
                        .query_row(
                            "SELECT status FROM messages WHERE message_id = ?1",
                            [message_id],
                            |row| row.get::<_, String>(0),
                        )
                        .ok()
                })
                .unwrap_or_else(|| "<norow>".to_string());
            if wanted.contains(&status.as_str()) || std::time::Instant::now() >= deadline {
                return status;
            }
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
    }

    fn shutdown(&self) {
        let _ = self.run_cli(&[
            "shutdown",
            "--workspace",
            self.workspace_str(),
            "--yes",
            "--json",
        ]);
        self.kill_exact_owned();
    }
}

impl Drop for SendPathCase {
    fn drop(&mut self) {
        self.kill_exact_owned();
    }
}

#[derive(Debug, serde::Serialize)]
struct DbRow {
    message_id: String,
    sender: String,
    recipient: String,
    owner_team_id: Option<String>,
    status: Option<String>,
    delivery_attempts: Option<i64>,
    error: Option<String>,
    delivered_at: Option<String>,
}

fn json_stdout(output: &Output, context: &str) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|_| {
        panic!(
            "{context}: expected JSON stdout; stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

fn str_field<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}

/// Invariant 1 — persist-before-recovery with the coordinator really dead:
/// the send must persist exactly one row, report honestly (never delivered at
/// return), and the self-heal must be loud (auto-restart evidenced in the
/// response). NOTE: the canonical path self-heals (loud ensure respawns the
/// coordinator — the praised 0.5.22 behavior), so the
/// `queued_coordinator_unavailable` durable-blocker wire (owner proposal)
/// applies only when the ensure itself FAILS — a state with no canonical
/// trigger on this platform; that branch is DEFERRED per MUST-15 and belongs
/// to unit territory (src/**/tests), where an ensure-failure can be
/// synthesized legally.
#[test]
#[serial(env)]
fn c1_resolved_send_with_dead_coordinator_persists_one_row_and_self_heals_loudly() {
    let case = SendPathCase::start("carc-c1");
    case.kill_coordinator();

    let value = case.send_json(&["w1", "carc c1 probe", "--team", TEAM]);
    let rows = case.db_rows("carc c1 probe");

    assert_eq!(
        value.get("ok"),
        Some(&json!(true)),
        "C1: persistence success must be reported ok=true even when the coordinator was down: {value}"
    );
    assert!(
        str_field(&value, "message_id").is_some_and(|id| id.starts_with("msg_")),
        "C1: a store-backed msg_* id must be returned (no synthetic no-row outcome): {value}"
    );
    assert_eq!(
        value.get("delivered"),
        Some(&json!(false)),
        "C1: ok=true means persisted, never delivered at return time: {value}"
    );
    assert_eq!(
        value.get("coordinator_auto_restarted"),
        Some(&json!(true)),
        "C1: recovering from a dead coordinator must be LOUD (auto-restart surfaced in the \
         response), never silent: {value}"
    );
    assert_eq!(
        rows.len(),
        1,
        "C1: exactly one persisted row; rows={rows:?}"
    );
    case.shutdown();
}

/// Invariant 2 — all-entrypoint parity: positional, --to-name alias, and the
/// real MCP send_message tool share one persisted fingerprint.
#[test]
#[serial(env)]
fn c2_positional_alias_and_mcp_share_one_persisted_fingerprint() {
    let case = SendPathCase::start("carc-c2");

    let positional = case.send_json(&["w2", "carc c2 positional", "--team", TEAM]);
    assert_eq!(positional.get("ok"), Some(&json!(true)), "{positional}");
    let alias = case.send_json(&["--to-name", "w2", "carc c2 alias", "--team", TEAM]);
    assert_eq!(alias.get("ok"), Some(&json!(true)), "{alias}");

    // Real MCP server process against the real quick-started workspace —
    // worker identity w1, owner scope = the real runtime team key.
    let mut worker = mcp_sim_harness::spawn_mcp_client(&case.workspace, "w1", TEAM);
    let mcp = worker.call_tool(
        "send_message",
        json!({"to": "w2", "content": "carc c2 mcp"}),
    );
    assert!(
        mcp.body.get("message_id").and_then(Value::as_str).is_some()
            || mcp.body.get("status").and_then(Value::as_str) == Some("accepted"),
        "C2: MCP send must enter the persisted primitive; body={} raw={}",
        mcp.body,
        mcp.raw
    );

    for (label, needle, sender) in [
        ("positional", "carc c2 positional", "leader"),
        ("alias", "carc c2 alias", "leader"),
        ("mcp", "carc c2 mcp", "w1"),
    ] {
        let rows = case.db_rows(needle);
        assert_eq!(
            rows.len(),
            1,
            "C2 ({label}): exactly one row; rows={rows:?}"
        );
        let row = &rows[0];
        assert!(
            row.message_id.starts_with("msg_"),
            "C2 ({label}): store-backed id; rows={rows:?}"
        );
        assert_eq!(row.recipient, "w2", "C2 ({label}): resolved recipient");
        assert_eq!(row.sender, sender, "C2 ({label}): trusted sender identity");
        assert_eq!(
            row.owner_team_id.as_deref(),
            Some(TEAM),
            "C2 ({label}): canonical team scope"
        );
    }
    case.shutdown();
}

/// Invariant 3 — pre-persist refusals have zero DB side effects, and refusal
/// is reserved for resolution/identity errors, never availability.
#[test]
#[serial(env)]
fn c3_pre_persist_refusals_leave_zero_db_side_effects() {
    let case = SendPathCase::start("carc-c3");
    let leader_form = format!("{TEAM}/leader");
    let probes: Vec<(&str, Vec<&str>)> = vec![
        (
            "unknown-recipient",
            vec!["nosuchworker", "carc c3 unknown", "--team", TEAM],
        ),
        (
            "unresolvable-name",
            vec!["--to-name", "nosuchteam/agent", "carc c3 name"],
        ),
        (
            "unbound-leader",
            vec![leader_form.as_str(), "carc c3 leader", "--team", TEAM],
        ),
    ];
    for (label, args) in &probes {
        let mut full = vec!["send"];
        full.extend(args.iter().copied());
        full.extend_from_slice(&["--workspace", case.workspace_str(), "--json"]);
        let output = case.run_cli(&full);
        let value: Option<Value> = serde_json::from_slice(&output.stdout).ok();
        let accepted = output.status.success()
            && value
                .as_ref()
                .and_then(|v| v.get("ok"))
                .and_then(Value::as_bool)
                == Some(true);
        assert!(
            !accepted,
            "C3 ({label}): resolution/identity failures must refuse; got acceptance: {value:?}"
        );
    }
    for needle in ["carc c3 unknown", "carc c3 name", "carc c3 leader"] {
        assert!(
            case.db_rows(needle).is_empty(),
            "C3: refusals must leave zero DB side effects; needle={needle}"
        );
    }
    case.shutdown();
}

/// Invariant 4 — recovery advances the SAME row; no replacement rows.
#[test]
#[serial(env)]
fn c4_coordinator_recovery_advances_same_row_without_replacement() {
    let case = SendPathCase::start("carc-c4");
    case.kill_coordinator();

    // The worker-origin primitive creates the durable coordinator blocker
    // directly, without letting the CLI's loud ensure path recover the
    // daemon before the row is persisted.
    let blocked = team_agent::messaging::send_message(
        &case.workspace,
        &team_agent::messaging::MessageTarget::Single("w1".to_string()),
        "carc c4 probe",
        &team_agent::messaging::SendOptions {
            origin: team_agent::messaging::SendOrigin::Mcp,
            sender: team_agent::messaging::TrustedSender::from_runtime_identity(
                team_agent::model::ids::AgentId::new("w2"),
            ),
            team: Some(team_agent::model::ids::TeamKey::new(TEAM)),
            ..Default::default()
        },
    )
    .expect("C4 fixture: blocked worker send must persist an id");
    assert!(
        blocked.ok,
        "C4 fixture: worker send must be persisted: {blocked:?}"
    );
    let message_id = blocked
        .message_id
        .as_deref()
        .unwrap_or_else(|| {
            panic!("C4 fixture: blocked worker send must persist an id: {blocked:?}")
        })
        .to_string();
    assert_eq!(
        blocked.status,
        team_agent::messaging::DeliveryStatus::Blocked,
        "C4 fixture: first row must remain deferred while coordinator is dead: {blocked:?}"
    );
    let before_rows = case.db_rows("carc c4 probe");
    assert_eq!(before_rows.len(), 1, "C4: one deferred row before recovery");
    assert_eq!(
        before_rows[0].message_id, message_id,
        "C4: original id before recovery"
    );
    assert_eq!(
        before_rows[0].status.as_deref(),
        Some("queued_coordinator_unavailable"),
        "C4: first row must be parked by the coordinator blocker; rows={before_rows:?}"
    );
    assert_eq!(
        case.event_count("message.delivered", &message_id),
        0,
        "C4: deferred row cannot have a delivered event before recovery"
    );
    // Let the killed daemon finish releasing its runtime files before the
    // independent CLI process performs the recovery start.
    std::thread::sleep(std::time::Duration::from_millis(250));

    // Canonical recovery trigger: a mutating send lazily ensures the
    // coordinator. This is intentionally not `status`, which is read-only.
    let trigger = case.send_json(&["w2", "carc c4 recovery trigger", "--team", TEAM]);
    assert_eq!(
        trigger.get("coordinator_auto_restarted"),
        Some(&json!(true)),
        "C4: trigger must prove loud coordinator recovery: {trigger}"
    );
    assert_eq!(
        trigger.pointer("/coordinator/ok"),
        Some(&json!(true)),
        "C4: trigger must report successful coordinator start: {trigger}"
    );
    assert!(
        trigger
            .pointer("/coordinator/pid")
            .and_then(Value::as_u64)
            .is_some_and(|pid| pid > 0),
        "C4: trigger must expose the recovered coordinator pid: {trigger}"
    );

    // Status is an observation only; it is valid here because the mutating
    // trigger above already established the recovery boundary.
    let status_output = case.run_cli(&[
        "status",
        "--workspace",
        case.workspace_str(),
        "--team",
        TEAM,
        "--json",
        "--detail",
    ]);
    assert!(
        !status_output.stdout.is_empty(),
        "C4 fixture: status must expose coordinator health after the trigger"
    );
    let status = json_stdout(&status_output, "C4 status");
    assert_eq!(
        status.pointer("/coordinator/service_available"),
        Some(&json!(true)),
        "C4: coordinator health must be positive before row advancement: {status}"
    );

    let final_status = case.wait_status(
        &message_id,
        &["delivered", "submitted_pending_acceptance"],
        20,
    );
    assert!(
        final_status == "delivered" || final_status == "submitted_pending_acceptance",
        "C4: after coordinator recovery the SAME message_id must advance out of the blocker; \
         final={final_status}"
    );
    let rows = case.db_rows("carc c4 probe");
    assert_eq!(
        rows.len(),
        1,
        "C4: recovery must reuse the original row, never create a replacement; rows={rows:?}"
    );
    assert_eq!(rows[0].message_id, message_id, "C4: same id end to end");
    assert_eq!(
        case.event_count("message.delivered", &message_id),
        1,
        "C4: same row must produce exactly one delivered event"
    );
    case.shutdown();
}

/// Invariant 5 — fanout rows are independent: one recipient's blocker leaves
/// the other recipient's row alone.
#[test]
#[serial(env)]
fn c5_fanout_rows_are_independent_under_partial_blockers() {
    let mut case = SendPathCase::start("carc-c5");
    // Canonical partial fault: kill w1's pane (w2 stays live, session intact).
    let pane_list = case
        .tmux(&[
            "list-panes",
            "-a",
            "-F",
            "#{window_name}__TA_FIELD__#{pane_id}",
        ])
        .expect("tmux list-panes");
    let w1_pane = String::from_utf8_lossy(&pane_list.stdout)
        .lines()
        .find_map(|line| {
            let mut cols = line.split("__TA_FIELD__");
            (cols.next()? == "w1").then(|| cols.next().map(ToString::to_string))?
        })
        .expect("w1 pane present");
    case.kill_pane =
        SendPathCase::command_snapshot(case.tmux(&["kill-pane", "-t", w1_pane.as_str()]));
    case.panes_after_kill = case.pane_tuple_snapshot();

    let value = case.send_json(&["w1,w2", "carc c5 fanout", "--team", TEAM]);
    assert_eq!(
        value.get("ok"),
        Some(&json!(true)),
        "C5: fanout with one blocked recipient must still persist both intents: {value}"
    );
    let rows = case.db_rows("carc c5 fanout");
    assert_eq!(rows.len(), 2, "C5: one row per recipient; rows={rows:?}");
    let w2_row = rows
        .iter()
        .find(|row| row.recipient == "w2")
        .expect("w2 row present");
    let w2_status = case.wait_status(&w2_row.message_id, &["delivered"], 15);
    assert_eq!(
        w2_status, "delivered",
        "C5: the live recipient must deliver despite the sibling blocker; diagnostic={}",
        case.persist_c5_failure_packet(json!({
            "stage": "w2_delivered",
            "observed_status": w2_status,
            "wanted": "delivered",
            "send": value,
            "w1_pane": w1_pane,
            "w2_message_id": w2_row.message_id,
            "w1_message_id": rows.iter().find(|row| row.recipient == "w1").map(|row| row.message_id.clone()),
        }))
    );
    let w1_row = rows
        .iter()
        .find(|row| row.recipient == "w1")
        .expect("w1 row present");
    let w1_status = case.wait_status(&w1_row.message_id, &["queued_pane_missing"], 15);
    assert_eq!(
        w1_status, "queued_pane_missing",
        "C5: the blocked recipient parks as its own durable blocker, not erased; diagnostic={}",
        case.persist_c5_failure_packet(json!({
            "stage": "w1_queued_pane_missing",
            "observed_status": w1_status,
            "wanted": "queued_pane_missing",
            "send": value,
            "w1_pane": w1_pane,
            "w1_message_id": w1_row.message_id,
            "w2_message_id": w2_row.message_id,
        }))
    );
    case.shutdown();
}
