//! Shared D1 (state-home authority) fixture helpers: a real native seat whose
//! framework binding is published at explicit roots, plus env-isolated CLI calls.
#![allow(dead_code)]
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::{json, Value};
use team_agent_contract::contract::types::ScopeId;

#[path = "direct_fixture.rs"]
pub mod direct_fixture;
#[path = "direct_native_fixture.rs"]
pub mod direct_native_fixture;

pub const BINDING: &str = "framework-binding.json";
pub const ARCHIVE: &str = "framework-binding.pre-state-home.json";
pub const INTENT: &str = ".state-home-prepared.json";

pub fn scratch(tag: &str) -> PathBuf {
    let base = std::env::temp_dir().canonicalize().unwrap().join(format!(
        "ta-d1-{tag}-{}-{}", std::process::id(), chrono::Utc::now().timestamp_nanos_opt().unwrap()));
    std::fs::create_dir_all(&base).unwrap();
    base
}

/// The 0.6.0/0.6.1 layout (168f990054): <workspace>/.team/runtime/contract/<scope>.
pub fn legacy_root(ws: &Path, scope: &ScopeId) -> PathBuf {
    ws.join(".team/runtime/contract").join(scope.as_str())
}

pub struct NativeTeam {
    pub fixture: direct_fixture::DirectFixture,
    pub native: Option<direct_native_fixture::NativeAuthority>,
    pub ws: PathBuf,
    pub scope: ScopeId,
}

pub fn native_team(binding_roots: impl FnOnce(&Path, &ScopeId) -> Vec<PathBuf>) -> NativeTeam {
    team(binding_roots, false)
}

/// Quiescent: the native seat was started and then torn down through the contract
/// Lifecycle, so `team-agent migrate` may cut it over.
pub fn quiescent_team(binding_roots: impl FnOnce(&Path, &ScopeId) -> Vec<PathBuf>) -> NativeTeam {
    team(binding_roots, true)
}

fn team(binding_roots: impl FnOnce(&Path, &ScopeId) -> Vec<PathBuf>, stopped: bool) -> NativeTeam {
    let fixture = direct_fixture::DirectFixture::new();
    let ws = fixture.root.canonicalize().unwrap();
    let mut state = json!({"active_team_key": "alpha", "tmux_socket": fixture.socket,
        "teams": {"alpha": {"team_id": "alpha", "team_name": "friendly", "status": "running",
            "tmux_socket": fixture.socket, "leader": {"pane_id": fixture.leader_pane, "provider": "retired-provider"},
            "agents": {"worker": {"pane_id": fixture.pane, "provider": "codex", "status": "running"}}}}});
    let mut scope = None;
    let roots = |selected: &ScopeId| {
        scope = Some(selected.clone());
        let mut roots = binding_roots(&ws, selected);
        roots.dedup();
        roots
    };
    let native = if stopped {
        direct_native_fixture::NativeAuthority::start_quiescent_with_binding_roots(&ws, "codex", roots)
    } else {
        direct_native_fixture::NativeAuthority::start_with_binding_roots(&ws, "codex", roots)
    };
    let worker = &mut state["teams"]["alpha"]["agents"]["worker"];
    worker["runtime_family"] = json!("contract");
    worker["native_endpoint"] = json!(native.socket);
    worker["pane_id"] = json!(native.pane);
    worker["pane_pid"] = json!(native.pid);
    if stopped {
        worker["status"] = json!("stopped");
    } else {
        worker["pane_birth_identity"] = json!(team_agent_contract::host::process::process_birth_identity(native.pid).unwrap());
    }
    fixture.set_state(&state);
    NativeTeam { fixture, native: Some(native), ws, scope: scope.unwrap() }
}

/// The CLI as an unbound operator under an explicit private state home.
pub fn cli(ws: &Path, state_home: &Path, args: &[&str]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_team-agent"));
    command.args(args).arg("--workspace").arg(ws).arg("--json").env("XDG_STATE_HOME", state_home);
    for (key, _) in std::env::vars().filter(|(key, _)| key.starts_with("TEAM_AGENT_")) {
        command.env_remove(key);
    }
    command.output().unwrap()
}

pub fn json_of(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|_| json!({
        "unparsed_stdout": String::from_utf8_lossy(&output.stdout),
        "stderr": String::from_utf8_lossy(&output.stderr),
        "exit": output.status.code(),
    }))
}

pub fn send(ws: &Path, state_home: &Path, text: &str) -> Value {
    json_of(&cli(ws, state_home, &["send", "worker", text, "--team", "alpha"]))
}
