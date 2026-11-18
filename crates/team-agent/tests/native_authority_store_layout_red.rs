//! 0.6.2 D1 at the store level, using the product layout (binding plus the store at
//! <selected root>/authority, as Backend::create lays it out). There is no tmux
//! session and no socket. In 0.6.1 the store lived in the workspace, so a 0777
//! drvfs/9p workspace tripped the store-root fence (store.rs `& 0o077`). Under
//! D1 the store is in the private state root and workspace mode bits are irrelevant.
#![cfg(unix)]
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use team_agent::contract_runtime::backend::{self, Backend, Binding};
use team_agent_contract::host::process::{fingerprint_file, resolve_cwd};
use team_agent_contract::orchestration::store::ProjectionStore;

const CHILD: &str = "store_layout_child_process";

fn chmod_tree(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    let meta = std::fs::symlink_metadata(path).unwrap();
    if meta.file_type().is_symlink() { return; }
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
    if meta.is_dir() {
        for entry in std::fs::read_dir(path).unwrap() { chmod_tree(&entry.unwrap().path(), mode); }
    }
}

/// Child process (env isolated): publish authority, optionally make the workspace
/// .team tree world-writable, then perform a pure read through the product API.
#[test]
#[ignore = "child process of native_authority_store_layout_red"]
fn store_layout_child_process() {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let Some(ws) = std::env::var_os("TA_LAYOUT_WS").map(PathBuf::from) else { return };
    let legacy = std::env::var("TA_LAYOUT_MODE").unwrap() == "legacy";
    let cwd = resolve_cwd(&ws).unwrap();
    let scope = backend::scope_for(&cwd, "alpha").unwrap();
    let root = if legacy { ws.join(".team/runtime/contract").join(scope.as_str()) } else { backend::root_for(&ws, &scope) };
    std::fs::create_dir_all(&root).unwrap();
    let transport_root = root.join("authority");
    let endpoint = format!("framework:{}", scope.as_str());
    ProjectionStore::create(&transport_root, scope.clone(), &endpoint).unwrap();
    let candidate = std::env::current_exe().unwrap().canonicalize().unwrap();
    let tmux = Command::new("sh").args(["-c", "command -v tmux"]).output().unwrap();
    let binding = Binding { version: 1, workspace: cwd, team: "alpha".into(), scope, endpoint,
        candidate_sha256: fingerprint_file(&candidate, 1024 * 1024 * 1024, Duration::from_secs(10)).unwrap(),
        candidate, tmux: PathBuf::from(String::from_utf8(tmux.stdout).unwrap().trim()), transport_root };
    std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600)
        .open(root.join("framework-binding.json")).unwrap()
        .write_all(&serde_json::to_vec(&binding).unwrap()).unwrap();
    if std::env::var("TA_LAYOUT_CHMOD").as_deref() == Ok("0777") {
        chmod_tree(&ws.join(".team"), 0o777);
    }
    let opened = Backend::open_readonly(&ws, "alpha")
        .unwrap_or_else(|error| panic!("read-only open of {} failed: {error}", root.display()));
    assert!(opened.store.unfinished().unwrap().is_empty(), "store not readable");
    println!("OPENED={}", root.display());
}

fn run_child(tag: &str, mode: &str, chmod: &str) -> (bool, String) {
    let base = std::env::temp_dir().canonicalize().unwrap().join(format!(
        "ta-layout-{tag}-{}-{}", std::process::id(), chrono::Utc::now().timestamp_nanos_opt().unwrap()));
    let ws = base.join("ws");
    std::fs::create_dir_all(ws.join(".team/runtime")).unwrap();
    let output = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", CHILD, "--ignored", "--nocapture", "--test-threads=1"])
        .env("TA_LAYOUT_WS", &ws).env("TA_LAYOUT_MODE", mode).env("TA_LAYOUT_CHMOD", chmod)
        .env("XDG_STATE_HOME", base.join("state"))
        .output().unwrap();
    let text = format!("{}\n{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
    let opened_in_workspace = text.lines().filter_map(|line| line.split("OPENED=").nth(1))
        .any(|root| Path::new(root.trim()).starts_with(&ws));
    let _ = std::fs::remove_dir_all(&base);
    (output.status.success() && text.contains("OPENED="), format!("{text}\nopened_in_workspace={opened_in_workspace}"))
}

/// RED on 0.6.1 (store under <ws>/.team, so 0777 trips the store-root fence);
/// GREEN under D1 (the store is in the private state root).
#[test]
fn product_layout_store_survives_a_world_writable_workspace() {
    let (ok, log) = run_child("product", "product", "0777");
    assert!(ok, "product-layout store must not depend on workspace mode bits:\n{log}");
    assert!(log.contains("opened_in_workspace=false"), "product authority was laid out inside the workspace:\n{log}");
}

/// INVARIANT: the real 0.6.1 store layout (<ws>/.team/runtime/contract/<scope>/authority)
/// of an existing team still opens read-only after the upgrade.
#[test]
fn legacy_layout_store_still_opens_readonly() {
    let (ok, log) = run_child("legacy", "legacy", "keep");
    assert!(ok, "0.6.1-layout store no longer opens:\n{log}");
}
