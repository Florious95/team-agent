//! A-7 retained send_message contract: a refused delivery without a persisted
//! message id must not fabricate a pollable-looking identifier.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[path = "support/mcp_sim_harness.rs"]
#[allow(dead_code)]
mod mcp_sim_harness;

use mcp_sim_harness::McpSimHarness;
use serde_json::{json, Value};
use serial_test::serial;

#[test]
#[serial(a7_mcp)]
fn a7_send_must_not_fabricate_message_id() {
    let harness = McpSimHarness::new();
    let _coordinator_guard = CoordinatorStopGuard {
        ws: harness.workspace_path().to_path_buf(),
    };
    let mut state = harness.state_value();
    for pointer in [
        "/agents/worker_a/status",
        "/teams/teamA/agents/worker_a/status",
    ] {
        if let Some(status) = state.pointer_mut(pointer) {
            *status = json!("session_drift");
        }
    }
    // Owner-less clients are legal only for the legacy single-team state shape.
    if let Some(obj) = state.as_object_mut() {
        obj.remove("teams");
        obj.remove("active_team_key");
    }
    team_agent::state::persist::save_runtime_state(harness.workspace_path(), &state).unwrap();
    let mut worker = harness.spawn_mcp_client("worker_b", "");

    let call = worker.call_tool(
        "send_message",
        json!({"to": "worker_a", "content": "A-7 fabrication probe"}),
    );

    let fabricated = extract_strings(&call.body)
        .into_iter()
        .find(|value| value.starts_with("mcp_"));
    assert!(
        fabricated.is_none(),
        "send must never invent an `mcp_<timestamp>` id not present in the store; fabricated={fabricated:?} body={} raw={}",
        call.body,
        call.raw
    );
}

/// The MCP fixture may start a workspace coordinator; stop it on drop.
struct CoordinatorStopGuard {
    ws: std::path::PathBuf,
}

impl Drop for CoordinatorStopGuard {
    fn drop(&mut self) {
        let _ = team_agent::coordinator::stop_coordinator(
            &team_agent::coordinator::WorkspacePath::new(self.ws.clone()),
        );
    }
}

fn extract_strings(value: &Value) -> Vec<String> {
    let mut out = Vec::new();
    match value {
        Value::String(s) => out.push(s.clone()),
        Value::Array(items) => {
            for item in items {
                out.extend(extract_strings(item));
            }
        }
        Value::Object(map) => {
            for item in map.values() {
                out.extend(extract_strings(item));
            }
        }
        _ => {}
    }
    out
}
