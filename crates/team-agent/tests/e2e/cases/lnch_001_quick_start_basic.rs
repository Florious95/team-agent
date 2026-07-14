//! E2E-LNCH-001 / E2E-LAUNCH-001 Quick-Start Writes Canonical Runtime Spec And State.
//!
//! Architecture: T1 §1 storage layers, T5 §1 runtime tree, T7 §2 quick-start.
//!
//! Black-box invariants:
//! - `ok == true` in JSON
//! - `.team/runtime/<team>/team.spec.yaml` exists
//! - `.team/runtime/state.json` exists
//! - state.active_team_key == team_id
//! - state.session_name == "team-<team_id>"
//! - state.tmux_endpoint and state.tmux_socket populated
//! - state.agents.<id> exists for every spec agent.

use crate::framework::*;
use serde_json::Value;

#[test]
fn lnch_001_quick_start_basic() {
    let ws = TestWorkspace::new("lnch001").with_fake_spec(&["a"]);
    let team_id = "lnch001";

    let out = quick_start_fake(&ws, team_id);

    // Diagnostics if it fails — quick-start can degrade for non-bug reasons
    // (no tmux, sandbox), but in our test env it should succeed.
    assert!(
        quick_start_launched(&out),
        "quick-start did not launch the team. exit={} stdout={} stderr={}",
        out.exit_code,
        out.stdout,
        out.stderr
    );
    let j = out.json();
    // The session name and worker spawn must be set even in degraded mode.
    assert_json_field_present(&j, "/session_name");

    // Spec written
    let spec_path = ws
        .path()
        .join(".team/runtime")
        .join(team_id)
        .join("team.spec.yaml");
    assert_file_exists(&spec_path);

    // state.json written and shape correct
    assert_file_exists(&ws.state_json_path());
    let state = ws.read_state();
    assert_json_field_eq_str(&state, "/active_team_key", team_id);
    assert_json_field_eq_str(&state, "/session_name", &worker_session_name(team_id));
    assert_json_field_present(&state, "/tmux_endpoint");
    assert_json_field_present(&state, "/tmux_socket");
    assert_json_field_present(&state, "/agents/a");

    // Cleanup the worker session so we don't leak state into other tests.
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

    // Sanity: state must be valid JSON
    let _: Value = state;
}
