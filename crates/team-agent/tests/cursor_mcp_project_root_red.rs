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

#[test]
#[serial(env)]
fn issue342_primary_is_stable_owned_and_retains_real_project_access() {
    use std::os::unix::fs::MetadataExt;
    use team_agent::lifecycle::{apply_cursor_spawn_workspace_pointers, cursor_mcp_project_dir};
    let guard = hermetic::HermeticTestEnv::enter("issue342-stable");
    let ws = guard.workspace("repo");
    std::fs::create_dir_all(ws.join(".cursor/rules")).unwrap();
    std::fs::write(ws.join(".cursor/rules/project.mdc"), "project instructions").unwrap();
    let cfg = config(&ws, "worker", "team");
    let a = prepare_cursor_seat_mcp(&ws, "worker", &cfg).unwrap().unwrap();
    let inode = std::fs::metadata(&a).unwrap().ino();
    assert_eq!(std::fs::metadata(&a).unwrap().mode() & 0o777, 0o700);
    assert_eq!(prepare_cursor_seat_mcp(&ws, "worker", &cfg).unwrap(), Some(a.clone()));
    assert_eq!(std::fs::metadata(&a).unwrap().ino(), inode);
    assert_eq!(cursor_mcp_project_dir(&ws, "worker").unwrap(), a);
    assert_eq!(std::fs::read_to_string(a.join(".cursor/rules/project.mdc")).unwrap(), "project instructions");
    let mut argv = vec!["agent".into(), "--workspace".into(), ws.to_string_lossy().into_owned()];
    apply_cursor_spawn_workspace_pointers(&mut argv, &ws, "worker").unwrap();
    assert_eq!(Path::new(&argv[2]), a);
    assert_eq!(argv.iter().filter(|s| s.as_str() == "--add-dir").count(), 1);
    assert_eq!(Path::new(argv.last().unwrap()), ws);
    let before = std::fs::read(a.join(".cursor/mcp.json")).unwrap();
    assert!(prepare_cursor_seat_mcp(&ws, "worker", &config(&ws, "worker", "foreign-team")).is_err());
    assert_eq!(std::fs::read(a.join(".cursor/mcp.json")).unwrap(), before);
    let b = prepare_cursor_seat_mcp(&ws, "other", &config(&ws, "other", "team")).unwrap().unwrap();
    assert_ne!(a, b);
}

#[test]
#[serial(env)]
fn issue342_true_worktree_primary_does_not_follow_git_file() {
    let guard = hermetic::HermeticTestEnv::enter("issue342-worktree");
    let repo = guard.workspace("repo");
    assert!(Command::new("git").args(["init", "--quiet", "--template="]).arg(&repo).status().unwrap().success());
    assert!(Command::new("git").arg("-C").arg(&repo)
        .args(["-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid", "commit", "--quiet", "--allow-empty", "--no-gpg-sign", "-m", "fixture"])
        .status().unwrap().success());
    let ws = guard.root().join("worktree");
    assert!(Command::new("git").arg("-C").arg(&repo).args(["worktree", "add", "--quiet", "--detach"]).arg(&ws).status().unwrap().success());
    let ws = ws.canonicalize().unwrap();
    assert!(ws.join(".git").is_file());
    let primary = prepare_cursor_seat_mcp(&ws, "worker", &config(&ws, "worker", "team")).unwrap().unwrap();
    assert!(!primary.starts_with(&ws) && !primary.starts_with(&repo));
    assert!(primary.ancestors().all(|p| std::fs::symlink_metadata(p.join(".git")).is_err()));
}

#[test]
#[serial(env)]
fn issue342_replaced_or_aliased_private_resources_are_not_adopted() {
    use std::os::unix::fs::{symlink, DirBuilderExt, PermissionsExt};
    let guard = hermetic::HermeticTestEnv::enter("issue342-fences");
    for mutation in ["inode", "primary-symlink", "cursor-symlink", "permissions", "git-file", "git-dir", "record-symlink"] {
        let ws = guard.workspace(mutation);
        let cfg = config(&ws, "worker", "team");
        let primary = prepare_cursor_seat_mcp(&ws, "worker", &cfg).unwrap().unwrap();
        let record = ws.join(".team/runtime/provider-config/worker/cursor-project.json");
        let before = std::fs::read(&record).unwrap();
        match mutation {
            "inode" | "primary-symlink" => {
                let retained = primary.with_extension("retained");
                std::fs::rename(&primary, &retained).unwrap();
                if mutation == "inode" {
                    std::fs::DirBuilder::new().mode(0o700).create(&primary).unwrap();
                } else { symlink(retained, &primary).unwrap(); }
            }
            "cursor-symlink" => {
                std::fs::rename(primary.join(".cursor"), primary.join("retained")).unwrap();
                symlink(primary.join("retained"), primary.join(".cursor")).unwrap();
            }
            "permissions" => std::fs::set_permissions(&primary, std::fs::Permissions::from_mode(0o755)).unwrap(),
            "git-file" => std::fs::write(primary.join(".git"), "gitdir: foreign").unwrap(),
            "git-dir" => std::fs::create_dir(primary.join(".git")).unwrap(),
            "record-symlink" => {
                let retained = record.with_extension("retained");
                std::fs::rename(&record, &retained).unwrap();
                symlink(retained, &record).unwrap();
            }
            _ => unreachable!(),
        }
        assert!(prepare_cursor_seat_mcp(&ws, "worker", &cfg).is_err(), "must refuse {mutation}");
        assert_eq!(std::fs::read(&record).unwrap(), before, "must not rewrite {mutation} receipt");
    }
}

#[test]
#[serial(env)]
fn issue342_existing_unrecorded_primary_is_not_silently_migrated() {
    let guard = hermetic::HermeticTestEnv::enter("issue342-legacy");
    let ws = guard.workspace("repo");
    let old = ws.join(".team/runtime/provider-config/worker/cursor");
    std::fs::create_dir_all(old.join(".cursor")).unwrap();
    std::fs::write(old.join(".cursor/mcp.json"), "retained old session config").unwrap();
    let err = prepare_cursor_seat_mcp(&ws, "worker", &config(&ws, "worker", "team")).unwrap_err();
    assert!(err.to_string().contains("preserve its cwd/SID"));
    assert_eq!(std::fs::read_to_string(old.join(".cursor/mcp.json")).unwrap(), "retained old session config");
    assert!(!old.parent().unwrap().join("cursor-project.json").exists());
}

#[test]
#[serial(env)]
fn issue342_mismatched_identity_has_no_primary_effect_and_bad_server_cannot_be_approved() {
    use team_agent::lifecycle::{cursor_mcp_project_dir, enable_cursor_workspace_mcp};
    let guard = hermetic::HermeticTestEnv::enter("issue342-identity");
    let ws = guard.workspace("repo");
    let planned = cursor_mcp_project_dir(&ws, "worker").unwrap();
    assert!(prepare_cursor_seat_mcp(&ws, "worker", &config(&ws, "foreign", "team")).is_err());
    assert!(!planned.exists());
    let cfg = config(&ws, "worker", "team");
    let primary = prepare_cursor_seat_mcp(&ws, "worker", &cfg).unwrap().unwrap();
    let mut bad: serde_json::Value = serde_json::from_slice(&std::fs::read(primary.join(".cursor/mcp.json")).unwrap()).unwrap();
    bad["mcpServers"]["team_orchestrator"]["command"] = "/missing/old-team-agent".into();
    std::fs::write(primary.join(".cursor/mcp.json"), serde_json::to_vec(&bad).unwrap()).unwrap();
    let err = enable_cursor_workspace_mcp(&ws, Some(&primary)).unwrap_err();
    assert!(err.to_string().contains("server executable is unavailable"), "must fail private config preflight, not run the approval command: {err}");
}
