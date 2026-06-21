//! E2E-REC-001 Repair-state updates task projection and team_state.md.

use crate::framework::*;

#[test]
fn rec_001_repair_state_basic() {
    let team_id = "rec001";
    let ws = TestWorkspace::new(team_id).with_fake_spec(&["a"]);
    let qs = quick_start_fake(&ws, team_id);
    assert!(quick_start_launched(&qs), "quick-start: {}", qs.stdout);

    let out = run_ta(
        &ws,
        &[
            "repair-state",
            "--workspace",
            ws.path().to_str().unwrap(),
            "--task",
            "task_initial",
            "--assignee",
            "a",
            "--status",
            "done",
            "--summary",
            "repaired by e2e",
            "--json",
        ],
    );
    assert!(out.is_success(), "repair-state stderr={}", out.stderr);
    let j = out.json();
    assert_json_field_eq_bool(&j, "/ok", true);
    assert_json_field_eq_str(&j, "/task_id", "task_initial");
    assert_json_field_eq_str(&j, "/before/status", "pending");
    assert_json_field_eq_str(&j, "/after/status", "done");
    assert_json_field_eq_str(&j, "/after/last_result_summary", "repaired by e2e");
    let state_file = j.pointer("/state_file").and_then(|v| v.as_str()).unwrap();
    assert_file_exists(std::path::Path::new(state_file));

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
