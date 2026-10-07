//! E2E-DIRTY-002 Missing tmux backing is reported by the surviving status readiness path.

use crate::framework::*;

#[test]
fn dirty_002_missing_tmux_socket_reports_not_ready() {
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

    // `wait-ready` retired (#289); the surviving readiness surface is `status`.
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
    let runtime_status = j
        .get("nodes")
        .and_then(serde_json::Value::as_array)
        .and_then(|nodes| {
            nodes
                .iter()
                .find(|node| node.get("name").and_then(|v| v.as_str()) == Some("a"))
        })
        .and_then(|node| node.get("runtime_status"))
        .and_then(|v| v.as_str());
    assert!(
        runtime_status.is_some() && runtime_status != Some("running"),
        "missing tmux backing should not be reported as a running worker: {j}"
    );
}
