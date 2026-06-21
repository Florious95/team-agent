//! E2E-REST-012 Restart mixed resume/fresh.
//!
//! Two workers: one has valid session_id + missing backing, the other has no
//! session_id at all. Both should be refused for distinct reasons WITHOUT
//! a teardown — the JSON must report both, not collapse them to one
//! generic refusal.
//!
//! Invariants:
//! - ok == false
//! - JSON mentions BOTH agent ids ("a" and "b") somewhere in the unresumable
//!   surface (unresumable array, or per-agent reasons).

use crate::framework::*;
use serde_json::json;

#[test]
fn rest_012_restart_mixed_unresumable_reports_each_agent() {
    let team_id = "rest012";
    let ws = TestWorkspace::new(team_id).with_fake_spec(&["a", "b"]);
    let qs = quick_start_fake(&ws, team_id);
    assert!(quick_start_launched(&qs), "quick-start: {}", qs.stdout);

    let ws_path = ws.path().to_str().unwrap();
    let _ = run_ta(
        &ws,
        &["shutdown", "--workspace", ws_path, "--keep-logs", "--json"],
    );

    // a: codex with session_id + missing backing
    // b: codex with NO session_id
    let mut state = ws.read_state();
    let agents = state
        .pointer_mut("/agents")
        .and_then(|v| v.as_object_mut())
        .expect("agents");

    let a = agents.get_mut("a").and_then(|v| v.as_object_mut()).unwrap();
    a.insert("provider".into(), json!("codex"));
    a.insert("session_id".into(), json!("sess-rest012-missing"));
    a.insert("rollout_path".into(), json!("/tmp/ta-e2e-rest012-nowhere/rollout.jsonl"));
    a.insert("first_send_at".into(), json!("2026-01-01T00:00:00Z"));

    let b = agents.get_mut("b").and_then(|v| v.as_object_mut()).unwrap();
    b.insert("provider".into(), json!("codex"));
    // No session_id, no rollout_path.
    b.insert("first_send_at".into(), json!("2026-01-01T00:00:00Z"));

    std::fs::write(
        ws.state_json_path(),
        serde_json::to_string_pretty(&state).unwrap(),
    )
    .expect("write state");

    let out = run_ta(&ws, &["restart", ws_path, "--json"]);
    let j = out.json();
    let ok = j.pointer("/ok").and_then(|v| v.as_bool()).unwrap_or(true);
    assert!(!ok, "restart must refuse mixed unresumable; got {j}");

    let dump = serde_json::to_string(&j).unwrap();
    assert!(
        dump.contains("\"a\""),
        "JSON should mention agent 'a' in unresumable surface; got {dump}"
    );
    assert!(
        dump.contains("\"b\""),
        "JSON should mention agent 'b' in unresumable surface; got {dump}"
    );
}
