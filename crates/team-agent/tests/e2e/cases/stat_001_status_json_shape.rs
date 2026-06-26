//! E2E-STAT-001 Status JSON exposes the expected machine-readable shape.

use crate::framework::*;

#[test]
fn stat_001_status_json_shape() {
    let team_id = "stat001";
    let ws = TestWorkspace::new(team_id).with_fake_spec(&["a", "b"]);
    let qs = quick_start_fake(&ws, team_id);
    assert!(quick_start_launched(&qs), "quick-start: {}", qs.stdout);

    // 0.4.x compact slim: default `status --json` exposes only 7 top-level
    // fields. Diagnostic blocks (coordinator, readiness, tmux_session_present)
    // moved behind `--detail`.
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
    // Slim payload assertions. `activity` is present only when the classifier
    // has emitted at least one tick — fake fixtures don't populate it.
    assert_json_field_present(&j, "/agents/a");
    assert_json_field_present(&j, "/agents/b");
    assert_json_field_present(&j, "/ready");
    assert!(
        j.pointer("/not_ready").is_some(),
        "status JSON must include `not_ready` (null when ready, object otherwise): {j}"
    );
    assert_json_field_eq_str(&j, "/session_name", &worker_session_name(team_id));
    // Diagnostic fields must NOT be in the slim default.
    assert!(
        j.pointer("/coordinator/status").is_none(),
        "0.4.x: `coordinator` moved to --detail; got {j}"
    );
    assert!(
        j.pointer("/readiness/ready").is_none(),
        "0.4.x: `readiness` moved to --detail; got {j}"
    );
    assert!(
        j.pointer("/tmux_session_present").is_none(),
        "0.4.x: `tmux_session_present` moved to --detail; got {j}"
    );

    // --detail re-exposes the full diagnostic payload.
    let detail = run_ta(
        &ws,
        &[
            "status",
            "--workspace",
            ws.path().to_str().unwrap(),
            "--json",
            "--detail",
        ],
    );
    assert!(detail.is_success(), "status --detail stderr={}", detail.stderr);
    let d = detail.json();
    assert_json_field_present(&d, "/coordinator/status");
    assert_json_field_present(&d, "/readiness/ready");
    assert!(
        d.pointer("/tmux_session_present")
            .and_then(|v| v.as_bool())
            .is_some(),
        "--detail must still include boolean tmux_session_present: {d}"
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
