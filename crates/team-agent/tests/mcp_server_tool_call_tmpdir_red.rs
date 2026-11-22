//! RED gate: MCP tool calls must not depend on a usable TMPDIR.
//!
//! The stdio `mcp-server` inherits its environment from the host agent. On 0.6.0,
//! `send_message` and `get_team_status` succeed even when TMPDIR names a missing
//! or unwritable directory. Staging tool output in a temporary file must not add
//! a hard TMPDIR dependency that turns every tool call into an error frame.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[path = "support/hermetic.rs"]
mod hermetic_guard;
#[path = "support/memory_fixture.rs"]
mod memory_fixture;

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

use serde_json::{json, Value};

use hermetic_guard::HermeticTestEnv;

#[test]
#[serial_test::serial(env)]
fn tool_calls_succeed_when_tmpdir_is_unusable() {
    let env = HermeticTestEnv::enter("mcp-tmpdir");
    let ws = env.workspace("tmpdir");
    memory_fixture::seed_runtime_state(&ws, 0);
    let binary = std::env::var_os("TEAM_AGENT_MEMORY_PROBE_BIN")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| env!("CARGO_BIN_EXE_team-agent").into());
    let missing_tmp = env.root().join("missing-tmpdir");

    let mut child = Command::new(&binary)
        .arg("mcp-server")
        .arg("--workspace")
        .arg(&ws)
        .current_dir(&ws)
        .env("HOME", env.home())
        .env("TMPDIR", &missing_tmp)
        .env("TEAM_AGENT_ID", memory_fixture::SENDER)
        .env("TEAM_AGENT_AGENT_ID", memory_fixture::SENDER)
        .env("TEAM_AGENT_OWNER_TEAM_ID", memory_fixture::ACTIVE_TEAM)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn mcp-server");
    let mut stdin = child.stdin.take().expect("stdin");
    let mut stdout = BufReader::new(child.stdout.take().expect("stdout"));
    let mut rpc = |id: u64, method: &str, params: Value| -> Value {
        let request = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        writeln!(stdin, "{request}").expect("write request");
        stdin.flush().expect("flush request");
        let mut line = String::new();
        stdout.read_line(&mut line).expect("read response");
        serde_json::from_str(&line)
            .unwrap_or_else(|err| panic!("{method}: invalid frame ({err}): {line}"))
    };

    rpc(1, "initialize", json!({"protocolVersion": "2024-11-05"}));
    let send = rpc(
        2,
        "tools/call",
        json!({"name": "send_message", "arguments": {"to": "leader", "content": "tmpdir probe", "mailbox": true}}),
    );
    let status = rpc(
        3,
        "tools/call",
        json!({"name": "get_team_status", "arguments": {}}),
    );
    drop(stdin);
    let _ = child.wait();

    let mut failures = Vec::new();
    match send.get("result") {
        None => failures.push(format!(
            "send_message returned a JSON-RPC error: {}",
            send["error"]
        )),
        Some(result) => {
            let body: Value = result["content"][0]["text"]
                .as_str()
                .and_then(|text| serde_json::from_str(text).ok())
                .unwrap_or(Value::Null);
            if body["ok"] != json!(true) {
                failures.push(format!("send_message was not stored: {body}"));
            }
        }
    }
    if status.get("result").is_none() {
        failures.push(format!(
            "get_team_status returned a JSON-RPC error: {}",
            status["error"]
        ));
    }
    assert!(
        failures.is_empty(),
        "tool calls failed with TMPDIR={}:\n  {}",
        missing_tmp.display(),
        failures.join("\n  ")
    );
}
