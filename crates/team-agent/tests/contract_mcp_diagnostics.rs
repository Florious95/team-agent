//! The real CLI entry reports pre-registration failures without speaking on MCP stdout.
#![cfg(unix)]
#[path = "../../team-agent-contract/tests/support/mod.rs"]
mod support;

use serde_json::Value;
use std::path::Path;
use std::process::{Command, Output};

fn invoke(workspace: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_team-agent"))
        .args(["contract-mcp", "--workspace"])
        .arg(workspace)
        .args([
            "--team",
            "current",
            "--seat",
            "worker",
            "--instance",
            "instance-test",
            "--generation",
            "1",
        ])
        .output()
        .unwrap()
}

fn events(bytes: &[u8]) -> Vec<Value> {
    String::from_utf8_lossy(bytes)
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|event| event["event"] == "contract.mcp")
        .collect()
}

#[test]
fn invalid_arguments_have_bounded_stderr_entry_and_never_reveal_the_raw_arguments() {
    let output = Command::new(env!("CARGO_BIN_EXE_team-agent"))
        .args(["contract-mcp", "--unsupported", "private-argument-canary"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(!String::from_utf8_lossy(&output.stderr).contains("private-argument-canary"));
    let records = events(&output.stderr);
    assert_eq!(records[0]["stage"], "entry");
    assert_eq!(records[0]["entry_ppid"], std::process::id());
    assert!(records[0]["pid"].as_u64().is_some_and(|pid| pid > 0));
    assert!(records.iter().any(|event| {
        event["stage"] == "arguments"
            && event["outcome"] == "error"
            && event["error"] == "native runtime metadata is invalid"
    }));
    assert_eq!(records.last().unwrap()["stage"], "exit");
    assert!(output
        .stderr
        .split(|byte| *byte == b'\n')
        .all(|line| line.len() <= 8192));
}

#[test]
fn missing_backend_records_entry_and_exact_failed_stage_before_database_registration() {
    let sandbox = support::Sandbox::new();
    let logs = sandbox.parent.join(".team/logs");
    std::fs::create_dir_all(&logs).unwrap();
    let output = invoke(&sandbox.parent);
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let early = events(&output.stderr);
    assert_eq!(early[0]["stage"], "entry");
    let records = events(&std::fs::read(logs.join("events.jsonl")).unwrap());
    assert_eq!(records[0]["stage"], "entry");
    assert_eq!(records[0]["seat"], "worker");
    assert_eq!(records[0]["instance"], "instance-test");
    assert_eq!(records[0]["generation"], 1);
    assert_eq!(records[0]["pid"], early[0]["pid"]);
    assert_eq!(records[0]["entry_ppid"], std::process::id());
    assert!(records.iter().any(|event| {
        event["stage"] == "backend_open"
            && event["outcome"] == "error"
            && event["error"].as_str().is_some_and(|text| !text.is_empty())
    }));
    assert!(records
        .iter()
        .all(|event| event["stage"] != "register_connection"));
    assert_eq!(records.last().unwrap()["stage"], "exit");
    assert!(!sandbox.parent.join(".team/runtime").exists());
    assert!(!sandbox.root.exists());
}

#[test]
fn entry_diagnostics_do_not_follow_a_redirected_log_directory() {
    let sandbox = support::Sandbox::new();
    let foreign = sandbox.parent.join("foreign");
    std::fs::create_dir(&foreign).unwrap();
    std::fs::create_dir(sandbox.parent.join(".team")).unwrap();
    std::os::unix::fs::symlink(&foreign, sandbox.parent.join(".team/logs")).unwrap();
    let output = invoke(&sandbox.parent);
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(events(&output.stderr).iter().any(|event| {
        event["stage"] == "backend_open" && event["outcome"] == "error"
    }));
    assert!(!foreign.join("events.jsonl").exists());
}
