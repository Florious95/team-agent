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
        let root = std::env::temp_dir().join(format!(
            "ta-direct-dev-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap()
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
(root / 'ready').touch()
with (root / 'received').open('ab', buffering=0) as out:
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
