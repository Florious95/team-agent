#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[path = "support/hermetic.rs"]
mod hermetic_guard;
#[allow(dead_code)]
fn _hermetic_boundary_marker(_: &hermetic_guard::HermeticTestEnv) {}

use std::io::Write as _;
use std::process::{Command, Stdio};

fn tmp_ws(tag: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!(
        "ta-rs-mcp-stdio-{tag}-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&path).unwrap();
    path
}

#[test]
fn mcp_server_binary_subcommand_routes_to_stdio_wire_main() {
    // Python console script `team_orchestrator` runs mcp_server.server.main:
    // one JSON-RPC initialize line on stdin yields exactly one JSON-RPC response
    // on stdout. Rust's single binary must expose the equivalent mcp-server
    // subcommand; falling through the ordinary CLI unknown-subcommand path is RED.
    let exe = env!("CARGO_BIN_EXE_team-agent");
    let ws = tmp_ws("initialize");
    let mut child = Command::new(exe)
        .arg("mcp-server")
        .arg("--workspace")
        .arg(&ws)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn team-agent mcp-server");
    {
        let stdin = child.stdin.as_mut().expect("stdin");
        writeln!(
            stdin,
            r#"{{"jsonrpc":"2.0","id":1,"method":"initialize","params":{{"protocolVersion":"X"}}}}"#
        )
        .unwrap();
    }
    let output = child.wait_with_output().expect("wait");
    assert!(
        output.status.success(),
        "mcp-server must exit cleanly on EOF; status={:?}, stderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 1, "one initialize request writes one frame: {stdout}");
    let frame: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(frame["jsonrpc"], serde_json::json!("2.0"));
    assert_eq!(frame["id"], serde_json::json!(1));
    assert_eq!(frame["result"]["protocolVersion"], serde_json::json!("X"));
    assert_eq!(frame["result"]["serverInfo"]["name"], serde_json::json!("team_orchestrator"));
    assert_eq!(frame["result"]["capabilities"], serde_json::json!({"tools": {}}));
    let _ = std::fs::remove_dir_all(&ws);
}
