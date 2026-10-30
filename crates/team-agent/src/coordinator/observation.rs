//! Read-only legacy residue and host-boot observations; no scheduler or heartbeat writer.
use super::WorkspacePath;
use serde_json::Value;
use std::path::PathBuf;

fn coordinator_heartbeat_path(workspace: &WorkspacePath) -> PathBuf {
    crate::model::paths::runtime_dir(workspace.as_path()).join("coordinator_tick.json")
}

pub(crate) fn probe_host_boot_id() -> Option<String> {
    if let Ok(override_id) = std::env::var("TEAM_AGENT_TEST_HOST_BOOT_ID") {
        if !override_id.is_empty() {
            return Some(override_id);
        }
    }
    // Linux: stable UUID for the current boot.
    if let Ok(raw) = std::fs::read_to_string("/proc/sys/kernel/random/boot_id") {
        let trimmed = raw.trim();
        if !trimmed.is_empty() {
            return Some(format!("linux-{trimmed}"));
        }
    }
    // macOS/BSD: kern.boottime via sysctl. Cheap and root-free.
    #[cfg(target_os = "macos")]
    {
        if let Ok(output) = std::process::Command::new("sysctl")
            .args(["-n", "kern.boottime"])
            .output()
        {
            if output.status.success() {
                let text = String::from_utf8_lossy(&output.stdout);
                let trimmed = text.trim();
                if !trimmed.is_empty() {
                    return Some(format!("macos-{trimmed}"));
                }
            }
        }
    }
    None
}

/// 0.5.41 Slice 1: expose the last-written heartbeat sidecar so
/// `cli::diagnose` / `cli::status_port` can consume it read-only. Any
/// error (missing file, malformed JSON) returns None — callers treat
/// that as "unknown, fall back to existing facts" per locate §8 risk
/// note, they must never guess a stale/fresh verdict from absence.
/// ---
/// purpose: 只读取出最近一次写入的心跳 sidecar
/// params:
///   workspace: workspace 根
/// returns: 解析成功才有值；文件缺失或 JSON 损坏一律 None。调用方须把 None 当作「未知」回落到既有事实，不得据此断定新鲜或陈旧
/// ---
pub fn read_coordinator_heartbeat(workspace: &WorkspacePath) -> Option<Value> {
    let path = coordinator_heartbeat_path(workspace);
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}
