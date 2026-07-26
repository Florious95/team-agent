//! E2E-REC-002 Doctor emits provider/coordinator/tmux health checks.

use crate::framework::*;

#[test]
fn rec_002_doctor_checks() {
    let team_id = "rec002";
    let ws = TestWorkspace::new(team_id).with_fake_spec(&["a"]);
    let qs = quick_start_fake(&ws, team_id);
    assert!(quick_start_launched(&qs), "quick-start: {}", qs.stdout);

    let out = run_ta(
        &ws,
        &[
            "doctor",
            "--workspace",
            ws.path().to_str().unwrap(),
            "--json",
        ],
    );
    assert!(out.is_success(), "doctor stderr={}", out.stderr);
    let j = out.json();
    assert_json_field_eq_bool(&j, "/ok", true);
    assert_json_field_eq_str(&j, "/coordinator/status", "running");
    assert_json_field_eq_bool(&j, "/coordinator/schema_ok", true);
    assert_json_field_eq_bool(&j, "/tmux/installed", true);
    assert_json_field_eq_bool(&j, "/profile_smoke/ok", true);

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
