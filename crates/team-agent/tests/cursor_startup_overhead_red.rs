#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]
#![cfg(unix)]
//! RED (Item 1): `team-agent start --provider cursor` must not serialize
//! redundant native round-trips before the Cursor TUI is launched.
//!
//! Measured on 0.5.129 (live, healthy network): 4x `--version`, 2x
//! `--list-models` (backend::preflight and start_with_peer_transfer each run
//! discovery::discover_runtime), 1x `create-chat`, then the TUI — ~23s before
//! the pane even exists. Cursor accepts a locally allocated UUID with
//! `--resume <id>` and recalls it exactly, so `create-chat` is not required.
//!
//! The fake installation below records every invocation of the PATH-selected
//! `cursor-agent`; its TUI exits immediately, so only the pre-spawn sequence is
//! judged (the start itself is expected to fail at spawn).

#[path = "e2e/framework.rs"]
mod framework;

use framework::{run_ta_env, TestWorkspace};
use serde_json::Value;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

const VERSION: &str = "2026.10.22-27dcf5f";
const NATIVE_CHAT: &str = "11111111-2222-4333-8444-555555555555";

fn executable(path: &Path, body: &str) {
    fs::write(path, body).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
}

/// `<root>/cursor-fake/<VERSION>/{cursor-agent,node,index.js}`: the vendor
/// layout `cursor::native::probe_runtime` accepts. Returns the PATH directory.
fn fake_cursor(root: &Path, log: &Path) -> PathBuf {
    let dir = root.join("cursor-fake").join(VERSION);
    fs::create_dir_all(&dir).unwrap();
    let log = log.display();
    executable(
        &dir.join("cursor-agent"),
        &format!(
            "#!/bin/sh\n\
             case \"$1\" in\n\
             --version) echo --version >> '{log}'; echo {VERSION}; exit 0 ;;\n\
             --list-models) echo --list-models >> '{log}'; printf 'Available models\\n\\ngrok-4.7-high - Grok 4.7 High\\n\\nTip: use --model <id> (or /model <id> in interactive mode) to switch.\\n'; exit 0 ;;\n\
             create-chat) echo create-chat >> '{log}'; echo {NATIVE_CHAT}; exit 0 ;;\n\
             esac\n\
             echo \"TUI $*\" >> '{log}'\n\
             exit 7\n"
        ),
    );
    fs::copy("/bin/sleep", dir.join("node")).unwrap();
    fs::set_permissions(dir.join("node"), fs::Permissions::from_mode(0o700)).unwrap();
    fs::write(dir.join("index.js"), b"// fake entry\n").unwrap();
    dir
}

/// Kill exactly the SUT-created tmux endpoints on Drop.
fn own_sockets(ws: &TestWorkspace) {
    let Ok(raw) = fs::read_to_string(ws.state_json_path()) else { return };
    let state: Value = serde_json::from_str(&raw).unwrap_or(Value::Null);
    let mut sockets = vec![state["tmux_socket"].clone()];
    if let Some(teams) = state["teams"].as_object() {
        for team in teams.values() {
            sockets.push(team["tmux_socket"].clone());
        }
    }
    for socket in sockets.iter().filter_map(Value::as_str).filter(|s| !s.is_empty()) {
        let path = PathBuf::from(socket);
        let sut_owned = path.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("ta-"));
        if sut_owned && path.exists() {
            ws.register_owned_tmux_socket(&path);
        }
    }
}

fn is_uuid(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(index, byte)| {
            if [8, 13, 18, 23].contains(&index) { byte == b'-' } else { byte.is_ascii_hexdigit() }
        })
}

#[test]
fn cursor_start_runs_one_discovery_and_no_native_chat_allocation_before_the_tui() {
    let ws = TestWorkspace::new("cursor-overhead").with_owned_caller();
    let root = ws.caller.as_ref().unwrap().root().to_path_buf();
    let log = root.join("cursor-calls.log");
    let bin = fake_cursor(&root, &log);
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap_or_default());
    let out = run_ta_env(
        &ws,
        &[
            "start", "executor", "--provider", "cursor", "--model", "grok-4.7-high",
            "--bypass", "true", "--prompt", "Probe the startup overhead.", "--json",
        ],
        &[("PATH", path.as_str())],
    );
    own_sockets(&ws);
    let calls = fs::read_to_string(&log).unwrap_or_default();
    let lines: Vec<&str> = calls.lines().collect();
    let count = |name: &str| lines.iter().filter(|line| **line == name).count();
    let tui: Vec<&str> = lines.iter().copied().filter(|line| line.starts_with("TUI ")).collect();
    let context = format!(
        "cursor-agent calls before/at spawn:\n{calls}\nstart exit={} stdout={} stderr={}",
        out.exit_code, out.stdout, out.stderr
    );

    assert_eq!(tui.len(), 1, "the TUI must be reached exactly once\n{context}");
    assert_eq!(lines.last().copied(), tui.first().copied(), "nothing runs after the TUI exec\n{context}");
    let observed = (count("--list-models"), count("create-chat"), count("--version") <= 2);
    assert_eq!(
        observed,
        (1, 0, true),
        "expected exactly one catalog discovery, no native create-chat and at most two \
         version probes before the TUI; observed (list-models, create-chat, version<=2)={observed:?}\n{context}"
    );

    let args: Vec<&str> = tui[0].split_whitespace().collect();
    let resume = args.iter().position(|arg| *arg == "--resume")
        .and_then(|index| args.get(index + 1))
        .unwrap_or_else(|| panic!("the TUI still receives an exact preassigned session\n{context}"));
    assert!(is_uuid(resume), "the preassigned session is a canonical UUID: {resume}\n{context}");
    assert_ne!(*resume, NATIVE_CHAT, "the session ID must not come from native create-chat\n{context}");
}
