//! ---
//! purpose: grok pending id 入 state，捕获后整队 restart 走 --resume
//! contract:
//!   provides:
//!     - name: grok-fresh-pending-matches-argv
//!       what: fresh spawn 的 `_pending_session_id` 等于 argv `--session-id`
//!     - name: grok-restart-resume-argv
//!       what: session_id + 磁盘存档齐时 restart 发 `--resume <uuid>`
//!     - name: grok-classify-resume-when-archive-present
//!       what: classify 在存档存在时决策 Resume，缺失时 Refuse 而非静默 fresh
//! boundary:
//!   - 不改 reset-agent --discard-session
//!   - 不覆盖 claude 三拍
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
use crate::provider::session_scan::grok::{grok_session_archive_present, grok_session_dir};
use crate::state::persist::{load_runtime_state, save_runtime_state};
use serial_test::serial;
use team_agent::lifecycle::{quick_start_with_transport_in_workspace, restart_with_transport};
use team_agent::transport::test_support::OfflineTransport;

#[test]
#[serial(env)]
fn grok_fresh_spawn_binds_pending_session_id_to_argv_session_id() {
    let (ws, _home, _guard, team) = seed_grok("pending");
    let transport = OfflineTransport::new();
    quick_start_with_transport_in_workspace(&ws, &team, None, true, Some("grokteam"), &transport)
        .expect("grok fresh start");
    let argv = first_spawn_argv(&transport);
    let sid = argv_flag(&argv, "--session-id").expect("--session-id on grok fresh argv");
    assert!(
        argv_flag(&argv, "--resume").is_none(),
        "fresh spawn must not --resume; argv={argv:?}"
    );
    let state = load_runtime_state(&ws).expect("state");
    let pending = state
        .pointer("/agents/grok_writer/_pending_session_id")
        .and_then(|v| v.as_str());
    assert_eq!(
        pending,
        Some(sid.as_str()),
        "pending must match argv session-id; agent={:?}",
        state.pointer("/agents/grok_writer")
    );
    assert_eq!(
        state
            .pointer("/agents/grok_writer/session_id")
            .and_then(|v| v.as_str()),
        None,
        "session_id stays null until capture; agent={:?}",
        state.pointer("/agents/grok_writer")
    );
}

#[test]
#[serial(env)]
fn grok_restart_uses_resume_when_session_captured_and_archive_present() {
    let (ws, home, _guard, team) = seed_grok("resume");
    let first = OfflineTransport::new();
    quick_start_with_transport_in_workspace(&ws, &team, None, true, Some("grokteam"), &first)
        .expect("initial grok start");
    let sid = argv_flag(&first_spawn_argv(&first), "--session-id").expect("fresh session-id");
    let dir = grok_session_dir(&ws, &sid).expect("grok session dir");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("events.jsonl"), b"").unwrap();
    assert!(grok_session_archive_present(&dir));
    let _ = home;

    let mut state = load_runtime_state(&ws).expect("load");
    {
        let agent = state
            .pointer_mut("/agents/grok_writer")
            .expect("grok_writer")
            .as_object_mut()
            .expect("agent object");
        agent.insert("session_id".to_string(), serde_json::json!(sid.clone()));
        agent.insert(
            "rollout_path".to_string(),
            serde_json::json!(dir.to_string_lossy()),
        );
        agent.insert(
            "spawn_cwd".to_string(),
            serde_json::json!(ws.to_string_lossy()),
        );
        agent.insert("capture_state".to_string(), serde_json::json!("captured"));
    }
    save_runtime_state(&ws, &state).expect("save captured");

    let restart = OfflineTransport::new();
    restart_with_transport(&ws, false, Some("grokteam"), &restart)
        .expect("restart without --allow-fresh must resume grok");
    let argv = first_spawn_argv(&restart);
    assert!(
        argv_contains_adjacent(&argv, &["--resume", sid.as_str()]),
        "captured grok must restart with --resume <uuid>; argv={argv:?}"
    );
    assert!(
        argv_flag(&argv, "--session-id").is_none(),
        "resume argv must not mint a new --session-id; argv={argv:?}"
    );
}

#[test]
#[serial(env)]
fn grok_classify_resumes_when_archive_present_and_refuses_when_missing() {
    let base = tmp_dir("classify");
    let home = tmp_dir("classify-home");
    let ws = base.join("ws");
    std::fs::create_dir_all(&ws).unwrap();
    let _guard = HomeGuard::set(&home);
    let sid = "cccccccc-cccc-4ccc-8ccc-cccccccccccc";
    let dir = grok_session_dir(&ws, sid).expect("dir");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("summary.json"), b"{}").unwrap();

    let present = serde_json::json!({
        "agents": {
            "w1": {
                "provider": "grok",
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

    let missing_sid = "dddddddd-dddd-4ddd-8ddd-dddddddddddd";
    let missing = serde_json::json!({
        "agents": {
            "w1": {
                "provider": "grok",
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
        "missing grok archive must Refuse, not silent fresh; got {:?}",
        refuse.decisions[0].decision
    );
}

fn seed_grok(tag: &str) -> (PathBuf, PathBuf, HomeGuard, PathBuf) {
    let ws = tmp_dir(tag);
    let home = tmp_dir(&format!("{tag}-home"));
    seed_grok_home(&home, Some(&ws));
    let guard = HomeGuard::set(&home);
    let team = write_grok_role(&ws);
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

fn write_grok_role(ws: &Path) -> PathBuf {
    let team = ws.join("grokteam");
    std::fs::create_dir_all(team.join("agents")).unwrap();
    std::fs::write(
        team.join("TEAM.md"),
        "---\nname: grokteam\nobjective: grok resume.\nprovider: grok\ndangerously_skip_permissions: false\n---\n\nTeam.\n",
    )
    .unwrap();
    std::fs::write(
        team.join("agents").join("grok_writer.md"),
        "---\nname: grok_writer\nrole: Grok Writer\nprovider: grok\nmodel: grok-4.6\nauth_mode: subscription\ndangerously_skip_permissions: false\ntools:\n  - mcp_team\n---\n\nWorker.\n",
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
        "ta-rs-grok-resume-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::canonicalize(dir).unwrap()
}
