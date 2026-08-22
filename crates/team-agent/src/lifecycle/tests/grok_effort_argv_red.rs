//! ---
//! purpose: grok 席位角色 effort 必须进入 argv；缺字段不发 flag
//! contract:
//!   provides:
//!     - name: grok-effort-medium-reaches-argv
//!       what: effort: medium ⇒ 相邻 `--effort medium`
//!     - name: grok-effort-absent-omits-flag
//!       what: 角色无 effort 字段 ⇒ argv 中 `--effort` 次数为 0
//!     - name: grok-effort-survives-restart
//!       what: restart 后仍带同一档
//!     - name: grok-effort-no-longer-silently-dropped
//!       what: 已废除「grok effort 被静默丢弃 + effort_unsupported」
//! boundary: 只钉 Provider::Grok 的 effort 启动契约；不改 claude/codex/cursor
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
use team_agent::lifecycle::{quick_start_with_transport_in_workspace, restart_with_transport};
use team_agent::transport::test_support::OfflineTransport;

#[test]
#[serial(env)]
fn grok_role_effort_medium_reaches_argv() {
    let (ws, _home, _guard, team) = seed_grok("effort-medium", Some("medium"));
    let transport = OfflineTransport::new();
    quick_start_with_transport_in_workspace(&ws, &team, None, true, Some("grokteam"), &transport)
        .expect("grok role with effort: medium must start");
    let argv = first_spawn_argv(&transport);
    assert!(
        argv_contains_adjacent(&argv, &["--effort", "medium"]),
        "declared effort must appear as adjacent `--effort medium`; argv={argv:?}"
    );
}

#[test]
#[serial(env)]
fn grok_role_without_effort_emits_zero_effort_flags() {
    let (ws, _home, _guard, team) = seed_grok("effort-absent", None);
    let transport = OfflineTransport::new();
    quick_start_with_transport_in_workspace(&ws, &team, None, true, Some("grokteam"), &transport)
        .expect("grok role without effort must start");
    let argv = first_spawn_argv(&transport);
    let count = argv.iter().filter(|a| *a == "--effort").count();
    assert_eq!(
        count, 0,
        "absent effort must not invent a default flag; argv={argv:?}"
    );
}

#[test]
#[serial(env)]
fn grok_restart_keeps_declared_effort_medium() {
    let (ws, _home, _guard, team) = seed_grok("effort-restart", Some("medium"));
    let first = OfflineTransport::new();
    quick_start_with_transport_in_workspace(&ws, &team, None, true, Some("grokteam"), &first)
        .expect("initial grok start");
    assert!(
        argv_contains_adjacent(&first_spawn_argv(&first), &["--effort", "medium"]),
        "precondition: first start must carry medium; argv={:?}",
        first_spawn_argv(&first)
    );

    seed_healthy_coordinator(&ws);

    let restart = OfflineTransport::new();
    restart_with_transport(&ws, true, Some("grokteam"), &restart)
        .expect("restart --allow-fresh must start the grok seat");
    let argv = first_spawn_argv(&restart);
    assert!(
        argv_contains_adjacent(&argv, &["--effort", "medium"]),
        "restart must keep the persisted effort; argv={argv:?}"
    );
}

#[test]
#[serial(env)]
fn grok_role_effort_max_is_rejected_at_compile() {
    let (ws, _home, _guard, team) = seed_grok("effort-max", Some("max"));
    let err =
        team_agent::compiler::compile_team(&team).expect_err("grok + effort:max must not compile");
    let text = err.to_string();
    assert!(
        text.contains("max") && (text.contains("claude") || text.contains("only supported")),
        "max must stay claude-only at compile; err={text}"
    );
}

#[test]
#[serial(env)]
fn grok_medium_effort_no_longer_emits_unsupported_event() {
    // 已废除的行为：旧实现把 grok 的 effort 静默丢掉并写 provider.effort_unsupported，
    // 此断言证明它确实没了。
    let (ws, _home, _guard, team) = seed_grok("effort-tombstone", Some("medium"));
    let transport = OfflineTransport::new();
    quick_start_with_transport_in_workspace(&ws, &team, None, true, Some("grokteam"), &transport)
        .expect("grok role with effort: medium must start");
    let events = team_agent::event_log::EventLog::new(&ws)
        .tail(0)
        .expect("events");
    let dropped = events.iter().any(|event| {
        event.get("event").and_then(serde_json::Value::as_str)
            == Some("provider.effort_unsupported")
    });
    assert!(
        !dropped,
        "grok medium must not emit provider.effort_unsupported; events={events:?}"
    );
}

/// restart 的就绪门要求 `coordinator_health().ok`，而单测里 `start_coordinator`
/// spawn 的是 `std::env::current_exe()` —— 也就是 libtest 测试二进制本身，它一启动
/// 就以 `Unrecognized option: 'workspace'` 退出并变成僵尸，`pid_is_running` 的
/// `ps stat=` 一看到 `Z` 就判 not running。于是就绪门只能靠「抢在死婴被看到之前
/// 探到它」这个竞态过关，慢机/高负载上必然超时。这里按本仓既有写法（见
/// `copilot_provider_red.rs` / `restart_build_before_destroy_0540_contract.rs`）
/// 预先把 pid 文件与 metadata 指向测试进程自己，让 `start_coordinator` 走
/// `AlreadyRunning` 早返回，竞态被删掉而不是被调宽。
fn seed_healthy_coordinator(workspace: &Path) {
    let workspace = crate::coordinator::WorkspacePath::new(workspace.to_path_buf());
    let pid = crate::coordinator::Pid::new(std::process::id());
    std::fs::create_dir_all(
        crate::coordinator::coordinator_pid_path(&workspace)
            .parent()
            .unwrap(),
    )
    .unwrap();
    crate::coordinator::write_coordinator_metadata(
        &workspace,
        pid,
        crate::coordinator::MetadataSource::Boot,
    )
    .expect("write coordinator metadata");
    std::fs::write(
        crate::coordinator::coordinator_pid_path(&workspace),
        pid.to_string(),
    )
    .expect("write coordinator pid");
}

fn seed_grok(tag: &str, effort: Option<&str>) -> (PathBuf, PathBuf, HomeGuard, PathBuf) {
    let ws = tmp_dir(tag);
    let home = tmp_dir(&format!("{tag}-home"));
    seed_grok_home(&home, Some(&ws));
    let guard = HomeGuard::set(&home);
    let team = write_grok_role(&ws, effort);
    (ws, home, guard, team)
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
    hay.windows(needle.len())
        .any(|window| window.iter().map(String::as_str).eq(needle.iter().copied()))
}

fn write_grok_role(ws: &Path, effort: Option<&str>) -> PathBuf {
    let team = ws.join("grokteam");
    std::fs::create_dir_all(team.join("agents")).unwrap();
    std::fs::write(
        team.join("TEAM.md"),
        "---\nname: grokteam\nobjective: grok effort argv.\nprovider: grok\ndangerously_skip_permissions: false\n---\n\nTeam.\n",
    )
    .unwrap();
    let effort_line = match effort {
        Some(value) => format!("effort: {value}\n"),
        None => String::new(),
    };
    std::fs::write(
        team.join("agents").join("grok_writer.md"),
        format!(
            "---\nname: grok_writer\nrole: Grok Writer\nprovider: grok\nmodel: grok-4.6\n{effort_line}auth_mode: subscription\ndangerously_skip_permissions: false\ntools:\n  - mcp_team\n---\n\nWorker.\n"
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
        "ta-rs-grok-effort-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::canonicalize(dir).unwrap()
}
