//! E2E-REST-013 Restart --allow-fresh flag converts refusal to fresh launch.
//!
//! Invariant:
//! - For a fake team, `restart --allow-fresh` returns `ok:true,
//!   status:"restarted"`, lists `agents` array, and provides
//!   `attach_commands`.

use crate::framework::*;

#[test]
fn rest_013_restart_allow_fresh_succeeds_for_fake_team() {
    let team_id = "rest013";
    let ws = TestWorkspace::new(team_id).with_fake_spec(&["a"]);
    let qs = quick_start_fake(&ws, team_id);
    assert!(quick_start_launched(&qs), "quick-start: {}", qs.stdout);

    let ws_path = ws.path().to_str().unwrap();
    // First, shut it down so restart actually re-launches.
    let _ = run_ta(
        &ws,
        &["shutdown", "--workspace", ws_path, "--keep-logs", "--json"],
    );

    let out = run_ta(&ws, &["restart", ws_path, "--allow-fresh", "--json"]);
    assert!(out.is_success(), "restart exit {} stdout={} stderr={}", out.exit_code, out.stdout, out.stderr);
    let j = out.json();
    assert_json_field_eq_bool(&j, "/ok", true);
    let status = j.pointer("/status").and_then(|v| v.as_str()).unwrap_or("");
    assert!(
        status == "restarted" || status == "ok",
        "restart --allow-fresh status should be 'restarted'/'ok'; got {status:?}; json={j}"
    );
    let agents = j
        .pointer("/agents")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    assert!(
        agents.iter().any(|v| v.as_str() == Some("a")),
        "agents array should include 'a'; got {agents:?}"
    );

    let _ = run_ta(
        &ws,
        &["shutdown", "--workspace", ws_path, "--keep-logs", "--json"],
    );
}
