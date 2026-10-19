//! Issue #342: a per-seat directory inside Git is not a private native MCP root.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[path = "support/hermetic.rs"]
mod hermetic;

use std::path::Path;
use std::process::Command;
use serial_test::serial;
use team_agent::lifecycle::prepare_cursor_seat_mcp;
use team_agent::provider::McpConfig;

fn config(workspace: &Path, seat: &str, owner: &str) -> McpConfig {
    McpConfig { raw: serde_json::json!({"team_orchestrator": {
        "command": std::env::current_exe().unwrap(),
        "args": ["mcp-server", "--workspace", workspace],
        "env": {
            "TEAM_AGENT_ID": seat,
            "TEAM_AGENT_WORKSPACE": workspace,
            "TEAM_AGENT_OWNER_TEAM_ID": owner,
            "TEAM_AGENT_AUTH_MODE": "subscription"
        }
    }}) }
}

#[test]
#[serial(env)]
fn issue342_git_repo_primary_does_not_inherit_stale_root_mcp() {
    let guard = hermetic::HermeticTestEnv::enter("issue342-git-root");
    let ws = guard.workspace("repo");
    assert!(Command::new("git").args(["init", "--quiet", "--template="])
        .arg(&ws).status().unwrap().success());
    let root = Command::new("git").arg("-C").arg(&ws)
        .args(["rev-parse", "--show-toplevel"]).output().unwrap();
    assert!(root.status.success(), "positive control must be a real Git repository");
    assert_eq!(Path::new(String::from_utf8(root.stdout).unwrap().trim()), ws);
    std::fs::create_dir_all(ws.join(".cursor")).unwrap();
    let stale = br#"{"mcpServers":{"team_orchestrator":{"command":"/missing/old-team-agent","env":{"TEAM_AGENT_ID":"foreign-seat"}}}}"#;
    std::fs::write(ws.join(".cursor/mcp.json"), stale).unwrap();

    let primary = prepare_cursor_seat_mcp(&ws, "worker", &config(&ws, "worker", "team"))
        .unwrap().unwrap();
    assert!(!primary.starts_with(&ws), "native MCP primary must not inherit the repository Git root");
    assert!(primary.ancestors().all(|p| std::fs::symlink_metadata(p.join(".git")).is_err()));
    assert_eq!(std::fs::read(ws.join(".cursor/mcp.json")).unwrap(), stale);
    let own: serde_json::Value = serde_json::from_slice(&std::fs::read(primary.join(".cursor/mcp.json")).unwrap()).unwrap();
    assert_eq!(own["mcpServers"]["team_orchestrator"]["env"]["TEAM_AGENT_ID"], "worker");
    assert_eq!(own["mcpServers"]["team_orchestrator"]["env"]["TEAM_AGENT_OWNER_TEAM_ID"], "team");
}
