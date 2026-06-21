//! E2E-STAT-002 Status reports a stopped team as stopped/missing, not live.

use crate::framework::*;

#[test]
fn stat_002_status_stopped_team() {
    let team_id = "stat002";
    let ws = TestWorkspace::new(team_id).with_fake_spec(&["a"]);
    let qs = quick_start_fake(&ws, team_id);
    assert!(quick_start_launched(&qs), "quick-start: {}", qs.stdout);

    let shut = run_ta(
        &ws,
        &[
            "shutdown",
            "--workspace",
            ws.path().to_str().unwrap(),
            "--keep-logs",
            "--json",
        ],
    );
    assert!(shut.is_success(), "shutdown stderr={}", shut.stderr);

    let out = run_ta(
        &ws,
        &[
            "status",
            "--workspace",
            ws.path().to_str().unwrap(),
            "--json",
        ],
    );
    assert!(out.is_success(), "status stderr={}", out.stderr);
    let j = out.json();
    assert_json_field_eq_bool(&j, "/tmux_session_present", false);
    assert_json_field_eq_str(&j, "/coordinator/status", "missing");
    assert_json_field_eq_str(&j, "/agents/a/status", "stopped");
}
