//! purpose: cursor 席位必须把身份写进它实际会读的 mcp.json env
//! contract: overlay 后 `<workspace>/.cursor/mcp.json` 含 team_orchestrator.env.TEAM_AGENT_ID
//!   （不是 `.team/runtime/mcp/*.json`，也不是靠 pane env 继承）
//! boundary: 只覆盖 cursor launch 产物；不改 claude/codex/copilot/grok 路径
//!
//! 生产侧判据，未经血统审计。

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[path = "../../../tests/support/hermetic.rs"]
mod hermetic_guard;
#[allow(dead_code)]
fn _hermetic_boundary_marker(_: &hermetic_guard::HermeticTestEnv) {}

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serial_test::serial;
use team_agent::lifecycle::quick_start_with_transport_in_workspace;
use team_agent::lifecycle::{
    apply_cursor_mcp_overlay, apply_cursor_subscription_proxy_env,
    apply_cursor_workspace_physical_path, cursor_mcp_enable_argv, physical_workspace_path,
    LifecycleError,
};
use team_agent::provider::McpConfig;
use team_agent::transport::test_support::OfflineTransport;

#[test]
fn cursor_overlay_writes_identity_into_mcp_json_env() {
    let ws = tmp_dir("cursor-mcp-id");
    apply_cursor_mcp_overlay(&ws, &sample_mcp_config("seat-a", &ws.to_string_lossy()))
        .expect("overlay write");
    let text = std::fs::read_to_string(ws.join(".cursor/mcp.json")).unwrap();
    assert!(
        text.contains("\"team_orchestrator\""),
        "cursor overlay must use the canonical server name"
    );
    assert!(
        text.contains("\"TEAM_AGENT_ID\"") && text.contains("seat-a"),
        "cursor must write TEAM_AGENT_ID into json env; pane env is not inherited"
    );
    assert!(
        text.contains("\"TEAM_AGENT_OWNER_TEAM_ID\"") && text.contains("t1"),
        "owner team must live in json env"
    );
    assert!(
        text.contains("\"TEAM_AGENT_AUTH_MODE\"") && text.contains("subscription"),
        "auth mode must live in json env"
    );
    assert!(
        !text.contains("{workspace}") && !text.contains("{agent_id}"),
        "placeholders must already be resolved"
    );
    assert!(
        !text.contains("team-agent"),
        "legacy inbound key must not remain"
    );
}

#[test]
fn cursor_overlay_refuses_missing_identity() {
    let ws = tmp_dir("cursor-mcp-missing-id");
    let cfg = McpConfig {
        raw: serde_json::json!({
            "team_orchestrator": {
                "command": "/bin/team-agent-test",
                "args": ["mcp-server"],
                "env": { "TEAM_AGENT_WORKSPACE": "/ws" }
            }
        }),
    };
    let err = apply_cursor_mcp_overlay(&ws, &cfg).expect_err("missing TEAM_AGENT_ID must fail");
    match err {
        LifecycleError::StatePersist(text) => {
            assert!(
                text.contains("TEAM_AGENT_ID"),
                "refusal must name the missing identity key"
            );
        }
        other => panic!("expected StatePersist, got {other:?}"),
    }
}

#[test]
fn cursor_overlay_keeps_unrelated_servers_and_replaces_orchestrator() {
    let ws = tmp_dir("cursor-mcp-merge");
    let dir = ws.join(".cursor");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("mcp.json"),
        r#"{
  "mcpServers": {
    "keep-me": { "command": "/bin/keep" },
    "team-agent": { "command": "/old/team-agent" },
    "team_orchestrator": { "command": "/stale" }
  }
}"#,
    )
    .unwrap();

    apply_cursor_mcp_overlay(&ws, &sample_mcp_config("seat-b", "/ws-b")).expect("overlay merge");
    let text = std::fs::read_to_string(dir.join("mcp.json")).unwrap();
    assert!(
        text.contains("keep-me"),
        "unrelated project MCP servers must survive"
    );
    assert!(
        !text.contains("/old/team-agent") && !text.contains("/stale"),
        "stale orchestrator/legacy names must be replaced"
    );
    assert!(
        text.contains("seat-b") && text.contains("/ws-b"),
        "new identity must land"
    );
}

#[test]
fn cursor_second_seat_overwrites_json_identity_without_exclusive_gate() {
    let ws = tmp_dir("cursor-mcp-two");
    apply_cursor_mcp_overlay(&ws, &sample_mcp_config("first", "/ws")).expect("first");
    apply_cursor_mcp_overlay(&ws, &sample_mcp_config("second", "/ws")).expect("second");
    let text = std::fs::read_to_string(ws.join(".cursor/mcp.json")).unwrap();
    assert!(
        text.contains("second"),
        "last writer wins on a shared --workspace; no grok-style exclusive gate"
    );
    assert!(
        !text.contains("\"first\""),
        "previous seat identity must not remain in the single json file"
    );
}

#[test]
fn cursor_enable_argv_is_documented_subcommand_without_workspace_flag() {
    let argv = cursor_mcp_enable_argv();
    assert_eq!(
        argv,
        vec![
            "agent".to_string(),
            "mcp".to_string(),
            "enable".to_string(),
            "team_orchestrator".to_string()
        ]
    );
}

#[test]
fn cursor_workspace_flag_is_rewritten_to_physical_path() {
    let ws = tmp_dir("cursor-phys");
    let mut argv = vec![
        "agent".to_string(),
        "--workspace".to_string(),
        ws.to_string_lossy().into_owned(),
    ];
    apply_cursor_workspace_physical_path(&mut argv, &ws);
    let physical = physical_workspace_path(&ws);
    assert_eq!(argv[2], physical.to_string_lossy().as_ref());
}

#[test]
#[serial(env)]
fn cursor_subscription_proxy_copies_keys_without_requiring_profile() {
    let prev = std::env::var("HTTPS_PROXY").ok();
    std::env::set_var("HTTPS_PROXY", "http://127.0.0.1:9");
    let mut env = std::collections::BTreeMap::new();
    let presence = apply_cursor_subscription_proxy_env(&mut env);
    let has_key = env.contains_key("HTTPS_PROXY");
    let value_len = env.get("HTTPS_PROXY").map(String::len);
    match prev {
        Some(value) => std::env::set_var("HTTPS_PROXY", value),
        None => std::env::remove_var("HTTPS_PROXY"),
    }
    assert!(
        presence.https_proxy,
        "presence must be true when the process has HTTPS_PROXY"
    );
    assert!(has_key, "subscription env must receive the proxy key");
    assert_eq!(
        value_len,
        Some(18),
        "copied value length must match the fixture"
    );
}

#[test]
fn cursor_spawn_writes_identity_into_project_mcp_json() {
    let ws = tmp_dir("cursor-mcp-spawn");
    let team = write_cursor_team(&ws, "cursortm", "cursor_writer");
    let config_path = ws.join(".cursor").join("mcp.json");
    assert!(
        !config_path.exists(),
        "precondition: project cursor mcp.json must be absent before spawn"
    );

    quick_start_with_transport_in_workspace(
        &ws,
        &team,
        None,
        true,
        Some("cursortm"),
        &OfflineTransport::new(),
    )
    .expect("cursor quick-start through offline transport should spawn");

    let text = std::fs::read_to_string(&config_path).unwrap_or_else(|err| {
        panic!(
            "cursor spawn must materialize {}; cursor only reads this workspace file: {err}",
            config_path.display()
        )
    });
    let workspace = ws.to_string_lossy();
    assert!(
        text.contains("\"team_orchestrator\""),
        "spawned overlay must declare team_orchestrator"
    );
    assert!(
        text.contains("\"mcp-server\"") && text.contains("\"--workspace\""),
        "args must launch mcp-server"
    );
    assert!(
        text.contains(workspace.as_ref()),
        "workspace must be the real path, not a placeholder"
    );
    assert!(
        text.contains("\"TEAM_AGENT_ID\"") && text.contains("cursor_writer"),
        "identity must be in json env"
    );
    assert!(
        !text.contains("{workspace}")
            && !text.contains("{agent_id}")
            && !text.contains("{team_id}"),
        "placeholders must be resolved before cursor reads the file"
    );
}

fn write_cursor_team(ws: &Path, team_key: &str, agent_id: &str) -> PathBuf {
    let team = ws.join(team_key);
    std::fs::create_dir_all(team.join("agents")).unwrap();
    std::fs::write(
        team.join("TEAM.md"),
        format!(
            "---\nname: {team_key}\nobjective: cursor MCP overlay contract.\nprovider: cursor_agent\ndangerously_skip_permissions: true\n---\n\nTeam.\n"
        ),
    )
    .unwrap();
    std::fs::write(
        team.join("agents").join(format!("{agent_id}.md")),
        format!(
            "---\nname: {agent_id}\nrole: Cursor Writer\nprovider: cursor_agent\nmodel: sonnet-4-thinking\nauth_mode: subscription\ndangerously_skip_permissions: true\ntools:\n  - mcp_team\n---\n\nWorker.\n"
        ),
    )
    .unwrap();
    team
}

fn sample_mcp_config(agent_id: &str, workspace: &str) -> McpConfig {
    McpConfig {
        raw: serde_json::json!({
            "team_orchestrator": {
                "command": "/bin/team-agent-test",
                "args": ["mcp-server", "--workspace", workspace],
                "env": {
                    "TEAM_AGENT_ID": agent_id,
                    "TEAM_AGENT_WORKSPACE": workspace,
                    "TEAM_AGENT_OWNER_TEAM_ID": "t1",
                    "TEAM_AGENT_AUTH_MODE": "subscription",
                }
            }
        }),
    }
}

fn tmp_dir(tag: &str) -> PathBuf {
    static N: AtomicU64 = AtomicU64::new(0);
    let root =
        std::env::var("TEAM_AGENT_TEST_TMP").unwrap_or_else(|_| "/Volumes/nvme/tmp".to_string());
    let dir = Path::new(&root).join(format!(
        "ta-rs-cursor-mcp-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::canonicalize(dir).unwrap()
}
