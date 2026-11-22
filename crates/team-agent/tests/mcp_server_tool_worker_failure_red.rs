//! RED gate: a failure while handling one tool call must not end the MCP session.
//!
//! The stdio `mcp-server` lives as long as its host agent session. Its contract
//! is that every error surfaces as a JSON-RPC frame for the failing request and
//! the loop keeps serving. If a tool call is delegated to a separate worker
//! process, a worker that cannot run must become an error frame for that request
//! id. It must not tear down the session and strip the agent of its MCP tools.
//!
//! Deterministic injection: run the server from a private copy of the binary,
//! then delete the copy. This happens in the field when a runtime directory is
//! removed while sessions still use it. A server that only uses its own mapped
//! image keeps working; one that re-executes its path must answer with an error
//! frame and stay alive.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[path = "support/hermetic.rs"]
mod hermetic_guard;
#[path = "support/memory_fixture.rs"]
mod memory_fixture;

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use hermetic_guard::HermeticTestEnv;

const EOF_EXIT_MAX: Duration = Duration::from_secs(2);

#[test]
#[serial_test::serial(env)]
fn tool_call_failure_after_binary_path_removal_keeps_session_alive() {
    let env = HermeticTestEnv::enter("mcp-worker-failure");
    let ws = env.workspace("worker-failure");
    memory_fixture::seed_runtime_state(&ws, 0);

    let source = std::env::var_os("TEAM_AGENT_MEMORY_PROBE_BIN")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| env!("CARGO_BIN_EXE_team-agent").into());
    let private_bin = env.root().join("private-bin");
    std::fs::create_dir_all(&private_bin).unwrap();
    let exe = private_bin.join("team-agent");
    std::fs::copy(&source, &exe).expect("copy team-agent binary");

    let mut child = Command::new(&exe)
        .arg("mcp-server")
        .arg("--workspace")
        .arg(&ws)
        .current_dir(&ws)
        .env("HOME", env.home())
        .env("TEAM_AGENT_ID", memory_fixture::SENDER)
        .env("TEAM_AGENT_AGENT_ID", memory_fixture::SENDER)
        .env("TEAM_AGENT_OWNER_TEAM_ID", memory_fixture::ACTIVE_TEAM)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn mcp-server from private binary copy");
    let mut stdin = child.stdin.take().expect("stdin");
    let mut stdout = BufReader::new(child.stdout.take().expect("stdout"));
    let mut rpc = |id: u64, method: &str, params: Value| -> Option<Value> {
        let request = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        if writeln!(stdin, "{request}")
            .and_then(|_| stdin.flush())
            .is_err()
        {
            return None;
        }
        let mut line = String::new();
        match stdout.read_line(&mut line) {
            Ok(n) if n > 0 => Some(serde_json::from_str(&line).expect("response frame is JSON")),
            _ => None,
        }
    };
    let status = json!({"name": "get_team_status", "arguments": {}});

    assert!(rpc(1, "initialize", json!({"protocolVersion": "2024-11-05"})).is_some());
    let before = rpc(2, "tools/call", status.clone()).expect("tools/call answers before injection");
    assert!(
        before.get("result").is_some(),
        "baseline tools/call: {before}"
    );

    std::fs::remove_file(&exe).expect("remove private binary copy");

    let mut failures = Vec::new();
    match rpc(3, "tools/call", status) {
        None => failures.push(
            "tools/call after binary removal: no response frame; the MCP session was torn down"
                .to_string(),
        ),
        Some(frame) => {
            if frame["id"] != json!(3) {
                failures.push(format!(
                    "tools/call after binary removal answered the wrong id: {frame}"
                ));
            }
            if frame.get("result").is_none() && frame.get("error").is_none() {
                failures.push(format!(
                    "tools/call after binary removal: not a JSON-RPC response: {frame}"
                ));
            }
        }
    }
    match rpc(4, "tools/list", json!({})) {
        Some(frame) if frame["result"]["tools"].is_array() => {}
        other => failures.push(format!(
            "session must keep serving after a failed tool call; tools/list -> {other:?}"
        )),
    }

    drop(stdin);
    let started = Instant::now();
    let mut exit = None;
    while started.elapsed() < EOF_EXIT_MAX * 5 {
        if let Some(status) = child.try_wait().expect("try_wait") {
            exit = Some(status);
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    match exit {
        Some(status) if status.success() => {}
        Some(status) => failures.push(format!(
            "server must exit cleanly on stdin EOF, got {status}"
        )),
        None => {
            let _ = child.kill();
            let _ = child.wait();
            failures.push(format!(
                "server did not exit within {:?} of stdin EOF",
                EOF_EXIT_MAX * 5
            ));
        }
    }

    assert!(
        failures.is_empty(),
        "one failed tool call must not end the MCP session:\n  {}",
        failures.join("\n  ")
    );
}
