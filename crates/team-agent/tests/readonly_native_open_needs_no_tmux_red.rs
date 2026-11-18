//! 0.6.2 observable contract (a): a pure read of committed native authority performs
//! zero tmux resolution. 0.6.0/0.6.1 re-resolved tmux through PATH on every
//! `Backend::open_mode`, including read-only opens (3344d0d17), so a read failed or
//! stat()ed every PATH entry when tmux was not first on PATH.
#![cfg(unix)]
use std::path::{Path, PathBuf};
use std::process::Command;

#[path = "support/direct_native_fixture.rs"]
mod direct_native_fixture;

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
}

#[test]
fn readonly_native_authority_open_needs_no_tmux_on_path() {
    let base = std::env::temp_dir().canonicalize().unwrap().join(format!(
        "ta-ro-{}-{}", std::process::id(), chrono::Utc::now().timestamp_nanos_opt().unwrap()));
    std::fs::create_dir_all(base.join("bin")).unwrap();
    let root = base.join("ws");
    std::fs::create_dir_all(root.join(".team/runtime")).unwrap();
    let authority = direct_native_fixture::NativeAuthority::start(&root, "codex");
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
