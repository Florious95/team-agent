//! E2E-AGENT-001 Start-agent repairs a missing/stopped worker window.
//!
//! Black-box invariant:
//! - After `stop-agent a`, `start-agent a --allow-fresh --no-display` returns
//!   ok and rebinds agent `a` to a live pane/window in state.

use crate::framework::*;
use std::time::Duration;

#[test]
fn agent_001_start_agent_repairs_missing_window() {
    let team_id = "agent001";
    let ws = TestWorkspace::new(team_id).with_fake_spec(&["a"]);
    let qs = quick_start_fake(&ws, team_id);
    assert!(quick_start_launched(&qs), "quick-start: {}", qs.stdout);

    let session = worker_session_name(team_id);
    let stopped = run_ta(
        &ws,
        &[
            "stop-agent",
            "a",
            "--workspace",
            ws.path().to_str().unwrap(),
            "--json",
        ],
    );
    assert!(stopped.is_success(), "stop-agent stderr={}", stopped.stderr);
    assert_json_field_eq_bool(&stopped.json(), "/ok", true);

    let out = run_ta(
        &ws,
        &[
            "start-agent",
            "a",
            "--workspace",
            ws.path().to_str().unwrap(),
            "--allow-fresh",
            "--no-display",
            "--json",
        ],
    );
    assert!(
        out.is_success(),
        "start-agent exit {}; stdout={} stderr={}",
        out.exit_code,
        out.stdout,
        out.stderr
    );
    let j = out.json();
    assert_json_field_eq_bool(&j, "/ok", true);
    assert_json_field_eq_str(&j, "/agent_id", "a");

    wait_for_or_panic(
        "worker session present after start-agent",
        || tmux_session_exists_for_workspace(&ws, &session),
        Duration::from_secs(5),
    );
    let state = ws.read_state();
    let agent = state_agent(&state, "a");
    assert_eq!(
        agent.get("status").and_then(|v| v.as_str()),
        Some("running")
    );
    assert!(
        agent.get("pane_id").and_then(|v| v.as_str()).is_some(),
        "start-agent should write a pane_id; agent={agent}"
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
