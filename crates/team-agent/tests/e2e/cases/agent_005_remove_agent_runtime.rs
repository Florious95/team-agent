//! E2E-AGENT-005 Remove-agent removes a runtime worker without resurrection.

use crate::framework::*;

#[test]
fn agent_005_remove_agent_runtime() {
    let team_id = "agent005";
    let ws = TestWorkspace::new(team_id).with_fake_spec(&["a", "b"]);
    let qs = quick_start_fake(&ws, team_id);
    assert!(quick_start_launched(&qs), "quick-start: {}", qs.stdout);

    let out = run_ta(
        &ws,
        &[
            "remove-agent",
            "b",
            "--workspace",
            ws.path().to_str().unwrap(),
            "--from-spec",
            "--confirm",
            "--force",
            "--json",
        ],
    );
    assert!(
        out.is_success(),
        "remove-agent exit {}; stdout={} stderr={}",
        out.exit_code,
        out.stdout,
        out.stderr
    );
    let j = out.json();
    assert_json_field_eq_bool(&j, "/ok", true);
    assert_json_field_eq_str(&j, "/agent_id", "b");
    assert_json_field_eq_str(&j, "/status", "removed");

    let state = ws.read_state();
    assert!(
        !state_has_agent(&state, "b"),
        "removed agent b should be absent from top-level state; state={state}"
    );
    let session = worker_session_name(team_id);
    assert!(
        !tmux_window_exists_for_workspace(&ws, &session, "b"),
        "removed worker window b should be absent"
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
