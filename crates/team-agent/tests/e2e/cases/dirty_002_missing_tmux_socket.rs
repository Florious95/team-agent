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
    let status = run_ta(
        &ws,
        &["status", "--workspace", ws.path().to_str().unwrap(), "--json"],
    );
    assert!(status.is_success(), "status stderr={}", status.stderr);
    let status = status.json();
    let runtime_status = status
        .get("nodes")
        .and_then(serde_json::Value::as_array)
        .and_then(|nodes| {
            nodes
                .iter()
                .find(|node| node.get("name").and_then(|v| v.as_str()) == Some("a"))
        })
        .and_then(|node| node.get("runtime_status"))
        .and_then(|value| value.as_str());
    assert!(
        runtime_status.is_some() && runtime_status != Some("running"),
        "status must also retain the missing-runtime fact: {status}"
    );
}
