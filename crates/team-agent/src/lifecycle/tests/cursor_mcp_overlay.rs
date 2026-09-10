//! purpose: cursor 席位必须把身份写进它实际会读的 mcp.json env
//! contract: 隔离默认开时 overlay 写 per-seat 工程根 `.cursor/mcp.json`（`--workspace` /
//!   enable cwd 同一根）；关隔离时写 `<workspace>/.cursor/mcp.json`。不是
//!   `.team/runtime/mcp/*.json`，也不是靠 pane env 继承。
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
    apply_cursor_mcp_overlay, apply_cursor_spawn_workspace_pointers,
    apply_cursor_subscription_proxy_env, apply_cursor_workspace_physical_path,
    cursor_mcp_enable_argv, cursor_mcp_enable_working_dir, cursor_mcp_json_path,
    cursor_mcp_project_dir, physical_workspace_path, prepare_cursor_seat_mcp, LifecycleError,
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
        !text.contains("\"team-agent\""),
        "legacy inbound key must not remain as a server name"
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
    let selected = cursor_mcp_json_path(&ws, "cursor_writer").unwrap();
    assert!(
        !selected.exists() && !ws.join(".cursor/mcp.json").exists(),
        "precondition: selected project and workspace overlay must be absent before spawn"
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

    let text = std::fs::read_to_string(&selected).unwrap_or_else(|err| {
        panic!(
            "cursor spawn must materialize {}; isolation default reads this per-seat file: {err}",
            selected.display()
        )
    });
    assert_cursor_identity_overlay(&text, "cursor_writer", &ws);
    assert!(
        !ws.join(".cursor/mcp.json").exists(),
        "default isolation must not dual-write the team workspace overlay"
    );
    let project = cursor_mcp_project_dir(&ws, "cursor_writer").unwrap();
    let enable_cwd = cursor_mcp_enable_working_dir(&ws, Some(&project));
    assert_eq!(enable_cwd, physical_workspace_path(&project));
    let argv_workspace = spawn_argv_workspace_flag(&ws).expect("spawn argv --workspace");
    assert_eq!(argv_workspace, physical_workspace_path(&project).to_string_lossy());
}

#[test]
#[serial(env)]
fn cursor_spawn_isolation_off_writes_workspace_mcp_json() {
    let key = "TEAM_AGENT_CURSOR_MCP_ISOLATION";
    let prev = std::env::var(key).ok();
    std::env::set_var(key, "0");
    let ws = tmp_dir("cursor-mcp-spawn-legacy");
    let team = write_cursor_team(&ws, "cursortm", "cursor_writer");
    let result = quick_start_with_transport_in_workspace(
        &ws,
        &team,
        None,
        true,
        Some("cursortm"),
        &OfflineTransport::new(),
    );
    match prev {
        Some(value) => std::env::set_var(key, value),
        None => std::env::remove_var(key),
    }
    result.expect("legacy isolation-off cursor spawn");
    let config_path = ws.join(".cursor").join("mcp.json");
    let text = std::fs::read_to_string(&config_path).unwrap_or_else(|err| {
        panic!(
            "isolation-off spawn must write {}; cursor --workspace is the team root: {err}",
            config_path.display()
        )
    });
    assert_cursor_identity_overlay(&text, "cursor_writer", &ws);
    assert!(
        !cursor_mcp_json_path(&ws, "cursor_writer")
            .unwrap()
            .exists(),
        "isolation-off must not materialize the per-seat project overlay"
    );
    let argv_workspace = spawn_argv_workspace_flag(&ws).expect("spawn argv --workspace");
    assert_eq!(argv_workspace, physical_workspace_path(&ws).to_string_lossy());
}

fn assert_cursor_identity_overlay(text: &str, agent_id: &str, workspace: &Path) {
    let workspace = workspace.to_string_lossy();
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
        "TEAM_AGENT_WORKSPACE / mcp args must be the real team path, not a placeholder"
    );
    assert!(
        text.contains("\"TEAM_AGENT_ID\"") && text.contains(agent_id),
        "identity must be in json env"
    );
    assert!(
        !text.contains("{workspace}")
            && !text.contains("{agent_id}")
            && !text.contains("{team_id}"),
        "placeholders must be resolved before cursor reads the file"
    );
}

fn spawn_argv_workspace_flag(workspace: &Path) -> Option<String> {
    let log = workspace.join(".team").join("logs").join("events.jsonl");
    let text = std::fs::read_to_string(log).ok()?;
    for line in text.lines().rev() {
        let value: serde_json::Value = serde_json::from_str(line).ok()?;
        if value.get("event").and_then(serde_json::Value::as_str)
            != Some("provider.worker.spawn_argv")
        {
            continue;
        }
        let argv = value.get("argv")?.as_array()?;
        let mut items = argv.iter().filter_map(serde_json::Value::as_str);
        while let Some(item) = items.next() {
            if item == "--workspace" {
                return items.next().map(str::to_string);
            }
        }
    }
    None
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

#[test]
#[serial(env)]
fn isolated_cursor_prepare_materializes_per_seat_project_not_workspace_overlay() {
    let ws = tmp_dir("cursor-iso-clean");
    let project = prepare_cursor_seat_mcp(&ws, "seat-iso", &sample_mcp_config("seat-iso", &ws.to_string_lossy()))
        .expect("prepare isolated cursor")
        .expect("isolation defaults on");
    assert!(
        project.ends_with("provider-config/seat-iso/cursor"),
        "per-seat project must be materialized: {}",
        project.display()
    );
    assert!(project.join(".cursor").is_dir());
    let overlay = std::fs::read_to_string(cursor_mcp_json_path(&ws, "seat-iso").unwrap()).unwrap();
    assert!(
        overlay.contains("seat-iso") && overlay.contains("team_orchestrator"),
        "overlay must land in the per-seat mcp.json"
    );
    assert!(
        !ws.join(".cursor/mcp.json").exists(),
        "clean workspace overlay must not be written when isolation is on"
    );
    let enable_cwd = cursor_mcp_enable_working_dir(&ws, Some(&project));
    assert_eq!(enable_cwd, physical_workspace_path(&project));
    let mut argv = vec![
        "agent".to_string(),
        "--workspace".to_string(),
        ws.to_string_lossy().into_owned(),
    ];
    apply_cursor_spawn_workspace_pointers(&mut argv, &ws, "seat-iso").unwrap();
    let workspace_flag = argv
        .windows(2)
        .find(|pair| pair[0] == "--workspace")
        .map(|pair| pair[1].as_str())
        .unwrap();
    assert_eq!(workspace_flag, physical_workspace_path(&project).to_string_lossy());
    assert_eq!(
        include_str!("../launch/spawn.rs").contains("prepare_cursor_seat_mcp("),
        true,
        "fresh spawn must call the shared materializer, not only compute the path"
    );
    assert!(
        include_str!("../restart/common.rs").contains("prepare_cursor_seat_mcp("),
        "restart spawn_agent_window must use the same materializer"
    );
}

#[test]
fn cursor_enable_working_dir_does_not_depend_on_cfg_test_skip() {
    let ws = tmp_dir("cursor-enable-cwd");
    let project = ws.join(".team/runtime/provider-config/seat/cursor");
    std::fs::create_dir_all(&project).unwrap();
    assert_eq!(
        cursor_mcp_enable_working_dir(&ws, Some(&project)),
        physical_workspace_path(&project)
    );
    assert_eq!(
        cursor_mcp_enable_working_dir(&ws, None),
        physical_workspace_path(&ws)
    );
}

fn tmp_dir(tag: &str) -> PathBuf {
    static N: AtomicU64 = AtomicU64::new(0);
    // 见 gate_fixtures::scratch_dir：⛔ 不硬编码绝对路径，默认标准临时目录。
    let root = std::env::var_os("TEAM_AGENT_TEST_TMP")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let dir = root.join(format!(
        "ta-rs-cursor-mcp-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::canonicalize(dir).unwrap()
}
