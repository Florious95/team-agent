//! E2E-DIRTY-003 Orphan coordinator gate returns explicit machine JSON.

use crate::framework::*;

#[test]
fn dirty_003_orphan_coordinator_gate_has_explicit_shape() {
    let ws = TestWorkspace::new("dirty003");
    let out = run_ta(
        &ws,
        &[
            "doctor",
            "--workspace",
            ws.path().to_str().unwrap(),
            "--gate",
            "orphans",
            "--json",
        ],
    );
    assert!(
        out.is_success(),
        "doctor --gate orphans stderr={}",
        out.stderr
    );
    let j = out.json();
    assert_json_field_eq_str(&j, "/gate", "orphans");
    assert_json_field_present(&j, "/ok");
    assert_json_field_present(&j, "/orphans");
    assert_json_field_present(&j, "/action_required");
}
