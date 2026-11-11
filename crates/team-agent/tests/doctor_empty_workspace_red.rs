//! Doctor performs read-only checks for an existing but empty workspace.
//!
//! 262f6cdb (doctor v1) + A3 (ec3ef36e): without runtime state the L1 scope
//! link fails with a typed "no runtime state" message and a creation hint
//! (ok:false, exit 1, L2-L6 BLOCKED), without creating runtime state.

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

    assert_eq!(
        output.status.code(),
        Some(1),
        "doctor on an existing empty workspace reports its missing runtime as a typed L1 failure; stdout={} stderr={}",
        stdout(&output),
        stderr(&output)
    );
    assert_eq!(body.get("ok").and_then(Value::as_bool), Some(false), "{body}");
    let links = body["links"].as_array().expect("doctor v1 links");
    assert_eq!(links.len(), 6, "{body}");
    assert_eq!(links[0]["status"], "FAIL", "{body}");
    let message = links[0]["message"].as_str().unwrap_or_default();
    assert!(message.contains("has no runtime state") && !message.contains("No such file or directory"),
        "empty workspace must get a typed L1 message, not a raw ENOENT: {body}");
    assert!(links[1..].iter().all(|link| link["status"] == "BLOCKED"), "{body}");
    assert!(body["next_action"]["context"].as_str().is_some_and(|hint| hint.contains("team-agent start AGENT --team")),
        "empty workspace must get a creation hint: {body}");
    assert!(!workspace.join(".team").exists());
}

#[test]
fn doctor_missing_workspace_and_invalid_configuration_still_fail_readonly() {
    let root = tmp_dir("invalid-workspaces");
    let home = tmp_dir("invalid-home");
    let missing = root.join("missing");
    let invalid = root.join("invalid");
    std::fs::create_dir(&invalid).unwrap();
    std::fs::write(invalid.join("team.spec.yaml"), "invalid: [\n").unwrap();

    for workspace in [&missing, &invalid] {
        let output = run(
            &["doctor", "--workspace", workspace.to_str().unwrap(), "--json"],
            &root,
            &home,
        );
        let body: Value = serde_json::from_slice(&output.stdout).unwrap();
        // 262f6cdb: an unusable workspace is an L1 scope failure (exit 1)
        // naming that workspace; nothing downstream runs or writes.
        assert_eq!(output.status.code(), Some(1), "{body}");
        assert_eq!(body["ok"], false, "{body}");
        let links = body["links"].as_array().expect("doctor v1 links");
        assert_eq!(links[0]["status"], "FAIL", "{body}");
        assert!(links[0]["message"].as_str().is_some_and(|message| message.contains(workspace.to_str().unwrap())), "{body}");
        assert!(links[1..].iter().all(|link| link["status"] == "BLOCKED"), "{body}");
        assert!(!workspace.join(".team").exists());
    }
    assert!(!missing.exists());
    assert_eq!(std::fs::read_to_string(invalid.join("team.spec.yaml")).unwrap(), "invalid: [\n");
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
