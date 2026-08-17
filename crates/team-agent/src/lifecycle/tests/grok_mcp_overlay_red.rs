//! purpose: grok 席位必须把 team-agent MCP 写进 grok 实际会读的项目配置
//! contract: grok worker spawn 之后 `<workspace>/.grok/config.toml` 含
//!   `[mcp_servers.team_orchestrator]`，command/args/env 已替换成这次席位的真实值
//!   （不是 `{workspace}` 占位，也不是 `.team/runtime/mcp/*.json`）
//! boundary: 只覆盖 grok launch 产物；不改 claude/codex/copilot 路径
//!
//! 修之前判红：基线只把 MCP 写到 `.team/runtime/mcp/<agent>.json`，grok CLI
//! 不读那份文件，所以项目作用域 config.toml 不会出现已解析的 team-agent 段。

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
    apply_grok_mcp_overlay, ensure_grok_login_and_folder_trust, LifecycleError,
};
use team_agent::provider::McpConfig;
use team_agent::transport::test_support::OfflineTransport;

#[test]
fn grok_overlay_writes_canonical_team_orchestrator_server_name() {
    let ws = tmp_dir("grok-mcp-name");
    apply_grok_mcp_overlay(&ws, &sample_mcp_config("name-seat", "/ws-name"))
        .expect("overlay write");
    let text = std::fs::read_to_string(ws.join(".grok/config.toml")).unwrap();
    assert!(
        text.contains("[mcp_servers.team_orchestrator]"),
        "grok overlay must use the canonical server name the runtime contract cites; text={text}"
    );
    assert!(
        text.contains("[mcp_servers.team_orchestrator.env]"),
        "env table must sit under team_orchestrator, not a second name; text={text}"
    );
    assert!(
        !text.contains("mcp_servers.team-agent"),
        "canonical name is team_orchestrator; leftover team-agent would namespace tools as team-agent__*; text={text}"
    );
}

#[test]
fn grok_overlay_migrates_legacy_team_agent_table_away() {
    let ws = tmp_dir("grok-mcp-migrate");
    let grok = ws.join(".grok");
    std::fs::create_dir_all(&grok).unwrap();
    std::fs::write(
        grok.join("config.toml"),
        r#"[mcp_servers.keep-me]
command = "/bin/keep"

[mcp_servers.team-agent]
command = "/old/team-agent"
args = ["stale"]
enabled = true

[mcp_servers.team-agent.env]
TEAM_AGENT_ID = "stale-id"
TEAM_AGENT_WORKSPACE = "/stale-ws"
TEAM_AGENT_OWNER_TEAM_ID = "stale-team"
TEAM_AGENT_AUTH_MODE = "subscription"
"#,
    )
    .unwrap();

    apply_grok_mcp_overlay(&ws, &sample_mcp_config("migrated-seat", "/ws-migrated"))
        .expect("overlay migrate");
    let text = std::fs::read_to_string(grok.join("config.toml")).unwrap();

    assert!(
        !text.contains("mcp_servers.team-agent"),
        "legacy [mcp_servers.team-agent] must be removed or grok exposes two Team MCP servers; text={text}"
    );
    assert!(
        text.contains("[mcp_servers.team_orchestrator]"),
        "migrated file must declare the canonical server; text={text}"
    );
    assert!(
        text.contains("TEAM_AGENT_ID = \"migrated-seat\"")
            && text.contains("TEAM_AGENT_WORKSPACE = \"/ws-migrated\""),
        "identity env must land on the new table, not vanish during rename; text={text}"
    );
    assert!(
        text.contains("[mcp_servers.keep-me]"),
        "unrelated project MCP servers must survive the rename; text={text}"
    );
    assert!(
        !text.contains("stale-id") && !text.contains("/old/team-agent"),
        "stale identity/command from the old table must not remain; text={text}"
    );
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
fn two_grok_seats_sharing_cwd_must_refuse_to_start() {
    let ws = tmp_dir("grok-cwd-collision");
    let team = write_grok_team_agents(&ws, "grokteam", &["g1", "g2"]);

    let result = quick_start_with_transport_in_workspace(
        &ws,
        &team,
        None,
        true,
        Some("grokteam"),
        &OfflineTransport::new(),
    );

    let err = match result {
        Ok(report) => panic!(
            "two grok seats sharing cwd must refuse to start; silent overlay overwrite \
             would make both seats use the last TEAM_AGENT_ID; report={report:?}"
        ),
        Err(error) => error.to_string(),
    };
    assert!(
        err.contains("already occupies this workspace") || err.contains("g1"),
        "error must name the occupying grok seat; err={err}"
    );
    assert!(
        err.contains(".grok/config.toml") && err.to_ascii_lowercase().contains("directory-scoped"),
        "error must name the directory-scoped MCP cause; err={err}"
    );
    assert!(
        err.contains("g1") && err.contains("g2"),
        "error must name both colliding grok seats; err={err}"
    );
    assert!(
        !err.contains("worktree") && !err.contains("then retry"),
        "must not promise a remedy this version cannot honor; err={err}"
    );
    assert!(
        !ws.join(".grok").join("config.toml").exists(),
        "must refuse before writing overlay; a leftover .grok/config.toml is a half-written identity"
    );
}

#[test]
#[serial(env)]
fn grok_untrusted_folder_refuses_to_start() {
    let ws = tmp_dir("grok-untrusted");
    let home = tmp_dir("grok-untrusted-home");
    seed_grok_home(&home, None);
    let _guard = HomeGuard::set(&home);
    let team = write_grok_team(&ws, "grokteam", "grok_writer");
    let err = quick_start_with_transport_in_workspace(
        &ws,
        &team,
        None,
        true,
        Some("grokteam"),
        &OfflineTransport::new(),
    )
    .expect_err("untrusted folder must not start a grok seat");
    let text = err.to_string();
    assert!(
        text.contains("not trusted") && text.contains("action:"),
        "untrusted-folder error must be actionable; err={text}"
    );
    assert!(
        text.contains("grok --trust") || text.contains("/hooks-trust"),
        "next step must name grok --trust or /hooks-trust; err={text}"
    );
}

#[test]
#[serial(env)]
fn grok_missing_login_refuses_to_start() {
    let ws = tmp_dir("grok-nologin");
    let home = tmp_dir("grok-nologin-home");
    std::fs::create_dir_all(home.join(".grok")).unwrap();
    std::fs::write(
        home.join(".grok").join("trusted_folders.toml"),
        format!("[folders.\"{}\"]\ntrusted = true\n", ws.display()),
    )
    .unwrap();
    let _guard = HomeGuard::set(&home);
    let err = ensure_grok_login_and_folder_trust(&ws).expect_err("missing auth.json");
    match err {
        LifecycleError::RequirementUnmet(text) => {
            assert!(
                text.contains("grok login") && text.contains("action:"),
                "login error must tell the operator to run grok login; err={text}"
            );
        }
        other => panic!("expected RequirementUnmet, got {other:?}"),
    }
}

#[test]
#[serial(env)]
fn grok_spawn_writes_resolved_team_agent_into_project_grok_config() {
    let ws = tmp_dir("grok-mcp-overlay");
    let home = tmp_dir("grok-mcp-overlay-home");
    seed_grok_home(&home, Some(&ws));
    let _guard = HomeGuard::set(&home);
    let team = write_grok_team(&ws, "grokteam", "grok_writer");
    let config_path = ws.join(".grok").join("config.toml");
    assert!(
        !config_path.exists(),
        "precondition: project grok config must be absent before spawn; path={}",
        config_path.display()
    );

    quick_start_with_transport_in_workspace(
        &ws,
        &team,
        None,
        true,
        Some("grokteam"),
        &OfflineTransport::new(),
    )
    .expect("grok quick-start through offline transport should spawn");

    let text = std::fs::read_to_string(&config_path).unwrap_or_else(|err| {
        panic!(
            "grok spawn must materialize {}; grok CLI only reads this project-scope file: {err}",
            config_path.display()
        )
    });
    let expected_command = current_team_agent_command();
    let workspace = ws.to_string_lossy();

    assert!(
        text.contains("[mcp_servers.team_orchestrator]"),
        "grok project config must declare [mcp_servers.team_orchestrator]; path={} text={text}",
        config_path.display()
    );
    assert!(
        text.contains(&format!("command = \"{expected_command}\""))
            || text.contains(&format!("command = '{expected_command}'")),
        "command must be the running team-agent binary, not a PATH name; expected={expected_command} text={text}"
    );
    assert!(
        text.contains("\"mcp-server\"") && text.contains("\"--workspace\""),
        "args must launch mcp-server; text={text}"
    );
    assert!(
        text.contains(&format!("\"{workspace}\""))
            || text.contains(&format!("'{workspace}'")),
        "args/--workspace must be the real workspace, not {{workspace}}; workspace={workspace} text={text}"
    );
    assert!(
        !text.contains("{workspace}")
            && !text.contains("{agent_id}")
            && !text.contains("{team_id}"),
        "placeholders must be resolved before grok reads the file; text={text}"
    );
    assert!(
        text.contains("enabled = true"),
        "grok mcp add --scope project writes enabled = true; text={text}"
    );
    assert!(
        text.contains("[mcp_servers.team_orchestrator.env]"),
        "identity env must be under [mcp_servers.team_orchestrator.env]; text={text}"
    );
    assert!(
        !text.contains("mcp_servers.team-agent"),
        "spawned overlay must not leave the misnamed team-agent table; text={text}"
    );
    assert!(
        text.contains("TEAM_AGENT_ID = \"grok_writer\"")
            || text.contains("TEAM_AGENT_ID = 'grok_writer'"),
        "TEAM_AGENT_ID must be this grok seat, not a leftover placeholder; text={text}"
    );
    assert!(
        text.contains(&format!("TEAM_AGENT_WORKSPACE = \"{workspace}\""))
            || text.contains(&format!("TEAM_AGENT_WORKSPACE = '{workspace}'")),
        "TEAM_AGENT_WORKSPACE must match spawn cwd; workspace={workspace} text={text}"
    );
    assert!(
        text.contains("TEAM_AGENT_OWNER_TEAM_ID = \"grokteam\"")
            || text.contains("TEAM_AGENT_OWNER_TEAM_ID = 'grokteam'"),
        "TEAM_AGENT_OWNER_TEAM_ID must be the runtime team key; text={text}"
    );
    assert!(
        text.contains("TEAM_AGENT_AUTH_MODE = \"subscription\"")
            || text.contains("TEAM_AGENT_AUTH_MODE = 'subscription'"),
        "TEAM_AGENT_AUTH_MODE must come from the resolved MCP config; text={text}"
    );
}

fn current_team_agent_command() -> String {
    let exe = std::env::current_exe().expect("current_exe");
    match std::fs::canonicalize(&exe) {
        Ok(canon) => canon.to_string_lossy().into_owned(),
        Err(_) => exe.to_string_lossy().into_owned(),
    }
}

fn write_grok_team(ws: &Path, team_key: &str, agent_id: &str) -> PathBuf {
    write_grok_team_agents(ws, team_key, &[agent_id])
}

fn write_grok_team_agents(ws: &Path, team_key: &str, agent_ids: &[&str]) -> PathBuf {
    let team = ws.join(team_key);
    std::fs::create_dir_all(team.join("agents")).unwrap();
    std::fs::write(
        team.join("TEAM.md"),
        format!(
            "---\nname: {team_key}\nobjective: grok MCP overlay contract.\nprovider: grok\ndangerously_skip_permissions: false\n---\n\nTeam.\n"
        ),
    )
    .unwrap();
    for agent_id in agent_ids {
        std::fs::write(
            team.join("agents").join(format!("{agent_id}.md")),
            format!(
                "---\nname: {agent_id}\nrole: Grok Writer\nprovider: grok\nmodel: grok-4\nauth_mode: subscription\ndangerously_skip_permissions: false\ntools:\n  - mcp_team\n---\n\nWorker.\n"
            ),
        )
        .unwrap();
    }
    team
}

fn seed_grok_home(home: &Path, trusted_cwd: Option<&Path>) {
    let grok = home.join(".grok");
    std::fs::create_dir_all(&grok).unwrap();
    std::fs::write(grok.join("auth.json"), r#"{"test":"ok"}"#).unwrap();
    if let Some(cwd) = trusted_cwd {
        std::fs::write(
            grok.join("trusted_folders.toml"),
            format!("[folders.\"{}\"]\ntrusted = true\n", cwd.display()),
        )
        .unwrap();
    }
}

struct HomeGuard {
    prev: Option<String>,
}

impl HomeGuard {
    fn set(home: &Path) -> Self {
        let prev = std::env::var("HOME").ok();
        std::env::set_var("HOME", home);
        Self { prev }
    }
}

impl Drop for HomeGuard {
    fn drop(&mut self) {
        match &self.prev {
            Some(value) => std::env::set_var("HOME", value),
            None => std::env::remove_var("HOME"),
        }
    }
}

fn tmp_dir(tag: &str) -> PathBuf {
    static N: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "ta-rs-grok-mcp-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::canonicalize(dir).unwrap()
}
