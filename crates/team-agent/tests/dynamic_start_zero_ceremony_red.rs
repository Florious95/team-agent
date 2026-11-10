//! Zero-ceremony dynamic start.
//!
//! A leader opened in a brand-new directory runs `team-agent start <agent>
//! [args]` with no TEAM.md, no agents/ and no team name. The seat must be
//! admitted under the implicit team `current` and its pane must come up.
//! Trailing `--provider/--bypass/--model/--prompt` customize the new seat;
//! without them the role defaults from the calling leader (provider) and the
//! safe bypass value (false).
//!
//! Black-box: real CLI binary, real tmux, fake-worker backed panes, isolated
//! HOME. RED on 0.5.129 / 4f6640b6: every case refuses with
//! "no initialized team; use start --from TEAMDIR".
#![cfg(unix)]
#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

#[path = "e2e/framework.rs"]
mod framework;
#[path = "support/hermetic.rs"]
mod hermetic;

use framework::{run_ta, run_ta_env, ta_binary, tmux_windows_on_socket, wait_for, TaResult, TestWorkspace};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;
use team_agent::model::yaml::{self, Value as Yaml};

const TEAM: &str = "current";
const READY: Duration = Duration::from_secs(30);
const POLL: Duration = Duration::from_millis(200);

fn start(ws: &TestWorkspace, agent: &str, extra: &[&str]) -> TaResult {
    let mut args = vec!["start", agent];
    args.extend_from_slice(extra);
    args.push("--json");
    run_ta(ws, &args)
}

#[track_caller]
fn ok(out: &TaResult, what: &str) -> Value {
    assert!(
        out.is_success(),
        "{what}: exit={} stdout={} stderr={}",
        out.exit_code,
        out.stdout,
        out.stderr
    );
    let body = out.json();
    assert_eq!(body["ok"], true, "{what}: {body}");
    body
}

/// Register every SUT-created (`tmux-<uid>/ta-*`) endpoint so Drop kills
/// exactly them. The fixture caller's own socket stays with its fixture.
fn own_sockets(ws: &TestWorkspace) {
    let Ok(raw) = fs::read_to_string(ws.state_json_path()) else { return };
    let state: Value = serde_json::from_str(&raw).unwrap_or(Value::Null);
    let mut sockets = vec![state["tmux_socket"].clone()];
    sockets.push(state.pointer(&format!("/teams/{TEAM}/tmux_socket")).cloned().unwrap_or(Value::Null));
    sockets.push(
        state.pointer(&format!("/teams/{TEAM}/leader_receiver/tmux_socket")).cloned().unwrap_or(Value::Null),
    );
    let mut seen = Vec::new();
    for socket in sockets.iter().filter_map(Value::as_str).filter(|s| !s.is_empty()) {
        let path = PathBuf::from(socket);
        let sut_owned = path.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("ta-"))
            && path.parent().and_then(Path::file_name).and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("tmux-"));
        if sut_owned && path.exists() && !seen.contains(&path) {
            ws.register_owned_tmux_socket(&path);
            seen.push(path);
        }
    }
}

fn pane_text(socket: &str, target: &str) -> String {
    Command::new("tmux")
        .args(["-S", socket, "capture-pane", "-p", "-J", "-S", "-200", "-t", target])
        .stdin(Stdio::null())
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default()
}

fn same_provider(actual: &str, expected: &str) -> bool {
    let norm = |p: &str| if p == "claude_code" { "claude".to_string() } else { p.to_string() };
    norm(actual) == norm(expected)
}

/// `agent` is a running seat of the implicit team and its pane is live.
#[track_caller]
fn assert_seat_up(ws: &TestWorkspace, agent: &str, provider: &str) -> Value {
    let state = ws.read_state();
    assert_eq!(state["active_team_key"], TEAM, "implicit team must be `current`: {state}");
    let row = state
        .pointer(&format!("/teams/{TEAM}/agents/{agent}"))
        .unwrap_or_else(|| panic!("{agent} not admitted under teams.{TEAM}: {state}"))
        .clone();
    assert_eq!(row["status"], "running", "{agent} row: {row}");
    let actual = row["provider"].as_str().unwrap_or_default();
    assert!(same_provider(actual, provider), "{agent} provider {actual:?} != {provider:?}: {row}");
    let socket = state["tmux_socket"].as_str().filter(|s| !s.is_empty())
        .unwrap_or_else(|| panic!("no tmux_socket recorded: {state}"));
    let session = state["session_name"].as_str().filter(|s| !s.is_empty())
        .unwrap_or_else(|| panic!("no session_name recorded: {state}"));
    let windows = tmux_windows_on_socket(socket, session);
    assert!(windows.iter().any(|w| w == agent), "no {agent} window in {session}@{socket}: {windows:?}");
    let target = format!("{session}:{agent}");
    let marker = format!("TEAM_AGENT_FAKE_READY agent={agent}");
    assert!(
        wait_for(|| pane_text(socket, &target).contains(&marker), READY, POLL),
        "{agent} pane never came up; pane={}",
        pane_text(socket, &target)
    );
    row
}

fn executable(path: &Path, body: &str) {
    fs::write(path, body).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

fn spec_agent(ws: &TestWorkspace, agent: &str) -> Yaml {
    let path = team_agent::model::paths::runtime_spec_path(ws.path(), TEAM);
    let raw = fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    yaml::loads(&raw)
        .unwrap()
        .get("agents")
        .and_then(Yaml::as_list)
        .and_then(|agents| agents.iter().find(|a| a.get("id").and_then(Yaml::as_str) == Some(agent)).cloned())
        .unwrap_or_else(|| panic!("{agent} missing from runtime spec {}: {raw}", path.display()))
}

fn snapshot(root: &Path) -> BTreeMap<PathBuf, Option<Vec<u8>>> {
    fn visit(root: &Path, at: &Path, map: &mut BTreeMap<PathBuf, Option<Vec<u8>>>) {
        for entry in fs::read_dir(at).unwrap() {
            let path = entry.unwrap().path();
            let key = path.strip_prefix(root).unwrap().to_path_buf();
            if key.starts_with(".e2e-last-command-observation.json") {
                continue; // fixture's own receipt, not SUT output
            }
            if path.is_dir() {
                map.insert(key, None);
                visit(root, &path, map);
            } else {
                map.insert(key, Some(fs::read(&path).unwrap()));
            }
        }
    }
    let mut map = BTreeMap::new();
    visit(root, root, &mut map);
    map
}

#[track_caller]
fn assert_virgin(ws: &TestWorkspace) {
    let entries = snapshot(ws.path());
    assert!(entries.is_empty(), "precondition: brand-new empty directory, found {:?}", entries.keys());
}

#[test]
fn start_in_empty_directory_admits_worker_under_current_and_brings_up_pane() {
    let ws = TestWorkspace::new("dynstart-empty").with_owned_caller();
    assert_virgin(&ws);

    let out = start(&ws, "worker-1", &["--provider", "fake", "--bypass", "false"]);
    own_sockets(&ws);
    ok(&out, "zero-ceremony start in an empty directory");
    let first = assert_seat_up(&ws, "worker-1", "fake");

    // Re-running the same command is idempotent: the live pane is preserved.
    let again = ok(&start(&ws, "worker-1", &[]), "repeat start of a live dynamic seat");
    let row = assert_seat_up(&ws, "worker-1", "fake");
    assert_eq!(row["pane_id"], first["pane_id"], "repeat start must not respawn: {again}");

    // The synthesized role is durable: stop + bare start needs no ceremony.
    let stop = run_ta(&ws, &["stop", "worker-1", "--json"]);
    ok(&stop, "stop dynamic seat");
    ok(&start(&ws, "worker-1", &[]), "bare restart of a stopped dynamic seat");
    assert_seat_up(&ws, "worker-1", "fake");
}

#[test]
fn start_without_trailing_args_defaults_role_from_calling_leader() {
    // The fixture leader is TEAM_AGENT_LEADER_PROVIDER=fake.
    let ws = TestWorkspace::new("dynstart-default").with_owned_caller();
    assert_virgin(&ws);

    let out = start(&ws, "worker-1", &[]);
    own_sockets(&ws);
    ok(&out, "bare `start worker-1` in an empty directory");
    assert_seat_up(&ws, "worker-1", "fake");
    let spec = spec_agent(&ws, "worker-1");
    assert!(
        matches!(spec.get("dangerously_skip_permissions"), Some(Yaml::Bool(false))),
        "default role must not silently enable bypass: {spec:?}"
    );
}

#[test]
fn trailing_arguments_customize_the_new_agent() {
    let ws = TestWorkspace::new("dynstart-args").with_owned_caller();
    assert_virgin(&ws);
    let prompt = "Own the parser module and report findings.";

    let out = start(
        &ws,
        "worker-2",
        &["--provider", "fake", "--bypass", "false", "--model", "fake", "--prompt", prompt],
    );
    own_sockets(&ws);
    ok(&out, "start with trailing customization");
    assert_seat_up(&ws, "worker-2", "fake");
    let spec = spec_agent(&ws, "worker-2");
    assert_eq!(spec.get("model").and_then(Yaml::as_str), Some("fake"), "{spec:?}");
    assert!(format!("{spec:?}").contains(prompt), "--prompt must become the role prompt: {spec:?}");
}

#[test]
fn dry_run_and_invalid_arguments_leave_an_empty_directory_untouched() {
    let ws = TestWorkspace::new("dynstart-dry").with_owned_caller();
    assert_virgin(&ws);

    let body = ok(&start(&ws, "worker-1", &["--dry-run"]), "dry-run zero-ceremony start");
    assert_eq!(body["scope"]["team"], TEAM, "dry-run must plan under `current`: {body}");
    let steps = body["steps"].as_array().cloned().unwrap_or_default();
    assert!(steps.iter().any(|s| s["agent"] == "worker-1"), "dry-run must plan worker-1: {body}");
    assert_virgin(&ws);

    for bad in [&["--provider", "imaginary"][..], &["--bypass", "maybe"][..], &["--bypass"][..]] {
        let out = start(&ws, "worker-1", bad);
        assert!(!out.is_success(), "invalid {bad:?} must refuse: {}", out.stdout);
        assert_virgin(&ws);
    }
}

#[test]
fn a_failed_dynamic_start_does_not_poison_the_retry() {
    let ws = TestWorkspace::new("dynstart-retry").with_owned_caller();
    assert_virgin(&ws);
    // A provider whose launch preflight fails: the first attempt cannot start a seat.
    let bin = ws.caller.as_ref().unwrap().root().join("dynstart-retry-bin");
    fs::create_dir_all(&bin).unwrap();
    executable(&bin.join("pi"), "#!/bin/sh\nexit 1\n");
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap_or_default());
    let first = run_ta_env(
        &ws,
        &["start", "worker-1", "--provider", "pi", "--model", "openai-codex/gpt-6-luna", "--bypass", "false", "--json"],
        &[("PATH", path.as_str())],
    );
    own_sockets(&ws);
    assert!(!first.is_success(), "broken provider must fail the first attempt: {}", first.stdout);

    // The user corrects the trailing arguments; the retry must not be refused.
    let retry = start(&ws, "worker-1", &["--provider", "fake", "--bypass", "false"]);
    own_sockets(&ws);
    ok(&retry, &format!("retry with corrected arguments after a failed first attempt ({})", first.stdout));
    assert_seat_up(&ws, "worker-1", "fake");
}

/// The real journey: `team-agent claude` opens a leader in an empty directory
/// (a PATH `claude` shim stands in for Claude Code), then the leader's tool
/// context runs `start`. A bare start inherits the leader's provider.
#[test]
fn start_from_a_leader_opened_by_the_launcher_in_an_empty_directory() {
    let ws = TestWorkspace::new("dynstart-leader").with_owned_caller();
    assert_virgin(&ws);
    let caller = ws.caller.as_ref().unwrap();
    let lab = caller.root().join("dynstart-lab");
    let bin = lab.join("bin");
    fs::create_dir_all(&bin).unwrap();
    let leader_env = lab.join("leader.env");
    // Fail closed: a default that resolves to Pi must never launch the host's real Pi.
    executable(&bin.join("pi"), "#!/bin/sh\nexit 1\n");
    executable(
        &bin.join("claude"),
        &format!(
            r#"#!/bin/sh
if [ -n "${{TEAM_AGENT_AGENT_ID:-}}" ]; then
  env > '{lab}/worker-'"$TEAM_AGENT_AGENT_ID"'.env'
  exec '{ta}' fake-worker --workspace "$TEAM_AGENT_WORKSPACE" --agent-id "$TEAM_AGENT_AGENT_ID"
fi
case "${{1:-}}" in
  --version|-v) echo '2.1.309 (Claude Code)'; exit 0 ;;
  mcp|config|doctor|auth|update|--help|-h) exit 0 ;;
esac
env >'{lab}/leader.env.tmp' && mv '{lab}/leader.env.tmp' '{env}'
sleep 60 & wait $!
"#,
            lab = lab.display(),
            ta = ta_binary().display(),
            env = leader_env.display(),
        ),
    );

    // The launcher needs a terminal: host it in an exact-owned tmux server.
    let host = hermetic::short_tmux_socket("dynlead");
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap_or_default());
    let mut launch = Command::new(hermetic::fixture_tmux_binary());
    launch
        .args(["-u", "-S"])
        .arg(&host)
        .args(["-f", "/dev/null", "new-session", "-d", "-s", "launch", "-x", "200", "-y", "50", "-c"])
        .arg(ws.path())
        .arg(format!("exec '{}' claude", ta_binary().display()))
        .env("HOME", caller.home())
        .env("PATH", &path);
    for key in hermetic::CALLER_IDENTITY_ENVS {
        launch.env_remove(key);
    }
    let out = launch.output().unwrap();
    assert!(out.status.success(), "host tmux: {out:?}");
    caller.register_owned_tmux_socket(&host);
    let launched = wait_for(|| leader_env.is_file(), READY, POLL);
    own_sockets(&ws);
    assert!(
        launched,
        "launcher never opened the leader; host pane={}",
        pane_text(host.to_str().unwrap(), "launch")
    );
    // Leader identity only; endpoint annotations may be filled in later.
    let owner = |ws: &TestWorkspace| {
        let state = ws.read_state();
        let owner = &state["teams"][TEAM]["team_owner"];
        ["pane_id", "provider", "leader_session_uuid", "owner_epoch"].map(|key| owner[key].clone())
    };
    let owner_before = owner(&ws);
    assert!(owner_before[0].is_string(), "launcher must incept a leader for `current`: {}", ws.read_state());

    let leader_run = |args: &[&str]| -> TaResult {
        let mut command = Command::new(ta_binary());
        command.args(args).current_dir(ws.path()).stdin(Stdio::null()).env_clear();
        for line in fs::read_to_string(&leader_env).unwrap().lines() {
            if let Some((key, value)) = line.split_once('=') {
                if !key.is_empty() && key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
                    command.env(key, value);
                }
            }
        }
        let out = command.output().unwrap();
        TaResult {
            argv: args.iter().map(|s| s.to_string()).collect(),
            exit_code: out.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        }
    };

    let out = leader_run(&["start", "worker-1", "--provider", "fake", "--bypass", "false", "--json"]);
    own_sockets(&ws);
    ok(&out, "leader starts an explicit dynamic seat");
    assert_seat_up(&ws, "worker-1", "fake");

    let out = leader_run(&["start", "worker-2", "--json"]);
    own_sockets(&ws);
    ok(&out, "leader starts a bare dynamic seat");
    assert_seat_up(&ws, "worker-2", "claude");
    assert!(lab.join("worker-worker-2.env").is_file(), "bare seat must launch through the leader's provider command");

    assert_eq!(owner(&ws), owner_before, "dynamic start must not displace the leader");
}
