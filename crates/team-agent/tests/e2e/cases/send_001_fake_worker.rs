//! E2E-SEND-001 Send To Fake Worker Delivers Token, Reports, And Collects.
//!
//! Architecture: T4 §4 delivery FSM, T6 §1 L6 message invariants, T1 §6 team.db.
//!
//! Black-box invariants:
//! - `ok == true` in JSON
//! - `message_id` is generated as a correlation id
//! - `target == "a"`, `sender == "leader"`
//! - the message row records recipient `a`, status `delivered`, and delivered_at
//! - the fake worker receives the message and reports a result
//! - a real stdio MCP report_result is returned by CLI collect.
//!
//! ---
//! purpose: Prove fake-worker delivery with a durable timeout evidence path
//! contract:
//!   provides:
//!     - name: send_001_delivers_to_fake_worker
//!       what: Binds message row, worker result, and collect output to one message id
//!   depends:
//!     - crate::framework::wait_for_delivery_or_panic
//!     - messaging rows/events and fake-worker runtime
//! boundary:
//!   - Test-only E2E evidence; no delivery product edits
//! maturity: wired
//! ---

use crate::framework::*;
use rusqlite::Connection;
use serde_json::Value;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

#[test]
fn send_001_delivers_to_fake_worker() {
    let team_id = "send001";
    let ws = TestWorkspace::new(team_id).with_fake_spec(&["a"]);
    let qs = quick_start_fake(&ws, team_id);
    assert!(
        quick_start_workers_available(&qs),
        "quick-start did not launch: {}",
        qs.stdout
    );
    let socket = ws
        .read_state()
        .get("tmux_socket")
        .and_then(Value::as_str)
        .map(std::path::PathBuf::from)
        .expect("quick-start must record its exact tmux socket");
    ws.register_owned_tmux_socket(&socket);

    let canary = format!("send001-worker-canary-{}", std::process::id());
    let out = run_ta(
        &ws,
        &[
            "send",
            "a",
            &canary,
            "--workspace",
            ws.path().to_str().unwrap(),
            "--json",
        ],
    );

    assert!(
        out.is_success(),
        "send exit {}; stdout={} stderr={}",
        out.exit_code,
        out.stdout,
        out.stderr
    );
    let j = out.json();

    assert_json_field_eq_bool(&j, "/ok", true);
    let message_id = j
        .pointer("/message_id")
        .and_then(|v| v.as_str())
        .expect("send must return message_id");
    let fake_worker_summary = format!("Fake worker handled message {message_id}");
    assert_json_field_eq_str(&j, "/target", "a");
    assert_json_field_eq_str(&j, "/sender", "leader");

    wait_for_delivery_or_panic(
        &ws,
        message_id,
        "a",
        "message row delivered to worker a",
        || {
            message_truth(ws.path(), message_id).is_some_and(|truth| {
                truth.recipient == "a"
                    && truth.status == "delivered"
                    && truth.delivered_at.is_some()
            })
        },
        Duration::from_secs(10),
    );
    wait_for_delivery_or_panic(
        &ws,
        message_id,
        "a",
        "fake worker received message and reported result",
        || {
            result_truth_for_message(ws.path(), message_id).is_some_and(|truth| {
                truth.task_id == message_id && truth.summary == fake_worker_summary
            })
        },
        Duration::from_secs(10),
    );
    let fake_worker_result = result_truth_for_message(ws.path(), message_id)
        .expect("spawned fake worker result must remain queryable before collect");
    assert_ne!(
        fake_worker_result.task_id, "manual",
        "fake worker must preserve production message-scope attribution"
    );

    let owner_team_id = ws.read_state()["active_team_key"]
        .as_str()
        .expect("active_team_key")
        .to_string();
    let report_summary = format!("worker a MCP received {message_id}: {canary}");
    run_worker_mcp_report_result(ws.path(), "a", &owner_team_id, &report_summary);

    let conn = Connection::open(ws.path().join(".team/runtime/team.db"))
        .expect("open durable result store");
    let stored_status: String = conn
        .query_row(
            "select status from results where result_id = ?1",
            [&fake_worker_result.result_id],
            |row| row.get(0),
        )
        .expect("spawned fake worker result remains durable");
    assert_eq!(
        stored_status, "collected",
        "report_result auto-finalization must close the spawned fake worker result"
    );
    let supplemental_count: i64 = conn
        .query_row(
            "select count(*) from results where envelope like ?1",
            [format!("%{report_summary}%")],
            |row| row.get(0),
        )
        .expect("count explicit MCP result");
    assert_eq!(
        supplemental_count, 1,
        "explicit stdio MCP report must remain a durable auto-finalized result"
    );

    // cleanup
    let _ = run_ta(
        &ws,
        &[
            "shutdown",
            "--workspace",
            ws.path().to_str().unwrap(),
            "--keep-logs",
            "--json",
        ],
    );
}

struct MessageTruth {
    recipient: String,
    status: String,
    delivered_at: Option<String>,
}

fn message_truth(workspace: &Path, message_id: &str) -> Option<MessageTruth> {
    let conn = Connection::open(workspace.join(".team/runtime/team.db")).ok()?;
    conn.query_row(
        "select recipient, status, delivered_at from messages where message_id = ?1",
        [message_id],
        |row| {
            Ok(MessageTruth {
                recipient: row.get(0)?,
                status: row.get(1)?,
                delivered_at: row.get(2)?,
            })
        },
    )
    .ok()
}

struct ResultTruth {
    result_id: String,
    task_id: String,
    summary: String,
}

fn result_truth_for_message(workspace: &Path, message_id: &str) -> Option<ResultTruth> {
    let conn = Connection::open(workspace.join(".team/runtime/team.db")).ok()?;
    let mut stmt = conn
        .prepare("select result_id, task_id, envelope from results order by created_at desc")
        .ok()?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })
        .ok()?;
    let found = rows
        .filter_map(Result::ok)
        .find_map(|(result_id, task_id, raw)| {
            let envelope: Value = serde_json::from_str(&raw).ok()?;
            let summary = envelope["summary"].as_str()?.to_string();
            summary.contains(message_id).then_some(ResultTruth {
                result_id,
                task_id,
                summary,
            })
        });
    found
}

fn run_worker_mcp_report_result(
    workspace: &Path,
    agent_id: &str,
    owner_team_id: &str,
    summary: &str,
) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_team-agent"))
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
        .expect("spawn real stdio MCP server");
    {
        let stdin = child.stdin.as_mut().expect("MCP stdin");
        writeln!(
            stdin,
            r#"{{"jsonrpc":"2.0","id":1,"method":"initialize","params":{{"protocolVersion":"2024-11-05"}}}}"#
        )
        .expect("write MCP initialize");
        writeln!(
            stdin,
            "{}",
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "tools/call",
                "params": {
                    "name": "report_result",
                    "arguments": {
                        "summary": summary,
                        "status": "success",
                        "tests": [{"command": "send001-fake-worker-receipt", "status": "passed"}]
                    }
                }
            })
        )
        .expect("write MCP report_result");
    }
    let output = child
        .wait_with_output()
        .expect("wait for MCP report_result");
    assert!(
        output.status.success(),
        "MCP server failed: status={:?} stderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let response = stdout
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .find(|frame| frame["id"] == Value::from(2))
        .unwrap_or_else(|| panic!("missing report_result response; stdout={stdout}"));
    assert_ne!(
        response["result"]["isError"],
        Value::Bool(true),
        "worker MCP report_result failed: {response}"
    );
}
