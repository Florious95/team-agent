//! E2E-LNCH-004 Quick-start ignores the legacy `display_backend: none` field
//! and materializes the default one-window-per-agent silent tmux topology.
//!
//! Invariants:
//! - no display backend field is emitted in the quick-start/state projections
//! - state.agents.<id>.window == "<id>" for every agent.

use crate::framework::*;

#[test]
fn lnch_004_display_backend_none_topology() {
    let team_id = "lnch004";
    let ws = TestWorkspace::new(team_id).with_fake_spec(&["a", "b"]);
    let ws_path = ws.path().to_str().unwrap();

    let out = quick_start_fake(&ws, team_id);
    assert!(quick_start_workers_available(&out), "quick-start: {}", out.stdout);

    let j = out.json();
    assert!(
        j.pointer("/display_backend").is_none(),
        "legacy display_backend must be omitted from quick-start output: {j}"
    );

    let state = ws.read_state();
    assert!(
        state.pointer("/display_backend").is_none(),
        "legacy display_backend must be omitted from state: {state}"
    );
    assert_json_field_eq_str(&state, "/agents/a/window", "a");
    assert_json_field_eq_str(&state, "/agents/b/window", "b");

    let _ = run_ta(
        &ws,
        &["shutdown", "--workspace", ws_path, "--keep-logs", "--json"],
    );
}
