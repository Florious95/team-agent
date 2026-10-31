#![cfg(unix)]

use serde_json::{json, Value};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn workspace(root: &Path, name: &str, team: &str) -> PathBuf {
    let path = root.join(name);
    std::fs::create_dir_all(path.join(".team/runtime")).unwrap();
    let team_state = json!({
        "team_key": team, "team_id": team,
        "agents": {
            "worker-a": {"owner_team_id": team, "status": "running"},
            "peer-c": {"owner_team_id": team, "status": "running"}
        }
    });
    std::fs::write(
        path.join(".team/runtime/state.json"),
        serde_json::to_vec(&json!({
            "active_team_key": team, "teams": {team: team_state}
        }))
        .unwrap(),
    )
    .unwrap();
    path
}

fn command(source: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_team-agent"));
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("TEAM_AGENT_") {
            command.env_remove(key);
        }
    }
    command
        .env_remove("TMUX")
        .env_remove("TMUX_PANE")
        .env("HOME", source)
        .env("TEAM_AGENT_ID", "worker-a")
        .env("TEAM_AGENT_OWNER_TEAM_ID", "alpha")
        .env("TEAM_AGENT_WORKSPACE", source)
        .current_dir(source);
    command
}

fn cli_output(source: &Path, target: &Path, team: &str, args: &[&str]) -> std::process::Output {
    command(source)
        .args(args)
        .arg("--workspace")
        .arg(target)
        .args(["--team", team, "--json"])
        .output()
        .unwrap()
}

fn cli(source: &Path, target: &Path, team: &str, args: &[&str]) -> Value {
    let output = cli_output(source, target, team, args);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn qualified_sender_excludes_only_its_actual_seat_from_broadcast() {
    let root = tempfile::tempdir().unwrap();
    let source = workspace(root.path(), "source", "alpha");
    let target = workspace(root.path(), "target", "beta");
    for (workspace, team, expected) in [
        (&source, "alpha", vec!["peer-c"]),
        (&target, "beta", vec!["peer-c", "worker-a"]),
    ] {
        let result = cli(
            &source,
            workspace,
            team,
            &["send", "*", "identity probe", "--mailbox"],
        );
        let mut recipients: Vec<_> = result["messages"]
            .as_array()
            .unwrap()
            .iter()
            .map(|message| message["recipient"].as_str().unwrap())
            .collect();
        recipients.sort_unstable();
        assert_eq!(recipients, expected);
        let inbox = cli(&source, workspace, team, &["inbox", "peer-c", "-n", "1"]);
        assert_eq!(
            inbox["messages"][0]["sender"],
            format!(
                "{}::alpha/worker-a",
                source.canonicalize().unwrap().display()
            )
        );
    }
}

fn mcp(source: &Path, targets: &[Value]) -> Vec<Value> {
    let mut child = command(source)
        .args(["mcp-server", "--workspace"])
        .arg(source)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    for (index, target) in targets.iter().enumerate() {
        writeln!(
            input,
            "{}",
            json!({
                "jsonrpc": "2.0", "id": index + 1, "method": "tools/call",
                "params": {"name": "send_message", "arguments": {
                    "to": target, "content": "scope probe", "mailbox": true
                }}
            })
        )
        .unwrap();
    }
    drop(input);
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let responses: Vec<Value> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(responses.len(), targets.len());
    responses
        .into_iter()
        .map(|response| {
            serde_json::from_str(response["result"]["content"][0]["text"].as_str().unwrap())
                .unwrap()
        })
        .collect()
}

#[test]
fn mcp_admission_rejects_all_targets_before_any_effect() {
    let root = tempfile::tempdir().unwrap();
    let source = workspace(root.path(), "source", "alpha");
    let pid = source.join(".team/runtime/coordinator.pid");
    let stale = std::process::id().to_string();
    std::fs::write(&pid, &stale).unwrap();
    for bad in ["beta/worker-a", "unknown-peer", "worker-a", "", "bad\nname"] {
        let bodies = mcp(&source, &[json!(["peer-c", bad])]);
        assert_eq!(bodies[0]["error_code"], "peer_not_in_scope");
        assert_eq!(bodies[0]["exc_type"], "PeerNotInScope");
        assert_eq!(bodies[0]["invalid_targets"], json!([bad]));
        assert_eq!(bodies[0]["not_attempted"], json!(["peer-c"]));
        assert_eq!(std::fs::read_to_string(&pid).unwrap(), stale);
    }
    let inbox = cli(&source, &source, "alpha", &["inbox", "peer-c", "-n", "10"]);
    assert_eq!(inbox["messages"], json!([]));
}

#[test]
fn cli_admission_rejects_all_targets_before_any_effect() {
    let root = tempfile::tempdir().unwrap();
    let source = workspace(root.path(), "source", "alpha");
    let pid = source.join(".team/runtime/coordinator.pid");
    let stale = std::process::id().to_string();
    std::fs::write(&pid, &stale).unwrap();
    for bad in ["bogus", "worker-a", "", "bad\nname"] {
        let to = format!("peer-c,{bad}");
        let positional = ["send", to.as_str(), "admission probe", "--mailbox"];
        let named = ["send", "--to", to.as_str(), "admission probe", "--mailbox"];
        for args in [positional.as_slice(), named.as_slice()] {
            let output = cli_output(&source, &source, "alpha", args);
            assert!(!output.status.success());
            let body: Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(body["error_code"], "peer_not_in_scope");
            assert_eq!(body["invalid_targets"], json!([bad]));
            assert_eq!(body["not_attempted"], json!(["peer-c"]));
            assert_eq!(std::fs::read_to_string(&pid).unwrap(), stale);
        }
    }
    let inbox = cli(&source, &source, "alpha", &["inbox", "peer-c", "-n", "10"]);
    assert_eq!(inbox["messages"], json!([]));
}

#[test]
fn duplicate_admitted_targets_have_one_receipt_and_record() {
    let root = tempfile::tempdir().unwrap();
    let source = workspace(root.path(), "source", "alpha");
    let bodies = mcp(&source, &[json!(["peer-c", "peer-c"])]);
    assert_eq!(bodies[0]["messages"].as_array().unwrap().len(), 1);
    let inbox = cli(&source, &source, "alpha", &["inbox", "peer-c", "-n", "10"]);
    assert_eq!(inbox["messages"].as_array().unwrap().len(), 1);
}

#[test]
fn stale_pid_reuse_is_nonblocking_and_status_does_not_clean_it() {
    let root = tempfile::tempdir().unwrap();
    let source = workspace(root.path(), "source", "alpha");
    let pid = source.join(".team/runtime/coordinator.pid");
    let meta = source.join(".team/runtime/coordinator.json");
    let stale = std::process::id().to_string();
    std::fs::write(&pid, &stale).unwrap();
    std::fs::write(&meta, format!("{{\"pid\":{stale}}}")).unwrap();
    let observed = team_agent::coordinator::legacy_coordinator_value(&source).unwrap();
    assert_eq!(observed["status"], "legacy_pid_unverified");
    assert!(!team_agent::coordinator::retire_legacy_coordinator_for_status(&source).unwrap());
    assert_eq!(std::fs::read_to_string(&pid).unwrap(), stale);
    assert!(meta.exists());
    assert!(!team_agent::coordinator::retire_legacy_coordinator(&source).unwrap());
    assert!(!pid.exists());
    assert!(!meta.exists());
    assert!(!source.join(".team/runtime/team.db").exists());
}

#[test]
fn execution_failure_does_not_abort_an_admitted_sibling() {
    let root = tempfile::tempdir().unwrap();
    let source = workspace(root.path(), "source", "alpha");
    let state_path = source.join(".team/runtime/state.json");
    let mut state: Value = serde_json::from_slice(&std::fs::read(&state_path).unwrap()).unwrap();
    state["teams"]["alpha"]["agents"]["peer-d"] =
        json!({"owner_team_id":"alpha", "status":"running"});
    std::fs::write(&state_path, serde_json::to_vec(&state).unwrap()).unwrap();
    let inbox = source.join(".team/inbox/alpha");
    std::fs::create_dir_all(&inbox).unwrap();
    std::fs::write(inbox.join("peer-c"), b"injected append-path obstruction").unwrap();
    let result = cli(
        &source,
        &source,
        "alpha",
        &["send", "peer-c,peer-d", "runtime probe", "--mailbox"],
    );
    assert_eq!(result["status"], "partial");
    assert_eq!(result["messages"][0]["ok"], false);
    assert_eq!(result["messages"][1]["recipient"], "peer-d");
    assert_eq!(result["messages"][1]["stored"], true);
}
