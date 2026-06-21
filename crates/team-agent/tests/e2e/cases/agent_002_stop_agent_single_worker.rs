//! E2E-AGENT-002 Stop-agent stops a single worker without stopping siblings.

use crate::framework::*;

#[test]
fn agent_002_stop_agent_stops_only_target_worker() {
    let team_id = "agent002";
    let ws = TestWorkspace::new(team_id).with_fake_spec(&["a", "b"]);
    let qs = quick_start_fake(&ws, team_id);
    assert!(quick_start_launched(&qs), "quick-start: {}", qs.stdout);

    let out = run_ta(
        &ws,
        &[
            "stop-agent",
            "a",
            "--workspace",
            ws.path().to_str().unwrap(),
            "--json",
        ],
    );
    assert!(
        out.is_success(),
        "stop-agent exit {}; stdout={} stderr={}",
        out.exit_code,
        out.stdout,
        out.stderr
    );
    let j = out.json();
    assert_json_field_eq_bool(&j, "/ok", true);
    assert_json_field_eq_str(&j, "/agent_id", "a");
    assert_json_field_eq_bool(&j, "/stopped", true);

    let state = ws.read_state();
    assert_eq!(
        state_agent(&state, "a")
            .get("status")
            .and_then(|v| v.as_str()),
        Some("stopped")
    );
    assert_eq!(
        state_agent(&state, "b")
            .get("status")
            .and_then(|v| v.as_str()),
        Some("running")
    );

    let session = worker_session_name(team_id);
    assert!(
        tmux_window_exists_for_workspace(&ws, &session, "b"),
        "sibling worker window b should remain live"
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
