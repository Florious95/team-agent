//! purpose: cursor 席位必须把身份写进它实际会读的 mcp.json env
//! contract: overlay 后 provider-config/<id>/cursor/.cursor/mcp.json 含 team_orchestrator.env.TEAM_AGENT_ID
//!   （不是改 HOME，也不是靠 pane env 继承）
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
    cursor_mcp_enable_argv, cursor_mcp_json_path, cursor_mcp_project_dir, physical_workspace_path,
    refuse_second_cursor_occupant, LifecycleError,
};
use team_agent::provider::McpConfig;
use team_agent::transport::test_support::OfflineTransport;

#[test]
fn cursor_overlay_writes_identity_into_mcp_json_env() {
    let ws = tmp_dir("cursor-mcp-id");
    apply_cursor_mcp_overlay(&ws, &sample_mcp_config("seat-a", &ws.to_string_lossy()))
        .expect("overlay write");
    let path = cursor_mcp_json_path(&ws, "seat-a").unwrap();
    let rendered = path.to_string_lossy();
    assert!(
        rendered.contains("provider-config") && rendered.contains("/cursor/"),
        "identity must land under provider-config/<id>/cursor, not a HOME fork: {rendered}"
    );
    assert!(
        !rendered.contains("/.team/runtime/cursor-mcp/"),
        "must not use the rejected COPILOT_HOME-style cursor-mcp tree: {rendered}"
    );
    let text = std::fs::read_to_string(&path).unwrap();
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
    let project = std::fs::read_to_string(dir.join("mcp.json")).unwrap();
    assert!(
        project.contains("keep-me"),
        "unrelated project MCP servers must survive in workspace mcp.json"
    );
    assert!(
        !project.contains("/old/team-agent") && !project.contains("/stale"),
        "stale orchestrator/legacy names must be scrubbed from the shared project file"
    );
    let isolated = std::fs::read_to_string(cursor_mcp_json_path(&ws, "seat-b").unwrap()).unwrap();
    assert!(
        isolated.contains("seat-b") && isolated.contains("/ws-b"),
        "new identity must land in the per-seat mcp.json"
    );
}

#[test]
fn cursor_second_seat_keeps_both_identities_when_isolated() {
    let ws = tmp_dir("cursor-mcp-two");
    apply_cursor_mcp_overlay(&ws, &sample_mcp_config("first", "/ws")).expect("first");
    apply_cursor_mcp_overlay(&ws, &sample_mcp_config("second", "/ws")).expect("second");
    let first = std::fs::read_to_string(cursor_mcp_json_path(&ws, "first").unwrap()).unwrap();
    let second = std::fs::read_to_string(cursor_mcp_json_path(&ws, "second").unwrap()).unwrap();
    assert!(
        first.contains("\"first\"") && !first.contains("\"second\""),
        "seat first must keep its TEAM_AGENT_ID"
    );
    assert!(
        second.contains("\"second\"") && !second.contains("\"first\""),
        "seat second must keep its TEAM_AGENT_ID"
    );
}

#[test]
#[serial(env)]
fn cursor_shared_overlay_last_writer_is_the_destruction_tooth() {
    let previous = std::env::var("TEAM_AGENT_CURSOR_MCP_ISOLATION").ok();
    std::env::set_var("TEAM_AGENT_CURSOR_MCP_ISOLATION", "0");
    let ws = tmp_dir("cursor-mcp-shared-red");
    apply_cursor_mcp_overlay(&ws, &sample_mcp_config("first", "/ws")).expect("first");
    apply_cursor_mcp_overlay(&ws, &sample_mcp_config("second", "/ws")).expect("second");
    let text = std::fs::read_to_string(ws.join(".cursor/mcp.json")).unwrap();
    restore_isolation_env(previous);
    assert!(
        text.contains("second"),
        "destruction: shared mcp.json last writer is second"
    );
    assert!(
        !text.contains("\"first\""),
        "destruction: criterion 2 goes red — first identity is gone"
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
fn cursor_spawn_pointers_use_per_seat_workspace_and_add_dir() {
    let ws = tmp_dir("cursor-pointers");
    apply_cursor_mcp_overlay(&ws, &sample_mcp_config("seat-p", &ws.to_string_lossy()))
        .expect("overlay");
    let mut argv = vec![
        "agent".to_string(),
        "--workspace".to_string(),
        ws.to_string_lossy().into_owned(),
    ];
    apply_cursor_spawn_workspace_pointers(&mut argv, &ws, "seat-p").expect("pointers");
    let project = physical_workspace_path(&cursor_mcp_project_dir(&ws, "seat-p").unwrap());
    let team = physical_workspace_path(&ws);
    assert_eq!(argv[2], project.to_string_lossy().as_ref());
    assert!(
        argv.windows(2)
            .any(|pair| pair[0] == "--add-dir" && pair[1] == team.to_string_lossy().as_ref()),
        "true workspace must be added with documented --add-dir: {argv:?}"
    );
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
    let config_path = cursor_mcp_json_path(&ws, "cursor_writer").expect("iso path");
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
            "cursor spawn must materialize per-seat {}; cursor reads --workspace/.cursor/mcp.json: {err}",
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

fn restore_isolation_env(previous: Option<String>) {
    match previous {
        Some(value) => std::env::set_var("TEAM_AGENT_CURSOR_MCP_ISOLATION", value),
        None => std::env::remove_var("TEAM_AGENT_CURSOR_MCP_ISOLATION"),
    }
}

#[test]
fn refuse_second_allows_when_isolation_on() {
    let ws = tmp_dir("cursor-refuse-iso-on");
    let spec = team_agent::model::yaml::loads(
        "agents:\n  - id: a\n    provider: cursor_agent\n  - id: b\n    provider: cursor_agent\n",
    )
    .expect("spec");
    refuse_second_cursor_occupant(&ws, "b", Some(&spec)).expect("isolation on allows second");
}

#[test]
#[serial(env)]
fn refuse_second_still_blocks_when_isolation_disabled() {
    let previous = std::env::var("TEAM_AGENT_CURSOR_MCP_ISOLATION").ok();
    std::env::set_var("TEAM_AGENT_CURSOR_MCP_ISOLATION", "0");
    let ws = tmp_dir("cursor-refuse-iso-off");
    let spec = team_agent::model::yaml::loads(
        "agents:\n  - id: a\n    provider: cursor_agent\n  - id: b\n    provider: cursor_agent\n",
    )
    .expect("spec");
    let err = refuse_second_cursor_occupant(&ws, "b", Some(&spec));
    restore_isolation_env(previous);
    let err = err.expect_err("isolation off must fail-closed");
    match err {
        LifecycleError::RequirementUnmet(text) => {
            assert!(
                text.contains("cursor_agent seat already occupies this workspace"),
                "fail-closed text must stay: {text}"
            );
            assert!(
                text.contains("last-writer"),
                "reason must still name last-writer: {text}"
            );
        }
        other => panic!("expected RequirementUnmet, got {other:?}"),
    }
}
