//! E2E-SHUT-004 Shutdown cleans the coordinator sidecar.
//!
//! Black-box invariants:
//! - After shutdown, the JSON `coordinator.status` is `missing` (or some
//!   "stopped"/"gone" label) — NOT `running`.
//! - `residuals.processes`, `residuals.sessions`, `residuals.owned_files`
//!   are empty arrays (no leaked sidecar).
//! - `spared_sessions` is populated only with sessions OUTSIDE this
//!   workspace; the worker session is in `killed_sessions`, not spared.

use crate::framework::*;
use serde_json::Value;

#[test]
fn shut_004_shutdown_cleans_coordinator_sidecar() {
    let team_id = "shut004";
    let ws = TestWorkspace::new(team_id).with_fake_spec(&["a"]);
    let qs = quick_start_fake(&ws, team_id);
    assert!(quick_start_launched(&qs), "quick-start: {}", qs.stdout);

    let ws_path = ws.path().to_str().unwrap();
    let out = run_ta(
        &ws,
        &["shutdown", "--workspace", ws_path, "--keep-logs", "--json"],
    );
    assert!(
        out.is_success(),
        "shutdown exit {}; stderr={}",
        out.exit_code,
        out.stderr
    );
    let j = out.json();

    assert_json_field_eq_bool(&j, "/ok", true);

    // Coordinator must not be "running" after shutdown.
    let coord_status = j
        .pointer("/coordinator/status")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    assert_ne!(
        coord_status, "running",
        "coordinator.status should not be 'running' after shutdown; full JSON: {j}"
    );

    // No residuals
    let empty = Value::Array(vec![]);
    let procs = j.pointer("/residuals/processes").unwrap_or(&empty);
    let sessions = j.pointer("/residuals/sessions").unwrap_or(&empty);
    let files = j.pointer("/residuals/owned_files").unwrap_or(&empty);
    assert_eq!(
        procs.as_array().map(|a| a.len()).unwrap_or(0),
        0,
        "residuals.processes should be empty; got {procs}"
    );
    assert_eq!(
        sessions.as_array().map(|a| a.len()).unwrap_or(0),
        0,
        "residuals.sessions should be empty; got {sessions}"
    );
    assert_eq!(
        files.as_array().map(|a| a.len()).unwrap_or(0),
        0,
        "residuals.owned_files should be empty; got {files}"
    );

    // Worker session must not be in spared_sessions.
    let spared = j
        .pointer("/spared_sessions")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let session = worker_session_name(team_id);
    assert!(
        !spared.iter().any(|v| v.as_str() == Some(session.as_str())),
        "worker session {session} should NOT be in spared_sessions; got {spared:?}"
    );
}
