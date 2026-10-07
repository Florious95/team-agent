//! E2E-DIRTY-002 Shutdown runtime absence remains visible through doctor.

use crate::framework::*;

#[test]
fn dirty_002_shutdown_runtime_is_not_present() {
    let team_id = "dirty002";
    let ws = TestWorkspace::new(team_id).with_fake_spec(&["a"]);
    let qs = quick_start_fake(&ws, team_id);
    assert!(quick_start_workers_available(&qs), "quick-start: {}", qs.stdout);

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
            "doctor",
            "--workspace",
            ws.path().to_str().unwrap(),
            "--json",
        ],
    );
    let j = out.json();
    assert_eq!(
        j.pointer("/runtime/status").and_then(|v| v.as_str()),
        Some("not_present"),
        "a shut-down team must not be reported as a running runtime: {j}"
    );
}
