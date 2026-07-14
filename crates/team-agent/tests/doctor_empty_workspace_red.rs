//! Doctor must fail honestly for an existing but empty workspace.
//!
//! User-visible contract: an installer or operator pointing `team-agent doctor --json` at an
//! existing directory with no Team Agent spec and no runtime must get a nonzero command and
//! `ok:false`, not a fabricated healthy result.

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
fn doctor_bogus_empty_workspace_without_spec_or_runtime_exits_nonzero() {
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
        !output.status.success(),
        "doctor on an existing empty workspace must exit nonzero when no TEAM.md/team.spec.yaml/runtime exists; stdout={} stderr={}",
        stdout(&output),
        stderr(&output)
    );
    assert_eq!(
        body.get("ok").and_then(Value::as_bool),
        Some(false),
        "doctor JSON must be honest ok:false for an empty workspace with no spec/runtime; body={body}"
    );
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
