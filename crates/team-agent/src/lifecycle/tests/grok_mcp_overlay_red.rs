//! purpose: grok 席位必须把 team-agent MCP 写进 grok 实际会读的项目配置
//! contract: grok worker spawn 之后 `<workspace>/.grok/config.toml` 含
//!   `[mcp_servers.team-agent]`，command/args/env 已替换成这次席位的真实值
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

use team_agent::lifecycle::quick_start_with_transport_in_workspace;
use team_agent::transport::test_support::OfflineTransport;

#[test]
fn grok_spawn_writes_resolved_team_agent_into_project_grok_config() {
    let ws = tmp_dir("grok-mcp-overlay");
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
        text.contains("[mcp_servers.team-agent]"),
        "grok project config must declare [mcp_servers.team-agent]; path={} text={text}",
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
        text.contains("[mcp_servers.team-agent.env]"),
        "identity env must be under [mcp_servers.team-agent.env]; text={text}"
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
    let team = ws.join(team_key);
    std::fs::create_dir_all(team.join("agents")).unwrap();
    std::fs::write(
        team.join("TEAM.md"),
        format!(
            "---\nname: {team_key}\nobjective: grok MCP overlay contract.\nprovider: grok\n---\n\nTeam.\n"
        ),
    )
    .unwrap();
    std::fs::write(
        team.join("agents").join(format!("{agent_id}.md")),
        format!(
            "---\nname: {agent_id}\nrole: Grok Writer\nprovider: grok\nmodel: grok-4\nauth_mode: subscription\ntools:\n  - mcp_team\n---\n\nWorker.\n"
        ),
    )
    .unwrap();
    team
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
