//! E2E-REST-001 Restart Refuses Leader-Prefixed Worker Session Before Kill.
//!
//! Known bug (0.3.39): `state.session_name = team-agent-leader-*` made the
//! launcher's cleanup mis-kill the leader session.
//!
//! Black-box invariant:
//! - When state.session_name starts with `team-agent-leader-`, restart must
//!   refuse (ok == false) BEFORE issuing any tmux kill against that session.
//!   The JSON must name the dirty-session reason (status or error mentions
//!   leader/topology/refused/atomicity); it must NOT silently rewrite state
//!   to hide the dirty session.

use crate::framework::*;

#[test]
fn rest_001_refuses_leader_prefixed_worker_session() {
    let team_id = "rest001";
    let ws = TestWorkspace::new(team_id).with_fake_spec(&["a"]);

    // Bootstrap via quick-start so state.json has a realistic shape, then
    // poison session_name.
    let qs = quick_start_fake(&ws, team_id);
    assert!(quick_start_launched(&qs), "quick-start did not launch: {}", qs.stdout);

    // Shutdown to remove the legit worker session before we poison state.
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

    let leader_prefixed = "team-agent-leader-claude-x";

    // Poison session_name in both top-level state and the active team entry.
    ws.inject_state("session_name", serde_json::Value::String(leader_prefixed.to_string()));
    let teams = ws.read_state().get("teams").cloned().unwrap_or_default();
    if let Some(active) = ws.read_state().get("active_team_key").and_then(|v| v.as_str()).map(|s| s.to_string()) {
        if let Some(mut teams_obj) = teams.as_object().cloned() {
            if let Some(entry) = teams_obj.get_mut(&active).and_then(|v| v.as_object_mut()) {
                entry.insert("session_name".to_string(), serde_json::Value::String(leader_prefixed.to_string()));
            }
            ws.inject_state("teams", serde_json::Value::Object(teams_obj));
        }
    }

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
    assert!(!ok, "restart must refuse when session_name is leader-prefixed; got {}", j);

    // Status/error must surface a refusal-style label, not bare-green.
    let status = j.pointer("/status").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let error = j.pointer("/error").and_then(|v| v.as_str()).unwrap_or("").to_string();
    let combined = format!("{status} {error}").to_lowercase();
    let refusal_terms = ["refused", "leader", "topology", "atomicity", "worker_session", "blocked", "dirty"];
    assert!(
        refusal_terms.iter().any(|t| combined.contains(t)),
        "restart refusal JSON should mention dirty/refused/leader/topology; got status={status:?} error={error:?}"
    );
}
