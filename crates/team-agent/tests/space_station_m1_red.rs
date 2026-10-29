//! Space Station M1 golden property suite (Stage 1 — messaging kernel).
//!
//! Design: `.team/artifacts/STAGE1-M1-TEST-DESIGN-OPUS.md` (owner: test-opus-avatar).
//! Blueprint: `.team/artifacts/SPACE-STATION-STEP3-OPUS-REVIEW-REFINEMENT.md`.
//!
//! Naming:
//! - `red_*`   must FAIL on `release/v0.5.128-all-astra@21baf77f` and PASS after M1.
//! - `guard_*` PASSES on the baseline and must stay green through M1. A guard protects a property
//!   that M1 must not regress.
//!
//! Rules for whoever turns these green:
//! - Do not weaken an assertion, lengthen a timeout, or delete a case to get green. If an oracle
//!   is wrong, change it with a written reason in the design doc.
//! - Tests drive public surfaces only: the `team-agent` binary, `TeamOrchestratorTools`, and files
//!   under `.team/`. Inbox files are found by content, not by a hard-coded path, so the C3
//!   segment layout can change.
//! - Every physical receiver is a private tmux server under a per-test temp root. No test touches
//!   the user's tmux server, the live workspace, or any provider credentials.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use team_agent::mcp_server::TeamOrchestratorTools;
use team_agent::messaging::MessageTarget;
use team_agent::model::enums::ResultStatus;
use team_agent::model::ids::{AgentId, TeamKey};

const DEAD_PANE: &str = "%999";
const QUIET: Duration = Duration::from_millis(1500);

// ───────────────────────────── fixture ─────────────────────────────

struct Pane {
    id: String,
    pid: u32,
}

struct Station {
    root: PathBuf,
    socket: String,
    started: bool,
}

const RECEIVER: &str = r#"import os, sys, tty
from pathlib import Path
name, root = sys.argv[1], Path(sys.argv[2])
tty.setraw(0)
os.write(1, b'\x1b[?2004h')
with (root / f'received-{name}').open('ab', buffering=0) as out:
    (root / f'ready-{name}').touch()
    while True:
        data = os.read(0, 65536)
        if not data:
            break
        out.write(data)
"#;

/// A Claude Code look-alike that draws a ready screen whose composer row is
/// `❯` + U+00A0 + draft. The layout follows the release grammar in
/// `team-agent-contract/src/providers/claude/interaction.rs`: rules at column 0,
/// composer between the last two rules, and no spinner. It never echoes input.
const CLAUDE_FAKE: &str = r#"import os, sys, tty
from pathlib import Path
name, root = sys.argv[1], Path(sys.argv[2])
draft = (root / f'draft-{name}').read_text()
tty.setraw(0)
rule = '─' * 100
lines = ['', ' ✻ Welcome to Claude Code', '', rule, '❯\u00a0' + draft, rule, '  ? for shortcuts']
os.write(1, b'\x1b[?2004h\x1b[2J\x1b[H' + '\r\n'.join(lines).encode())
with (root / f'received-{name}').open('ab', buffering=0) as out:
    (root / f'ready-{name}').touch()
    while True:
        data = os.read(0, 65536)
        if not data:
            break
        out.write(data)
"#;

impl Station {
    fn new(label: &str) -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "ta-m1red-{label}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join(".team/runtime")).unwrap();
        fs::write(root.join("receiver.py"), RECEIVER).unwrap();
        fs::write(root.join("claude_fake.py"), CLAUDE_FAKE).unwrap();
        let socket = root.join("tmux.sock").to_string_lossy().into_owned();
        Self { root, socket, started: false }
    }

    fn tmux(&self) -> Command {
        let mut command = Command::new("tmux");
        command.args(["-S", &self.socket]);
        command
    }

    fn spawn(&mut self, name: &str, script: &str) -> Pane {
        let program = format!(
            "python3 '{}' '{name}' '{}'",
            self.root.join(script).display(),
            self.root.display()
        );
        let output = if self.started {
            self.tmux()
                .args(["new-window", "-d", "-t", "m1", "-P", "-F", "#{pane_id} #{pane_pid}"])
                .arg(program)
                .output()
                .unwrap()
        } else {
            let output = self
                .tmux()
                .args([
                    "new-session", "-d", "-x", "120", "-y", "40", "-s", "m1", "-P", "-F",
                    "#{pane_id} #{pane_pid}",
                ])
                .arg(program)
                .output()
                .unwrap();
            let _ = self.tmux().args(["set-option", "-g", "default-size", "120x40"]).output();
            self.started = true;
            output
        };
        let text = String::from_utf8(output.stdout).unwrap();
        assert!(
            output.status.success() && !text.trim().is_empty(),
            "FIXTURE: tmux could not start a private server on {} (does the sandbox forbid unix sockets?): {}",
            self.socket,
            String::from_utf8_lossy(&output.stderr)
        );
        let mut parts = text.split_whitespace();
        let pane = Pane {
            id: parts.next().unwrap().to_string(),
            pid: parts.next().unwrap().parse().unwrap(),
        };
        let ready = self.root.join(format!("ready-{name}"));
        self.wait_for("receiver ready", || ready.exists());
        pane
    }

    fn receiver(&mut self, name: &str) -> Pane {
        self.spawn(name, "receiver.py")
    }

    fn claude_fake(&mut self, name: &str, draft: &str) -> Pane {
        fs::write(self.root.join(format!("draft-{name}")), draft).unwrap();
        let pane = self.spawn(name, "claude_fake.py");
        // Let the screen settle before anyone captures it.
        std::thread::sleep(Duration::from_millis(300));
        pane
    }

    fn kill_server(&mut self) {
        let _ = self.tmux().arg("kill-server").output();
        self.started = false;
        let deadline = Instant::now() + Duration::from_secs(5);
        while Path::new(&self.socket).exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn write_state(&self, state: &Value) {
        fs::write(
            self.root.join(".team/runtime/state.json"),
            serde_json::to_vec_pretty(state).unwrap(),
        )
        .unwrap();
    }

    fn received(&self, name: &str) -> Vec<u8> {
        fs::read(self.root.join(format!("received-{name}"))).unwrap_or_default()
    }

    fn wait_for(&self, what: &str, mut condition: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !condition() {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn wait_received(&self, name: &str, needle: &[u8]) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let bytes = self.received(name);
            if contains(&bytes, needle) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "receiver `{name}` never got {:?}; got {:?}",
                String::from_utf8_lossy(needle),
                String::from_utf8_lossy(&bytes)
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn assert_quiet(&self, name: &str, why: &str) {
        std::thread::sleep(QUIET);
        let bytes = self.received(name);
        assert!(
            bytes.is_empty(),
            "{why}: receiver `{name}` must get zero bytes, got {:?}",
            String::from_utf8_lossy(&bytes)
        );
    }

    fn cli(&self, args: &[&str], env: &[(&str, &str)]) -> Output {
        self.command(args, env).output().unwrap()
    }

    fn command(&self, args: &[&str], env: &[(&str, &str)]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_team-agent"));
        command
            .args(args)
            .args(["--workspace", self.root.to_str().unwrap(), "--json"])
            .env_remove("TEAM_AGENT_ID")
            .env_remove("TEAM_AGENT_AGENT_ID")
            .env_remove("TEAM_AGENT_WORKSPACE")
            .env_remove("TEAM_AGENT_OWNER_TEAM_ID")
            .env_remove("TEAM_AGENT_TEAM_ID")
            .stdin(Stdio::null());
        for (key, value) in env {
            command.env(key, value);
        }
        command
    }

    fn tools(&self, agent: &str, team: &str) -> TeamOrchestratorTools {
        TeamOrchestratorTools::with_identity(
            &self.root,
            Some(AgentId::new(agent)),
            Some(TeamKey::new(team)),
        )
    }

    /// `team-agent inbox <recipient>` through the public CLI. Panics with the CLI output on failure.
    fn inbox(&self, recipient: &str, team: &str) -> Value {
        let output = self.cli(&["inbox", recipient, "-n", "200", "--team", team], &[]);
        assert!(
            output.status.success(),
            "`team-agent inbox {recipient}` must succeed: {} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    }

    /// Every regular file under `.team/` except the runtime snapshot. Inbox files are found by
    /// content so that the C3 segment layout stays the implementer's choice.
    fn team_files(&self) -> Vec<PathBuf> {
        fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
            let Ok(entries) = fs::read_dir(dir) else { return };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    walk(&path, out);
                } else if path.file_name().is_some_and(|name| name != "state.json") {
                    out.push(path);
                }
            }
        }
        let mut out = Vec::new();
        walk(&self.root.join(".team"), &mut out);
        out
    }

    fn files_containing(&self, needle: &str) -> Vec<PathBuf> {
        self.team_files()
            .into_iter()
            .filter(|path| contains(&fs::read(path).unwrap_or_default(), needle.as_bytes()))
            .collect()
    }
}

impl Drop for Station {
    fn drop(&mut self) {
        let _ = self.tmux().arg("kill-server").output();
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty() && haystack.windows(needle.len()).any(|window| window == needle)
}

fn row(pane: Option<&Pane>, provider: &str) -> Value {
    match pane {
        Some(pane) => json!({"pane_id": pane.id, "pane_pid": pane.pid, "provider": provider, "status": "running"}),
        None => json!({"pane_id": DEAD_PANE, "pane_pid": 999_999, "provider": provider, "status": "running"}),
    }
}

/// One team snapshot in the release state shape. `leader`/agents: `None` means a dead pane.
fn team(
    station: &Station,
    key: &str,
    leader: Option<&Pane>,
    agents: &[(&str, Option<&Pane>, &str)],
) -> Value {
    let mut map = serde_json::Map::new();
    for (name, pane, provider) in agents {
        map.insert((*name).to_string(), row(*pane, provider));
    }
    json!({"team_id": key, "team_name": key, "status": "running", "tmux_socket": station.socket,
        "leader": row(leader, "pi"), "agents": map})
}

fn snapshot(station: &Station, teams: &[(&str, Value)]) -> Value {
    let mut map = serde_json::Map::new();
    for (key, value) in teams {
        map.insert((*key).to_string(), value.clone());
    }
    json!({"active_team_key": teams[0].0, "tmux_socket": station.socket, "teams": map})
}

fn stdout_json(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|_| {
        json!({"unparsed_stdout": String::from_utf8_lossy(&output.stdout),
            "stderr": String::from_utf8_lossy(&output.stderr)})
    })
}

/// Messages in an inbox CLI result whose JSON mentions `needle`.
fn inbox_hits(inbox: &Value, needle: &str) -> Vec<Value> {
    inbox["messages"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|message| message.to_string().contains(needle))
        .collect()
}

fn find_string_field<'a>(value: &'a Value, key: &str, out: &mut Vec<&'a str>) {
    match value {
        Value::Object(map) => {
            for (name, child) in map {
                if name == key {
                    if let Some(text) = child.as_str() {
                        out.push(text);
                    }
                }
                find_string_field(child, key, out);
            }
        }
        Value::Array(items) => items.iter().for_each(|item| find_string_field(item, key, out)),
        _ => {}
    }
}

fn report(tools: &TeamOrchestratorTools, summary: &str, task: Option<&str>) -> Value {
    let ok = tools
        .report_result(None, Some(summary), ResultStatus::Success, None, None, None, None, None, task, None)
        .unwrap_or_else(|error| panic!("report_result must succeed: {error:?}"));
    Value::Object(ok.fields)
}

// ───────────────────── R-1 unified core: CLI ≡ MCP ─────────────────────

/// Blueprint: Inquiry-2 §3.3 (one send core), proposal C3 inbox-first.
/// A worker's message to a leader whose pane is gone must be durably stored and reported the
/// same way by the CLI and by MCP. The baseline CLI exits with an error after it already wrote
/// the inbox record, so a retry duplicates it.
#[test]
fn red_m1_r1_cli_and_mcp_report_identical_disposition_when_leader_pane_is_gone() {
    let s = Station::new("r1");
    s.write_state(&snapshot(&s, &[("alpha", team(&s, "alpha", None, &[("worker", None, "pi")]))]));

    let mcp = s
        .tools("worker", "alpha")
        .send_message(&MessageTarget::Single("leader".into()), "parity via mcp", None, None, None)
        .expect("MCP send to a gone leader must store, not fail")
        .to_value();
    let cli = s.cli(
        &["send", "leader", "parity via cli", "--team", "alpha"],
        &[("TEAM_AGENT_ID", "worker"), ("TEAM_AGENT_OWNER_TEAM_ID", "alpha")],
    );
    let cli_value = stdout_json(&cli);
    assert!(
        cli.status.success(),
        "CLI must not fail after the message is durably stored (MCP said {mcp}); CLI said {cli_value}"
    );
    assert_eq!(
        cli_value["status"], mcp["status"],
        "CLI and MCP must share one send core and report the same disposition"
    );
    let inbox = s.inbox("leader", "alpha");
    assert_eq!(inbox_hits(&inbox, "parity via mcp").len(), 1, "{inbox}");
    assert_eq!(inbox_hits(&inbox, "parity via cli").len(), 1, "{inbox}");
}

// ───────────────────── R-2/3 per-recipient blast radius ─────────────────────

/// Blueprint: canon S1 / D2.1. One dead recipient must not stop delivery to the others.
#[test]
fn red_m1_r2_cli_fanout_one_dead_recipient_does_not_abort_the_rest() {
    let mut s = Station::new("r2");
    let live = s.receiver("live");
    let leader = s.receiver("leader");
    s.write_state(&snapshot(
        &s,
        &[("alpha", team(&s, "alpha", Some(&leader), &[("dead", None, "pi"), ("live", Some(&live), "codex")]))],
    ));
    let output = s.cli(&["send", "--to", "dead,live", "fan-out via cli", "--team", "alpha"], &[]);
    s.wait_received("live", b"fan-out via cli");
    let value = stdout_json(&output);
    assert!(
        value.to_string().contains("dead"),
        "the result must carry a per-recipient outcome for the dead recipient: {value}"
    );
}

#[test]
fn red_m1_r3_mcp_fanout_one_dead_recipient_does_not_abort_the_rest() {
    let mut s = Station::new("r3");
    let live = s.receiver("live");
    let leader = s.receiver("leader");
    s.write_state(&snapshot(
        &s,
        &[("alpha", team(&s, "alpha", Some(&leader), &[
            ("worker", Some(&leader), "pi"),
            ("dead", None, "pi"),
            ("live", Some(&live), "codex"),
        ]))],
    ));
    let result = s.tools("worker", "alpha").send_message(
        &MessageTarget::Fanout(vec!["dead".into(), "live".into()]),
        "fan-out via mcp",
        None,
        None,
        None,
    );
    s.wait_received("live", b"fan-out via mcp");
    let value = result.expect("a partial fan-out returns per-recipient receipts, not an error").to_value();
    assert!(value.to_string().contains("dead"), "{value}");
}

// ───────────────────── R-4 identity: stale pane reuse ─────────────────────

/// Blueprint: C1 identity (pane process pid, + birth when available), G8.
/// After a tmux server restart on the same socket, `%0` names a different process. The actuator
/// must refuse with zero bytes rather than type into it.
#[test]
fn red_m1_r4_stale_pane_id_after_server_restart_receives_zero_bytes() {
    let mut s = Station::new("r4");
    let first = s.receiver("first");
    let leader_alive = first.id.clone();
    s.write_state(&snapshot(&s, &[("alpha", team(&s, "alpha", Some(&first), &[("worker", Some(&first), "codex")]))]));
    let ok = s.cli(&["send", "worker", "before restart", "--team", "alpha"], &[]);
    assert!(ok.status.success(), "positive control: {}", String::from_utf8_lossy(&ok.stdout));
    s.wait_received("first", b"before restart");

    s.kill_server();
    let second = s.receiver("second");
    assert_eq!(second.id, leader_alive, "fixture precondition: tmux reissued the same pane id");
    assert_ne!(second.pid, first.pid, "fixture precondition: a different process owns the pane");

    let output = s.cli(&["send", "worker", "after restart", "--team", "alpha"], &[]);
    s.assert_quiet("second", "a recycled pane id is not the recorded seat");
    assert!(
        !output.status.success() || stdout_json(&output)["status"] != "sent",
        "the receipt must not claim delivery: {}",
        String::from_utf8_lossy(&output.stdout)
    );
}

// ───────────────────── R-5 composer protection ─────────────────────

/// Blueprint: G8 / F2. Never type into a composer that holds someone else's draft. The positive
/// control proves the fake screen is a valid ready Claude composer.
#[test]
fn red_m1_r5_claude_composer_with_a_draft_receives_zero_bytes() {
    let mut s = Station::new("r5");
    let ready = s.claude_fake("claude-ready", "");
    let busy = s.claude_fake("claude-draft", "half typed human draft");
    let leader = s.receiver("leader");
    s.write_state(&snapshot(
        &s,
        &[("alpha", team(&s, "alpha", Some(&leader), &[
            ("ready", Some(&ready), "claude"),
            ("drafting", Some(&busy), "claude"),
        ]))],
    ));
    let control = s.cli(&["send", "ready", "composer was empty", "--team", "alpha"], &[]);
    assert!(control.status.success(), "positive control: {}", String::from_utf8_lossy(&control.stdout));
    s.wait_received("claude-ready", b"composer was empty");

    let output = s.cli(&["send", "drafting", "must not clobber the draft", "--team", "alpha"], &[]);
    s.assert_quiet("claude-draft", "a non-empty composer belongs to someone else");
    assert_ne!(
        stdout_json(&output)["status"], "sent",
        "the receipt must say deferred/busy, not sent: {}",
        String::from_utf8_lossy(&output.stdout)
    );
}

// ───────────────────── R-6/7 content handling (F14) ─────────────────────

/// Blueprint: F14. ESC is neutralized to U+241B in the live render, not refused.
#[test]
fn red_m1_r6_cli_send_with_escape_bytes_is_delivered_neutralized() {
    let mut s = Station::new("r6");
    let worker = s.receiver("worker");
    let leader = s.receiver("leader");
    s.write_state(&snapshot(&s, &[("alpha", team(&s, "alpha", Some(&leader), &[("worker", Some(&worker), "codex")]))]));
    let output = s.cli(&["send", "worker", "colored \u{1b}[31mred\u{1b}[0m text", "--team", "alpha"], &[]);
    assert!(output.status.success(), "ESC must not be refused: {}", String::from_utf8_lossy(&output.stdout));
    s.wait_received("worker", "colored \u{241b}[31mred\u{241b}[0m text".as_bytes());
    let bytes = s.received("worker");
    let start = bytes.windows(6).position(|w| w == b"\x1b[200~").unwrap() + 6;
    let end = bytes.windows(6).position(|w| w == b"\x1b[201~").unwrap();
    assert!(!bytes[start..end].contains(&0x1b), "no raw ESC may appear inside the paste body");
}

/// Blueprint: F14. A worker report whose summary carries ANSI colour still reaches the leader
/// live, and the stored record keeps the original bytes.
#[test]
fn red_m1_r6b_report_result_with_ansi_summary_is_reported_live() {
    let mut s = Station::new("r6b");
    let leader = s.receiver("leader");
    s.write_state(&snapshot(&s, &[("alpha", team(&s, "alpha", Some(&leader), &[("worker", Some(&leader), "pi")]))]));
    let value = report(&s.tools("worker", "alpha"), "build \u{1b}[32mOK\u{1b}[0m", None);
    assert_eq!(value["status"], "reported", "{value}");
    s.wait_received("leader", "build \u{241b}[32mOK".as_bytes());
    let inbox = s.inbox("leader", "alpha");
    assert_eq!(inbox_hits(&inbox, "build \u{1b}[32mOK").len(), 1, "stored verbatim: {inbox}");
}

/// Blueprint: F14 (P2). A live render above 64 KiB is truncated with a pointer, and the full
/// text stays retrievable from the recipient's inbox.
#[test]
fn red_m1_r7_oversized_message_is_truncated_live_and_stored_in_full() {
    let mut s = Station::new("r7");
    let worker = s.receiver("worker");
    let leader = s.receiver("leader");
    s.write_state(&snapshot(&s, &[("alpha", team(&s, "alpha", Some(&leader), &[("worker", Some(&worker), "codex")]))]));
    let big = format!("{}END-OF-BIG-MESSAGE", "a".repeat(70_000));
    let output = s.cli(&["send", "worker", &big, "--team", "alpha"], &[]);
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stdout));
    s.wait_received("worker", b"\x1b[201~");
    let bytes = s.received("worker");
    let start = bytes.windows(6).position(|w| w == b"\x1b[200~").unwrap() + 6;
    let end = bytes.windows(6).position(|w| w == b"\x1b[201~").unwrap();
    let body = &bytes[start..end];
    assert!(body.len() <= 65_536 + 512, "live render must be bounded, got {} bytes", body.len());
    assert!(contains(body, b"inbox"), "truncated render must point at the stored record");
    assert_eq!(inbox_hits(&s.inbox("worker", "alpha"), "END-OF-BIG-MESSAGE").len(), 1);
}

// ───────────────────── R-8..10 append-only inbox (C3) ─────────────────────

/// Blueprint: F5 torn-tail rule. A crash mid-append leaves an unterminated fragment. The next
/// append must keep every acknowledged record readable. Baseline: the next record is glued onto
/// the fragment, and the whole inbox becomes unreadable.
#[test]
fn red_m1_r8_torn_tail_followed_by_a_new_append_keeps_the_inbox_readable() {
    // Inbox-only property: the leader pane is gone, so no tmux server is needed.
    let s = Station::new("r8");
    s.write_state(&snapshot(&s, &[("alpha", team(&s, "alpha", None, &[("worker", None, "pi")]))]));
    let tools = s.tools("worker", "alpha");
    tools.send_message(&MessageTarget::Single("leader".into()), "first durable", None, None, None).unwrap();
    let files = s.files_containing("first durable");
    assert_eq!(files.len(), 1, "exactly one inbox file holds the record: {files:?}");
    use std::io::Write;
    fs::OpenOptions::new()
        .append(true)
        .open(&files[0])
        .unwrap()
        .write_all(b"{\"v\":1,\"kind\":\"message\",\"seq\":2,\"data\":{\"text\":\"torn")
        .unwrap();
    tools.send_message(&MessageTarget::Single("leader".into()), "second durable", None, None, None).unwrap();
    let inbox = s.inbox("leader", "alpha");
    assert_eq!(inbox_hits(&inbox, "first durable").len(), 1, "{inbox}");
    assert_eq!(inbox_hits(&inbox, "second durable").len(), 1, "{inbox}");
}

/// Blueprint: C3 frame (`v`, `seq`, `checksum`). Records are versioned and sequenced so readers
/// can detect gaps and future versions.
#[test]
fn red_m1_r9_inbox_records_are_versioned_sequenced_and_checksummed() {
    // Inbox-only property: the leader pane is gone, so no tmux server is needed.
    let s = Station::new("r9");
    s.write_state(&snapshot(&s, &[("alpha", team(&s, "alpha", None, &[("worker", None, "pi")]))]));
    let tools = s.tools("worker", "alpha");
    for n in 1..=3 {
        tools
            .send_message(&MessageTarget::Single("leader".into()), &format!("sequenced-{n}"), None, None, None)
            .unwrap();
    }
    let files = s.files_containing("sequenced-1");
    assert_eq!(files.len(), 1, "{files:?}");
    let text = fs::read_to_string(&files[0]).unwrap();
    let frames: Vec<Value> = text
        .lines()
        .filter(|line| line.contains("sequenced-"))
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(frames.len(), 3);
    let mut previous = None;
    for frame in &frames {
        assert_eq!(frame["v"], 1, "every record carries its contract version: {frame}");
        let seq = frame["seq"].as_u64().unwrap_or_else(|| panic!("record without seq: {frame}"));
        if let Some(previous) = previous {
            assert_eq!(seq, previous + 1, "seq must be contiguous within a recipient log");
        }
        previous = Some(seq);
        let checksum = frame["checksum"].as_str().unwrap_or("");
        assert!(
            checksum.len() == 64 && checksum.bytes().all(|b| b.is_ascii_hexdigit()),
            "record without a sha256 checksum: {frame}"
        );
    }
}

/// Blueprint: per-team root `R=.team/station-v1/<team>` (accepted Astra correction 6). Two teams
/// with the same recipient name never share a storage file.
#[test]
fn red_m1_r10_two_teams_never_share_an_inbox_file() {
    // Inbox-only property: leaders are gone, so no tmux server is needed.
    let s = Station::new("r10");
    s.write_state(&snapshot(&s, &[
        ("alpha", team(&s, "alpha", None, &[("worker", None, "pi")])),
        ("beta", team(&s, "beta", None, &[("worker", None, "pi")])),
    ]));
    s.tools("worker", "alpha")
        .send_message(&MessageTarget::Single("leader".into()), "alpha-only-payload", None, None, None)
        .unwrap();
    s.tools("worker", "beta")
        .send_message(&MessageTarget::Single("leader".into()), "beta-only-payload", None, None, None)
        .unwrap();
    for path in s.files_containing("alpha-only-payload") {
        let bytes = fs::read(&path).unwrap();
        assert!(!contains(&bytes, b"beta-only-payload"), "{} mixes two teams", path.display());
    }
    assert_eq!(inbox_hits(&s.inbox("leader", "alpha"), "beta-only-payload").len(), 0);
}

/// Blueprint: G1/G2 tolerant reader. A record from a newer contract version must not break the
/// reader, and must not be presented as an ordinary v1 message.
#[test]
fn red_m1_r16_newer_version_records_stay_opaque_and_never_break_reading() {
    // Inbox-only property: the leader pane is gone, so no tmux server is needed.
    let s = Station::new("r16");
    s.write_state(&snapshot(&s, &[("alpha", team(&s, "alpha", None, &[("worker", None, "pi")]))]));
    let tools = s.tools("worker", "alpha");
    tools.send_message(&MessageTarget::Single("leader".into()), "known one", None, None, None).unwrap();
    let files = s.files_containing("known one");
    assert_eq!(files.len(), 1, "{files:?}");
    use std::io::Write;
    fs::OpenOptions::new()
        .append(true)
        .open(&files[0])
        .unwrap()
        .write_all(b"{\"v\":2,\"kind\":\"message\",\"content\":\"v2 must stay opaque\",\"text\":\"v2 must stay opaque\"}\n")
        .unwrap();
    tools.send_message(&MessageTarget::Single("leader".into()), "known two", None, None, None).unwrap();
    let inbox = s.inbox("leader", "alpha");
    assert_eq!(inbox_hits(&inbox, "known one").len(), 1, "{inbox}");
    assert_eq!(inbox_hits(&inbox, "known two").len(), 1, "{inbox}");
    for hit in inbox_hits(&inbox, "v2 must stay opaque") {
        let marked = hit.to_string().contains("opaque") && (hit.get("opaque").is_some() || hit["kind"] == "opaque");
        assert!(marked, "a v2 record may be listed only as an explicitly opaque entry, got {hit}");
    }
}

// ───────────────────── R-11/12 results ─────────────────────

/// Blueprint: C3 `idem` dedupe. An identical retry of report_result (lost response, MCP retry) is
/// one record, and both calls return the same id.
#[test]
fn red_m1_r11_identical_report_result_retry_is_idempotent() {
    // Inbox-only property: the leader pane is gone, so no tmux server is needed.
    let s = Station::new("r11");
    s.write_state(&snapshot(&s, &[("alpha", team(&s, "alpha", None, &[("worker", None, "pi")]))]));
    let tools = s.tools("worker", "alpha");
    let first = report(&tools, "same result twice", Some("T-9"));
    let second = report(&tools, "same result twice", Some("T-9"));
    assert_eq!(first["message_id"], second["message_id"], "retry must return the original id");
    assert_eq!(inbox_hits(&s.inbox("leader", "alpha"), "same result twice").len(), 1);
}

/// Blueprint: proposal C2 step 6 turn marker / M1.4. A task sent with `--task` is attributed to
/// the worker's next result without the coordinator or the SQLite messages table.
#[test]
fn red_m1_r12_report_result_is_attributed_to_the_last_submitted_task() {
    let mut s = Station::new("r12");
    let worker = s.receiver("worker");
    let leader = s.receiver("leader");
    s.write_state(&snapshot(&s, &[("alpha", team(&s, "alpha", Some(&leader), &[("worker", Some(&worker), "codex")]))]));
    let sent = s.cli(&["send", "worker", "please do T-42", "--task", "T-42", "--team", "alpha"], &[]);
    assert!(sent.status.success(), "{}", String::from_utf8_lossy(&sent.stdout));
    s.wait_received("worker", b"please do T-42");
    report(&s.tools("worker", "alpha"), "attributed result", None);
    let inbox = s.inbox("leader", "alpha");
    let hits = inbox_hits(&inbox, "attributed result");
    assert_eq!(hits.len(), 1, "{inbox}");
    let mut task_ids = Vec::new();
    find_string_field(&hits[0], "task_id", &mut task_ids);
    assert!(task_ids.contains(&"T-42"), "result must carry task T-42, found {task_ids:?}");
}

// ───────────────────── R-13..15 excision ratchets (M1.7) ─────────────────────

fn production_sources() -> Vec<(PathBuf, String)> {
    let crate_src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        for entry in fs::read_dir(dir).unwrap().flatten() {
            let path = entry.path();
            let name = path.file_name().unwrap().to_string_lossy().to_string();
            if path.is_dir() {
                if name != "tests" && name != "test_support" {
                    walk(&path, out);
                }
            } else if name.ends_with(".rs") && !name.ends_with("_tests.rs") && name != "tests.rs" {
                out.push(path);
            }
        }
    }
    walk(&crate_src, &mut files);
    files
        .into_iter()
        .map(|path| {
            let text = fs::read_to_string(&path).unwrap();
            (path, production_part(&text))
        })
        .collect()
}

/// Drop comment lines and everything from a trailing `#[cfg(test)] mod` onward.
fn production_part(text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let mut out = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        if trimmed == "#[cfg(test)]" {
            let next = lines[index + 1..].iter().map(|l| l.trim()).find(|l| !l.is_empty()).unwrap_or("");
            if next.starts_with("mod ") || next.starts_with("pub mod ") || next.starts_with("pub(crate) mod ") {
                break;
            }
        }
        out.push(if trimmed.starts_with("//") { "" } else { *line });
    }
    out.join("\n")
}

fn offenders(patterns: &[&str], skip_line: impl Fn(&str) -> bool) -> Vec<String> {
    let mut found = Vec::new();
    for (path, text) in production_sources() {
        for (number, line) in text.lines().enumerate() {
            if skip_line(line) {
                continue;
            }
            if patterns.iter().any(|pattern| line.contains(pattern)) {
                found.push(format!("{}:{}: {}", path.display(), number + 1, line.trim()));
            }
        }
    }
    found
}

/// M1.7: the coordinator queue pump no longer delivers anything.
#[test]
fn red_m1_r13_no_production_caller_of_the_queued_delivery_pump() {
    let found = offenders(&["deliver_pending_messages("], |line| line.contains("fn deliver_pending_messages"));
    assert!(found.is_empty(), "queued delivery pump still wired:\n{}", found.join("\n"));
}

/// M1.7 / taskbook item 1: no production path writes SQLite `messages`/`contract_outbox`
/// through the legacy funnels.
#[test]
fn red_m1_r14_no_production_caller_of_the_sqlite_message_funnels() {
    let found = offenders(
        &["messaging::send_message(", "report_result_for_owner_team(", "framework::enqueue("],
        |line| {
            line.contains("pub fn send_message")
                || line.contains("pub fn report_result_for_owner_team")
                || line.contains("fn report_result_for_owner_team_inner")
                || line.contains("pub fn enqueue")
        },
    )
    .into_iter()
    .filter(|line| !line.contains("/src/messaging/results.rs"))
    .collect::<Vec<_>>();
    assert!(found.is_empty(), "legacy SQLite message funnels still called:\n{}", found.join("\n"));
}

/// M1.7: the contract pump no longer delivers outbox rows or drains forwards.
#[test]
fn red_m1_r15_contract_pump_no_longer_delivers_or_forwards() {
    let found = offenders(&["deliver_one(&mut", "forward::drain("], |_| false);
    assert!(found.is_empty(), "contract outbox delivery still wired:\n{}", found.join("\n"));
}

// ───────────────────── guards (green on baseline, must stay green) ─────────────────────

/// Taskbook item 2. A stopped "coordinator" and a SQLite EXCLUSIVE lock on `team.db` must not
/// slow down or block CLI send, MCP send or report_result.
#[cfg(unix)]
#[test]
fn guard_m1_g1_stopped_coordinator_and_locked_database_do_not_block_messaging() {
    let mut s = Station::new("g1");
    let worker = s.receiver("worker");
    let leader = s.receiver("leader");
    s.write_state(&snapshot(&s, &[("alpha", team(&s, "alpha", Some(&leader), &[("worker", Some(&worker), "codex")]))]));
    let db_path = s.root.join(".team/runtime/team.db");
    let db = rusqlite::Connection::open(&db_path).unwrap();
    db.execute_batch("create table messages(id text); begin exclusive; insert into messages values('held');")
        .unwrap();
    let before = fs::read(&db_path).unwrap();
    let mut stopped = Command::new("sleep").arg("600").spawn().unwrap();
    let pid = stopped.id().to_string();
    assert!(Command::new("kill").args(["-STOP", &pid]).status().unwrap().success());
    fs::write(s.root.join(".team/runtime/coordinator.pid"), &pid).unwrap();
    fs::write(
        s.root.join(".team/runtime/coordinator.json"),
        json!({"pid": stopped.id(), "protocol_version": 2, "message_store_schema_version": 5}).to_string(),
    )
    .unwrap();

    let started = Instant::now();
    let cli = s.cli(&["send", "worker", "under lock via cli", "--team", "alpha"], &[]);
    assert!(cli.status.success(), "{}", String::from_utf8_lossy(&cli.stdout));
    s.wait_received("worker", b"under lock via cli");
    let tools = s.tools("worker", "alpha");
    tools.send_message(&MessageTarget::Single("leader".into()), "under lock via mcp", None, None, None).unwrap();
    s.wait_received("leader", b"under lock via mcp");
    report(&tools, "under lock result", None);
    let elapsed = started.elapsed();

    let _ = Command::new("kill").args(["-KILL", &pid]).status();
    let _ = stopped.wait();
    drop(db);
    assert!(elapsed < Duration::from_secs(5), "messaging waited on coordinator/db: {elapsed:?}");
    assert_eq!(fs::read(&db_path).unwrap().len(), before.len(), "team.db must not be written");
}

/// Blueprint: F1. An interrupted send must not wedge the seat; the next send succeeds without
/// manual repair.
#[cfg(unix)]
#[test]
fn guard_m1_g2_killed_send_does_not_wedge_the_next_send() {
    let mut s = Station::new("g2");
    let worker = s.receiver("worker");
    let leader = s.receiver("leader");
    s.write_state(&snapshot(&s, &[("alpha", team(&s, "alpha", Some(&leader), &[("worker", Some(&worker), "claude")]))]));
    let mut child = s
        .command(&["send", "worker", &"interrupted ".repeat(2_000), "--team", "alpha"], &[])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    std::thread::sleep(Duration::from_millis(40));
    let _ = child.kill();
    let _ = child.wait();
    let output = s.cli(&["send", "worker", "after the interrupted send", "--team", "alpha"], &[]);
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stdout));
    s.wait_received("worker", b"after the interrupted send");
}
