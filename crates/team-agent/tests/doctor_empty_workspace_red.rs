//! Doctor performs read-only static checks for an existing but empty workspace.
//!
//! Without a spec or runtime, an otherwise healthy workspace succeeds with
//! `ok:true` and `runtime.status:not_present`, without creating runtime state.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[path = "support/hermetic.rs"]
mod hermetic_guard;
#[allow(dead_code)]
fn _hermetic_boundary_marker(_: &hermetic_guard::HermeticTestEnv) {}

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::Value;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_team-agent")
}

#[test]
fn doctor_empty_workspace_without_spec_or_runtime_passes_static_checks() {
    let workspace = tmp_dir("empty-workspace");
    let home = tmp_dir("home");

    let output = run(
        &[
            "doctor",
            "--workspace",
            workspace.to_str().unwrap(),
            "--json",
        ],
        &workspace,
        &home,
    );
    let body: Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "doctor --json must emit parseable JSON even when it fails; parse_error={error}; stdout={}; stderr={}",
            stdout(&output),
            stderr(&output)
        )
    });

    assert!(
        output.status.success(),
        "doctor on an existing empty workspace must pass static checks without requiring a spec/runtime; stdout={} stderr={}",
        stdout(&output),
        stderr(&output)
    );
    assert_eq!(
        body.get("ok").and_then(Value::as_bool),
        Some(true),
        "doctor JSON must report successful static checks for an empty workspace; body={body}"
    );
    assert_eq!(body["runtime"]["status"], "not_present", "{body}");
    assert_eq!(body["issues"], serde_json::json!([]), "{body}");
    assert!(!workspace.join(".team/runtime/state.json").exists());
    assert!(!workspace.join(".team/runtime/team.db").exists());
}

fn run(args: &[&str], cwd: &Path, home: &Path) -> Output {
    Command::new(bin())
        .args(args)
        .current_dir(cwd)
        .env("HOME", home)
        .env("TMPDIR", std::env::temp_dir())
        .env("LANG", "C")
        .env("LC_ALL", "C")
        .env_remove("TEAM_AGENT_ID")
        .env_remove("TEAM_AGENT_OWNER_TEAM_ID")
        .env_remove("TEAM_AGENT_TEAM_ID")
        .env_remove("TMUX")
        .env_remove("TMUX_PANE")
        .output()
        .expect("run team-agent doctor")
}

fn tmp_dir(tag: &str) -> PathBuf {
    static CTR: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "ta-doctor-empty-red-{tag}-{}-{}",
        std::process::id(),
        CTR.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::canonicalize(dir).unwrap()
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).to_string()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).to_string()
}
