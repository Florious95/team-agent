//! E2E-LNCH-004 Quick-start with `display_backend: none` materializes a
//! one-window-per-agent topology and reports it in state.json.
//!
//! Invariants:
//! - state.display_backend == "none"
//! - state.agents.<id>.window == "<id>" for every agent.
//! - Quick-start JSON also reports display_backend == "none".

use crate::framework::*;

#[test]
fn lnch_004_display_backend_none_topology() {
    let team_id = "lnch004";
    let ws = TestWorkspace::new(team_id).with_fake_spec(&["a", "b"]);
    let ws_path = ws.path().to_str().unwrap();

    let out = quick_start_fake(&ws, team_id);
    assert!(quick_start_launched(&out), "quick-start: {}", out.stdout);

    let j = out.json();
    assert_json_field_eq_str(&j, "/display_backend", "none");

    let state = ws.read_state();
    assert_json_field_eq_str(&state, "/display_backend", "none");
    assert_json_field_eq_str(&state, "/agents/a/window", "a");
    assert_json_field_eq_str(&state, "/agents/b/window", "b");

    let _ = run_ta(
        &ws,
        &["shutdown", "--workspace", ws_path, "--keep-logs", "--json"],
    );
}
