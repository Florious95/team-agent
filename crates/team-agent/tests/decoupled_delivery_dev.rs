//! Developer-owned tests for the physical actuator and independent inbound log.
use std::fs;
use std::process::{Command, Output};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use team_agent::mcp_server::TeamOrchestratorTools;
use team_agent::messaging::MessageTarget;
use team_agent::model::enums::ResultStatus;
use team_agent::model::ids::{AgentId, TeamKey};

#[path = "support/direct_fixture.rs"]
mod direct_fixture;
#[cfg(unix)]
#[path = "support/direct_native_fixture.rs"]
mod direct_native_fixture;

// B6 / c28a675f: reuse the shared fixture's genuinely distinct receiver panes.
struct Fixture(direct_fixture::DirectFixture);
impl std::ops::Deref for Fixture {
    type Target = direct_fixture::DirectFixture;
    fn deref(&self) -> &Self::Target { &self.0 }
}
impl Fixture {
    fn new() -> Self {
        let fixture = Self(direct_fixture::DirectFixture::new());
        assert_ne!(fixture.pane, fixture.leader_pane);
        assert!(Command::new("tmux")
            .args(["-S", &fixture.socket, "rename-session", "-t", "oracle", "direct-test"])
            .status().unwrap().success());
        fixture.set_state(&json!({"active_team_key": "alpha", "tmux_socket": fixture.socket,
            "teams": {"alpha": {"team_id": "alpha", "team_name": "friendly", "status": "stopped",
                "tmux_socket": fixture.socket, "leader": {"pane_id": fixture.leader_pane, "provider": "retired-provider"},
                "agents": {"worker": {"pane_id": fixture.pane, "provider": "retired-provider", "status": "running"}}}}}));
        fixture
    }
    // B9 / cfd9eb90: perform the legitimate first-send stamp before measuring no-repair.
    // The measured tests retain exact byte equality, not an expanded mutation allowlist.
    fn prime_first_send(&self) {
        let result = self.leader_tools().send_message_with_presentation(
            &MessageTarget::Single("worker".into()), "fixture first-send precondition",
            None, None, None, None, None).unwrap().to_value();
        assert_eq!(result["status"], "sent", "{result}");
        self.received("Team Agent message from leader:\n\nfixture first-send precondition");
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
    fn tools(&self) -> TeamOrchestratorTools {
        TeamOrchestratorTools::with_identity(
            &self.root,
            Some(AgentId::new("worker")),
            Some(TeamKey::new("alpha")),
        )
    }

    /// M5 admission ruling: an explicit self-target is refused before any effect, so MCP sends
    /// addressed to `worker` come from the separate leader seat (B6 / c28a675f).
    fn leader_tools(&self) -> TeamOrchestratorTools {
        TeamOrchestratorTools::with_identity(
            &self.root,
            Some(AgentId::new("leader")),
            Some(TeamKey::new("alpha")),
        )
    }
}
#[test]
fn native_pane_fallback_uses_its_private_endpoint_without_rewriting_snapshot() {
    let fixture = Fixture::new();
    fixture.prime_first_send();
    let path = fixture.root.join(".team/runtime/state.json");
    let mut state: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    let team = &mut state["teams"]["alpha"];
    team["tmux_socket"] = json!("unrelated-workspace-socket");
    team["session_name"] = json!("unrelated-workspace-session");
    team["agents"]["worker"]["pane_id"] = Value::Null;
    team["agents"]["worker"]["native_pane"] = json!(fixture.pane);
    team["agents"]["worker"]["native_endpoint"] = json!(fixture.socket);
    // B9 / cfd9eb90: both projections contain the same deliberately stale metadata.
    let state = team_agent::state::projection::project_top_level_view(&state, "alpha");
    let before = serde_json::to_vec_pretty(&state).unwrap();
    fs::write(&path, &before).unwrap();
    let located = fixture.cli(&["locate", "--team", "alpha"]);
    assert!(located.status.success(), "{}", String::from_utf8_lossy(&located.stderr));
    let located: Value = serde_json::from_slice(&located.stdout).unwrap();
    assert_eq!(located["endpoints"][0]["agent"], "worker");
    assert_eq!(located["endpoints"][0]["socket"], fixture.socket);
    let sent = fixture.cli(&["send", "worker", "native fallback", "--team", "alpha"]);
    assert!(sent.status.success(), "{}", String::from_utf8_lossy(&sent.stderr));
    // B5 / a68f07e4: an unbound CLI caller is the operator, not the leader.
    fixture.received("Team Agent message from operator:\n\nnative fallback");
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
    log.write(json.dumps({{'at_ns': time.time_ns(), 'args': sys.argv[1:]}}) + '\n')
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
    // Compare a cross-process integer clock; preserve the native Claude floor without float rounding.
    let elapsed = submits[0]["at_ns"].as_u64().unwrap() - paste["at_ns"].as_u64().unwrap();
    eprintln!("paste floor elapsed={elapsed}ns; trace={calls:?}");
    assert!(u128::from(elapsed) >= team_agent_contract::providers::claude::interaction::PASTE_TO_SUBMIT_FLOOR.as_nanos(), "{elapsed}ns; trace={calls:?}");
    assert_eq!(fs::read(fixture.root.join("received")).unwrap(), b"\x1b[200~paced submit\x1b[201~\r");
}


#[test]
fn codex_mentions_are_terminated_inside_the_paste_for_all_native_aliases() {
    for dialect in ["codex", "codex_cli", "codex_tui", "pi"] {
        let fixture = Fixture::new();
        let mut expected = Vec::new();
        for content in ["literal α @file", "literal α $skill"] {
            team_agent::direct_delivery::direct_send(
                &fixture.socket, &fixture.pane, dialect, content,
            ).unwrap();
            let pasted = if dialect == "pi" { content.to_owned() } else { format!("{content}\n") };
            fixture.received(&pasted);
            expected.extend_from_slice(format!("\x1b[200~{pasted}\x1b[201~\r").as_bytes());
            assert_eq!(fs::read(fixture.root.join("received")).unwrap(), expected, "{dialect}");
        }
    }
}

#[cfg(unix)]
#[test]
fn copy_mode_is_cancelled_before_paste_and_again_before_the_single_enter() {
    use std::os::unix::fs::PermissionsExt;
    for dialect in ["codex", "codex_tui", "pi"] {
        let fixture = Fixture::new();
        let wrapper_dir = fixture.root.join("wrapper");
        fs::create_dir(&wrapper_dir).unwrap();
        let wrapper = wrapper_dir.join("tmux");
        // Re-enter copy-mode after the paste to exercise the second cancellation
        // independently of the copy-mode already present before delivery starts.
        fs::write(&wrapper, format!(r#"#!/usr/bin/env python3
import os, subprocess, sys
os.environ['PATH'] = {path}
args = sys.argv[1:]
result = subprocess.run(['tmux', *args])
if result.returncode == 0 and 'paste-buffer' in args:
    result = subprocess.run(['tmux', '-S', {socket}, 'copy-mode', '-t', {pane}])
sys.exit(result.returncode)
"#, path=json!(std::env::var("PATH").unwrap()), socket=json!(fixture.socket), pane=json!(fixture.pane))).unwrap();
        fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(Command::new("tmux")
            .args(["-S", &fixture.socket, "copy-mode", "-t", &fixture.pane])
            .status().unwrap().success());
        let mode = Command::new("tmux")
            .args(["-S", &fixture.socket, "display-message", "-p", "-t", &fixture.pane, "#{pane_in_mode}"])
            .output().unwrap();
        assert_eq!(String::from_utf8_lossy(&mode.stdout).trim(), "1");
        let output = fixture.cli_command(&[
            "send", "--socket", &fixture.socket, "--pane", &fixture.pane,
            "--dialect", dialect, "copy-mode payload @file",
        ]).env("PATH", format!("{}:{}", wrapper_dir.display(), std::env::var("PATH").unwrap()))
            .output().unwrap();
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        let pasted = if dialect == "pi" { "copy-mode payload @file" } else { "copy-mode payload @file\n" };
        fixture.received(pasted);
        assert_eq!(fs::read(fixture.root.join("received")).unwrap(),
            format!("\x1b[200~{pasted}\x1b[201~\r").as_bytes(), "{dialect}");
    }
}

#[test]
fn logical_send_uses_live_pane_despite_stale_metadata_dead_status_and_missing_mcp() {
    let fixture = Fixture::new();
    fixture.prime_first_send();
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
        // B9 / cfd9eb90: do not confuse document mirror normalization with repair.
        let stale = team_agent::state::projection::project_top_level_view(&stale, "alpha");
        let bytes = serde_json::to_vec_pretty(&stale).unwrap();
        fs::write(&path, &bytes).unwrap();
        let content = format!("live despite stale {field}");
        let output = fixture.cli(&["send", "worker", &content, "--team", "alpha"]);
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        // B5 / a68f07e4: CLI operator and MCP leader remain distinct physical frames.
        fixture.received(&format!("Team Agent message from operator:\n\n{content}"));
        let result = fixture.leader_tools().send_message_with_presentation(
            &MessageTarget::Single("worker".into()), &content, None, None, None,

        None, None).unwrap().to_value();
        assert_eq!(result["status"], "sent");
        fixture.received(&format!("Team Agent message from leader:\n\n{content}"));
        assert_eq!(fs::read(&path).unwrap(), bytes);
        // R-M1a (M1 store-first): both entrypoints stored one record each for this content.
        assert_eq!(worker_inbox_hits(&fixture, &content), 2, "{content}");
        assert!(!fixture.root.join(".team/db").exists());
    }
    fs::write(&path, serde_json::to_vec(&valid).unwrap()).unwrap();
    let output = fixture.cli(&["send", "worker", "live despite dead status", "--team", "alpha"]);
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    fixture.received("Team Agent message from operator:\n\nlive despite dead status");
    assert!(!fixture.root.join(".team/db").exists());
    assert!(Command::new("tmux").args(["-S", &fixture.socket, "kill-pane", "-t", &fixture.pane])
        .status().unwrap().success());
    // R-M1b: a missing pane stores the message with an explanatory delivery_error and types nothing.
    let gone = fixture.cli(&["send", "worker", "no live target", "--team", "alpha"]);
    assert!(gone.status.success(), "{}", String::from_utf8_lossy(&gone.stdout));
    let receipt: Value = serde_json::from_slice(&gone.stdout).unwrap();
    assert_eq!((receipt["status"].clone(), receipt["live"].clone()), (json!("stored"), json!(false)), "{receipt}");
    assert!(receipt["delivery_error"].as_str().is_some_and(|e| !e.is_empty()), "{receipt}");
    assert!(!fs::read_to_string(fixture.root.join("received")).unwrap_or_default().contains("no live target"));
}

/// Inbox records of `worker` (team `alpha`) whose JSON mentions `text`.
fn worker_inbox_hits(fixture: &Fixture, text: &str) -> usize {
    team_agent::inbox_store::read(&fixture.root, "worker", 10_000, Some("alpha")).unwrap()["messages"]
        .as_array().cloned().unwrap_or_default().iter().filter(|r| r.to_string().contains(text)).count()
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
    fixture.prime_first_send();
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
    // B5 / a68f07e4: preserve MCP leader framing below; the CLI is an operator.
    fixture.received("Team Agent message from operator:\n\ndirect CLI");
    let result = fixture
        .leader_tools()
        .send_message_with_presentation(
            &MessageTarget::Single("worker".into()),
            "direct MCP",
            None,
            None,
            None,

        None, None)
        .unwrap()
        .to_value();
    assert_eq!(result["status"], "sent");
    fixture.received("Team Agent message from leader:\n\ndirect MCP");
    assert_eq!(fs::read(state_path).unwrap(), before);
    assert_eq!(
        fs::read(db_path).unwrap(),
        b"poisoned database must never be opened"
    );
    // R-M1a (M1 store-first): the CLI and MCP sends are each stored once; nothing else changed.
    assert_eq!(worker_inbox_hits(&fixture, "direct CLI"), 1);
    assert_eq!(worker_inbox_hits(&fixture, "direct MCP"), 1);
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
        .send_message_with_presentation(
            &MessageTarget::Single("leader".into()),
            "reply",
            None,
            None,
            None,

        None, None)
        .unwrap()
        .to_value();
    assert_eq!(message["status"], "sent");
    fixture.leader_received("Team Agent message from worker:\n\nreply");
    let result = tools
        .report_result_with_presentation(
            None,
            Some("completed"),
            Some("success"),
            None,
            None,
            None,
            None,
            None,
            None,
            None,

        None)
        .unwrap();
    assert_eq!(result.fields["status"], "reported");
    assert!(!fixture.root.join(".team/runtime/team.db").exists());
    // Inbox must remain available even when the entire runtime snapshot is corrupt.
    fs::write(
        fixture.root.join(".team/runtime/state.json"),
        b"invalid JSON",
    )
    .unwrap();
    // R-M5 (team-scoped inbox): with the snapshot unreadable the caller names the team explicitly.
    let output = fixture.cli(&["inbox", "leader", "-n", "1", "--team", "alpha"]);
    assert!(
        output.status.success(),
        "{} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["messages"].as_array().unwrap().len(), 1);
    assert_eq!(value["messages"][0]["content"], "completed");
    let repeated = fixture.cli(&["inbox", "leader", "-n", "1", "--team", "alpha"]);
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
        .send_message_with_presentation(
            &MessageTarget::Single("leader".into()),
            "detached reply",
            None,
            None,
            None,

        None, None)
        .unwrap()
        .to_value();
    assert_eq!(reply["status"], "stored");
    assert!(reply["delivery_error"].is_string());
    let mailbox = fixture
        .leader_tools()
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
    let real_tmux = Command::new("sh").args(["-c", "command -v tmux"]).output().unwrap();
    assert!(real_tmux.status.success());
    let real_tmux = String::from_utf8(real_tmux.stdout).unwrap().trim().to_owned();
    let bin = fixture.root.join("fake-bin");
    fs::create_dir(&bin).unwrap();
    let wrapper = bin.join("tmux");
    fs::write(&wrapper, r#"#!/usr/bin/env python3
import json, os, subprocess, sys, time
args = sys.argv[1:]
action = args[2] if args[0] in ('-S', '-L') else args[0]
started = time.time_ns()
if action == 'paste-buffer' and os.environ.get('TMUX_TEST_FAIL_PASTE'):
    status = 7
else:
    # Recording only: all identity observations and successful input calls are real.
    data = sys.stdin.buffer.read() if action == 'load-buffer' else None
    status = subprocess.run([os.environ['TMUX_TEST_REAL'], *args], input=data).returncode
with open(os.environ['TMUX_TEST_TRACE'], 'a') as log:
    log.write(json.dumps({'args': args, 'action': action, 'started': started,
                          'finished': time.time_ns()}) + '\n')
sys.exit(status)
"#).unwrap();
    fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o755)).unwrap();
    let path = std::env::join_paths(
        std::iter::once(bin).chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
    ).unwrap();
    let trace = fixture.root.join("tmux-trace.jsonl");

    // R-DD2E / 5365c445 / 4e8ff838 / cursor/interaction.rs:16-19:
    // native authority selects the canonical one-key policy; raw opt-in remains explicit.
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
        ("cursor_agent", Some("double-enter"), "cursor-double-enter", 100, 2),
        ("cursor", Some("enter"), "cursor", 100, 1),
        ("retired-provider", None, "retired-provider", 0, 1),
    ];
    for (provider, configured, expected, floor_ms, enter_count) in cases {
        // Each row starts without a backend; no prior row's authority can satisfy the negative.
        let fixture = Fixture::new();
        let state_path = fixture.root.join(".team/runtime/state.json");
        let mut state: Value = serde_json::from_slice(&fs::read(&state_path).unwrap()).unwrap();
        let worker = &mut state["teams"]["alpha"]["agents"]["worker"];
        worker["provider"] = json!(provider);
        if let Some(dialect) = configured { worker["dialect"] = json!(dialect); }
        fs::write(&state_path, serde_json::to_vec(&state).unwrap()).unwrap();
        let database = fixture.root.join(".team/runtime/team.db");
        fs::write(&database, b"must remain unopened").unwrap();
        let recognized = team_agent::contract_runtime::registry::recognizes(provider);
        let explicit_double = configured == Some("double-enter");
        if recognized {
            let error = team_agent::direct_delivery::directory(&fixture.root, Some("alpha"))
                .unwrap().endpoint("worker").unwrap_err();
            assert!(error.contains("native_authority_missing"), "{provider}: {error}");
            assert!(!error.contains("No such file or directory"), "{provider}: {error}");
            if explicit_double {
                fs::write(&trace, b"").unwrap();
                let refused = fixture.command(&["send", "worker", "unbound native", "--team", "alpha"])
                    .env("PATH", &path).env("TMUX_TEST_REAL", &real_tmux)
                    .env("TMUX_TEST_TRACE", &trace).output().unwrap();
                let receipt: Value = serde_json::from_slice(&refused.stdout).unwrap();
                assert_eq!(receipt["live"], false, "{receipt}");
                assert!(receipt["delivery_error"].as_str().unwrap().contains("native_authority_missing"));
                for line in fs::read_to_string(&trace).unwrap().lines() {
                    let call: Value = serde_json::from_str(line).unwrap();
                    assert!(!matches!(call["action"].as_str(), Some("load-buffer" | "paste-buffer" | "send-keys")), "{call}");
                }
            }
        }
        // B7: actual Lifecycle commitment, captured native identity and real MCP descendant.
        let authority = recognized.then(|| direct_native_fixture::NativeAuthority::start(&fixture.root, provider));
        if let Some(native) = &authority {
            let worker = &mut state["teams"]["alpha"]["agents"]["worker"];
            worker["runtime_family"] = json!("contract");
            worker["native_endpoint"] = json!(native.socket);
            worker["pane_id"] = json!(native.pane);
            worker["pane_pid"] = json!(native.pid);
            worker["pane_birth_identity"] = json!(team_agent_contract::host::process::process_birth_identity(native.pid).unwrap());
            fs::write(&state_path, serde_json::to_vec(&state).unwrap()).unwrap();
        }
        // B9 / cfd9eb90: stamp the real first submission before measuring unchanged bytes.
        let primed = fixture.cli(&["send", "worker", "policy first-send precondition", "--team", "alpha"]);
        assert!(primed.status.success(), "{provider}: {}", String::from_utf8_lossy(&primed.stdout));
        let primed: Value = serde_json::from_slice(&primed.stdout).unwrap();
        assert_eq!(primed["live"], true, "{provider}: {primed}");
        let snapshot = fs::read(&state_path).unwrap();
        if explicit_double {
            let saved: Value = serde_json::from_slice(&snapshot).unwrap();
            assert_eq!(saved["teams"]["alpha"]["agents"]["worker"]["dialect"], "double-enter");
        }
        let endpoint = team_agent::direct_delivery::directory(&fixture.root, Some("alpha"))
            .unwrap().endpoint("worker").unwrap();
        let native_dialect = if explicit_double { "cursor" } else { expected };
        let native_enters = if explicit_double { 1 } else { enter_count };
        assert_eq!(endpoint.dialect, native_dialect, "{provider}");
        if let Some(native) = &authority {
            assert_eq!(endpoint.socket, native.socket);
            assert_eq!(endpoint.pane_id, native.pane);
        }
        let mut sends = vec![
            (vec!["send", "worker", "policy test", "--team", "alpha"], native_enters),
            (vec!["send", "--socket", &endpoint.socket, "--pane", &endpoint.pane_id,
                "--dialect", &endpoint.dialect, "policy test"], native_enters),
        ];
        if explicit_double {
            // R-DD2E R1(c): two keys only through the documented explicit locator option.
            sends.push((vec!["send", "--socket", &endpoint.socket, "--pane", &endpoint.pane_id,
                "--dialect", "cursor-double-enter", "policy test"], enter_count));
        }
        for (args, expected_enters) in sends {
            fs::write(&trace, b"").unwrap();
            let output = fixture.command(&args).env("PATH", &path)
                .env("TMUX_TEST_REAL", &real_tmux).env("TMUX_TEST_TRACE", &trace).output().unwrap();
            assert!(output.status.success(), "{provider}: {}", String::from_utf8_lossy(&output.stdout));
            let receipt: Value = serde_json::from_slice(&output.stdout).unwrap();
            if args[1] == "worker" { assert_eq!(receipt["live"], true, "{receipt}"); }
            else { assert_eq!(receipt["delivery"], "physical", "{receipt}"); }
            let calls: Vec<Value> = fs::read_to_string(&trace).unwrap().lines()
                .map(|line| serde_json::from_str::<Value>(line).unwrap())
                .filter(|call| matches!(call["action"].as_str(), Some("load-buffer" | "paste-buffer" | "send-keys")))
                .collect();
            assert_eq!(calls.len(), 3, "{provider}: {calls:?}");
            assert_eq!(calls[0]["action"], "load-buffer");
            assert_eq!(calls[1]["action"], "paste-buffer");
            assert_eq!(calls[2]["action"], "send-keys");
            let paste_args = calls[1]["args"].as_array().unwrap();
            assert!(paste_args.contains(&json!("-p")) && paste_args.contains(&json!("-r")));
            let elapsed_ns = calls[2]["started"].as_u64().unwrap() - calls[1]["finished"].as_u64().unwrap();
            assert!(elapsed_ns >= floor_ms * 1_000_000, "{provider}: floor not honored: {elapsed_ns}");
            assert_eq!(calls[2]["args"].as_array().unwrap().iter()
                .filter(|arg| **arg == json!("Enter")).count(), expected_enters, "{provider}");
            assert_eq!(fs::read(&state_path).unwrap(), snapshot);
            assert_eq!(fs::read(&database).unwrap(), b"must remain unopened");
        }
    }
    // Keep the failed-paste subject fresh even if the native matrix outlives its watchdog.
    let fixture = Fixture::new();
    fs::write(&trace, b"").unwrap();
    let failed = fixture
        .command(&["send", "worker", "must not submit"])
        .env("PATH", path)
        .env("TMUX_TEST_REAL", real_tmux)
        .env("TMUX_TEST_TRACE", &trace)
        .env("TMUX_TEST_FAIL_PASTE", "1")
        .output()
        .unwrap();
    // R-M1b: the message is stored first; a failed paste is never followed by a submit key and is
    // reported on the receipt (not live, with a delivery error) instead of replaying.
    let receipt: Value = serde_json::from_slice(&failed.stdout).unwrap_or(Value::Null);
    assert_eq!(receipt["live"], false, "{receipt}");
    assert!(receipt["delivery_error"].as_str().is_some_and(|error| !error.is_empty()), "{receipt}");
    assert!(!fs::read_to_string(trace).unwrap().contains("send-keys"));
    let stored = team_agent::inbox_store::read(&fixture.root, "worker", 50, Some("alpha")).unwrap();
    let copies = stored["messages"].as_array().unwrap().iter()
        .filter(|record| record.to_string().contains("must not submit")).count();
    assert_eq!(copies, 1, "stored exactly once: {stored}");
}

#[test]
fn cursor_opt_in_confirmation_is_two_enters_and_never_a_third() {
    let fixture = Fixture::new();
    let path = fixture.root.join(".team/runtime/state.json");
    // R-DD2E / 5365c445 / 4e8ff838 / cursor/interaction.rs:16-19:
    // native Cursor has a one-Enter budget; double Enter is the explicit raw opt-in.
    let before = fs::read(&path).unwrap();
    let started = Instant::now();
    let output = fixture.cli(&["send", "--socket", &fixture.socket, "--pane", &fixture.pane,
        "--dialect", "cursor-double-enter", "cursor confirmation"]);
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    assert!(started.elapsed() >= Duration::from_millis(100));
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["ok"], true);
    assert_eq!(result["delivery"], "physical");
    assert_eq!(result["pane_id"], fixture.pane);
    assert!(result["accepted"].is_null());
    assert!(result.get("dialect").is_none());
    let expected = b"\x1b[200~cursor confirmation\x1b[201~\r\r";
    fixture.wait_for(|| {
        fs::read(fixture.root.join("received"))
            .unwrap_or_default()
            .len()
            >= expected.len()
    });
    assert_eq!(fs::read(fixture.root.join("received")).unwrap(), expected);
    std::thread::sleep(Duration::from_millis(250));
    assert_eq!(fs::read(fixture.root.join("received")).unwrap(), expected);
    assert_eq!(fs::read(&path).unwrap(), before);
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
