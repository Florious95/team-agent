//! E2E-AGENT-003 Reset-agent with --discard-session reports discarded session.

use crate::framework::*;
use serde_json::json;

#[test]
fn agent_003_reset_agent_discard_session_reports_reset() {
    let team_id = "agent003";
    let ws = TestWorkspace::new(team_id).with_fake_spec(&["a"]);
    let qs = quick_start_fake(&ws, team_id);
    assert!(quick_start_launched(&qs), "quick-start: {}", qs.stdout);

    ws.mutate_agent_everywhere("a", |agent| {
        agent.insert("session_id".to_string(), json!("sess-agent-003"));
        agent.insert("rollout_path".to_string(), json!("/missing/agent003.jsonl"));
        agent.insert("captured_at".to_string(), json!("2026-01-01T00:00:00Z"));
        agent.insert("captured_via".to_string(), json!("fixture"));
        agent.insert("attribution_confidence".to_string(), json!("high"));
    });

    let out = run_ta(
        &ws,
        &[
            "reset-agent",
            "a",
            "--workspace",
            ws.path().to_str().unwrap(),
            "--discard-session",
            "--no-display",
            "--json",
        ],
    );
    assert!(
        out.is_success(),
        "reset-agent exit {}; stdout={} stderr={}",
        out.exit_code,
        out.stdout,
        out.stderr
    );
    let j = out.json();
    assert_json_field_eq_bool(&j, "/ok", true);
    assert_json_field_eq_str(&j, "/agent_id", "a");
    assert_json_field_eq_str(&j, "/status", "reset");
    assert_json_field_eq_str(&j, "/discarded_session_id", "sess-agent-003");
    assert!(
        j.pointer("/session_id").is_some(),
        "reset JSON should include session_id: {j}"
    );

    let state = ws.read_state();
    assert_eq!(
        state_agent(&state, "a")
            .get("status")
            .and_then(|v| v.as_str()),
        Some("running")
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
