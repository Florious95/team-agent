//! E2E-STAT-003 Dirty state is surfaced through diagnose/status JSON.

use crate::framework::*;
use serde_json::json;

#[test]
fn stat_003_status_dirty_state_reporting() {
    let team_id = "stat003";
    let ws = TestWorkspace::new(team_id).with_fake_spec(&["a"]);
    let qs = quick_start_fake(&ws, team_id);
    assert!(quick_start_launched(&qs), "quick-start: {}", qs.stdout);

    ws.inject_state(
        "leader_receiver",
        json!({"mode": "direct_tmux", "status": "attached", "pane_id": ""}),
    );

    let out = run_ta(
        &ws,
        &[
            "diagnose",
            "--workspace",
            ws.path().to_str().unwrap(),
            "--json",
        ],
    );
    let j = out.json();
    assert_json_field_eq_bool(&j, "/ok", false);
    let issues = j
        .pointer("/issues")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    assert!(
        !issues.is_empty(),
        "dirty state should produce diagnose issues: {j}"
    );
    let dump = serde_json::to_string(&j).unwrap();
    assert!(
        dump.contains("leader_not_attached") || dump.contains("worker_window_missing"),
        "diagnose should name dirty-state reason; got {dump}"
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
