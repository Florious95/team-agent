//! 0.6.2 decision D1: durable native authority lives in a user-private state root
//! ($XDG_STATE_HOME, else $HOME/.local/state), never inside the workspace, so a
//! DrvFS/9p workspace that cannot hold 0700 does not weaken any authority fence.
//! Old live teams in the 0.6.1 in-workspace layout keep working; two competing
//! roots for one scope are refused explicitly instead of silently picking one.
#![cfg(unix)]
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{json, Value};

#[path = "support/direct_fixture.rs"]
mod direct_fixture;
#[path = "support/direct_native_fixture.rs"]
mod direct_native_fixture;

const CHILD: &str = "state_home_root_child_process";

/// Child process: print the authority root the product selects for TA_D1_WS under
/// this process's real environment (no in-process env mutation in parallel tests).
#[test]
#[ignore = "child process of native_authority_state_home_red"]
fn state_home_root_child_process() {
    let Some(ws) = std::env::var_os("TA_D1_WS") else { return };
    let cwd = team_agent_contract::host::process::resolve_cwd(Path::new(&ws)).unwrap();
    let scope = team_agent::contract_runtime::backend::scope_for(&cwd, "alpha").unwrap();
    println!("ROOT={}", team_agent::contract_runtime::backend::root_for(Path::new(&ws), &scope).display());
}

fn selected_root(ws: &Path, env: &[(&str, Option<&Path>)]) -> PathBuf {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command.args(["--exact", CHILD, "--ignored", "--nocapture", "--test-threads=1"]).env("TA_D1_WS", ws);
    for (key, value) in env {
        match value { Some(value) => { command.env(key, value); } None => { command.env_remove(key); } }
    }
    let output = command.output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "{stdout}\n{}", String::from_utf8_lossy(&output.stderr));
    PathBuf::from(stdout.lines().find_map(|line| line.split("ROOT=").nth(1)).expect("child printed no root").trim())
}

fn scratch(tag: &str) -> PathBuf {
    let base = std::env::temp_dir().canonicalize().unwrap().join(format!(
        "ta-d1-{tag}-{}-{}", std::process::id(), chrono::Utc::now().timestamp_nanos_opt().unwrap()));
    std::fs::create_dir_all(&base).unwrap();
    base
}

fn legacy_root(ws: &Path, scope: &team_agent_contract::contract::types::ScopeId) -> PathBuf {
    // The 0.6.0/0.6.1 layout (168f990054): <workspace>/.team/runtime/contract/<scope>.
    ws.join(".team/runtime/contract").join(scope.as_str())
}

struct NativeTeam {
    fixture: direct_fixture::DirectFixture,
    _native: direct_native_fixture::NativeAuthority,
    ws: PathBuf,
}

fn native_team(binding_roots: impl FnOnce(&Path, &team_agent_contract::contract::types::ScopeId) -> Vec<PathBuf>) -> NativeTeam {
    let fixture = direct_fixture::DirectFixture::new();
    let ws = fixture.root.canonicalize().unwrap();
    let mut state = json!({"active_team_key": "alpha", "tmux_socket": fixture.socket,
        "teams": {"alpha": {"team_id": "alpha", "team_name": "friendly", "status": "running",
            "tmux_socket": fixture.socket, "leader": {"pane_id": fixture.leader_pane, "provider": "retired-provider"},
            "agents": {"worker": {"pane_id": fixture.pane, "provider": "codex", "status": "running"}}}}});
    let native = direct_native_fixture::NativeAuthority::start_with_binding_roots(&ws, "codex",
        |scope| { let mut roots = binding_roots(&ws, scope); roots.dedup(); roots });
    let worker = &mut state["teams"]["alpha"]["agents"]["worker"];
    worker["runtime_family"] = json!("contract");
    worker["native_endpoint"] = json!(native.socket);
    worker["pane_id"] = json!(native.pane);
    worker["pane_pid"] = json!(native.pid);
    worker["pane_birth_identity"] = json!(team_agent_contract::host::process::process_birth_identity(native.pid).unwrap());
    fixture.set_state(&state);
    NativeTeam { fixture, _native: native, ws }
}

fn send(team: &NativeTeam, state_home: &Path, text: &str) -> Value {
    let output = Command::new(env!("CARGO_BIN_EXE_team-agent"))
        .args(["send", "worker", text, "--team", "alpha", "--workspace"]).arg(&team.ws).arg("--json")
        .env("XDG_STATE_HOME", state_home)
        .env_clear_team_agent()
        .output().unwrap();
    assert!(output.status.success(), "stdout:\n{}\nstderr:\n{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
    serde_json::from_slice(&output.stdout).unwrap()
}

/// RED on 0.6.1: the root is <workspace>/.team/runtime/contract/<scope>.
#[test]
fn authority_root_resolves_under_private_state_home_not_workspace() {
    let base = scratch("root");
    let ws = base.join("ws");
    std::fs::create_dir_all(&ws).unwrap();
    let (state, home) = (base.join("state"), base.join("home"));
    let explicit = selected_root(&ws, &[("XDG_STATE_HOME", Some(&state)), ("HOME", Some(&home))]);
    let fallback = selected_root(&ws, &[("XDG_STATE_HOME", None), ("HOME", Some(&home))]);
    let relative = selected_root(&ws, &[("XDG_STATE_HOME", Some(Path::new("relative/state"))), ("HOME", Some(&home))]);
    let _ = std::fs::remove_dir_all(&base);
    for (case, root, expected) in [("XDG_STATE_HOME", &explicit, state.clone()),
        ("HOME fallback", &fallback, home.join(".local/state")),
        ("relative XDG_STATE_HOME is ignored", &relative, home.join(".local/state"))] {
        assert!(root.starts_with(&expected) && !root.starts_with(&ws),
            "{case}: authority root {} must be under {} and outside the workspace {}", root.display(), expected.display(), ws.display());
    }
}

/// INVARIANT (green on 0.6.1, must stay green): a live team published in the 0.6.1
/// in-workspace layout keeps receiving sends after the upgrade.
#[test]
fn legacy_in_workspace_authority_of_a_live_team_keeps_delivering() {
    let base = scratch("legacy");
    let state_home = base.join("state");
    let team = native_team(|ws, scope| vec![legacy_root(ws, scope)]);
    let receipt = send(&team, &state_home, "legacy layout still delivers");
    drop(team);
    let _ = std::fs::remove_dir_all(&base);
    assert_eq!(receipt["delivery"], "physical", "{receipt}");
}

/// RED on 0.6.1 (only one location exists, so it delivers): authority published at
/// both the legacy and the state-home root for one scope is refused, with no input.
#[test]
fn competing_authority_roots_are_refused_explicitly() {
    let base = scratch("competing");
    let state_home = base.join("state");
    let team = native_team(|ws, scope| {
        let state_root = selected_root(ws, &[("XDG_STATE_HOME", Some(&state_home))]);
        vec![legacy_root(ws, scope), state_root]
    });
    let receipt = send(&team, &state_home, "competing roots must not deliver");
    drop(team);
    let _ = std::fs::remove_dir_all(&base);
    assert_ne!(receipt["delivery"], "physical", "competing authority roots were silently resolved: {receipt}");
    assert_eq!(receipt["live"], false, "{receipt}");
    assert!(receipt["delivery_error"].as_str().is_some_and(|error| !error.is_empty()), "{receipt}");
}

/// INVARIANT for DrvFS/9p: with authority in the state root, world-writable mode bits
/// on the workspace .team tree (what a 0777 drvfs mount reports) never block send.
#[test]
fn send_ignores_world_writable_workspace_tree_with_state_home_authority() {
    use std::os::unix::fs::PermissionsExt;
    let base = scratch("drvfs");
    let state_home = base.join("state");
    let team = native_team(|ws, _| vec![selected_root(ws, &[("XDG_STATE_HOME", Some(&state_home))])]);
    let mut stack = vec![team.ws.join(".team")];
    while let Some(path) = stack.pop() {
        let meta = std::fs::symlink_metadata(&path).unwrap();
        if meta.file_type().is_symlink() || meta.file_type().is_socket_like() { continue; }
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o777)).unwrap();
        if meta.is_dir() { stack.extend(std::fs::read_dir(&path).unwrap().map(|entry| entry.unwrap().path())); }
    }
    let receipt = send(&team, &state_home, "world-writable workspace still delivers");
    drop(team);
    let _ = std::fs::remove_dir_all(&base);
    assert_eq!(receipt["delivery"], "physical", "{receipt}");
}

trait SocketLike { fn is_socket_like(&self) -> bool; }
impl SocketLike for std::fs::FileType {
    fn is_socket_like(&self) -> bool {
        use std::os::unix::fs::FileTypeExt;
        self.is_socket() || self.is_fifo()
    }
}

/// The sender is an unbound operator: drop every inherited framework identity.
trait ClearTeamAgent { fn env_clear_team_agent(&mut self) -> &mut Self; }
impl ClearTeamAgent for Command {
    fn env_clear_team_agent(&mut self) -> &mut Self {
        for (key, _) in std::env::vars().filter(|(key, _)| key.starts_with("TEAM_AGENT_")) { self.env_remove(key); }
        self
    }
}
