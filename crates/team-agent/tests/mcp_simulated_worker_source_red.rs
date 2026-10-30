#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[path = "support/mcp_sim_harness.rs"]
mod mcp_sim_harness;

#[path = "support/hermetic.rs"]
mod hermetic_guard;
#[allow(dead_code)]
fn _hermetic_boundary_marker(_: &hermetic_guard::HermeticTestEnv) {}

use std::collections::BTreeSet;

use mcp_sim_harness::{McpSimHarness, McpToolCall};
use serde_json::json;
use team_agent::event_log::EventLog;
use team_agent::messaging::{
    mirror_peer_message_to_leader, send_to_leader_receiver, DeliveryStatus,
};
use team_agent::model::ids::TaskId;

// #230 -> #236 N35 obsolete note:
// OLD: idle_reminder_uses_same_leader_delivery_funnel counted idle reminder as one of the
// leader-delivery callers.
// NEW (leader裁决, #236 N35): idle/stuck/deadlock reminders are deleted nag paths; push_idle_reminder
// is intentionally no-op and must not be a leader delivery caller. Keep funnel coverage for
// report_result / send_to_leader / broadcast-to-leader / peer_mirror below.

fn leader_inbox_record(harness: &McpSimHarness, id: &str) -> serde_json::Value {
    let inbox = team_agent::inbox_store::read(harness.workspace_path(), "leader", 64, Some("teamA")).unwrap();
    inbox["messages"].as_array().unwrap().iter()
        .find(|record| record["message_id"] == id).cloned().expect("durable scoped inbox record")
}

fn assert_mcp_tool_success(call: &McpToolCall, context: &str) {
    assert!(
        !call.is_error,
        "{context}: MCP tools/call must not return isError=true; body={} raw={}",
        call.body, call.raw
    );
    assert!(
        call.body.get("ok").and_then(|v| v.as_bool()) != Some(false),
        "{context}: MCP tool body must not be an ok=false refusal; body={} raw={}",
        call.body,
        call.raw
    );
}

fn assert_deliver_to_leader_submit(events: &str, context: &str) {
    assert!(
        events.contains("deliver_to_leader.submit"),
        "{context}: N31/N32 funnel requires the shared deliver-to-leader primitive to emit deliver_to_leader.submit; events={events}"
    );
    assert_no_queued_only_or_fallback_success(events, context);
}

fn assert_no_queued_only_or_fallback_success(events: &str, context: &str) {
    assert!(
        !events.contains("\"notification_status\": \"queued_only\"")
            && !events.contains("\"channel\": \"fallback_inbox\"")
            && !events.contains("\"status\": \"fallback_log\""),
        "{context}: queued-only notification and fallback inbox are diagnostic/degraded states, not successful leader delivery; events={events}"
    );
}


#[test]
fn mcp_harness_uses_same_binary_env_jsonrpc_trace_and_no_cli_fallback() {
    let harness = McpSimHarness::new();
    let mut worker_a = harness.spawn_mcp_client("worker_a", "teamA");
    let call = worker_a.call_tool(
        "send_message",
        json!({
            "to": "leader",
            "content": "MCP_SIM_TRACE_GUARD_CANARY"
        }),
    );
    let spawn = worker_a.spawn_spec();

    assert!(
        spawn.program.ends_with("team-agent"),
        "I-1: MCP simulation must start the candidate team-agent binary; spawn={spawn:?}"
    );
    assert_eq!(
        spawn.args,
        vec![
            "mcp-server".to_string(),
            "--workspace".to_string(),
            harness.workspace_display()
        ],
        "I-1: MCP simulation entrypoint must be `team-agent mcp-server --workspace <workspace>`; spawn={spawn:?}"
    );
    assert_eq!(
        spawn.env["TEAM_AGENT_ID"], "worker_a",
        "I-7: sender identity is spawn-time env"
    );
    assert_eq!(
        spawn.env["TEAM_AGENT_OWNER_TEAM_ID"], "teamA",
        "I-7: owner team scope is spawn-time env"
    );
    assert_eq!(
        spawn.env["TEAM_AGENT_WORKSPACE"],
        harness.workspace_display(),
        "I-1/I-7: workspace env must match the --workspace argument"
    );
    assert!(
        spawn
            .args
            .iter()
            .all(|arg| !matches!(arg.as_str(), "fake-worker" | "codex" | "claude")),
        "I-5: MCP simulation must not start provider CLI processes; spawn={spawn:?}"
    );
    let mcp_entry_sources = [
        "src/mcp_server/wire.rs",
        "src/mcp_server/tools.rs",
        "src/mcp_server/types.rs",
    ]
    .into_iter()
    .map(|relative| {
        std::fs::read_to_string(format!("{}/{}", env!("CARGO_MANIFEST_DIR"), relative)).unwrap()
    })
    .collect::<Vec<_>>()
    .join("\n");
    assert!(
        !mcp_entry_sources.contains("Command::new(\"codex\")")
            && !mcp_entry_sources.contains("Command::new(\"claude\")")
            && !mcp_entry_sources.contains("Command::new(\"openai\")")
            && !mcp_entry_sources.contains("anthropic.")
            && !mcp_entry_sources.contains("openai."),
        "I-5/MUST-NOT-13: MCP simulated worker tests exercise only local stdio/tool plumbing; mcp-server must not spawn provider CLIs or call provider SDKs"
    );

    let trace = worker_a.trace_entries();
    assert!(
        worker_a.trace_path().exists() && !trace.is_empty(),
        "I-6: MCP simulation must persist an RPC trace evidence file; path={:?}",
        worker_a.trace_path()
    );
    assert!(
        trace.iter().all(|entry| entry["request"]["jsonrpc"] == json!("2.0")),
        "I-2/I-6: every request in the trace must be JSON-RPC 2.0, not legacy frames; trace={trace:?}"
    );
    assert!(
        trace.iter().any(|entry| {
            entry["request"]["method"] == json!("tools/call")
                && entry["request"]["params"]["name"] == json!("send_message")
        }),
        "I-2/I-6: worker-as-source coverage must include a JSON-RPC tools/call trace; trace={trace:?}"
    );

    let test_source = include_str!("mcp_simulated_worker_source_red.rs");
    let harness_source = include_str!("support/mcp_sim_harness.rs");
    assert!(
        !test_source.contains(concat!("insert", " into messages"))
            && !harness_source.contains(concat!("insert", " into messages"))
            && !test_source.contains(concat!("--", "from"))
            && !harness_source.contains(concat!("--", "from"))
            && !test_source.contains(concat!(".arg(", "\"send\"", ")"))
            && !harness_source.contains(concat!(".arg(", "\"send\"", ")")),
        "I-6: MCP simulation tests must not fake worker-as-source via direct DB INSERT or CLI sender fallback"
    );
    assert_mcp_tool_success(&call, "I-2/I-6 JSON-RPC worker send trace guard");
}

#[test]
fn mcp_stdio_case_matrix_does_not_replace_subscription_cases() {
    const MCP_STDIO_CASES: &[&str] = &[
        "CR-012", "CR-013", "CR-014", "CR-025", "CR-038", "CR-059", "CR-068",
    ];
    const SUBSCRIPTION_CASES: &[&str] = &[
        "CR-002", "CR-008", "CR-011", "CR-016", "CR-018", "CR-019", "CR-032", "CR-034", "CR-036",
        "CR-041", "CR-043", "CR-044", "CR-045", "CR-046", "CR-048", "CR-049", "CR-050", "CR-060",
    ];
    let mcp = MCP_STDIO_CASES.iter().copied().collect::<BTreeSet<_>>();
    let sub = SUBSCRIPTION_CASES.iter().copied().collect::<BTreeSet<_>>();
    let overlap = mcp.intersection(&sub).copied().collect::<Vec<_>>();

    assert!(
        overlap.is_empty(),
        "I-8: MCP-stdio FREE coverage must not replace subscription/provider-behavior cases; overlap={overlap:?}"
    );
    assert!(
        mcp.contains("CR-068"),
        "CR-068 is included as the ordinary worker->leader progress-message MCP case"
    );
}

#[test]
fn n31_n32_source_guard_has_no_report_result_queue_or_fallback_success_path() {
    let results = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/messaging/results.rs"
    ))
    .unwrap();
    let leader_receiver = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/messaging/leader_receiver.rs"
    ))
    .unwrap();

    assert!(
        !results.contains("queue_report_result_notification")
            && !results.contains("\"notification_status\": \"queued\"")
            && !results.contains("\"notification_status\": \"queued_only\""),
        "I-3/MUST-8: report_result must not schedule a parallel queued-only leader notification path; results.rs still contains queued path"
    );
    assert!(
        !leader_receiver.contains("DeliveryStatus::FallbackLog")
            && !leader_receiver.contains("fallback_inbox"),
        "I-4: fallback inbox may be diagnostic only and must not be returned as successful delivery"
    );
}

#[test]
fn mcp_worker_send_to_leader_uses_live_leader_receiver_not_refusal_or_fallback() {
    let harness = McpSimHarness::new();
    let mut worker_a = harness.spawn_mcp_client("worker_a", "teamA");
    let canary = "MCP_SIM_SEND_MESSAGE_CANARY";
    let call = worker_a.call_tool("send_message", json!({"to": "leader", "content": canary}));
    assert_mcp_tool_success(&call, "send_message(to=leader)");
    assert_eq!(call.body["delivery"], "physical", "{}", call.body);
    let record = leader_inbox_record(&harness, call.body["message_id"].as_str().unwrap());
    assert_eq!(record["sender"], "worker_a");
    assert_eq!(record["team"], "teamA");
    assert_eq!(record["content"], canary);
    assert!(harness.message_rows_containing(canary).is_empty(), "no coordinator queue");
    assert!(harness.pane_contains_count("leader", canary) >= 1);
    assert_eq!(harness.pane_contains_count("worker_a", canary), 0);
    assert_eq!(harness.pane_contains_count("team_b_leader", canary), 0);
}

#[test]
fn mcp_worker_report_result_is_leader_visible_once_not_queued_only() {
    let harness = McpSimHarness::new();
    let mut worker_a = harness.spawn_mcp_client("worker_a", "teamA");
    let canary = "MCP_SIM_REPORT_RESULT_CANARY";
    let call = worker_a.call_tool("report_result", json!({
        "task_id": "task_mcp", "agent_id": "worker_a", "status": "success", "summary": canary
    }));
    assert_mcp_tool_success(&call, "report_result");
    let record = leader_inbox_record(&harness, call.body["message_id"].as_str().unwrap());
    assert_eq!(record["sender"], "worker_a");
    assert_eq!(record["team"], "teamA");
    assert_eq!(record["kind"], "result");
    assert_eq!(record["envelope"]["task_id"], "task_mcp");
    assert_eq!(record["envelope"]["agent_id"], "worker_a");
    assert_eq!(record["envelope"]["summary"], canary);
    assert_eq!(harness.scheduled_event_count(), 0);
    assert!(harness.message_rows_containing(canary).is_empty());
    if call.body["status"] == "reported" {
        assert!(harness.pane_contains_count("leader", canary) >= 1);
    } else {
        assert_eq!(call.body["status"], "stored");
        assert!(call.body["delivery_error"].as_str().is_some());
        assert_eq!(harness.pane_contains_count("leader", canary), 0);
    }
    assert_eq!(harness.pane_contains_count("worker_a", canary), 0);
    assert_eq!(harness.pane_contains_count("team_b_leader", canary), 0);
}

#[test]
fn mcp_worker_broadcast_fans_out_to_team_peers_without_sender_or_leader_mirror() {
    let harness = McpSimHarness::new();
    let mut worker_a = harness.spawn_mcp_client("worker_a", "teamA");
    let canary = "MCP_SIM_BROADCAST_CANARY";
    let call = worker_a.call_tool("send_message", json!({"to": "*", "content": canary}));
    assert_mcp_tool_success(&call, "broadcast");
    let results = call.body["results"].as_array().expect("per-recipient physical outcomes");
    let recipients = results.iter().map(|row| row["recipient"].as_str().unwrap()).collect::<BTreeSet<_>>();
    assert_eq!(recipients, ["worker_b", "worker_c"].into_iter().collect());
    for row in results { assert_eq!(row["delivery"], "physical", "{row}"); }
    assert!(harness.message_rows_containing(canary).is_empty());
    let inbox = team_agent::inbox_store::read(harness.workspace_path(), "leader", 64, Some("teamA")).unwrap();
    let records = inbox["messages"].as_array().unwrap();
    assert!(records.is_empty(), "peer broadcast must not create a leader mirror");
    assert_eq!(harness.pane_contains_count("leader", canary), 0);
    for recipient in ["worker_b", "worker_c"] {
        assert!(harness.pane_contains_count(recipient, canary) >= 1);
    }
    assert_eq!(harness.pane_contains_count("worker_a", canary), 0);
    assert_eq!(harness.pane_contains_count("team_b_leader", canary), 0);
}


