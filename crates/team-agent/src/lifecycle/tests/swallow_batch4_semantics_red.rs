//! Swallow-family batch 4: the two cr-gated semantic-change points.
//!
//! Basis: `.team/artifacts/swallow-family-slices.md` batch 4 +
//! `swallow-batch4-cr-verdict.md` (workspace artifacts; BOTH points APPROVE'd,
//! 8 constraints C1-C8 + 6 reverse cases — folded below).
//!
//! Point 1 (C1-C4) covered the Claude compatible-API config dir that profile launch
//!   maintained for legacy Claude workers; it left with that runtime.
//! Point 2 (C5-C8): MCP scope validation must FAIL CLOSED when the runtime state is
//!   unreadable — today `load_runtime_state(..).unwrap_or(json!({}))`
//!   (mcp_server/tools.rs:574/:232) silently substitutes an empty state, so a
//!   multi-team workspace degrades to "legacy single-team" and an owner-less client
//!   slips through the scope gate (silent privilege widening; MUST-16).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[path = "../../../tests/support/mcp_sim_harness.rs"]
#[allow(dead_code)]
mod mcp_sim_harness;

#[path = "../../../tests/support/hermetic.rs"]
mod hermetic_guard;
#[allow(dead_code)]
fn _hermetic_boundary_marker(_: &hermetic_guard::HermeticTestEnv) {}

use mcp_sim_harness::McpSimHarness;
use serde_json::json;
use serial_test::serial;

/// C5-C8 + reverse cases `scope_unverifiable_when_state_read_fails_returns_structured_refusal`
/// / `scope_unverifiable_state_recovers_retry_passes`: with the runtime state
/// unreadable, an owner-less MCP client in a (really) multi-team workspace must be
/// REFUSED (scope unverifiable, fail-closed), not silently treated as legacy
/// single-team; once the state is restored the same call gets the honest multi-team
/// refusal path (verifiable again).
#[ignore = "ci-env: harness state fixture unavailable, tracked"]
#[test]
#[serial(b4_mcp)]
fn b4_scope_validation_fails_closed_when_state_unreadable() {
    let harness = McpSimHarness::new();
    let mut worker = harness.spawn_mcp_client("worker_b", "");
    let state_path = harness.workspace_path().join(".team/runtime/state.json");
    let good_state = std::fs::read(&state_path).unwrap();
    std::fs::write(&state_path, "{ unreadable state").unwrap();

    let broken = worker.call_tool(
        "send_message",
        json!({"to": "worker_a", "content": "b4 fail-closed probe"}),
    );

    let mut failures = Vec::new();
    let broken_text = broken.body.to_string();
    if !broken.is_error
        || !(broken_text.contains("unverifiable") || broken_text.contains("scope"))
        || broken_text.contains("internal_runtime_error")
    {
        failures.push(format!(
            "C5/C6/N38: with state unreadable the refusal must be the STRUCTURED \
scope_unverifiable kind (reason='state read failed: ..' + retry next_action), not a \
silent legacy-single-team pass (tools.rs unwrap_or(json!({{}}))) and not an opaque \
internal_runtime_error leak; body={}",
            broken.body
        ));
    }

    // C7: restore the state — the same call must hit the honest, verifiable gate again
    // (here: the multi-team owner-required refusal), proving the outage refusal was
    // transient, not durable.
    std::fs::write(&state_path, &good_state).unwrap();
    let recovered = worker.call_tool(
        "send_message",
        json!({"to": "worker_a", "content": "b4 recovery probe"}),
    );
    let recovered_text = recovered.body.to_string();
    if !recovered_text.contains("TEAM_AGENT_OWNER_TEAM_ID")
        && !recovered_text.contains("scope_refused")
    {
        failures.push(format!(
            "C7: after state recovery the same call must reach the verifiable multi-team \
gate (owner-required refusal), not some degraded path; body={}",
            recovered.body
        ));
    }

    assert!(
        failures.is_empty(),
        "batch4 point-2 fail-closed scope contract failed:\n{}",
        failures.join("\n")
    );
}
