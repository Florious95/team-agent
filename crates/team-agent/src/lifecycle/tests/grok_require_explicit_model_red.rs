//! purpose: claude 席位角色文件缺 model 时仍能正常启动
//! contract: 缺 model 的 claude 角色启动成功并真的 spawn worker
//! boundary: legacy Grok worker runtime 已移除（grok 角色现走 grok_build 合约叶），
//!   本文件只保留 claude 缺 model 的回归钉子

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[path = "../../../tests/support/hermetic.rs"]
mod hermetic_guard;
#[allow(dead_code)]
fn _hermetic_boundary_marker(_: &hermetic_guard::HermeticTestEnv) {}

fn enter_hermetic(tag: &str) -> hermetic_guard::HermeticTestEnv {
    hermetic_guard::HermeticTestEnv::enter_with_leader(tag)
}

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serial_test::serial;
use team_agent::lifecycle::quick_start_with_transport_in_workspace;
use team_agent::transport::test_support::OfflineTransport;

/// claude 角色缺 model ⇒ 行为不变（仍能启动）。
#[test]
#[serial(env)]
fn claude_role_missing_model_still_starts() {
    let _hermetic = enter_hermetic("grok-claude-missing-model");
    let ws = tmp_dir("claude-no-model");
    super::launch_spawn::seed_healthy_coordinator(&ws);
    let team = write_role_team(
        &ws,
        "claudeteam",
        "clauder",
        "claude",
        None,
        "Claude Worker",
    );
    let transport = OfflineTransport::new();

    let result = quick_start_with_transport_in_workspace(
        &ws,
        &team,
        None,
        true,
        Some("claudeteam"),
        &transport,
    );
    assert!(
        result.is_ok(),
        "claude role without model must keep starting; err={result:?}"
    );
    let argv = first_spawn_argv(&transport);
    assert!(
        !argv.is_empty(),
        "claude seat must still spawn; argv={argv:?}"
    );
}

fn first_spawn_argv(transport: &OfflineTransport) -> Vec<String> {
    let records = transport.spawn_records();
    assert!(
        !records.is_empty(),
        "expected a worker spawn so argv can be inspected; records={records:?}"
    );
    records[0].1.clone()
}

fn write_role_team(
    ws: &Path,
    team_key: &str,
    agent_id: &str,
    provider: &str,
    model: Option<&str>,
    role: &str,
) -> PathBuf {
    let team = ws.join(team_key);
    std::fs::create_dir_all(team.join("agents")).unwrap();
    std::fs::write(
        team.join("TEAM.md"),
        format!(
            "---\nname: {team_key}\nobjective: explicit-model contract.\nprovider: {provider}\ndangerously_skip_permissions: false\n---\n\nTeam.\n"
        ),
    )
    .unwrap();
    let model_line = match model {
        Some(value) => format!("model: {value}\n"),
        None => String::new(),
    };
    std::fs::write(
        team.join("agents").join(format!("{agent_id}.md")),
        format!(
            "---\nname: {agent_id}\nrole: {role}\nprovider: {provider}\n{model_line}auth_mode: subscription\ndangerously_skip_permissions: false\ntools:\n  - mcp_team\n---\n\nWorker.\n"
        ),
    )
    .unwrap();
    team
}

fn tmp_dir(tag: &str) -> PathBuf {
    static N: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "ta-rs-grok-model-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::canonicalize(dir).unwrap()
}
