#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[path = "support/mcp_sim_harness.rs"]
#[allow(dead_code)]
mod mcp_sim_harness;

use mcp_sim_harness::McpSimHarness;
use serde_json::{json, Value};
use team_agent::state::persist::{load_runtime_state, save_runtime_state};
use team_agent::tmux_backend::TmuxBackend;
use team_agent::transport::{SessionName, Transport};

#[test]
fn mcp_stop_agent_stops_real_lifecycle_state_and_tmux_window() {
    let harness = McpSimHarness::new();
    let mut worker = harness.spawn_mcp_client("worker_b", "teamA");

    let call = worker.call_tool("stop_agent", json!({"agent_id": "worker_a"}));

    assert!(
        !call.is_error,
        "MCP stop_agent should return the real lifecycle success envelope, not an MCP protocol error; body={} raw={}",
        call.body,
        call.raw
    );
    assert_eq!(
        call.body["ok"],
        json!(true),
        "stop_agent should be ok only after lifecycle side effects; body={}",
        call.body
    );
    assert!(
        call.body.get("state_file").is_some() || call.body.get("stopped").is_some(),
        "stop_agent return must expose lifecycle evidence such as state_file/stopped, not a canned ok:true envelope; body={}",
        call.body
    );

    let state = harness.state_value();
    assert_eq!(
        selected_agent_status(&state, "teamA", "worker_a"),
        Some("stopped"),
        "MCP stop_agent must persist selected team agents.worker_a.status=stopped; state={state} body={}",
        call.body
    );
    assert!(
        !window_exists(harness.workspace_path(), &state, "worker_a"),
        "MCP stop_agent must close the target tmux window through the real lifecycle path; state={state} body={}",
        call.body
    );
}

#[test]
fn mcp_reset_agent_refuses_without_discard_and_discards_session_on_true_reset() {
    let harness = McpSimHarness::new();
    seed_worker_session(harness.workspace_path(), "teamA", "worker_a", "old-session");
    let mut worker = harness.spawn_mcp_client("worker_b", "teamA");

    let refused = worker.call_tool(
        "reset_agent",
        json!({"agent_id": "worker_a", "discard_session": false}),
    );
    assert!(
        refused.is_error,
        "discard_session=false returns body.ok=false, which the existing MCP wire contract maps to isError=true; body={} raw={}",
        refused.body,
        refused.raw
    );
    assert_eq!(
        refused.body["ok"],
        json!(false),
        "discard_session=false must not return ok:true; it should refuse reset because session discard was not explicit; body={}",
        refused.body
    );
    assert_eq!(
        refused.body["status"],
        json!("refused"),
        "discard_session=false refusal must be status=refused; body={}",
        refused.body
    );
    assert_eq!(
        refused.body["reason"],
        json!("discard_session_required"),
        "discard_session=false refusal must name reason=discard_session_required; body={}",
        refused.body
    );
    let refused_state = harness.state_value();
    assert_eq!(
        selected_agent_value(&refused_state, "teamA", "worker_a", "session_id"),
        Some(&json!("old-session")),
        "discard_session=false refusal must not clear the old session_id or perform a reset side effect; body={} state={refused_state}",
        refused.body
    );
    assert_eq!(
        selected_agent_status(&refused_state, "teamA", "worker_a"),
        Some("running"),
        "discard_session=false refusal must not respawn/rewrite worker status; body={} state={refused_state}",
        refused.body
    );

    let reset = worker.call_tool(
        "reset_agent",
        json!({"agent_id": "worker_a", "discard_session": true}),
    );
    let state = harness.state_value();
    let selected_session = selected_agent_value(&state, "teamA", "worker_a", "session_id");
    assert!(
        reset.is_error
            || reset.body.get("ok").and_then(Value::as_bool) == Some(false)
            || selected_session != Some(&json!("old-session")),
        "discard_session=true must either return a truthful lifecycle error/refusal or clear the old session before reporting ok:true; body={} state={state}",
        reset.body
    );
}

#[test]
fn mcp_fork_agent_missing_source_session_errors_without_state_or_spec_mutation() {
    let harness = McpSimHarness::new();
    let before = harness.state_value();
    assert!(
        source_session_missing(&before, "teamA", "worker_a"),
        "fixture sanity: source worker_a must have no session_id; state={before}"
    );
    let mut worker = harness.spawn_mcp_client("worker_b", "teamA");

    let call = worker.call_tool(
        "fork_agent",
        json!({"source_agent_id": "worker_a", "as_agent_id": "worker_fork", "label": "copy"}),
    );

    assert!(
        call.is_error || call.body.get("ok").and_then(Value::as_bool) == Some(false),
        "fork_agent from a source without session_id must return a real lifecycle/provider error, not a fake fork success; body={} raw={}",
        call.body,
        call.raw
    );
    let after = harness.state_value();
    assert!(
        selected_agent_value(&after, "teamA", "worker_fork", "status").is_none()
            && after.pointer("/agents/worker_fork").is_none(),
        "failed fork must not add target agent to state; before={before} after={after} body={}",
        call.body
    );
}

fn selected_agent_status<'a>(state: &'a Value, team: &str, agent: &str) -> Option<&'a str> {
    selected_agent_value(state, team, agent, "status").and_then(Value::as_str)
}

fn selected_agent_value<'a>(
    state: &'a Value,
    team: &str,
    agent: &str,
    key: &str,
) -> Option<&'a Value> {
    state
        .pointer(&format!("/teams/{team}/agents/{agent}/{key}"))
        .or_else(|| state.pointer(&format!("/agents/{agent}/{key}")))
}

fn source_session_missing(state: &Value, team: &str, agent: &str) -> bool {
    selected_agent_value(state, team, agent, "session_id").is_none_or(Value::is_null)
}

fn seed_worker_session(workspace: &std::path::Path, team: &str, agent: &str, session_id: &str) {
    let mut state = load_runtime_state(workspace).unwrap();
    if let Some(obj) = state
        .pointer_mut(&format!("/teams/{team}/agents/{agent}"))
        .and_then(Value::as_object_mut)
    {
        obj.insert("session_id".to_string(), json!(session_id));
        obj.insert(
            "rollout_path".to_string(),
            json!(workspace.join("rollout.jsonl").to_string_lossy()),
        );
    }
    if let Some(obj) = state
        .pointer_mut(&format!("/agents/{agent}"))
        .and_then(Value::as_object_mut)
    {
        obj.insert("session_id".to_string(), json!(session_id));
        obj.insert(
            "rollout_path".to_string(),
            json!(workspace.join("rollout.jsonl").to_string_lossy()),
        );
    }
    save_runtime_state(workspace, &state).unwrap();
}

fn window_exists(workspace: &std::path::Path, state: &Value, window: &str) -> bool {
    let Some(session) = state.get("session_name").and_then(Value::as_str) else {
        return false;
    };
    let backend = TmuxBackend::for_workspace(workspace);
    backend
        .list_windows(&SessionName::new(session))
        .unwrap_or_default()
        .iter()
        .any(|name| name.as_str() == window)
}
