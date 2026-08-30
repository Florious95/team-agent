//! ---
//! purpose: cursor 无假 pending；有 chatId+磁盘 marker 时 restart 走 --resume
//! contract:
//!   provides:
//!     - name: cursor-fresh-no-fake-session-id
//!       what: fresh spawn 不写 --session-id，_pending 空，session_id 空
//!     - name: cursor-restart-resume-argv
//!       what: session_id + store.db 齐时 restart 发 `--resume <chatId>`
//!     - name: cursor-classify-resume-when-archive-present
//!       what: classify 在 marker 存在时 Resume，缺失时 Refuse 而非静默 fresh
//! boundary:
//!   - 不改 reset-agent --discard-session
//!   - 不覆盖 grok/claude 三拍
//! maturity: wired
//! ---

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[path = "../../../tests/support/hermetic.rs"]
mod hermetic_guard;
#[allow(dead_code)]
fn _hermetic_boundary_marker(_: &hermetic_guard::HermeticTestEnv) {}

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::lifecycle::restart::classify_restart_plan_with_resume_validation;
use crate::lifecycle::types::ResumeDecision;
use crate::provider::session_scan::cursor::{
    cursor_session_archive_present, cursor_session_dir,
};
use crate::state::persist::{load_runtime_state, save_runtime_state};
use serial_test::serial;
use crate::model::ids::AgentId;
use team_agent::lifecycle::{
    fork_agent_with_transport, quick_start_with_transport_in_workspace, restart_with_transport,
};
use team_agent::transport::test_support::OfflineTransport;

#[test]
#[serial(env)]
fn cursor_fresh_spawn_does_not_invent_session_id() {
    let (ws, _home, _guard, team) = seed_cursor("pending");
    let transport = OfflineTransport::new();
    quick_start_with_transport_in_workspace(&ws, &team, None, true, Some("cursortm"), &transport)
        .expect("cursor fresh start");
    let argv = first_spawn_argv(&transport);
    assert!(
        argv_flag(&argv, "--session-id").is_none(),
        "cursor must not invent --session-id; argv={argv:?}"
    );
    assert!(
        argv_flag(&argv, "--resume").is_none(),
        "fresh spawn must not --resume; argv={argv:?}"
    );
    assert!(
        !argv.iter().any(|a| a == "--continue"),
        "fresh spawn must not --continue; argv={argv:?}"
    );
    let state = load_runtime_state(&ws).expect("state");
    assert_eq!(
        state
            .pointer("/agents/cursor_writer/_pending_session_id")
            .and_then(|v| v.as_str()),
        None,
        "no fake pending; agent={:?}",
        state.pointer("/agents/cursor_writer")
    );
    assert_eq!(
        state
            .pointer("/agents/cursor_writer/session_id")
            .and_then(|v| v.as_str()),
        None,
        "session_id stays null until capture; agent={:?}",
        state.pointer("/agents/cursor_writer")
    );
}

#[test]
#[serial(env)]
fn cursor_restart_uses_resume_when_session_captured_and_archive_present() {
    let (ws, home, _guard, team) = seed_cursor("resume");
    let first = OfflineTransport::new();
    quick_start_with_transport_in_workspace(&ws, &team, None, true, Some("cursortm"), &first)
        .expect("initial cursor start");
    let sid = "502896a1-72ba-4c53-9a86-b2da28780806";
    let dir = {
        let path = home
            .join(".cursor")
            .join("chats")
            .join("00a437742b92089861da7821a62f232a")
            .join(sid);
        std::fs::create_dir_all(&path).unwrap();
        std::fs::write(path.join("store.db"), b"").unwrap();
        path
    };
    assert!(cursor_session_archive_present(&dir));
    assert_eq!(cursor_session_dir(sid).as_deref(), Some(dir.as_path()));

    let mut state = load_runtime_state(&ws).expect("load");
    patch_cursor_writer(&mut state, sid, &dir, &ws);
    save_runtime_state(&ws, &state).expect("save captured");

    seed_healthy_coordinator(&ws);

    let restart = OfflineTransport::new();
    restart_with_transport(&ws, false, Some("cursortm"), &restart)
        .expect("restart without --allow-fresh must resume cursor");
    let argv = first_spawn_argv(&restart);
    assert!(
        argv_contains_adjacent(&argv, &["--resume", sid]),
        "captured cursor must restart with --resume <chatId>; argv={argv:?}"
    );
    assert!(
        argv_flag(&argv, "--session-id").is_none(),
        "resume argv must not mint --session-id; argv={argv:?}"
    );
    assert!(
        !argv.iter().any(|a| a == "--continue"),
        "resume argv must not use --continue; argv={argv:?}"
    );
}

#[test]
#[serial(env)]
fn cursor_classify_resumes_when_archive_present_and_refuses_when_missing() {
    let base = tmp_dir("classify");
    let home = tmp_dir("classify-home");
    let ws = base.join("ws");
    std::fs::create_dir_all(&ws).unwrap();
    let _guard = HomeGuard::set(&home);
    let sid = "502896a1-72ba-4c53-9a86-b2da28780806";
    let dir = home.join(".cursor").join("chats").join(sid);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("meta.json"), b"{}").unwrap();

    let present = serde_json::json!({
        "agents": {
            "w1": {
                "provider": "cursor_agent",
                "status": "running",
                "session_id": sid,
                "spawn_cwd": ws.to_string_lossy(),
                "rollout_path": dir.to_string_lossy(),
            }
        }
    });
    let plan = classify_restart_plan_with_resume_validation(Some(&ws), &present, false)
        .expect("classify present");
    assert_eq!(plan.decisions.len(), 1);
    assert_eq!(
        plan.decisions[0].decision,
        ResumeDecision::Resume,
        "archive present must Resume; unresumable={:?}",
        plan.unresumable
    );

    let missing_sid = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
    let missing = serde_json::json!({
        "agents": {
            "w1": {
                "provider": "cursor_agent",
                "status": "running",
                "session_id": missing_sid,
                "spawn_cwd": ws.to_string_lossy(),
            }
        }
    });
    let refuse = classify_restart_plan_with_resume_validation(Some(&ws), &missing, false)
        .expect("classify missing");
    assert_eq!(refuse.decisions.len(), 1);
    assert_eq!(
        refuse.decisions[0].decision,
        ResumeDecision::Refuse,
        "missing cursor archive must Refuse, not silent fresh; got {:?}",
        refuse.decisions[0].decision
    );
}

#[test]
#[serial(env)]
fn cursor_fork_agent_refuses_as_unsupported_not_unverified() {
    let (ws, _home, _guard, team) = seed_cursor("fork");
    let transport = OfflineTransport::new();
    quick_start_with_transport_in_workspace(&ws, &team, None, true, Some("cursortm"), &transport)
        .expect("cursor start");
    let result = fork_agent_with_transport(
        &ws,
        &AgentId::new("cursor_writer"),
        &AgentId::new("cursor_fork"),
        None,
        false,
        Some("cursortm"),
        &transport,
    );
    let err = match result {
        Ok(report) => panic!("cursor fork must refuse; report={report:?}"),
        Err(error) => error.to_string(),
    };
    let lower = err.to_ascii_lowercase();
    assert!(
        lower.contains("does not support") || err.contains("不支持"),
        "fork error must be unsupported; err={err}"
    );
    assert!(
        !lower.contains("unverified") && !err.contains("未验证"),
        "must not call unverified slash fork; err={err}"
    );
}

fn patch_cursor_writer(state: &mut serde_json::Value, sid: &str, dir: &Path, ws: &Path) {
    let mut patched = 0u32;
    let mut patch = |agent: &mut serde_json::Value| {
        if let Some(obj) = agent.as_object_mut() {
            obj.insert("session_id".to_string(), serde_json::json!(sid));
            obj.insert(
                "rollout_path".to_string(),
                serde_json::json!(dir.to_string_lossy()),
            );
            obj.insert(
                "spawn_cwd".to_string(),
                serde_json::json!(ws.to_string_lossy()),
            );
            obj.insert("capture_state".to_string(), serde_json::json!("captured"));
            patched += 1;
        }
    };
    if let Some(agent) = state.pointer_mut("/agents/cursor_writer") {
        patch(agent);
    }
    if let Some(teams) = state.get_mut("teams").and_then(|v| v.as_object_mut()) {
        for team in teams.values_mut() {
            if let Some(agent) = team
                .get_mut("agents")
                .and_then(|v| v.get_mut("cursor_writer"))
            {
                patch(agent);
            }
        }
    }
    assert!(
        patched > 0,
        "cursor_writer missing in state; keys={:?}",
        state.as_object().map(|o| o.keys().collect::<Vec<_>>())
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

fn seed_cursor(tag: &str) -> (PathBuf, PathBuf, HomeGuard, PathBuf) {
    let ws = tmp_dir(tag);
    let home = tmp_dir(&format!("{tag}-home"));
    let guard = HomeGuard::set(&home);
    let team = write_cursor_role(&ws);
    (ws, home, guard, team)
}

fn first_spawn_argv(transport: &OfflineTransport) -> Vec<String> {
    let records = transport.spawn_records();
    assert!(
        !records.is_empty(),
        "expected a worker spawn; records={records:?}"
    );
    records[0].1.clone()
}

fn argv_flag(argv: &[String], flag: &str) -> Option<String> {
    argv.windows(2)
        .find(|pair| pair[0] == flag)
        .map(|pair| pair[1].clone())
}

fn argv_contains_adjacent(hay: &[String], needle: &[&str]) -> bool {
    hay.windows(needle.len())
        .any(|window| window.iter().map(String::as_str).eq(needle.iter().copied()))
}

fn write_cursor_role(ws: &Path) -> PathBuf {
    let team = ws.join("cursortm");
    std::fs::create_dir_all(team.join("agents")).unwrap();
    std::fs::write(
        team.join("TEAM.md"),
        "---\nname: cursortm\nobjective: cursor resume.\nprovider: cursor_agent\ndangerously_skip_permissions: true\n---\n\nTeam.\n",
    )
    .unwrap();
    std::fs::write(
        team.join("agents").join("cursor_writer.md"),
        "---\nname: cursor_writer\nrole: Cursor Writer\nprovider: cursor_agent\nmodel: sonnet-4-thinking\nauth_mode: subscription\ndangerously_skip_permissions: true\ntools:\n  - mcp_team\n---\n\nWorker.\n",
    )
    .unwrap();
    team
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
        "ta-rs-cursor-resume-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::canonicalize(dir).unwrap()
}
