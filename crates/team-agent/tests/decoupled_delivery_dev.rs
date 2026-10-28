//! Developer-owned tests for the physical actuator and independent inbound log.
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use team_agent::mcp_server::TeamOrchestratorTools;
use team_agent::messaging::MessageTarget;
use team_agent::model::enums::ResultStatus;
use team_agent::model::ids::{AgentId, TeamKey};

struct Fixture {
    root: PathBuf,
    socket: String,
    pane: String,
}
impl Fixture {
    fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "ta-direct-dev-{}-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        fs::create_dir_all(root.join(".team/runtime")).unwrap();
        let socket = root.join("tmux.sock").to_string_lossy().into_owned();
        let script = root.join("receiver.py");
        fs::write(
            &script,
            format!(
                r#"import os, tty
from pathlib import Path
tty.setraw(0)
os.write(1, b'\x1b[?2004h')
root = Path({root:?})
with (root / 'received').open('ab', buffering=0) as out:
    (root / 'ready').touch()
    while True:
        out.write(os.read(0, 65536))
"#,
                root = root.to_string_lossy()
            ),
        )
        .unwrap();
        let output = Command::new("tmux")
            .args([
                "-S",
                &socket,
                "new-session",
                "-d",
                "-P",
                "-F",
                "#{pane_id}",
                "-s",
                "direct-test",
            ])
            .arg(format!("python3 '{}'", script.display()))
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let pane = String::from_utf8(output.stdout).unwrap().trim().to_string();
        let fixture = Self { root, socket, pane };
        fixture.wait_for(|| fixture.root.join("ready").exists());
        let state = json!({"active_team_key": "alpha", "tmux_socket": fixture.socket,
            "teams": {"alpha": {"team_id": "alpha", "team_name": "friendly", "status": "stopped",
                "tmux_socket": fixture.socket, "leader": {"pane_id": fixture.pane, "provider": "retired-provider"},
                "agents": {"worker": {"pane_id": fixture.pane, "provider": "retired-provider", "status": "dead"}}}}});
        fs::write(
            fixture.root.join(".team/runtime/state.json"),
            serde_json::to_vec(&state).unwrap(),
        )
        .unwrap();
        fixture
    }
    fn wait_for(&self, mut condition: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !condition() {
            assert!(
                Instant::now() < deadline,
                "physical receiver timed out: {:?}",
                fs::read(self.root.join("received"))
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    fn cli(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_team-agent"))
            .args(args)
            .args(["--workspace", self.root.to_str().unwrap(), "--json"])
            .env_remove("TEAM_AGENT_ID")
            .env_remove("TEAM_AGENT_AGENT_ID")
            .env_remove("TEAM_AGENT_WORKSPACE")
            .env_remove("TEAM_AGENT_OWNER_TEAM_ID")
            .env_remove("TEAM_AGENT_TEAM_ID")
            .output()
            .unwrap()
    }
    fn received(&self, expected: &str) {
        let expected = format!("\x1b[200~{expected}\x1b[201~\r").into_bytes();
        self.wait_for(|| {
            fs::read(self.root.join("received"))
                .unwrap_or_default()
                .windows(expected.len())
                .any(|bytes| bytes == expected)
        });
    }
    fn tools(&self) -> TeamOrchestratorTools {
        TeamOrchestratorTools::with_identity(
            &self.root,
            Some(AgentId::new("worker")),
            Some(TeamKey::new("alpha")),
        )
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = Command::new("tmux")
            .args(["-S", &self.socket, "kill-server"])
            .output();
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn logical_send_refuses_stale_binding_but_not_dead_status_or_missing_mcp() {
    let fixture = Fixture::new();
    assert!(Command::new("tmux")
        .args(["-S", &fixture.socket, "rename-window", "-t", &fixture.pane, "worker"])
        .status().unwrap().success());
    let output = Command::new("tmux")
        .args(["-S", &fixture.socket, "display-message", "-p", "-t", &fixture.pane, "#{pane_pid}"])
        .output().unwrap();
    assert!(output.status.success());
    let pid: u64 = String::from_utf8(output.stdout).unwrap().trim().parse().unwrap();
    let path = fixture.root.join(".team/runtime/state.json");
    let mut valid: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    valid["teams"]["alpha"]["session_name"] = json!("direct-test");
    valid["teams"]["alpha"]["agents"]["worker"]["window"] = json!("worker");
    valid["teams"]["alpha"]["agents"]["worker"]["pane_pid"] = json!(pid);
    for field in ["session_name", "window", "pane_pid"] {
        let mut stale = valid.clone();
        let worker = &mut stale["teams"]["alpha"]["agents"]["worker"];
        worker[field] = if field == "pane_pid" { json!(0) } else { json!("foreign") };
        let bytes = serde_json::to_vec(&stale).unwrap();
        fs::write(&path, &bytes).unwrap();
        let output = fixture.cli(&["send", "worker", "must not inject", "--team", "alpha"]);
        assert!(!output.status.success(), "stale {field} was accepted");
        assert!(fixture.tools().send_message(
            &MessageTarget::Single("worker".into()), "must not inject", None, None, None,
        ).is_err());
        assert_eq!(fs::read(&path).unwrap(), bytes);
        assert!(fs::read(fixture.root.join("received")).unwrap().is_empty());
        assert!(!fixture.root.join(".team/inbox").exists());
        assert!(!fixture.root.join(".team/db").exists());
    }
    fs::write(&path, serde_json::to_vec(&valid).unwrap()).unwrap();
    let output = fixture.cli(&["send", "worker", "live despite dead status", "--team", "alpha"]);
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    fixture.received("Team Agent message from leader:\n\nlive despite dead status");
    assert!(!fixture.root.join(".team/db").exists());
}

#[test]
fn raw_send_needs_no_snapshot_and_preserves_bracketed_multiline_payload() {
    let fixture = Fixture::new();
    fs::remove_file(fixture.root.join(".team/runtime/state.json")).unwrap();
    let text = "first line\nsecond 日本語 $HOME ; literal";
    let output = fixture.cli(&[
        "send",
        "--socket",
        &fixture.socket,
        "--pane",
        &fixture.pane,
        "--dialect",
        "enter",
        text,
    ]);
    assert!(
        output.status.success(),
        "{} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    fixture.received(text);
    assert!(!fixture.root.join(".team/inbox").exists());
    assert!(!fixture.root.join(".team/db").exists());
}

#[test]
fn cli_and_mcp_send_ignore_dead_coordinator_and_never_modify_runtime_or_database() {
    let fixture = Fixture::new();
    let state_path = fixture.root.join(".team/runtime/state.json");
    let before = fs::read(&state_path).unwrap();
    let db_path = fixture.root.join(".team/runtime/team.db");
    fs::create_dir_all(db_path.parent().unwrap()).unwrap();
    fs::write(&db_path, b"poisoned database must never be opened").unwrap();
    let output = fixture.cli(&["send", "worker", "direct CLI", "--team", "friendly"]);
    assert!(
        output.status.success(),
        "{} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    fixture.received("Team Agent message from leader:\n\ndirect CLI");
    let result = fixture
        .tools()
        .send_message(
            &MessageTarget::Single("worker".into()),
            "direct MCP",
            None,
            None,
            None,
        )
        .unwrap()
        .to_value();
    assert_eq!(result["status"], "sent");
    fixture.received("Team Agent message from worker:\n\ndirect MCP");
    assert_eq!(fs::read(state_path).unwrap(), before);
    assert_eq!(
        fs::read(db_path).unwrap(),
        b"poisoned database must never be opened"
    );
    assert!(!fixture.root.join(".team/inbox").exists());
    let located = fixture.cli(&["locate"]);
    assert!(
        located.status.success(),
        "{}",
        String::from_utf8_lossy(&located.stdout)
    );
    let endpoints: Value = serde_json::from_slice(&located.stdout).unwrap();
    assert_eq!(endpoints["endpoints"].as_array().unwrap().len(), 2);
}

#[test]
fn replies_and_results_append_to_inbox_without_database_or_receipt_state() {
    let fixture = Fixture::new();
    let tools = fixture.tools();
    let message = tools
        .send_message(
            &MessageTarget::Single("leader".into()),
            "reply",
            None,
            None,
            None,
        )
        .unwrap()
        .to_value();
    assert_eq!(message["status"], "sent");
    let result = tools
        .report_result(
            None,
            Some("completed"),
            ResultStatus::Success,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();
    assert_eq!(result.fields["status"], "reported");
    assert!(!fixture.root.join(".team/runtime/team.db").exists());
    // Inbox must remain available even when the entire runtime snapshot is corrupt.
    fs::write(
        fixture.root.join(".team/runtime/state.json"),
        b"invalid JSON",
    )
    .unwrap();
    let output = fixture.cli(&["inbox", "leader", "-n", "1"]);
    assert!(
        output.status.success(),
        "{} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["messages"].as_array().unwrap().len(), 1);
    assert_eq!(value["messages"][0]["content"], "completed");
    let repeated = fixture.cli(&["inbox", "leader", "-n", "1"]);
    assert_eq!(output.stdout, repeated.stdout);
}

#[test]
fn detached_leader_replies_and_offline_mailbox_are_still_stored() {
    let fixture = Fixture::new();
    let path = fixture.root.join(".team/runtime/state.json");
    let mut state: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    state["teams"]["alpha"]
        .as_object_mut()
        .unwrap()
        .remove("leader");
    state["teams"]["alpha"]["agents"]["worker"]
        .as_object_mut()
        .unwrap()
        .remove("pane_id");
    fs::write(&path, serde_json::to_vec(&state).unwrap()).unwrap();
    let tools = fixture.tools();
    let reply = tools
        .send_message(
            &MessageTarget::Single("leader".into()),
            "detached reply",
            None,
            None,
            None,
        )
        .unwrap()
        .to_value();
    assert_eq!(reply["status"], "stored");
    assert!(reply["delivery_error"].is_string());
    let mailbox = tools
        .send_message_with_presentation(
            &MessageTarget::Single("worker".into()),
            "offline MCP",
            None,
            None,
            None,
            Some(&Value::Bool(true)),
            None,
        )
        .unwrap()
        .to_value();
    assert_eq!(mailbox["status"], "stored");
    let cli = fixture.cli(&["send", "worker", "offline CLI", "--mailbox"]);
    assert!(
        cli.status.success(),
        "{}",
        String::from_utf8_lossy(&cli.stdout)
    );
    assert_eq!(
        team_agent::inbox_store::read(&fixture.root, "worker", 10, None).unwrap()["messages"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert!(!fixture.root.join(".team/runtime/team.db").exists());
}

#[test]
fn inbox_concurrent_append_and_partial_tail_are_independent_of_state() {
    let fixture = Fixture::new();
    let writers: Vec<_> = (0..8)
        .map(|writer| {
            let workspace = fixture.root.clone();
            std::thread::spawn(move || {
                for n in 0..10 {
                    team_agent::inbox_store::append(
                        &workspace,
                        "leader",
                        &json!({"content": format!("{writer}:{n}\n{}", "x".repeat(8000))}),
                    )
                    .unwrap();
                }
            })
        })
        .collect();
    for writer in writers {
        writer.join().unwrap();
    }
    let value = team_agent::inbox_store::read(&fixture.root, "leader", 100, None).unwrap();
    assert_eq!(value["messages"].as_array().unwrap().len(), 80);
    use std::io::Write;
    fs::OpenOptions::new()
        .append(true)
        .open(fixture.root.join(".team/inbox/leader.jsonl"))
        .unwrap()
        .write_all(b"{\"content\":")
        .unwrap();
    assert_eq!(
        team_agent::inbox_store::read(&fixture.root, "leader", 100, None).unwrap(),
        value
    );
    assert!(team_agent::inbox_store::append(&fixture.root, "../escape", &json!({})).is_err());
}
