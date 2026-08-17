//! purpose: grok 席位角色文件必须显式写 model，缺则拒绝启动
//! contract: 缺 model 的 grok 角色启动失败（错误含 model 与原因）；写了 model
//!   则该值进入 worker argv 的 `--model <值>`；claude 缺 model 行为不变
//! boundary: 只钉 Provider::Grok 的启动契约；不改 claude/codex/copilot 路径
//!
//! 修之前判红（40e91fba）：grok 角色缺 model 仍会成功启动（compiler 填
//! 内建默认 grok-4，argv 带 `--model grok-4`，看起来完全正常）。

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[path = "../../../tests/support/hermetic.rs"]
mod hermetic_guard;
#[allow(dead_code)]
fn _hermetic_boundary_marker(_: &hermetic_guard::HermeticTestEnv) {}

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serial_test::serial;
use team_agent::lifecycle::quick_start_with_transport_in_workspace;
use team_agent::transport::test_support::OfflineTransport;

/// 1. grok 角色缺 model ⇒ 启动被拒，错误含 "model" 与「内建默认 / 隐式来源」。
///    40e91fba 上会成功启动（有 overlay / 有 spawn）⇒ 本条红。
#[test]
#[serial(env)]
fn grok_role_missing_model_refuses_to_start() {
    let ws = tmp_dir("grok-no-model");
    let home = tmp_dir("grok-no-model-home");
    seed_grok_home(&home, Some(&ws));
    let _guard = HomeGuard::set(&home);
    let team = write_role_team(
        &ws,
        "grokteam",
        "grok_writer",
        "grok",
        None,
        "Grok Writer",
    );
    let transport = OfflineTransport::new();

    let result = quick_start_with_transport_in_workspace(
        &ws,
        &team,
        None,
        true,
        Some("grokteam"),
        &transport,
    );

    let err = match result {
        Ok(report) => panic!(
            "grok role without model must refuse to start; a built-in default \
             would silently pick the model; report={report:?}"
        ),
        Err(error) => error.to_string(),
    };
    let lower = err.to_ascii_lowercase();
    assert!(
        lower.contains("model"),
        "error must name the missing field; err={err}"
    );
    assert!(
        lower.contains("built-in") || lower.contains("builtin") || err.contains("内建"),
        "error must name the built-in default the framework would fill; err={err}"
    );
    assert!(
        lower.contains("implicit") || err.contains("隐式"),
        "error must reject every implicit source, not just one fallback; err={err}"
    );
    assert!(
        err.contains("model: grok-4.6") || err.contains("model: grok-4"),
        "error must show how to write the field; err={err}"
    );
    assert!(
        !err.to_ascii_lowercase().contains("worktree") && !err.contains("then retry"),
        "must not promise a remedy this version cannot honor; err={err}"
    );
    assert!(
        transport.spawn_records().is_empty(),
        "refusal must happen before spawn; records={:?}",
        transport.spawn_records()
    );
    assert!(
        !ws.join(".grok").join("config.toml").exists(),
        "must refuse before writing grok overlay; leftover .grok/config.toml means the seat started"
    );
}

/// 2. grok 角色写了 model ⇒ 正常启动，且该值真的进 argv（`--model <值>` 相邻）。
#[test]
#[serial(env)]
fn grok_role_explicit_model_reaches_argv() {
    let ws = tmp_dir("grok-with-model");
    let home = tmp_dir("grok-with-model-home");
    seed_grok_home(&home, Some(&ws));
    let _guard = HomeGuard::set(&home);
    let team = write_role_team(
        &ws,
        "grokteam",
        "grok_writer",
        "grok",
        Some("grok-4.6"),
        "Grok Writer",
    );
    let transport = OfflineTransport::new();

    quick_start_with_transport_in_workspace(
        &ws,
        &team,
        None,
        true,
        Some("grokteam"),
        &transport,
    )
    .expect("grok role with explicit model must start");

    let argv = first_spawn_argv(&transport);
    assert!(
        argv_contains_adjacent(&argv, &["--model", "grok-4.6"]),
        "declared model must appear as adjacent `--model grok-4.6` in worker argv; argv={argv:?}"
    );
}

/// 3. claude 角色缺 model ⇒ 行为不变（仍能启动，不被 grok 的硬性 model 要求误伤）。
#[test]
fn claude_role_missing_model_still_starts() {
    let ws = tmp_dir("claude-no-model");
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
        "claude role without model must keep starting; grok-only requirement leaked; err={result:?}"
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

fn argv_contains_adjacent(hay: &[String], needle: &[&str]) -> bool {
    if needle.is_empty() {
        return true;
    }
    hay.windows(needle.len())
        .any(|window| window.iter().map(String::as_str).eq(needle.iter().copied()))
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
        "ta-rs-grok-model-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::canonicalize(dir).unwrap()
}
