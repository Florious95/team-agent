//! E2E-REC-003 Diagnose emits runtime/provider issue and repair payloads.

use crate::framework::*;

#[test]
fn rec_003_diagnose_output() {
    let team_id = "rec003";
    let ws = TestWorkspace::new(team_id).with_fake_spec(&["a"]);
    let qs = quick_start_fake(&ws, team_id);
    assert!(quick_start_launched(&qs), "quick-start: {}", qs.stdout);

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
    assert_json_field_present(&j, "/event_log");
    assert_json_field_eq_str(&j, "/runtime/workspace", ws.path().to_str().unwrap());
    assert_json_field_eq_str(&j, "/runtime/team_key", team_id);
    assert_json_field_eq_bool(&j, "/providers/fake/installed", true);
    assert_json_field_present(&j, "/issues");
    assert_json_field_present(&j, "/suggested_repairs");

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
