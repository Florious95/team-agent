//! E2E-DIRTY-002 Missing tmux backing is reported by recovery/readiness paths.

use crate::framework::*;

#[test]
fn dirty_002_missing_tmux_socket_reports_not_ready() {
    let team_id = "dirty002";
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
            "wait-ready",
            "--workspace",
            ws.path().to_str().unwrap(),
            "--timeout",
            "0.1",
            "--json",
        ],
    );
    let j = out.json();
    assert_json_field_eq_bool(&j, "/ok", false);
    assert!(
        j.pointer("/readiness/process_started")
            .and_then(|v| v.as_bool())
            == Some(false)
            || j.pointer("/readiness/ready").and_then(|v| v.as_bool()) == Some(false),
        "missing tmux backing should not be ready: {j}"
    );
}
