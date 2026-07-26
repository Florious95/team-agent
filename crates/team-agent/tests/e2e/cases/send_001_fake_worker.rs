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
        quick_start_launched(&qs),
        "quick-start did not launch: {}",
        qs.stdout
    );

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
    assert_json_field_eq_str(&j, "/target", "a");
    assert_json_field_eq_str(&j, "/sender", "leader");

    wait_for_or_panic(
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
    wait_for_or_panic(
        "fake worker received message and reported result",
        || result_summary_for_message(ws.path(), message_id).is_some(),
        Duration::from_secs(10),
    );

    let owner_team_id = ws.read_state()["active_team_key"]
        .as_str()
        .expect("active_team_key")
        .to_string();
    let report_summary = format!("worker a MCP received {message_id}: {canary}");
    run_worker_mcp_report_result(ws.path(), "a", &owner_team_id, &report_summary);

    let collect = run_ta(
        &ws,
        &[
            "collect",
            "--workspace",
            ws.path().to_str().unwrap(),
            "--json",
        ],
    );
    assert!(
        collect.is_success(),
        "collect exit {}; stdout={} stderr={}",
        collect.exit_code,
        collect.stdout,
        collect.stderr
    );
    let collected = collect.json();
    let rows = collected["collected_results"]
        .as_array()
        .expect("collect must expose collected_results");
    assert!(
        rows.iter().any(|row| {
            row["agent_id"] == Value::String("a".to_string())
                && row["summary"] == Value::String(report_summary.clone())
        }),
        "collect did not return worker a's MCP result: {collected}"
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

fn result_summary_for_message(workspace: &Path, message_id: &str) -> Option<String> {
    let conn = Connection::open(workspace.join(".team/runtime/team.db")).ok()?;
    let mut stmt = conn
        .prepare("select envelope from results order by created_at desc")
        .ok()?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(0)).ok()?;
    let found = rows.filter_map(Result::ok).find_map(|raw| {
        let envelope: Value = serde_json::from_str(&raw).ok()?;
        envelope["summary"]
            .as_str()
            .filter(|summary| summary.contains(message_id))
            .map(str::to_string)
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
