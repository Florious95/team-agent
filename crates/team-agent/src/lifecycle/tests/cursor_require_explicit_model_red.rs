//! ---
//! purpose: cursor 席位角色必须显式写 model；写了 effort 必须编译失败
//! contract:
//!   provides:
//!     - name: cursor-missing-model-refuses
//!       what: 缺 model 的 cursor 角色启动失败（错误含 model）
//!     - name: cursor-explicit-model-reaches-argv
//!       what: 写了 model 则该值进入 `--model <值>`
//!     - name: cursor-effort-refuses-compile
//!       what: 角色写 effort 则失败，不得丢掉后仍起席
//!     - name: cursor-second-seat-isolated-identity
//!       what: 默认 per-seat MCP 隔离时同 workspace 两个 CursorAgent 都启动且身份各自保留
//! boundary:
//!   - 只钉 Provider::CursorAgent；不改 grok/claude 路径
//! maturity: wired
//! ---

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[path = "../../../tests/support/hermetic.rs"]
mod hermetic_guard;
#[allow(dead_code)]
fn _hermetic_boundary_marker(_: &hermetic_guard::HermeticTestEnv) {}

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serial_test::serial;
use team_agent::lifecycle::cursor_mcp_json_path;
use team_agent::lifecycle::quick_start_with_transport_in_workspace;
use team_agent::transport::test_support::OfflineTransport;

#[test]
#[serial(env)]
fn cursor_role_missing_model_refuses_to_start() {
    let ws = tmp_dir("cursor-no-model");
    let team = write_role_team(&ws, "cursortm", "cursor_writer", None, None);
    let transport = OfflineTransport::new();
    let result = quick_start_with_transport_in_workspace(
        &ws,
        &team,
        None,
        true,
        Some("cursortm"),
        &transport,
    );
    let err = match result {
        Ok(report) => panic!("cursor role without model must refuse; report={report:?}"),
        Err(error) => error.to_string(),
    };
    let lower = err.to_ascii_lowercase();
    assert!(
        lower.contains("model"),
        "error must name the missing field; err={err}"
    );
    assert!(
        lower.contains("built-in") || lower.contains("builtin") || err.contains("内建"),
        "error must name the built-in default; err={err}"
    );
    assert!(
        transport.spawn_records().is_empty(),
        "refusal must happen before spawn; records={:?}",
        transport.spawn_records()
    );
}

#[test]
#[serial(env)]
fn cursor_role_explicit_model_reaches_argv() {
    let ws = tmp_dir("cursor-with-model");
    let team = write_role_team(
        &ws,
        "cursortm",
        "cursor_writer",
        Some("sonnet-4-thinking"),
        None,
    );
    let transport = OfflineTransport::new();
    quick_start_with_transport_in_workspace(&ws, &team, None, true, Some("cursortm"), &transport)
        .expect("cursor role with explicit model must start");
    let argv = first_spawn_argv(&transport);
    assert!(
        argv_contains_adjacent(&argv, &["--model", "sonnet-4-thinking"]),
        "declared model must appear; argv={argv:?}"
    );
    assert!(
        argv_contains_adjacent(&argv, &["--trust"]),
        "argv must keep --trust; argv={argv:?}"
    );
}

#[test]
#[serial(env)]
fn cursor_role_with_effort_refuses_to_start() {
    let ws = tmp_dir("cursor-effort");
    let team = write_role_team(
        &ws,
        "cursortm",
        "cursor_writer",
        Some("sonnet-4-thinking"),
        Some("high"),
    );
    let transport = OfflineTransport::new();
    let result = quick_start_with_transport_in_workspace(
        &ws,
        &team,
        None,
        true,
        Some("cursortm"),
        &transport,
    );
    let err = match result {
        Ok(report) => panic!("cursor role with effort must refuse; report={report:?}"),
        Err(error) => error.to_string(),
    };
    let lower = err.to_ascii_lowercase();
    assert!(
        lower.contains("effort"),
        "error must name effort; err={err}"
    );
    assert!(
        transport.spawn_records().is_empty(),
        "must refuse before spawn; records={:?}",
        transport.spawn_records()
    );
}

#[test]
#[serial(env)]
fn cursor_second_seat_in_same_workspace_uses_isolated_identity() {
    let previous_isolation = std::env::var("TEAM_AGENT_CURSOR_MCP_ISOLATION").ok();
    std::env::remove_var("TEAM_AGENT_CURSOR_MCP_ISOLATION");
    let ws = tmp_dir("cursor-two");
    let team = ws.join("cursortm");
    std::fs::create_dir_all(team.join("agents")).unwrap();
    std::fs::write(
        team.join("TEAM.md"),
        "---\nname: cursortm\nobjective: two cursor seats.\nprovider: cursor_agent\ndangerously_skip_permissions: true\n---\n\nTeam.\n",
    )
    .unwrap();
    for id in ["cursor_a", "cursor_b"] {
        std::fs::write(
            team.join("agents").join(format!("{id}.md")),
            format!(
                "---\nname: {id}\nrole: Cursor Writer\nprovider: cursor_agent\nmodel: sonnet-4-thinking\nauth_mode: subscription\ndangerously_skip_permissions: true\ntools:\n  - mcp_team\n---\n\nWorker.\n"
            ),
        )
        .unwrap();
    }
    let transport = OfflineTransport::new();
    let result = quick_start_with_transport_in_workspace(
        &ws,
        &team,
        None,
        true,
        Some("cursortm"),
        &transport,
    );
    restore_isolation_env(previous_isolation);
    result.expect("default per-seat MCP isolation must allow both Cursor seats");
    let records = transport.spawn_records();
    assert_eq!(
        records.len(),
        2,
        "both Cursor seats must spawn; records={records:?}"
    );

    let first = std::fs::read_to_string(cursor_mcp_json_path(&ws, "cursor_a").unwrap())
        .expect("first seat MCP config");
    let second = std::fs::read_to_string(cursor_mcp_json_path(&ws, "cursor_b").unwrap())
        .expect("second seat MCP config");
    assert!(
        first.contains("\"cursor_a\"") && !first.contains("\"cursor_b\""),
        "first seat must retain its own TEAM_AGENT_ID"
    );
    assert!(
        second.contains("\"cursor_b\"") && !second.contains("\"cursor_a\""),
        "second seat must retain its own TEAM_AGENT_ID"
    );
}

fn restore_isolation_env(previous: Option<String>) {
    match previous {
        Some(value) => std::env::set_var("TEAM_AGENT_CURSOR_MCP_ISOLATION", value),
        None => std::env::remove_var("TEAM_AGENT_CURSOR_MCP_ISOLATION"),
    }
}

fn first_spawn_argv(transport: &OfflineTransport) -> Vec<String> {
    let records = transport.spawn_records();
    assert!(
        !records.is_empty(),
        "expected a worker spawn; records={records:?}"
    );
    records[0].1.clone()
}

fn argv_contains_adjacent(hay: &[String], needle: &[&str]) -> bool {
    hay.windows(needle.len())
        .any(|window| window.iter().map(String::as_str).eq(needle.iter().copied()))
}

fn write_role_team(
    ws: &Path,
    team_key: &str,
    agent_id: &str,
    model: Option<&str>,
    effort: Option<&str>,
) -> PathBuf {
    let team = ws.join(team_key);
    std::fs::create_dir_all(team.join("agents")).unwrap();
    std::fs::write(
        team.join("TEAM.md"),
        format!(
            "---\nname: {team_key}\nobjective: cursor model contract.\nprovider: cursor_agent\ndangerously_skip_permissions: true\n---\n\nTeam.\n"
        ),
    )
    .unwrap();
    let model_line = match model {
        Some(value) => format!("model: {value}\n"),
        None => String::new(),
    };
    let effort_line = match effort {
        Some(value) => format!("effort: {value}\n"),
        None => String::new(),
    };
    std::fs::write(
        team.join("agents").join(format!("{agent_id}.md")),
        format!(
            "---\nname: {agent_id}\nrole: Cursor Writer\nprovider: cursor_agent\n{model_line}{effort_line}auth_mode: subscription\ndangerously_skip_permissions: true\ntools:\n  - mcp_team\n---\n\nWorker.\n"
        ),
    )
    .unwrap();
    team
}

fn tmp_dir(tag: &str) -> PathBuf {
    static N: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "ta-rs-cursor-model-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::canonicalize(dir).unwrap()
}
