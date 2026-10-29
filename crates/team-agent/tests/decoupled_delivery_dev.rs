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
        self.command(args).output().unwrap()
    }
    fn cli_command(&self, args: &[&str]) -> Command {
        self.command(args)
    }
    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_team-agent"));
        command
            .args(args)
            .args(["--workspace", self.root.to_str().unwrap(), "--json"])
            .env_remove("TEAM_AGENT_ID")
            .env_remove("TEAM_AGENT_AGENT_ID")
            .env_remove("TEAM_AGENT_WORKSPACE")
            .env_remove("TEAM_AGENT_OWNER_TEAM_ID")
            .env_remove("TEAM_AGENT_TEAM_ID");
        command
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
fn native_pane_fallback_uses_its_private_endpoint_without_rewriting_snapshot() {
    let fixture = Fixture::new();
    let path = fixture.root.join(".team/runtime/state.json");
    let mut state: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    let team = &mut state["teams"]["alpha"];
    team["tmux_socket"] = json!("unrelated-workspace-socket");
    team["session_name"] = json!("unrelated-workspace-session");
    team["agents"]["worker"]["pane_id"] = Value::Null;
    team["agents"]["worker"]["native_pane"] = json!(fixture.pane);
    team["agents"]["worker"]["native_endpoint"] = json!(fixture.socket);
    let before = serde_json::to_vec(&state).unwrap();
    fs::write(&path, &before).unwrap();
    let located = fixture.cli(&["locate", "--team", "alpha"]);
    assert!(located.status.success(), "{}", String::from_utf8_lossy(&located.stderr));
    let located: Value = serde_json::from_slice(&located.stdout).unwrap();
    assert_eq!(located["endpoints"][0]["agent"], "worker");
    assert_eq!(located["endpoints"][0]["socket"], fixture.socket);
    let sent = fixture.cli(&["send", "worker", "native fallback", "--team", "alpha"]);
    assert!(sent.status.success(), "{}", String::from_utf8_lossy(&sent.stderr));
    fixture.received("Team Agent message from leader:\n\nnative fallback");
    assert_eq!(fs::read(&path).unwrap(), before);
    assert!(!fixture.root.join(".team/db").exists());
}

#[test]
fn bracketed_paste_and_single_enter_are_separate_paced_tmux_calls() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = Fixture::new();
    let wrapper_dir = fixture.root.join("wrapper");
    fs::create_dir(&wrapper_dir).unwrap();
    let trace = fixture.root.join("tmux-calls.jsonl");
    let wrapper = wrapper_dir.join("tmux");
    fs::write(&wrapper, format!(r#"#!/usr/bin/env python3
import json, os, sys, time
with open({trace}, 'a') as log:
    log.write(json.dumps({{'at': time.monotonic(), 'args': sys.argv[1:]}}) + '\n')
os.environ['PATH'] = {path}
os.execvp('tmux', ['tmux', *sys.argv[1:]])
"#, trace=json!(trace.to_string_lossy()), path=json!(std::env::var("PATH").unwrap()))).unwrap();
    fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o755)).unwrap();
    let output = fixture.cli_command(&[
        "send", "--socket", &fixture.socket, "--pane", &fixture.pane,
        "--dialect", "claude", "paced submit",
    ]).env("PATH", format!("{}:{}", wrapper_dir.display(), std::env::var("PATH").unwrap()))
        .output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    fixture.received("paced submit");
    let calls: Vec<Value> = fs::read_to_string(trace).unwrap().lines()
        .map(|line| serde_json::from_str(line).unwrap()).collect();
    let paste = calls.iter().find(|call| call["args"].as_array().unwrap().contains(&json!("paste-buffer"))).unwrap();
    let submits: Vec<_> = calls.iter().filter(|call| call["args"].as_array().unwrap().contains(&json!("send-keys"))).collect();
    assert_eq!(submits.len(), 1);
    assert!(!paste["args"].as_array().unwrap().contains(&json!("send-keys")));
    assert_eq!(submits[0]["args"].as_array().unwrap().last(), Some(&json!("Enter")));
    let elapsed = submits[0]["at"].as_f64().unwrap() - paste["at"].as_f64().unwrap();
    assert!(elapsed >= team_agent_contract::providers::claude::interaction::PASTE_TO_SUBMIT_FLOOR.as_secs_f64(), "{elapsed}");
    assert_eq!(fs::read(fixture.root.join("received")).unwrap(), b"\x1b[200~paced submit\x1b[201~\r");
}

#[test]
fn due_reminder_reaches_physical_leader_and_inbox_without_a_receipt_source() {
    let fixture = Fixture::new();
    let state_path = fixture.root.join(".team/runtime/state.json");
    let mut state: Value = serde_json::from_slice(&fs::read(&state_path).unwrap()).unwrap();
    let leader = &mut state["teams"]["alpha"]["leader"];
    leader["session_name"] = json!("old-leader-session");
    leader["window"] = json!("old-leader-window");
    leader["pane_pid"] = json!(0);
    fs::write(state_path, serde_json::to_vec(&state).unwrap()).unwrap();
    let store = team_agent::message_store::MessageStore::open(&fixture.root).unwrap();
    let conn = rusqlite::Connection::open(store.db_path()).unwrap();
    conn.execute(
        "insert into scheduled_events(owner_team_id, due_at, target, kind, payload_json, status, created_at) values ('alpha', '2000-01-01T00:00:00Z', 'leader', 'reminder', ?1, 'pending', '2000-01-01T00:00:00Z')",
        [json!({"text":"scheduled physical reminder", "trigger":"in 3s", "interval_ms":null}).to_string()],
    ).unwrap();
    let id = conn.last_insert_rowid();
    // The legacy argument deliberately differs from the published private endpoint.
    let transport = team_agent::tmux_backend::TmuxBackend::for_workspace(&fixture.root);
    let log = team_agent::event_log::EventLog::new(&fixture.root);
    let fired = team_agent::messaging::fire_due_scheduled_events(&fixture.root, &store, &transport, &log).unwrap();
    assert_eq!(fired, vec![id]);
    fixture.received("Team Agent reminder:\n\nscheduled physical reminder");
    let inbox = team_agent::inbox_store::read(&fixture.root, "leader", 10, Some("alpha")).unwrap();
    assert_eq!(inbox["messages"].as_array().unwrap().len(), 1);
    assert_eq!(inbox["messages"][0]["kind"], "reminder");
    assert_eq!(inbox["messages"][0]["content"], "scheduled physical reminder");
    let (status, result): (String, String) = conn.query_row("select status, result_json from scheduled_events where id=?1", [id], |row| Ok((row.get(0)?, row.get(1)?))).unwrap();
    assert_eq!(status, "done");
    assert_eq!(serde_json::from_str::<Value>(&result).unwrap()["delivery"], "physical");
    assert_eq!(conn.query_row("select count(*) from messages", [], |row| row.get::<_, i64>(0)).unwrap(), 0);
    let before = fs::read(fixture.root.join("received")).unwrap();
    assert!(team_agent::messaging::fire_due_scheduled_events(&fixture.root, &store, &transport, &log).unwrap().is_empty());
    assert_eq!(fs::read(fixture.root.join("received")).unwrap(), before);
}

#[test]
fn logical_send_uses_live_pane_despite_stale_metadata_dead_status_and_missing_mcp() {
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
        let content = format!("live despite stale {field}");
        let output = fixture.cli(&["send", "worker", &content, "--team", "alpha"]);
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        fixture.received(&format!("Team Agent message from leader:\n\n{content}"));
        let result = fixture.tools().send_message(
            &MessageTarget::Single("worker".into()), &content, None, None, None,
        ).unwrap().to_value();
        assert_eq!(result["status"], "sent");
        fixture.received(&format!("Team Agent message from worker:\n\n{content}"));
        assert_eq!(fs::read(&path).unwrap(), bytes);
        assert!(!fixture.root.join(".team/inbox").exists());
        assert!(!fixture.root.join(".team/db").exists());
    }
    fs::write(&path, serde_json::to_vec(&valid).unwrap()).unwrap();
    let output = fixture.cli(&["send", "worker", "live despite dead status", "--team", "alpha"]);
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    fixture.received("Team Agent message from leader:\n\nlive despite dead status");
    assert!(!fixture.root.join(".team/db").exists());
    assert!(Command::new("tmux").args(["-S", &fixture.socket, "kill-pane", "-t", &fixture.pane])
        .status().unwrap().success());
    assert!(!fixture.cli(&["send", "worker", "no live target", "--team", "alpha"]).status.success());
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
    let receipt: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(receipt["ok"], true);
    assert_eq!(receipt["delivery"], "physical");
    assert!(receipt["accepted"].is_null());
    assert!(receipt.get("message_id").is_none());
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

#[cfg(unix)]
#[test]
fn automatic_provider_dialects_preserve_native_floors_and_key_budgets() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = Fixture::new();
    let bin = fixture.root.join("fake-bin");
    fs::create_dir(&bin).unwrap();
    let wrapper = bin.join("tmux");
    fs::write(
        &wrapper,
        r#"#!/usr/bin/env python3
import json, os, sys, time
args = sys.argv[1:]
action = args[2] if args[0] in ('-S', '-L') else args[0]
# Named sends retain the selected-pane liveness guard; this fixture measures input framing.
if action == 'display-message':
    print('0')
    sys.exit(0)
started = time.monotonic_ns()
if action == 'load-buffer':
    sys.stdin.buffer.read()
if action == 'paste-buffer':
    time.sleep(0.02)
with open(os.environ['TMUX_TEST_TRACE'], 'a') as log:
    log.write(json.dumps({'args': args, 'action': action, 'started': started,
                          'finished': time.monotonic_ns()}) + '\n')
if action == 'paste-buffer' and os.environ.get('TMUX_TEST_FAIL_PASTE'):
    sys.exit(7)
"#,
    )
    .unwrap();
    fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o755)).unwrap();
    let path = std::env::join_paths(
        std::iter::once(bin).chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
    )
    .unwrap();
    let trace = fixture.root.join("tmux-trace.jsonl");
    let state_path = fixture.root.join(".team/runtime/state.json");
    let mut state: Value = serde_json::from_slice(&fs::read(&state_path).unwrap()).unwrap();
    let database = fixture.root.join(".team/runtime/team.db");
    fs::write(&database, b"must remain unopened").unwrap();

    // Approved direct-actuator policy, including historical wire synonyms.
    let cases = [
        ("cursor", None, "cursor", 100, 1),
        ("cursor_agent", None, "cursor", 100, 1),
        ("cursor-agent", None, "cursor", 100, 1),
        ("agent", None, "cursor", 100, 1),
        ("claude", None, "claude", 100, 1),
        ("claude_tui", None, "claude", 100, 1),
        ("claude-code", None, "claude", 100, 1),
        ("pi", None, "pi", 0, 1),
        ("pi_tui", None, "pi", 0, 1),
        ("codex", None, "codex", 0, 1),
        ("codex_cli", None, "codex", 0, 1),
        ("grok", None, "grok", 150, 1),
        ("grok_build", None, "grok", 150, 1),
        (
            "cursor_agent",
            Some("double-enter"),
            "cursor-double-enter",
            100,
            2,
        ),
        ("cursor", Some("enter"), "cursor", 100, 1),
        ("retired-provider", None, "retired-provider", 0, 1),
    ];
    for (provider, configured, expected, floor_ms, enter_count) in cases {
        let worker = state["teams"]["alpha"]["agents"]["worker"]
            .as_object_mut()
            .unwrap();
        worker.insert("provider".into(), json!(provider));
        worker.remove("dialect");
        if let Some(dialect) = configured {
            worker.insert("dialect".into(), json!(dialect));
        }
        let snapshot = serde_json::to_vec(&state).unwrap();
        fs::write(&state_path, &snapshot).unwrap();
        let endpoint = team_agent::direct_delivery::directory(&fixture.root, Some("alpha"))
            .unwrap()
            .endpoint("worker")
            .unwrap();
        assert_eq!(endpoint.dialect, expected, "{provider}");

        // Both semantic addressing and raw locator output must select the same policy.
        for args in [
            vec!["send", "worker", "policy test"],
            vec![
                "send",
                "--socket",
                &endpoint.socket,
                "--pane",
                &endpoint.pane_id,
                "--dialect",
                &endpoint.dialect,
                "policy test",
            ],
        ] {
            fs::write(&trace, b"").unwrap();
            let output = fixture
                .command(&args)
                .env("PATH", &path)
                .env("TMUX_TEST_TRACE", &trace)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{provider}: {}",
                String::from_utf8_lossy(&output.stdout)
            );
            let calls: Vec<Value> = fs::read_to_string(&trace)
                .unwrap()
                .lines()
                .map(|line| serde_json::from_str(line).unwrap())
                .collect();
            assert_eq!(calls.len(), 3, "{provider}: {calls:?}");
            assert_eq!(calls[0]["action"], "load-buffer");
            assert_eq!(calls[1]["action"], "paste-buffer");
            assert_eq!(calls[2]["action"], "send-keys");
            let paste_args = calls[1]["args"].as_array().unwrap();
            assert!(paste_args.contains(&json!("-p")) && paste_args.contains(&json!("-r")));
            let elapsed_ns =
                calls[2]["started"].as_u64().unwrap() - calls[1]["finished"].as_u64().unwrap();
            assert!(
                elapsed_ns >= floor_ms * 1_000_000,
                "{provider}: floor not honored: {elapsed_ns}"
            );
            assert_eq!(
                calls[2]["args"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter(|arg| **arg == json!("Enter"))
                    .count(),
                enter_count,
                "{provider}"
            );
            assert_eq!(fs::read(&state_path).unwrap(), snapshot);
            assert_eq!(fs::read(&database).unwrap(), b"must remain unopened");
        }
    }
    fs::write(&trace, b"").unwrap();
    let failed = fixture
        .command(&["send", "worker", "must not submit"])
        .env("PATH", path)
        .env("TMUX_TEST_TRACE", &trace)
        .env("TMUX_TEST_FAIL_PASTE", "1")
        .output()
        .unwrap();
    assert!(!failed.status.success());
    assert!(!fs::read_to_string(trace).unwrap().contains("send-keys"));
    assert!(!fixture.root.join(".team/inbox").exists());
}

#[test]
fn cursor_opt_in_confirmation_is_two_enters_and_never_a_third() {
    let fixture = Fixture::new();
    let path = fixture.root.join(".team/runtime/state.json");
    let mut state: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    state["teams"]["alpha"]["agents"]["worker"]["provider"] = json!("cursor_agent");
    state["teams"]["alpha"]["agents"]["worker"]["dialect"] = json!("double-enter");
    fs::write(&path, serde_json::to_vec(&state).unwrap()).unwrap();
    let started = Instant::now();
    let result = fixture
        .tools()
        .send_message(
            &MessageTarget::Single("worker".into()),
            "cursor confirmation",
            None,
            None,
            None,
        )
        .unwrap()
        .to_value();
    assert!(started.elapsed() >= Duration::from_millis(100));
    assert_eq!(result["dialect"], "cursor-double-enter");
    let expected = b"\x1b[200~Team Agent message from worker:\n\ncursor confirmation\x1b[201~\r\r";
    fixture.wait_for(|| {
        fs::read(fixture.root.join("received"))
            .unwrap_or_default()
            .len()
            >= expected.len()
    });
    assert_eq!(fs::read(fixture.root.join("received")).unwrap(), expected);
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
