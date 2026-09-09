//! Shared trusted nodeprobe fixture for status brief consumer contracts.
//! The receipt mirrors the accepted public source/capability contract; the
//! fixture is not a product bypass and is only selected through PATH in tests.

use serde_json::json;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

const SOURCE_REPO: &str = "Florious95/team-agent-scratch/nodeprobe";
const SOURCE_COMMIT: &str = "ff316dc0afe8ab280e61d30934e7624579be6224";
const SOURCE_TREE: &str = "5217a41aa914ddcb72c27f39f1b4af9ead68b1b6";
const CAPABILITIES: &[&str] = &["tmux.list-panes", "ps.pid_ppid_stat_comm"];
const FORBIDDEN: &[&str] = &["tmux.capture-pane", "tmux.attach", "tmux.send-keys", "process.argv", "pane_body"];

pub fn install_trusted_nodeprobe(
    dir: &Path,
    endpoint: &str,
    session: &str,
    window: &str,
    pane: &str,
    provider: &str,
) -> PathBuf {
    install_trusted_nodeprobe_with_activity(
        dir,
        endpoint,
        session,
        window,
        pane,
        provider,
        "idle",
        "normal",
    )
}

pub fn install_trusted_nodeprobe_with_activity(
    dir: &Path,
    endpoint: &str,
    session: &str,
    window: &str,
    pane: &str,
    provider: &str,
    activity: &str,
    health: &str,
) -> PathBuf {
    let binary = dir.join("nodeprobe");
    std::fs::create_dir_all(dir).expect("create nodeprobe fixture dir");
    let report = json!({
        "schema_version": 1,
        "socket": endpoint,
        "sampled_at": "2026-01-01T00:00:00Z",
        "nodes": [{
            "socket": endpoint,
            "session": session,
            "window_name": window,
            "pane_id": pane,
            "name": window,
            "provider": provider,
            "activity": activity,
            "health": health,
            "session_name": session,
            "evidence": {"method": "tmux_list_panes"}
        }]
    });
    let report_json = serde_json::to_string(&report).expect("serialize nodeprobe report");
    let script = format!(
        "#!/bin/sh\nif [ \"$1\" != \"-S\" ] && [ \"$1\" != \"-L\" ]; then exit 2; fi\nprintf '%s' '{}'\n",
        report_json.replace('\'', "'\\''")
    );
    std::fs::write(&binary, script.as_bytes()).expect("write nodeprobe fixture");
    set_executable(&binary);
    let digest = format!("{:x}", Sha256::digest(script.as_bytes()));
    let receipt = json!({
        "schema": "nodeprobe-capability-v1",
        "binary": "nodeprobe",
        "binary_sha256": digest,
        "source_repo": SOURCE_REPO,
        "source_commit": SOURCE_COMMIT,
        "source_tree": SOURCE_TREE,
        "target": nodeprobe_target(),
        "report_schema": 1,
        "capabilities": CAPABILITIES,
        "forbidden": FORBIDDEN,
    });
    std::fs::write(
        binary.with_file_name("nodeprobe.capability.json"),
        serde_json::to_vec(&receipt).expect("serialize nodeprobe receipt"),
    )
    .expect("write nodeprobe receipt");
    binary
}

pub fn path_with_probe(binary: &Path, existing: Option<&std::ffi::OsStr>) -> String {
    let prefix = binary.parent().expect("nodeprobe parent").to_string_lossy();
    let suffix = existing
        .map(|value| value.to_string_lossy().into_owned())
        .unwrap_or_default();
    format!("{prefix}:{suffix}")
}

fn nodeprobe_target() -> &'static str {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => "aarch64-apple-darwin",
        ("macos", "x86_64") => "x86_64-apple-darwin",
        ("linux", "x86_64") => "x86_64-unknown-linux-gnu",
        ("linux", "aarch64") => "aarch64-unknown-linux-gnu",
        ("windows", "x86_64") => "x86_64-pc-windows-msvc",
        _ => panic!("unsupported nodeprobe test target"),
    }
}

#[cfg(unix)]
fn set_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
        .expect("make nodeprobe executable");
}

#[cfg(not(unix))]
fn set_executable(_path: &Path) {}
