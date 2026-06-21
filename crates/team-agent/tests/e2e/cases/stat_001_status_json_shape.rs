//! E2E-STAT-001 Status JSON exposes the expected machine-readable shape.

use crate::framework::*;

#[test]
fn stat_001_status_json_shape() {
    let team_id = "stat001";
    let ws = TestWorkspace::new(team_id).with_fake_spec(&["a", "b"]);
    let qs = quick_start_fake(&ws, team_id);
    assert!(quick_start_launched(&qs), "quick-start: {}", qs.stdout);

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
    assert_json_field_present(&j, "/agents/a");
    assert_json_field_present(&j, "/agents/b");
    assert_json_field_present(&j, "/coordinator/status");
    assert_json_field_present(&j, "/readiness/ready");
    assert_json_field_eq_str(&j, "/session_name", &worker_session_name(team_id));
    assert!(
        j.pointer("/tmux_session_present")
            .and_then(|v| v.as_bool())
            .is_some(),
        "status JSON should include boolean tmux_session_present: {j}"
    );

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
}
