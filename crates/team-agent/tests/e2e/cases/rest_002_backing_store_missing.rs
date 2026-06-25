//! E2E-REST-002 / E2E-REST-010 Restart Unresumable With Missing Backing Store.
//!
//! 0.4.7 partial-resume note: the original injection-based fixture wrote
//! agent fields to the top-level `/agents` view, but `project_top_level_view`
//! re-projects from the nested `teams[<key>].agents` source on every
//! `select_runtime_state`, so the test's `provider=codex, session_id=...,
//! rollout_path=/tmp/nonexistent` overrides were silently overwritten before
//! restart ever read them. The agent classifier saw the original fake-team
//! state instead (provider=fake, session_id=null, first_send_at=null).
//!
//! Pre-0.4.7 this masquerade still produced "Refuse" because no_session_id
//! always refused without --allow-fresh. Post-0.4.7 partial-resume the same
//! never-captured state correctly auto-freshes (no context to lose). The
//! genuine "session_id=Some + backing missing → still Refuse" path is
//! covered at the unit level by upgrade_compat_0211_red::
//! restart_refuses_interacted_claude_worker_without_session_id_partial_resume_preserves_guard
//! and selection.rs's existing first_send_at + null session_id branch.
//!
//! This e2e fixture now asserts the observable end-state of the original
//! flow: a never-captured fake team auto-freshes on `restart` without
//! --allow-fresh, no panic, JSON well-formed.

use crate::framework::*;
use serde_json::json;

#[test]
fn rest_002_restart_auto_fresh_never_captured_fake_team_partial_resume() {
    let team_id = "rest002";
    let ws = TestWorkspace::new(team_id).with_fake_spec(&["a"]);
    let qs = quick_start_fake(&ws, team_id);
    assert!(quick_start_launched(&qs), "quick-start did not launch: {}", qs.stdout);

    // Shutdown the live worker so restart deals only with state.
    let _ = run_ta(
        &ws,
        &[
            "shutdown",
            "--workspace",
            ws.path().to_str().unwrap(),
            "--keep-logs",
            "--json",
        ],
    );

    // Top-level injection (historical pattern); kept here so the file diff
    // documents that this is the same state shape that previously refused
    // pre-0.4.7. The runtime overwrites it via projection.
    let mut state = ws.read_state();
    if let Some(a) = state
        .pointer_mut("/agents")
        .and_then(|v| v.as_object_mut())
        .and_then(|agents| agents.get_mut("a"))
        .and_then(|v| v.as_object_mut())
    {
        a.insert("session_id".into(), json!("sess-e2e-missing-backing"));
        a.insert(
            "rollout_path".into(),
            json!("/tmp/ta-e2e-rest002-nonexistent/rollout.jsonl"),
        );
        let path = ws.state_json_path();
        std::fs::write(&path, serde_json::to_string_pretty(&state).unwrap())
            .expect("write state.json");
    }

    let out = run_ta(
        &ws,
        &[
            "restart",
            ws.path().to_str().unwrap(),
            "--json",
        ],
    );
    let j = out.json();
    assert!(
        !out.stderr.contains("panicked"),
        "restart stderr contains panic: {}",
        out.stderr
    );

    // 0.4.7 partial-resume: never-captured fake worker auto-freshes.
    let ok = j.pointer("/ok").and_then(|v| v.as_bool()).unwrap_or(false);
    assert!(ok, "0.4.7: never-captured fake worker must auto-fresh on restart; got {}", j);
    let status = j.pointer("/status").and_then(|v| v.as_str()).unwrap_or("");
    assert_eq!(status, "restarted",
        "0.4.7: never-captured worker → status=restarted (auto-fresh); got {status:?}; json={j}");
}
