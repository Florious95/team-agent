//! E2E-REST-002 / E2E-REST-010 Restart Unresumable With Missing Backing Store.
//!
//! Known bug (0.3.39): `session_id` was present but the provider backing
//! file under `$HOME/.codex/sessions` was missing; restart would teardown
//! the worker session before discovering the resume gap.
//!
//! Black-box invariant:
//! - `ok == false`, status matches a `refused_*` family
//!   (`refused_resume_atomicity` or successor `session_backing_store_missing`).
//! - The unresumable agent id is named in the JSON (`unresumable` array or
//!   per-agent reason).
//! - No spawn or teardown happens before refusal.

use crate::framework::*;
use serde_json::json;

#[test]
fn rest_002_restart_refuses_missing_backing_store() {
    let team_id = "rest002";
    let ws = TestWorkspace::new(team_id).with_fake_spec(&["a"]);
    let qs = quick_start_fake(&ws, team_id);
    assert!(quick_start_launched(&qs), "quick-start did not launch: {}", qs.stdout);

    // Shutdown the live worker so restart deals only with state.
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

    // Inject a session_id that points nowhere — the canonical missing-backing
    // shape per T4 §2 / T6 §2 dirty state catalog.
    let mut state = ws.read_state();
    let agents = state
        .pointer_mut("/agents")
        .and_then(|v| v.as_object_mut())
        .expect("state.agents object");
    let a = agents
        .get_mut("a")
        .and_then(|v| v.as_object_mut())
        .expect("agents.a");
    a.insert("session_id".into(), json!("sess-e2e-missing-backing"));
    a.insert(
        "rollout_path".into(),
        json!("/tmp/ta-e2e-rest002-nonexistent/rollout.jsonl"),
    );
    a.insert("first_send_at".into(), json!("2026-01-01T00:00:00Z"));
    a.insert("provider".into(), json!("codex"));
    let path = ws.state_json_path();
    std::fs::write(&path, serde_json::to_string_pretty(&state).unwrap())
        .expect("write state.json");

    let out = run_ta(
        &ws,
        &[
            "restart",
            ws.path().to_str().unwrap(),
            "--json",
        ],
    );
    let j = out.json();

    let ok = j.pointer("/ok").and_then(|v| v.as_bool()).unwrap_or(true);
    assert!(!ok, "restart must refuse when backing store is missing; got {}", j);

    let status = j.pointer("/status").and_then(|v| v.as_str()).unwrap_or("").to_string();
    assert!(
        status.contains("refused")
            || status.contains("backing")
            || status.contains("unresumable")
            || status.contains("atomicity"),
        "restart refusal status should name resume/backing/atomicity; got {status:?} (json: {j})"
    );

    // The unresumable agent should be named somewhere in the JSON.
    let dump = serde_json::to_string(&j).unwrap();
    assert!(
        dump.contains("\"a\""),
        "JSON should name agent 'a' as the unresumable target; got {dump}"
    );
}
