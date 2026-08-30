//! E6 RED e2e contract: real CLI offline mailbox for `send --to-name <team>/leader`.
//!
//! References:
//! - plan `.team/artifacts/next-version-staged-plan.md` §4 E6.
//! - design `.team/artifacts/offline-mailbox-toname-design.md` §§3, 6, 8, 11.
//! - real-machine escape evidence:
//!   `.team/evidence/0.5.9-subscription-gate-20260707T143241Z-4645/`.
//!
//! Contract: with a real tmux workspace, live fake worker, running coordinator,
//! and no attached leader, a third-party `send --to-name <ws>::<key>/leader`
//! queues one target `team.db` row as `queued_until_leader_attach`. Later
//! `attach-leader` replays that same message id exactly once to the new leader
//! pane.
//!
//! ---
//! purpose: E6 real-CLI mailbox contract with an isolated tmux fixture
//! contract:
//!   provides:
//!     - hermetic workspace, HOME, and exact tmux endpoint ownership
//!   depends:
//!     - tests/support/hermetic.rs
//! boundary:
//!   - never uses ambient serial-test locks or the default tmux server
//! arch:
//!   allowed_dependencies:
//!     - rusqlite
//!     - serde_json
//!     - serial_test
//!     - std
//!   unresolved_disposition:
//!     - rule: unanchored_relative_path
//!       scope: task-scoped test-helper calls only
//!       disposition: bounded_unknown_no_production_edge
//!       rationale: required row/event/receipt probes remain test-local.
//! maturity: wired
//! ---

#![cfg(unix)]
#![allow(clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use rusqlite::{Connection, OptionalExtension};
use serde_json::{json, Value};
use serial_test::serial;

#[path = "support/hermetic.rs"]
mod hermetic_guard;
use hermetic_guard::{short_tmux_socket, HermeticTestEnv};

static COUNTER: AtomicU64 = AtomicU64::new(0);

#[test]
#[serial(env)]
fn e6_real_cli_live_team_unattached_leader_queues_then_attach_replays_once() {
    let case = E6Case::new("real-cli-mailbox");
    case.write_fake_team("twitter-autopub", "fake-worker");

    let quick_start = case.run_cli(
        case.target_workspace(),
        vec![
            "quick-start".into(),
            case.team_dir_arg(),
            "--workspace".into(),
            case.target_workspace_arg(),
            "--team".into(),
            case.team_key.clone(),
            "--no-display".into(),
            "--yes".into(),
            "--json".into(),
        ],
    );
    let quick_json = json_output(&quick_start, "quick-start fake team");
    assert_eq!(
        quick_json
            .pointer("/readiness/all_workers_spawned")
            .and_then(Value::as_bool),
        Some(true),
        "E6 e2e RED setup: fake worker must be spawned so target team is live even though quick-start may exit nonzero for leader_receiver_unbound; code={:?} output={quick_json}",
        quick_start.status.code()
    );

    let status = case.run_cli(
        case.target_workspace(),
        vec![
            "status".into(),
            "--workspace".into(),
            case.target_workspace_arg(),
            "--team".into(),
            case.team_key.clone(),
            "--detail".into(),
            "--json".into(),
        ],
    );
    let status_json = json_output(&status, "status after quick-start");
    assert_eq!(
        status_json
            .pointer("/coordinator/ok")
            .and_then(Value::as_bool),
        Some(true),
        "E6 e2e RED setup: coordinator must be running for the target team; status={status_json}"
    );
    assert_eq!(
        status_json
            .get("tmux_session_present")
            .and_then(Value::as_bool),
        Some(true),
        "E6 e2e RED setup: target tmux session must be live; status={status_json}"
    );
    assert!(
        status_json.get("leader_receiver").is_none()
            || status_json.get("leader_receiver").is_some_and(|v| {
                v.as_object()
                    .map(|object| object.is_empty())
                    .unwrap_or(false)
            }),
        "E6 e2e RED setup: leader must never have been attached before mailbox send; status={status_json}"
    );

    let token = unique_token("E6_REAL_CLI_MAILBOX");
    let to_name = format!(
        "{}::{}/leader",
        case.target_workspace().display(),
        case.team_key
    );
    let send = case.run_cli_as(
        case.sender_workspace(),
        vec![
            "send".into(),
            "--workspace".into(),
            case.sender_workspace_arg(),
            "--to-name".into(),
            to_name,
            token.clone(),
            "--json".into(),
        ],
        "third-party",
    );
    let body = json_output(&send, "third-party send --to-name unattached leader");
    let rows_after_send = message_rows(case.target_workspace(), &token);

    assert!(
        send.status.success() && body.get("ok").and_then(Value::as_bool) == Some(true),
        "E6 e2e RED: live target team with unattached leader must queue mailbox, not hard fail. \
         Expected ok=true/status=queued_until_leader_attach/message_id; got code={:?} output={body}; \
         rows_after_send={rows_after_send:?}; stderr={}",
        send.status.code(),
        String::from_utf8_lossy(&send.stderr)
    );
    assert_eq!(
        body.get("status").and_then(Value::as_str),
        Some("deferred"),
        "E6 e2e RED: unattached leader send must return presenter status=deferred; output={body}"
    );
    assert_eq!(
        body.get("deferred_reason").and_then(Value::as_str),
        Some("never_attached"),
        "E6 e2e RED: unattached leader send must explain deferred_reason=never_attached; output={body}"
    );
    assert_eq!(
        body.get("message_status").and_then(Value::as_str),
        Some("queued_until_leader_attach"),
        "E6 e2e RED: message_status must honestly stay queued until attach; output={body}"
    );
    assert_eq!(
        body.get("channel").and_then(Value::as_str),
        Some("leader_mailbox"),
        "E6 e2e RED: queued mailbox must identify channel=leader_mailbox; output={body}"
    );
    assert_eq!(
        body.get("delivered").and_then(Value::as_bool),
        Some(false),
        "E6 e2e RED: mailbox queue is not physical delivery; delivered must be false; output={body}"
    );
    let message_id = body
        .get("message_id")
        .and_then(Value::as_str)
        .unwrap_or_else(|| {
            panic!("E6 e2e RED: queued mailbox response must include message_id; output={body}")
        })
        .to_string();

    assert_eq!(
        rows_after_send.len(),
        1,
        "E6 e2e RED: queued mailbox must create exactly one target team.db row; rows={rows_after_send:?}; output={body}"
    );
    let row = &rows_after_send[0];
    assert_eq!(
        row.message_id, message_id,
        "E6 e2e RED: DB row message_id must match CLI response"
    );
    assert_eq!(
        row.owner_team_id.as_deref(),
        Some(case.team_key.as_str()),
        "E6 e2e RED: mailbox row owner_team_id must be target runtime key"
    );
    assert_eq!(
        row.recipient.as_deref(),
        Some("leader"),
        "E6 e2e RED: mailbox row recipient must be leader"
    );
    assert_eq!(
        row.status.as_deref(),
        Some("queued_until_leader_attach"),
        "E6 e2e RED: mailbox row status must be queued_until_leader_attach before attach"
    );
    assert_eq!(
        row.delivery_attempts, 0,
        "E6 e2e RED: queued mailbox must not be claimed/delivered before leader attach"
    );
    assert_eq!(
        row.error, None,
        "E6 e2e RED: queued mailbox must not carry a failure before replay"
    );
    assert_eq!(
        row.delivered_at, None,
        "E6 e2e RED: queued mailbox must not carry a delivery timestamp before replay"
    );
    assert_event_for_message(
        case.target_workspace(),
        "leader_mailbox.queued_until_attach",
        &message_id,
    );

    let state = runtime_state(case.target_workspace());
    let session_name = state
        .pointer(&format!("/teams/{}/session_name", case.team_key))
        .and_then(Value::as_str)
        .expect("session name in state")
        .to_string();
    let tmux_socket = state
        .pointer(&format!("/teams/{}/tmux_socket", case.team_key))
        .or_else(|| state.pointer(&format!("/teams/{}/tmux_endpoint", case.team_key)))
        .and_then(Value::as_str)
        .expect("tmux socket in state")
        .to_string();
    let pane = case.start_leader_pane(&tmux_socket, &session_name);

    let attach = case.run_cli(
        case.target_workspace(),
        vec![
            "attach-leader".into(),
            "--workspace".into(),
            case.target_workspace_arg(),
            "--team".into(),
            case.team_key.clone(),
            "--pane".into(),
            pane.clone(),
            "--provider".into(),
            "fake".into(),
            "--confirm".into(),
            "--json".into(),
        ],
    );
    let attach_json = json_output(&attach, "attach-leader after mailbox queue");
    assert!(
        attach.status.success() && attach_json.get("ok").and_then(Value::as_bool) == Some(true),
        "E6 e2e RED setup: attach-leader must succeed so queued mailbox can replay; code={:?} output={attach_json} stderr={}",
        attach.status.code(),
        String::from_utf8_lossy(&attach.stderr)
    );

    let attached_state = runtime_state(case.target_workspace());
    let receiver = attached_state
        .pointer(&format!("/teams/{}/leader_receiver", case.team_key))
        .expect("attached leader receiver in state");
    assert_eq!(
        receiver.get("status").and_then(Value::as_str),
        Some("attached")
    );
    assert_eq!(
        receiver.get("pane_id").and_then(Value::as_str),
        Some(pane.as_str())
    );
    assert_eq!(
        receiver.get("session_name").and_then(Value::as_str),
        Some(session_name.as_str())
    );
    assert_eq!(
        receiver.get("tmux_socket").and_then(Value::as_str),
        Some(tmux_socket.as_str())
    );
    assert_eq!(
        receiver.get("window_name").and_then(Value::as_str),
        Some("leader")
    );
    assert_eq!(
        receiver.get("window_index").and_then(Value::as_str),
        Some("1")
    );
    assert!(
        receiver
            .get("pane_tty")
            .and_then(Value::as_str)
            .is_some_and(|tty| !tty.is_empty()),
        "E6 e2e RED: attached target must persist pane tty; receiver={receiver}"
    );

    // Injection-case revision (MUST-10 boundary): attach must wake the mailbox
    // and physically inject, but a physical submit is not a provider receipt —
    // the SAME row parks as injected_awaiting_receipt, never jumps straight
    // to delivered on transport success alone.
    let accepted = wait_for_message_status(
        case.target_workspace(),
        &message_id,
        "injected_awaiting_receipt",
        &case.team_key,
        &tmux_socket,
        &pane,
    );
    assert!(
        accepted,
        "E6 e2e RED: attach-leader must requeue the same message_id={message_id} and park it as \
         injected_awaiting_receipt after the physical inject; rows={:?}",
        message_rows(case.target_workspace(), &token)
    );
    let pane_text = wait_for_pane_token(&tmux_socket, &pane, &token);
    let token_count = pane_text.matches(&token).count();
    assert_eq!(
        token_count, 1,
        "E6 e2e RED: attach replay must inject queued mailbox token exactly once; pane={pane} token={token} count={token_count} capture={pane_text:?}"
    );
    let rows_after_attach = message_rows(case.target_workspace(), &token);
    assert_eq!(
        rows_after_attach.len(),
        1,
        "E6 e2e RED: attach replay must reuse the same row, not create duplicates; rows={rows_after_attach:?}"
    );
    assert_eq!(
        rows_after_attach[0].message_id, message_id,
        "E6 e2e RED: attach replay must preserve the original queued message_id"
    );
    let row = &rows_after_attach[0];
    assert_eq!(
        row.delivery_attempts, 1,
        "E6 e2e RED: attach replay must have exactly one winning claim; row={row:?}"
    );
    assert_eq!(
        row.error.as_deref(),
        Some("leader_receipt_source_unavailable"),
        "E6 e2e RED: receipt-source failure must be persisted on the same row; row={row:?}"
    );
    assert_eq!(
        row.delivered_at, None,
        "E6 e2e RED: physical injection must not infer a provider receipt; row={row:?}"
    );
    let receipt = delivery_token(case.target_workspace(), &message_id)
        .expect("delivery token for physically injected message");
    assert_eq!(receipt.unique_token, message_id);
    assert!(
        !receipt.injected_at.is_empty(),
        "delivery token must persist inject time"
    );
    assert_eq!(
        receipt.consumed_at, None,
        "source-unavailable receipt cannot be consumed"
    );
    assert_eq!(
        receipt.failed_at, None,
        "source-unavailable is not an inject failure"
    );
    assert_eq!(
        receipt.failure_reason, None,
        "source-unavailable has its own typed boundary"
    );
    assert_event_for_message(
        case.target_workspace(),
        "leader_receiver.receipt_source_unavailable",
        &message_id,
    );
}

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_team-agent")
}

fn unique_token(prefix: &str) -> String {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{prefix}_{}_{}", std::process::id(), n)
}

fn json_output(output: &Output, label: &str) -> Value {
    let stdout = String::from_utf8(output.stdout.clone()).expect("stdout utf8");
    let trimmed = stdout.trim();
    assert!(
        !trimmed.is_empty(),
        "{label}: expected JSON stdout; code={:?} stderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    let json_start = trimmed.find('{').unwrap_or(0);
    serde_json::from_str(&trimmed[json_start..]).unwrap_or_else(|error| {
        panic!(
            "{label}: stdout must contain JSON object: {error}; stdout={stdout:?}; stderr={}",
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

fn runtime_state(workspace: &Path) -> Value {
    let path = workspace.join(".team/runtime/state.json");
    serde_json::from_str(&std::fs::read_to_string(&path).expect("read state.json"))
        .expect("state json")
}

#[derive(Debug)]
struct MessageRow {
    message_id: String,
    owner_team_id: Option<String>,
    recipient: Option<String>,
    status: Option<String>,
    delivery_attempts: i64,
    delivered_at: Option<String>,
    error: Option<String>,
}

#[derive(Debug)]
struct DeliveryToken {
    unique_token: String,
    injected_at: String,
    consumed_at: Option<String>,
    failed_at: Option<String>,
    failure_reason: Option<String>,
}

fn message_rows(workspace: &Path, token: &str) -> Vec<MessageRow> {
    let db = workspace.join(".team/runtime/team.db");
    if !db.exists() {
        return Vec::new();
    }
    let conn = Connection::open(db).expect("open team.db");
    let mut stmt = conn
        .prepare(
            "select message_id, owner_team_id, recipient, status, delivery_attempts, delivered_at, error \
             from messages where content like ?1 order by created_at",
        )
        .expect("prepare message query");
    stmt.query_map([format!("%{token}%")], |row| {
        Ok(MessageRow {
            message_id: row.get(0)?,
            owner_team_id: row.get(1)?,
            recipient: row.get(2)?,
            status: row.get(3)?,
            delivery_attempts: row.get(4)?,
            delivered_at: row.get(5)?,
            error: row.get(6)?,
        })
    })
    .expect("query messages")
    .map(|row| row.expect("message row"))
    .collect()
}

fn delivery_token(workspace: &Path, message_id: &str) -> Option<DeliveryToken> {
    let db = workspace.join(".team/runtime/team.db");
    let conn = Connection::open(db).expect("open team.db");
    conn.query_row(
        "select unique_token, injected_at, consumed_at, failed_at, failure_reason \
         from delivery_tokens where message_id = ?1",
        [message_id],
        |row| {
            Ok(DeliveryToken {
                unique_token: row.get(0)?,
                injected_at: row.get(1)?,
                consumed_at: row.get(2)?,
                failed_at: row.get(3)?,
                failure_reason: row.get(4)?,
            })
        },
    )
    .optional()
    .expect("query delivery token")
}

fn assert_event_for_message(workspace: &Path, event_name: &str, message_id: &str) {
    let path = workspace.join(".team/logs/events.jsonl");
    let matching = std::fs::read_to_string(&path)
        .expect("read event log")
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .find(|event| {
            event.get("event").and_then(Value::as_str) == Some(event_name)
                && event.get("message_id").and_then(Value::as_str) == Some(message_id)
        });
    assert!(
        matching.is_some(),
        "E6 e2e RED: expected durable {event_name} event bound to message_id={message_id}"
    );
}

fn query_message_status(workspace: &Path, message_id: &str) -> Result<Option<String>, String> {
    let db = workspace.join(".team/runtime/team.db");
    if !db.exists() {
        return Ok(None);
    }
    let conn = Connection::open(&db).map_err(|error| format!("open {}: {error}", db.display()))?;
    conn.query_row(
        "select status from messages where message_id = ?1",
        [message_id],
        |row| row.get::<_, String>(0),
    )
    .optional()
    .map_err(|error| format!("query status for {message_id}: {error}"))
}

fn wait_for_message_status(
    workspace: &Path,
    message_id: &str,
    status: &str,
    team_key: &str,
    tmux_socket: &str,
    pane: &str,
) -> bool {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        match query_message_status(workspace, message_id) {
            Ok(Some(current)) if current == status => return true,
            Ok(Some(_)) | Ok(None) => {}
            Err(error) => {
                panic!("E6 replay status query failed before assertion: {error}");
            }
        }
        thread::sleep(Duration::from_millis(250));
    }
    emit_replay_timeout_receipt(workspace, message_id, team_key, tmux_socket, pane);
    panic!("E6 replay did not reach expected status {status:?}");
}

fn emit_replay_timeout_receipt(
    workspace: &Path,
    message_id: &str,
    team_key: &str,
    tmux_socket: &str,
    pane: &str,
) {
    let row = message_rows_by_id(workspace, message_id);
    let token = delivery_token(workspace, message_id).map(|token| {
        json!({
            "unique_token": token.unique_token,
            "injected_at": token.injected_at,
            "consumed_at": token.consumed_at,
            "failed_at": token.failed_at,
            "failure_reason": token.failure_reason,
        })
    });
    let runtime = workspace.join(".team/runtime");
    let events = std::fs::read_to_string(workspace.join(".team/logs/events.jsonl"))
        .unwrap_or_default()
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|event| event.get("message_id").and_then(Value::as_str) == Some(message_id))
        .collect::<Vec<_>>();

    eprintln!(
        "E6_DIAGNOSTIC_TMUX_CALL socket={tmux_socket} pane={pane} format=session,window,pane,pid,tty,dead,command"
    );
    let tmux = Command::new("tmux")
        .args([
            "-S",
            tmux_socket,
            "display-message",
            "-p",
            "-t",
            pane,
            "#{session_name}\t#{window_name}\t#{pane_id}\t#{pane_pid}\t#{pane_tty}\t#{pane_dead}\t#{pane_current_command}",
        ])
        .output()
        .map(|output| {
            json!({
                "status": output.status.code(),
                "stdout": String::from_utf8_lossy(&output.stdout),
                "stderr": String::from_utf8_lossy(&output.stderr),
            })
        })
        .unwrap_or_else(|error| json!({"spawn_error": error.to_string()}));

    eprintln!(
        "E6_REPLAY_TIMEOUT_RECEIPT {}",
        json!({
            "workspace": workspace,
            "team_key": team_key,
            "message_id": message_id,
            "message_row": row,
            "delivery_token": token,
            "leader_receiver": runtime_state(workspace)
                .pointer(&format!("/teams/{team_key}/leader_receiver"))
                .cloned(),
            "coordinator": read_json_or_error(&runtime.join("coordinator.json")),
            "coordinator_tick": read_json_or_error(&runtime.join("coordinator_tick.json")),
            "coordinator_pid": std::fs::read_to_string(runtime.join("coordinator.pid"))
                .map(|value| value.trim().to_string())
                .unwrap_or_default(),
            "message_events": events,
            "tmux": tmux,
        })
    );
}

fn message_rows_by_id(workspace: &Path, message_id: &str) -> Value {
    let db = workspace.join(".team/runtime/team.db");
    let conn = match Connection::open(&db) {
        Ok(conn) => conn,
        Err(error) => return json!({"open_error": error.to_string(), "path": db}),
    };
    conn.query_row(
        "select message_id, owner_team_id, recipient, status, delivery_attempts, delivered_at, error \
         from messages where message_id = ?1",
        [message_id],
        |row| {
            Ok(json!({
                "message_id": row.get::<_, String>(0)?,
                "owner_team_id": row.get::<_, Option<String>>(1)?,
                "recipient": row.get::<_, Option<String>>(2)?,
                "status": row.get::<_, Option<String>>(3)?,
                "delivery_attempts": row.get::<_, i64>(4)?,
                "delivered_at": row.get::<_, Option<String>>(5)?,
                "error": row.get::<_, Option<String>>(6)?,
            }))
        },
    )
    .optional()
    .map(|value| json!({"row": value}))
    .unwrap_or_else(|error| json!({"query_error": error.to_string()}))
}

fn read_json_or_error(path: &Path) -> Value {
    std::fs::read_to_string(path)
        .map_err(|error| error.to_string())
        .and_then(|text| serde_json::from_str(&text).map_err(|error| error.to_string()))
        .unwrap_or_else(|error| json!({"path": path, "read_error": error}))
}

fn wait_for_pane_token(tmux_socket: &str, pane: &str, token: &str) -> String {
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut last = String::new();
    while Instant::now() < deadline {
        last = capture_pane(tmux_socket, pane);
        if last.contains(token) {
            return last;
        }
        thread::sleep(Duration::from_millis(250));
    }
    last
}

fn capture_pane(tmux_socket: &str, pane: &str) -> String {
    let output = Command::new("tmux")
        .args(["-S", tmux_socket, "capture-pane", "-p", "-t", pane])
        .output()
        .unwrap_or_else(|error| {
            panic!(
                "E6 apparatus fixture child/probe-exit: tmux capture-pane could not spawn; socket={tmux_socket} pane={pane} error={error}"
            )
        });
    assert_fixture_command_success(
        "tmux capture-pane",
        &output,
        &format!("socket={tmux_socket} pane={pane}"),
    );
    String::from_utf8_lossy(&output.stdout).to_string()
}

fn assert_fixture_command_success(label: &str, output: &Output, context: &str) {
    assert!(
        output.status.success(),
        "E6 apparatus fixture child/probe-exit: {label} exited unsuccessfully; {context} code={:?} stderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
}

struct E6Case {
    env: HermeticTestEnv,
    home: PathBuf,
    target_workspace: PathBuf,
    sender_workspace: PathBuf,
    team_dir: PathBuf,
    team_key: String,
    tmux_socket: PathBuf,
}

impl E6Case {
    fn new(tag: &str) -> Self {
        let env = HermeticTestEnv::enter(&format!("e6-{tag}"));
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let root = env
            .root()
            .join(format!("ta-e6-{tag}-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&root).expect("create e6 test root");
        let home = root.join("home");
        let target_workspace = root.join("target-ws");
        let sender_workspace = root.join("sender-ws");
        let team_dir = root.join("team");
        for dir in [&home, &target_workspace, &sender_workspace, &team_dir] {
            std::fs::create_dir_all(dir).expect("create e6 test dir");
        }
        Self {
            home,
            target_workspace: std::fs::canonicalize(target_workspace)
                .expect("canonical target workspace"),
            sender_workspace: std::fs::canonicalize(sender_workspace)
                .expect("canonical sender workspace"),
            team_dir: std::fs::canonicalize(team_dir).expect("canonical team dir"),
            // 0.5.43 debt-sweep (§6.1): E6 team_key must include
            // pid+counter so parallel runs / host-fixture cohabitation
            // don't collide on a fixed key like "mail059". Session
            // names derived from team_key inherit the uniqueness.
            team_key: format!("mail059-{}-{n}", std::process::id()),
            env,
            tmux_socket: short_tmux_socket(&format!("e6-{tag}")),
        }
    }

    fn target_workspace(&self) -> &Path {
        &self.target_workspace
    }

    fn sender_workspace(&self) -> &Path {
        &self.sender_workspace
    }

    fn target_workspace_arg(&self) -> String {
        self.target_workspace.to_string_lossy().to_string()
    }

    fn sender_workspace_arg(&self) -> String {
        self.sender_workspace.to_string_lossy().to_string()
    }

    fn team_dir_arg(&self) -> String {
        self.team_dir.to_string_lossy().to_string()
    }

    fn write_fake_team(&self, spec_name: &str, agent_id: &str) {
        let agents_dir = self.team_dir.join("agents");
        std::fs::create_dir_all(&agents_dir).expect("create agents dir");
        std::fs::write(
            self.team_dir.join("TEAM.md"),
            format!(
                "---\nname: {spec_name}\nobjective: E6 real CLI mailbox contract.\nprovider: fake\ndisplay_backend: none\n---\n\nFake-provider E6 mailbox contract team.\n"
            ),
        )
        .expect("write TEAM.md");
        std::fs::write(
            agents_dir.join(format!("{agent_id}.md")),
            format!(
                "---\nname: {agent_id}\nrole: Fake E6 Worker\nprovider: fake\nmodel: fake\nauth_mode: subscription\ndangerously_skip_permissions: false\ntools:\n  - mcp_team\n---\n\nFake worker keeping the team alive.\n"
            ),
        )
        .expect("write fake worker");
    }

    fn run_cli(&self, cwd: &Path, args: Vec<String>) -> Output {
        self.run_cli_with_identity(cwd, args, None)
    }

    fn run_cli_as(&self, cwd: &Path, args: Vec<String>, identity: &str) -> Output {
        self.run_cli_with_identity(cwd, args, Some(identity))
    }

    fn run_cli_with_identity(
        &self,
        cwd: &Path,
        args: Vec<String>,
        identity: Option<&str>,
    ) -> Output {
        let mut command = Command::new(bin());
        command
            .args(args)
            .current_dir(cwd)
            .env("HOME", &self.home)
            .env("TMUX", format!("{},12345,0", self.tmux_socket.display()));
        for key in [
            "TEAM_AGENT_LEADER_PANE_ID",
            "TEAM_AGENT_LEADER_SESSION_UUID",
            "TEAM_AGENT_LEADER_PROVIDER",
            "TEAM_AGENT_ID",
            "TEAM_AGENT_AGENT_ID",
            "TEAM_AGENT_TEAM_ID",
            "TEAM_AGENT_WORKSPACE",
            "TEAM_AGENT_OWNER_TEAM_ID",
            "TEAM_AGENT_AUTH_MODE",
            "TEAM_AGENT_MCP_AUTO_APPROVE",
            "TEAM_AGENT_MCP_AUTO_APPROVE_SOURCE",
            "TEAM_AGENT_LEADER_BYPASS",
            "TEAM_AGENT_LEADER_BYPASS_FLAG",
            "TEAM_AGENT_LEADER_BYPASS_PROVIDER",
            "TEAM_AGENT_LEADER_BYPASS_SOURCE",
            "TMUX_PANE",
        ] {
            command.env_remove(key);
        }
        if let Some(identity) = identity {
            command.env("TEAM_AGENT_ID", identity);
        }
        command.output().expect("run team-agent")
    }

    fn start_leader_pane(&self, tmux_socket: &str, session_name: &str) -> String {
        let output = Command::new("tmux")
            .args([
                "-S",
                tmux_socket,
                "new-window",
                "-d",
                "-P",
                "-F",
                "#{pane_id}",
                "-t",
                session_name,
                "-n",
                "leader",
                "-c",
                &self.target_workspace_arg(),
                "/bin/cat",
            ])
            .output()
            .unwrap_or_else(|error| panic!("E6 apparatus fixture child/probe-exit: tmux new-window could not spawn; error={error}"));
        assert_fixture_command_success("tmux new-window leader", &output, "leader pane creation");
        self.register_pane_pid(&String::from_utf8_lossy(&output.stdout).trim());
        self.env.register_owned_tmux_socket(&self.tmux_socket);
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    fn register_pane_pid(&self, pane: &str) {
        let output = Command::new("tmux")
            .args([
                "-S",
                self.tmux_socket.to_str().expect("tmux socket utf8"),
                "display-message",
                "-p",
                "-t",
                pane,
                "#{pane_pid}",
            ])
            .output()
            .unwrap_or_else(|error| panic!("E6 apparatus fixture child/probe-exit: tmux display-message could not spawn; pane={pane} error={error}"));
        assert_fixture_command_success(
            "tmux display-message pane pid",
            &output,
            &format!("pane={pane}"),
        );
        if let Ok(pid) = String::from_utf8_lossy(&output.stdout).trim().parse() {
            self.env.register_owned_pid(pid);
        }
    }
}

impl Drop for E6Case {
    fn drop(&mut self) {
        // 0.5.43 debt-sweep (§6.1): try `team-agent shutdown` first,
        // then fall back to exact `tmux -S <socket> kill-server` on
        // each workspace's persisted tmux_endpoint. Never scans host
        // sockets — the fallback only kills the endpoint recorded in
        // the state file THIS fixture wrote.
        for workspace in [&self.target_workspace, &self.sender_workspace] {
            register_persisted_pid(&self.env, workspace);
            let shutdown = Command::new(bin())
                .args([
                    "shutdown",
                    "--workspace",
                    &workspace.to_string_lossy(),
                    "--team",
                    &self.team_key,
                    "--json",
                ])
                .env("HOME", &self.home)
                .env("TMUX", format!("{},12345,0", self.tmux_socket.display()))
                .output();
            if !matches!(&shutdown, Ok(output) if output.status.success()) {
                let _ = Command::new("tmux")
                    .args([
                        "-S",
                        self.tmux_socket.to_str().expect("tmux socket utf8"),
                        "kill-server",
                    ])
                    .output();
            }
        }
    }
}

fn register_persisted_pid(env: &HermeticTestEnv, workspace: &Path) {
    let state_path = workspace.join(".team/runtime/state.json");
    let Ok(_) = std::fs::read_to_string(&state_path) else {
        return;
    };
    let pid_path = workspace.join(".team/runtime/coordinator.pid");
    if let Some(pid) = std::fs::read_to_string(pid_path)
        .ok()
        .and_then(|pid| pid.trim().parse().ok())
    {
        env.register_owned_pid(pid);
    }
}
