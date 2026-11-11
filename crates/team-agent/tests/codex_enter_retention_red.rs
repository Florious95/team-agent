//! RED: a Codex recipient keeps a Team Agent message in its composer because the single
//! Enter never submits it, while the sender reports `status:"sent"`.
//!
//! Root cause A: the recipient pane is in tmux copy-mode (a mouse wheel-up over an inline
//! `--no-alt-screen` Codex pane runs `copy-mode -e`). `paste-buffer` still reaches the
//! application, but unframed, and copy-mode's key table swallows `send-keys Enter`.
//! Root cause B: the payload's last token starts with `@`/`$`, so Codex opens its Mentions
//! popup and the single Enter inserts the highlighted item instead of submitting.
//!
//! The receiver is a fake Codex composer: python3, raw mode, bracketed paste enabled. It
//! applies the rule measured on codex-cli 0.166.0. Enter while the draft's last token
//! starts with `@`/`$` is consumed by the popup; otherwise Enter submits the draft as one turn.
//! Everything runs on a private tmux server (`-S <tempdir>/tmux.sock -f /dev/null`).
#![cfg(unix)]

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

/// Native seats resolve to `codex_tui`; `codex` is the alias a configured dialect can use.
const CODEX_DIALECTS: [&str; 2] = ["codex_tui", "codex"];
const HEADER: &str = "Team Agent message from leader:\n\n";
/// a68f07e4: station send from an unbound CLI caller presents `from operator`.
const SEMANTIC_HEADER: &str = "Team Agent message from operator:\n\n";

const RECEIVER: &str = r#"import codecs, json, os, signal, sys, tty
from pathlib import Path
signal.alarm(110)  # bounded lifetime even if the owning test never drops
root = Path(sys.argv[1])
tty.setraw(0)
os.write(1, b"".join(b"history line %d\r\n" % i for i in range(120)))
os.write(1, b"\x1b[?2004h")
START, END = b"\x1b[200~", b"\x1b[201~"
raw = (root / "received").open("ab", buffering=0)
log = (root / "events.jsonl").open("a", buffering=1)
typed = codecs.getincrementaldecoder("utf-8")("replace")
def emit(kind, **fields):
    log.write(json.dumps({"event": kind, **fields}, ensure_ascii=False) + "\n")
def popup_open(draft):
    return bool(draft) and not draft[-1].isspace() and draft.split()[-1][:1] in ("@", "$")
(root / "ready").touch()
buf, draft, pasting = b"", "", False
while True:
    chunk = os.read(0, 65536)
    raw.write(chunk)
    buf += chunk
    while buf:
        if pasting:
            end = buf.find(END)
            if end < 0:
                break
            draft += buf[:end].decode("utf-8", "replace")
            buf, pasting = buf[end + len(END):], False
        elif buf.startswith(START):
            buf, pasting = buf[len(START):], True
        elif buf[:1] == b"\x1b" and START.startswith(buf):
            break
        elif buf[:1] == b"\r":
            if popup_open(draft):
                emit("popup_consumed_enter", draft=draft)
            else:
                emit("turn", text=draft)
                draft = ""
            buf = buf[1:]
        elif buf[:1] in (b"\x1b", b"\x03"):
            emit("stray_control", byte=buf[0])
            buf = buf[1:]
        else:
            stop = min([i for i in (buf.find(b"\r"), buf.find(b"\x1b"), buf.find(b"\x03")) if i >= 0] or [len(buf)])
            draft += typed.decode(buf[:stop])
            buf = buf[stop:]
"#;

struct CodexPane {
    root: PathBuf,
    socket: String,
    pane: String,
}

impl CodexPane {
    fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            // Short: a unix socket path is limited to 104 bytes on macOS.
            "ta-cer-{}-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_subsec_nanos(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        fs::create_dir_all(root.join(".team/runtime")).unwrap();
        fs::create_dir_all(root.join("home")).unwrap();
        let script = root.join("codex_composer.py");
        fs::write(&script, RECEIVER).unwrap();
        let socket = root.join("t.sock").to_string_lossy().into_owned();
        let output = Command::new("tmux")
            .args(["-S", &socket, "-f", "/dev/null", "new-session", "-d", "-x", "120", "-y", "40",
                "-P", "-F", "#{pane_id}", "-s", "codex-enter-red"])
            .arg(format!("python3 -I '{}' '{}'", script.display(), root.display()))
            .output()
            .unwrap();
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        let pane = String::from_utf8(output.stdout).unwrap().trim().to_string();
        let fixture = Self { root, socket, pane };
        fixture.wait(Duration::from_secs(5), || fixture.root.join("ready").exists());
        fixture
    }

    fn wait(&self, budget: Duration, mut done: impl FnMut() -> bool) -> bool {
        let deadline = Instant::now() + budget;
        while !done() {
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        true
    }

    fn tmux(&self, args: &[&str]) -> String {
        let output = Command::new("tmux").args(["-S", &self.socket]).args(args).output().unwrap();
        assert!(output.status.success(), "tmux {args:?}: {}", String::from_utf8_lossy(&output.stderr));
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    fn in_mode(&self) -> bool {
        self.tmux(&["display-message", "-p", "-t", &self.pane, "#{pane_in_mode}"]) != "0"
    }

    /// What tmux's root `WheelUpPane` binding runs for a pane with `alternate_on=0` and
    /// `mouse_any_flag=0` (inline Codex), followed by the user reading a few lines up.
    fn scroll_into_copy_mode(&self) {
        self.tmux(&["copy-mode", "-e", "-t", &self.pane]);
        self.tmux(&["send-keys", "-t", &self.pane, "-X", "-N", "5", "scroll-up"]);
        assert!(self.in_mode(), "fixture precondition: pane must be in copy-mode");
    }

    fn cli(&self, args: &[&str]) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_team-agent"));
        command.args(args).args(["--workspace", self.root.to_str().unwrap(), "--json"]);
        // Never run the real binary against the developer's real HOME.
        command.env("HOME", self.root.join("home"));
        for (key, _) in std::env::vars() {
            if key.starts_with("TEAM_AGENT_") || key == "TMUX" || key == "TMUX_PANE" {
                command.env_remove(key);
            }
        }
        command.output().unwrap()
    }

    fn raw_send(&self, dialect: &str, text: &str) -> Output {
        self.cli(&["send", "--socket", &self.socket, "--pane", &self.pane, "--dialect", dialect, text])
    }

    /// Leader -> worker through station send. The legacy snapshot carries the configured
    /// Codex dialect, so delivery runs `render()` + `send_with_receipt()` + the Codex key sequence.
    fn semantic_send(&self, dialect: &str, content: &str) -> Output {
        let state = json!({"active_team_key": "alpha", "tmux_socket": self.socket,
            "teams": {"alpha": {"team_id": "alpha", "team_name": "codex-enter", "status": "running",
                "tmux_socket": self.socket, "leader": {"pane_id": self.pane, "provider": "retired-provider"},
                "agents": {"worker": {"pane_id": self.pane, "provider": "retired-provider",
                    "dialect": dialect, "status": "running"}}}}});
        fs::write(self.root.join(".team/runtime/state.json"), serde_json::to_vec(&state).unwrap()).unwrap();
        self.cli(&["send", "worker", content, "--team", "alpha"])
    }

    fn events(&self) -> Vec<Value> {
        fs::read_to_string(self.root.join("events.jsonl")).unwrap_or_default()
            .lines().map(|line| serde_json::from_str(line).unwrap()).collect()
    }

    fn received(&self) -> Vec<u8> {
        fs::read(self.root.join("received")).unwrap_or_default()
    }

    /// The composer outcome after one delivery: the first Enter-driven event plus a settle
    /// window, so a second Enter or stray control byte is also observed.
    fn outcome(&self) -> Vec<Value> {
        self.wait(Duration::from_secs(3), || !self.events().is_empty());
        std::thread::sleep(Duration::from_millis(300));
        self.events()
    }
}

impl Drop for CodexPane {
    fn drop(&mut self) {
        let _ = Command::new("tmux").args(["-S", &self.socket, "kill-server"]).output();
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|window| window == needle)
}

/// Exactly one submitted turn, carrying `expected` unchanged. Only trailing whitespace may
/// follow it. The popup must never consume an Enter, and no ESC/C-c may reach the composer.
fn one_intact_turn(pane: &CodexPane, output: &Output, expected: &str) -> Result<(), String> {
    let stdout = String::from_utf8_lossy(&output.stdout);
    if !output.status.success() {
        return Err(format!("send failed: {stdout} {}", String::from_utf8_lossy(&output.stderr)));
    }
    let events = pane.outcome();
    let turns: Vec<&Value> = events.iter().filter(|event| event["event"] == "turn").collect();
    let intact = turns.len() == 1 && turns[0]["text"].as_str().is_some_and(|text| {
        text.strip_prefix(expected).is_some_and(|rest| rest.chars().all(char::is_whitespace))
    });
    if intact && turns.len() == events.len() {
        return Ok(());
    }
    Err(format!(
        "expected one submitted turn {expected:?}; composer events={events:?}; pane_in_mode={}; \
         received={:?}; send stdout={stdout}",
        pane.in_mode(),
        String::from_utf8_lossy(&pane.received())
    ))
}

/// Root cause A specifics: the payload must reach the app as a bracketed paste (tmux drops
/// `-p` framing while the pane is in a mode), and the pane must be left out of copy-mode.
fn framed_and_mode_cleared(pane: &CodexPane, expected: &str) -> Result<(), String> {
    let framed = [b"\x1b[200~".as_slice(), expected.as_bytes()].concat();
    match (contains(&pane.received(), &framed), pane.in_mode()) {
        (true, false) => Ok(()),
        (framed, in_mode) => Err(format!(
            "bracketed_paste={framed} pane_in_mode_after_send={in_mode}; received={:?}",
            String::from_utf8_lossy(&pane.received())
        )),
    }
}

fn report(failures: Vec<String>) {
    assert!(failures.is_empty(), "\n{}\n", failures.join("\n\n"));
}

#[test]
fn control_plain_codex_send_submits_exactly_one_turn() {
    let mut failures = Vec::new();
    for dialect in CODEX_DIALECTS {
        let pane = CodexPane::new();
        let text = format!("{HEADER}control payload\nsecond line 日本語");
        let output = pane.raw_send(dialect, &text);
        if let Err(error) = one_intact_turn(&pane, &output, &text).and_then(|()| framed_and_mode_cleared(&pane, &text)) {
            failures.push(format!("[{dialect}] {error}"));
        } else if pane.received().iter().filter(|byte| **byte == b'\r').count() != 1 {
            failures.push(format!("[{dialect}] expected exactly one Enter: {:?}", String::from_utf8_lossy(&pane.received())));
        }
    }
    report(failures);
}

#[test]
fn copy_mode_pane_raw_send_cancels_mode_and_submits_one_turn() {
    let mut failures = Vec::new();
    for dialect in CODEX_DIALECTS {
        let pane = CodexPane::new();
        pane.scroll_into_copy_mode();
        let text = format!("{HEADER}请立即展开联调跑测并汇报首轮数据！");
        let output = pane.raw_send(dialect, &text);
        if let Err(error) = one_intact_turn(&pane, &output, &text).and_then(|()| framed_and_mode_cleared(&pane, &text)) {
            failures.push(format!("[A copy-mode raw {dialect}] {error}"));
        }
    }
    report(failures);
}

#[test]
fn copy_mode_pane_semantic_send_cancels_mode_and_submits_one_turn() {
    let pane = CodexPane::new();
    pane.scroll_into_copy_mode();
    let content = "致 Astra (Codex)：请立即运行你的 20 个测试 Drill。完成后立即汇报！";
    let output = pane.semantic_send("codex_tui", content);
    let expected = format!("{SEMANTIC_HEADER}{content}");
    let result = one_intact_turn(&pane, &output, &expected).and_then(|()| framed_and_mode_cleared(&pane, &expected));
    report(result.err().map(|error| format!("[A copy-mode semantic codex_tui] {error}")).into_iter().collect());
}

#[test]
fn trailing_mention_tail_raw_send_submits_one_intact_turn() {
    let tails = ["cc @leader", "see @src/main.rs", "using $leader", "ping me @", "costs 5 $", "并抄送 @leader"];
    let mut failures = Vec::new();
    for dialect in CODEX_DIALECTS {
        for tail in tails {
            let pane = CodexPane::new();
            let text = format!("{HEADER}Reply with the single word PONG, {tail}");
            let output = pane.raw_send(dialect, &text);
            if let Err(error) = one_intact_turn(&pane, &output, &text) {
                failures.push(format!("[B mention raw {dialect} {tail:?}] {error}"));
            }
        }
    }
    report(failures);
}

#[test]
fn trailing_mention_tail_semantic_send_submits_one_intact_turn() {
    let mut failures = Vec::new();
    for content in ["Report back to @leader", "Load the skill $leader"] {
        let pane = CodexPane::new();
        let output = pane.semantic_send("codex_tui", content);
        if let Err(error) = one_intact_turn(&pane, &output, &format!("{SEMANTIC_HEADER}{content}")) {
            failures.push(format!("[B mention semantic codex_tui {content:?}] {error}"));
        }
    }
    report(failures);
}
