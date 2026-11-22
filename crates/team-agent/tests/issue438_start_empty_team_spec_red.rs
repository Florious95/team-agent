//! Issue #438 RED (public CLI journey): a team whose last agent was removed must
//! still be able to start an agent.
//!
//! Live incident (GraphEngineering, team `current`, runtime 0.6.0): the leader removed
//! every seat; the last `remove-agent` wrote `.team/runtime/current/team.spec.yaml`
//! with `agents: []`. Every later attempt to bring a seat back then exited 1 with
//!   {"ok":false,"phase":"planning_or_selection",
//!    "error":"validation error: team.spec.yaml validation failed:\n- /agents: must be a non-empty list"}
//! for `start astra --provider pi ...`, `agent start astra ...` and
//! `start --from .team/start-source/current --dry-run`. The team was alive, its
//! leader attached, and no CLI path could leave the zero-agent state: `remove`
//! writes a roster that `start` refuses to read.
//!
//! Scenario card (public CLI only, every step through the real binary):
//!   1. `start alpha --provider fake` creates the implicit team `current`; the pane comes up.
//!   2. `stop alpha`, then `remove-agent alpha --from-spec --confirm`: the runtime spec now has
//!      `agents: []` (asserted as the precondition, exactly the incident's on-disk state).
//!   3. Expected: `start astra ...` / `agent start astra ...` plan and start the new seat;
//!      no start path is refused because the existing roster is empty.
//!      Actual (defect): refused in planning_or_selection with
//!      "/agents: must be a non-empty list" before any seat is planned.
//!
//! Hermetic: isolated HOME/tmux via the e2e framework, the built-in fake worker for
//! panes, and a PATH `pi` stub that never launches. No installed provider, credentials,
//! or model cost.
#![cfg(unix)]
#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

#[path = "e2e/framework.rs"]
mod framework;

use framework::{run_ta, run_ta_env, tmux_windows_on_socket, wait_for, TaResult, TestWorkspace};
use serde_json::Value;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;
use team_agent::model::yaml::{self, Value as Yaml};

const TEAM: &str = "current";
const SEED: &str = "alpha";
const AGENT: &str = "astra";
const EMPTY_ROSTER_REFUSAL: &str = "/agents: must be a non-empty list";
const READY: Duration = Duration::from_secs(30);
const POLL: Duration = Duration::from_millis(200);
const FAKE_SEAT: &[&str] = &["--provider", "fake", "--model", "fake", "--bypass", "false"];

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

/// The defect's exact signature: the zero-agent roster on disk is rejected by spec
/// validation before any seat is planned.
#[track_caller]
fn not_refused_for_empty_roster(out: &TaResult, what: &str) {
    let all = format!("{}\n{}", out.stdout, out.stderr);
    assert!(
        !all.contains(EMPTY_ROSTER_REFUSAL),
        "#438 {what}: a team whose last agent was removed must accept a new seat, but the \
         command was refused because the existing roster is empty.\n  argv: {:?}\n  exit: {}\n  stdout: {}\n  stderr: {}",
        out.argv,
        out.exit_code,
        out.stdout,
        out.stderr
    );
    assert!(
        out.exit_code != 101 && !all.contains("panicked at"),
        "#438 {what}: CLI crashed: exit={} {all}",
        out.exit_code
    );
}

/// Register every SUT-created (`tmux-<uid>/ta-*`) endpoint so Drop kills exactly them.
fn own_sockets(ws: &TestWorkspace) {
    let Ok(raw) = fs::read_to_string(ws.state_json_path()) else { return };
    let state: Value = serde_json::from_str(&raw).unwrap_or(Value::Null);
    let sockets = [
        state["tmux_socket"].clone(),
        state.pointer(&format!("/teams/{TEAM}/tmux_socket")).cloned().unwrap_or(Value::Null),
        state.pointer(&format!("/teams/{TEAM}/leader_receiver/tmux_socket")).cloned().unwrap_or(Value::Null),
    ];
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

/// `agent` is a running seat of team `current` and its fake pane is live.
#[track_caller]
fn assert_seat_up(ws: &TestWorkspace, agent: &str) {
    let state = ws.read_state();
    let row = state
        .pointer(&format!("/teams/{TEAM}/agents/{agent}"))
        .unwrap_or_else(|| panic!("{agent} not admitted under teams.{TEAM}: {state}"))
        .clone();
    assert_eq!(row["status"], "running", "{agent} row: {row}");
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
}

fn runtime_roster(ws: &TestWorkspace) -> (PathBuf, Option<Vec<String>>, String) {
    let path = team_agent::model::paths::runtime_spec_path(ws.path(), TEAM);
    let raw = fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let ids = yaml::loads(&raw).unwrap().get("agents").and_then(Yaml::as_list).map(|agents| {
        agents.iter().filter_map(|a| a.get("id").and_then(Yaml::as_str).map(str::to_string)).collect()
    });
    (path, ids, raw)
}

/// Steps 1-2 of the card: the incident's zero-agent team, produced by the CLI itself.
fn zero_agent_team(tag: &str) -> TestWorkspace {
    let ws = TestWorkspace::new(tag).with_owned_caller();
    let mut args = vec!["start", SEED];
    args.extend_from_slice(FAKE_SEAT);
    args.push("--json");
    let out = run_ta(&ws, &args);
    own_sockets(&ws);
    ok(&out, "fixture: create team `current` with one fake seat");
    assert_seat_up(&ws, SEED);
    ok(&run_ta(&ws, &["stop", SEED, "--json"]), "fixture: stop the only seat");
    ok(&run_ta(&ws, &["remove-agent", SEED, "--from-spec", "--confirm", "--json"]), "fixture: remove the only seat");

    let (path, ids, raw) = runtime_roster(&ws);
    assert_eq!(
        ids.as_deref(),
        Some(&[][..]),
        "precondition: removing the last agent must leave the incident's `agents: []` roster at {}:\n{raw}",
        path.display()
    );
    let state = ws.read_state();
    assert!(
        state.pointer(&format!("/teams/{TEAM}/agents/{SEED}")).is_none() && state["agents"].get(SEED).is_none(),
        "precondition: the removed seat must be gone from state.json: {state}"
    );
    ws
}

#[track_caller]
fn assert_roster_lists(ws: &TestWorkspace, agent: &str) {
    let (path, ids, raw) = runtime_roster(ws);
    assert!(
        ids.is_some_and(|ids| ids.iter().any(|id| id == agent)),
        "{agent} missing from runtime roster {}:\n{raw}",
        path.display()
    );
}

#[test]
fn start_agent_on_a_zero_agent_team_plans_and_starts_the_seat() {
    let ws = zero_agent_team("i438-start");

    let mut dry = vec!["start", AGENT];
    dry.extend_from_slice(FAKE_SEAT);
    dry.extend(["--dry-run", "--json"]);
    let out = run_ta(&ws, &dry);
    not_refused_for_empty_roster(&out, "`start AGENT --dry-run`");
    let plan = ok(&out, "`start AGENT --dry-run` on a zero-agent team");
    assert!(plan["steps"].to_string().contains(AGENT), "dry-run must plan {AGENT}: {plan}");
    assert_eq!(runtime_roster(&ws).1.as_deref(), Some(&[][..]), "dry-run must not write the roster");

    let mut real = vec!["start", AGENT];
    real.extend_from_slice(FAKE_SEAT);
    real.push("--json");
    let out = run_ta(&ws, &real);
    own_sockets(&ws);
    not_refused_for_empty_roster(&out, "`start AGENT`");
    ok(&out, "`start AGENT` on a zero-agent team");
    assert_seat_up(&ws, AGENT);
    assert_roster_lists(&ws, AGENT);
}

#[test]
fn agent_start_on_a_zero_agent_team_plans_and_starts_the_seat() {
    let ws = zero_agent_team("i438-agentstart");

    let mut args = vec!["agent", "start", AGENT];
    args.extend_from_slice(FAKE_SEAT);
    args.push("--json");
    let out = run_ta(&ws, &args);
    own_sockets(&ws);
    not_refused_for_empty_roster(&out, "`agent start AGENT`");
    ok(&out, "`agent start AGENT` on a zero-agent team");
    assert_seat_up(&ws, AGENT);
    assert_roster_lists(&ws, AGENT);
}

/// The incident's exact argv (plus `--dry-run`, so the native provider is never
/// launched). Only the empty-roster refusal is in scope here: whatever the native
/// planner decides afterwards about the stubbed `pi` is not this defect.
#[test]
fn incident_argv_native_pi_start_is_not_refused_for_the_empty_roster() {
    let ws = zero_agent_team("i438-incident");
    let bin = ws.path().join("i438-bin");
    fs::create_dir_all(&bin).unwrap();
    let stub = bin.join("pi");
    fs::write(&stub, "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then echo 0.70.0; exit 0; fi\necho 'i438 pi stub must not launch' >&2\nexit 3\n").unwrap();
    fs::set_permissions(&stub, fs::Permissions::from_mode(0o755)).unwrap();
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap_or_default());

    let out = run_ta_env(
        &ws,
        &[
            "start", AGENT, "--provider", "pi", "--model", "openai-codex/gpt-6.2-astra", "--bypass", "true",
            "--workspace", ".", "--team", TEAM, "--dry-run", "--json",
        ],
        &[("PATH", path.as_str())],
    );
    not_refused_for_empty_roster(&out, "incident argv `start astra --provider pi ... --team current`");
}

/// The incident's third refused command: `start --from <the team's own role source>
/// --dry-run` (GraphEngineering used `.team/start-source/current`, which still held its
/// role files) while the runtime roster is empty. A foreign `--from` directory is refused
/// earlier ("source differs") and would not reach the defect, so the fixture uses the
/// team's recorded `team_dir`. Only the empty-roster refusal is in scope; how the planner
/// reconciles the source with the runtime team is not.
#[test]
fn whole_team_start_from_source_is_not_refused_for_the_empty_roster() {
    let ws = zero_agent_team("i438-from");
    let state = ws.read_state();
    let recorded = state
        .pointer(&format!("/teams/{TEAM}/team_dir"))
        .and_then(Value::as_str)
        .or_else(|| state["team_dir"].as_str())
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| panic!("precondition: team `{TEAM}` must record its role source (team_dir): {state}"));
    let source = if recorded.is_absolute() { recorded } else { ws.path().join(recorded) };
    fs::create_dir_all(source.join("agents")).unwrap();
    if !source.join("TEAM.md").exists() {
        fs::write(source.join("TEAM.md"), "---\nname: current\nprovider: fake\n---\nZero-agent recovery source.\n").unwrap();
    }
    let role = source.join("agents").join(format!("{AGENT}.md"));
    if !role.exists() {
        fs::write(
            &role,
            format!("---\nname: {AGENT}\nrole: assistant\nprovider: fake\nmodel: fake\ndangerously_skip_permissions: false\n---\nReply to the leader.\n"),
        )
        .unwrap();
    }

    let out = run_ta(&ws, &["start", "--from", source.to_str().unwrap(), "--dry-run", "--json"]);
    assert!(
        !out.stdout.contains("source differs from selected team's role source"),
        "fixture: `--from` must name the team's own role source {}: {}",
        source.display(),
        out.stdout
    );
    not_refused_for_empty_roster(&out, "`start --from <team role source> --dry-run`");
}
