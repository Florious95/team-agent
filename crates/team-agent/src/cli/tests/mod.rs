//! cli · tests — RED 契约 lane(verbatim 从原 cli.rs 的 `#[cfg(test)] mod tests` 迁移)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use super::*;
use serde_json::json;

fn tmp_workspace() -> std::path::PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static CTR: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "ta-cli-test-{}-{}",
        std::process::id(),
        CTR.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(dir.join(".team").join("runtime")).unwrap();
    dir
}

fn seed_status_workspace() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "ta-cli-status-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let team = dir.join(".team");
    std::fs::create_dir_all(team.join("runtime")).unwrap();
    let state = json!({
        "session_name": "seeded-team",
        "leader": {"id": "leader"},
        "leader_receiver": {"pane_id": "%3", "pane_current_command": "codex", "status": "running"},
        "agents": {"a1": {"status": "running", "first_send_at": "2026-01-01T00:00:00Z"}},
        "tasks": [{"id": "t1", "title": "demo", "status": "open", "assignee": "a1"}],
    });
    std::fs::write(
        team.join("runtime").join("state.json"),
        serde_json::to_vec_pretty(&state).unwrap(),
    )
    .unwrap();
    dir
}

const DELEG_VALID_ROLE: &str = "---\nname: implementer\nrole: Implementation Engineer\nprovider: fake\nmodel: fake\nauth_mode: subscription\ntools:\n  - mcp_team\n---\n\nImplement bounded tasks.\n";
const DELEG_INVALID_ROLE: &str = "---\nname: broken\nrole: Broken Worker\nmodel: gpt-5.5\nauth_mode: subscription\ntools:\n  - mcp_team\n---\n\nNo provider field.\n";
const DELEG_TEAM_MD: &str = "---\nname: clidelegteam\nobjective: Delegation probe.\nprovider: fake\n---\n\nteam.\n";

fn deleg_uniq_dir(tag: &str) -> std::path::PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "ta-cli-deleg-{}-{}-{}",
        tag,
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn deleg_team_dir_with_healthy_coordinator() -> std::path::PathBuf {
    let base = deleg_uniq_dir("qs");
    let team = base.join("teamdir");
    std::fs::create_dir_all(team.join("agents")).unwrap();
    std::fs::write(team.join("TEAM.md"), DELEG_TEAM_MD).unwrap();
    std::fs::write(team.join("agents").join("implementer.md"), DELEG_VALID_ROLE).unwrap();
    let wp = crate::coordinator::WorkspacePath::new(base.clone());
    std::fs::create_dir_all(crate::model::paths::runtime_dir(&base)).unwrap();
    let _ = crate::message_store::MessageStore::open(&base).unwrap();
    let me = crate::coordinator::Pid::new(std::process::id());
    crate::coordinator::write_coordinator_metadata(&wp, me, crate::coordinator::MetadataSource::Boot).unwrap();
    std::fs::write(crate::coordinator::coordinator_pid_path(&wp), me.to_string()).unwrap();
    team
}

fn outcome_text(r: Result<CmdResult, CliError>) -> String {
    match r {
        Ok(cmd) => format!("{cmd:?}"),
        Err(e) => e.to_string(),
    }
}

mod base;
mod compile;
mod divergence;
mod lane_c;
mod leader_watch;
mod missing_subcommands;
mod repair_state_byte_lock;
mod peer_allow;
mod run_delegation;
mod status_send;
mod verb_validate;
mod verb_settle;
mod verb_profile;
mod main_preserved;
mod shutdown_kill_plan;
