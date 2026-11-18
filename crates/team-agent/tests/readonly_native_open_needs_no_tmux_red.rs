//! 0.6.2 observable contract (a): a pure read of committed native authority performs
//! zero tmux resolution. 0.6.0/0.6.1 re-resolved tmux through PATH on every
//! `Backend::open_mode`, including read-only opens (3344d0d17), so a read failed or
//! stat()ed every PATH entry when tmux was not first on PATH.
#![cfg(unix)]
use std::path::{Path, PathBuf};
use std::process::Command;


// One fixture module instance per test binary: TempWorkspace names use a per-module
// counter, so two copies would collide under parallel tests.
#[path = "support/d1_support.rs"]
mod d1;

const CHILD: &str = "readonly_open_child_process";

/// Runs only as a re-executed child with a PATH that contains no tmux anywhere.
#[test]
#[ignore = "child process of readonly_native_authority_open_needs_no_tmux_on_path"]
fn readonly_open_child_process() {
    let Some(root) = std::env::var_os("TA_READONLY_ROOT") else { return };
    let path = std::env::var_os("PATH").unwrap();
    assert!(std::env::split_paths(&path).all(|dir| !dir.join("tmux").exists()), "child PATH must not contain tmux");
    let backend = team_agent::contract_runtime::backend::Backend::open_readonly(Path::new(&root), "alpha")
        .unwrap_or_else(|error| panic!("read-only open required tmux or failed: {error}"));
    assert_eq!(backend.binding.team, "alpha");
    // A lazy open with no store access must not pass: read committed seat state.
    let seat = team_agent_contract::contract::types::SeatId::new("worker").unwrap();
    assert!(backend.store.seat(&seat).unwrap().is_some(), "read-only open did not read the committed seat");
}

#[test]
fn readonly_native_authority_open_needs_no_tmux_on_path() {
    let base = std::env::temp_dir().canonicalize().unwrap().join(format!(
        "ta-ro-{}-{}", std::process::id(), chrono::Utc::now().timestamp_nanos_opt().unwrap()));
    std::fs::create_dir_all(base.join("bin")).unwrap();
    let root = base.join("ws");
    std::fs::create_dir_all(root.join(".team/runtime")).unwrap();
    let authority = d1::direct_native_fixture::NativeAuthority::start(&root, "codex");
    let root = root.canonicalize().unwrap();
    let empty_bin: PathBuf = base.join("bin");
    let output = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", CHILD, "--ignored", "--nocapture", "--test-threads=1"])
        .env("TA_READONLY_ROOT", &root)
        .env("PATH", &empty_bin)
        .output()
        .unwrap();
    drop(authority);
    let _ = std::fs::remove_dir_all(&base);
    assert!(
        output.status.success() && String::from_utf8_lossy(&output.stdout).contains("1 passed"),
        "read-only native open must not need tmux:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// INVARIANT (green on 0.6.1, must stay green): tmux is resolved at the physical use
/// site from the invocation's PATH, never trusted from a persisted absolute path
/// (3344d0d17: a Homebrew upgrade moves the Cellar path). A stale binding.tmux must
/// not break a native send.
#[test]
fn native_send_ignores_a_stale_persisted_tmux_path() {
    let base = d1::scratch("stale-tmux");
    let state_home = base.join("state");
    let team = d1::native_team(|ws, scope| vec![d1::legacy_root(ws, scope)]);
    let binding_path = d1::legacy_root(&team.ws, &team.scope).join(d1::BINDING);
    let mut binding: serde_json::Value = serde_json::from_slice(&std::fs::read(&binding_path).unwrap()).unwrap();
    binding["tmux"] = serde_json::json!("/nonexistent/team-agent-stale/tmux");
    std::fs::write(&binding_path, serde_json::to_vec(&binding).unwrap()).unwrap();
    let receipt = d1::send(&team.ws, &state_home, "stale persisted tmux path still delivers");
    drop(team);
    let _ = std::fs::remove_dir_all(&base);
    assert_eq!(receipt["delivery"], "physical", "{receipt}");
}
