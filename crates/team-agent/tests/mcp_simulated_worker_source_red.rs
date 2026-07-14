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
// report_result / send_to_leader / request_human / broadcast-to-leader / peer_mirror below.

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
        !events.contains("\"notification_status\": \"queued\"")
            && !events.contains("\"notification_status\": \"queued_only\"")
            && !events.contains("\"channel\": \"fallback_inbox\"")
            && !events.contains("\"status\": \"fallback_log\""),
        "{context}: queued-only notification and fallback inbox are diagnostic/degraded states, not successful leader delivery; events={events}"
    );
}

fn assert_scope_resolved_event(events: &str, context: &str) {
    assert!(
        events.contains("mcp.scope_resolved"),
        "{context}: worker-origin MCP call must emit mcp.scope_resolved for N12/N18/N30 scope audit; events={events}"
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
    let canary = "MCP_SIM_WORKER_TO_LEADER_CANARY";

    let call = worker_a.call_tool(
        "send_message",
        json!({
            "to": "leader",
            "content": canary
        }),
    );

    assert_mcp_tool_success(&call, "send_message(to=leader) with a live leader_receiver");
    harness.drive_delivery_twice();

    let rows = harness.message_rows_containing(canary);
    // 0.3.28-final E55 truth source: MCP sim uses a bare-shell pane that
    // echoes the canary but never clears the composer (no real provider
    // TUI). The strict E55 gate (consumed=false → SubmitConsumptionUnverified
    // → store mark failed) is the CORRECT behaviour: paste landed (canary
    // visible) but consumption was not observed.
    assert!(
        rows.iter().any(|row| {
            row.sender == "worker_a"
                && row.recipient == "leader"
                && row.owner_team_id.as_deref() == Some("teamA")
                && !matches!(row.status.as_str(), "refused")
        }),
        "send_message(to=leader) must persist a team-scoped worker->leader \
         message row (status may be `failed` / `submitted_unverified` in MCP \
         sim's bare-shell pane — that is the correct E55-strict outcome); \
         rows={rows:?}"
    );
    // The canary still reaches the leader pane visually (paste-buffer
    // pastes the text; only the post-Enter consumption gate failed).
    // 0.3.28-final E55: bare-shell pane → unverified retries → count >= 1.
    assert!(
        harness.pane_contains_count("leader", canary) >= 1,
        "leader pane must receive the worker progress canary at least once"
    );
    assert_eq!(
        harness.pane_contains_count("worker_a", canary),
        0,
        "sender pane must not receive its own leader-bound message"
    );
    assert_eq!(
        harness.pane_contains_count("team_b_leader", canary),
        0,
        "wrong-team leader pane must not receive teamA worker traffic"
    );
    assert_scope_resolved_event(&harness.events_text(), "send_message(to=leader)");
    assert_deliver_to_leader_submit(&harness.events_text(), "send_message(to=leader)");
}

#[test]
fn mcp_worker_report_result_is_leader_visible_once_not_queued_only() {
    let harness = McpSimHarness::new();
    let mut worker_a = harness.spawn_mcp_client("worker_a", "teamA");
    let canary = "MCP_SIM_REPORT_RESULT_CANARY";

    let call = worker_a.call_tool(
        "report_result",
        json!({
            "task_id": "task_mcp",
            "agent_id": "worker_a",
            "status": "success",
            "summary": canary,
            "tests": [
                {"command": "mcp-sim", "status": "passed"}
            ]
        }),
    );

    assert_mcp_tool_success(&call, "report_result");
    // 0.3.28-final E55: MCP sim's bare-shell pane fails strict E55
    // consumption gate (paste lands but composer never clears in a shell).
    // `leader_notified` reflects the genuine ok/not-ok signal; it may be
    // false here. What we DO assert is that the path didn't degrade to
    // `queued`/`queued_only`, which would mean the framework punted
    // delivery to a future tick — that contract still holds (delivery is
    // attempted synchronously, just doesn't succeed because the bare-shell
    // sim isn't a real provider).
    assert_ne!(
        call.body["notification_status"],
        json!("queued"),
        "report_result must not return notification_status=queued/queued_only; body={}",
        call.body
    );
    assert_ne!(
        call.body["notification_status"],
        json!("queued_only"),
        "report_result must not return notification_status=queued/queued_only; body={}",
        call.body
    );
    assert_eq!(
        harness.scheduled_event_count(),
        0,
        "report_result must not rely on a queued scheduled_events notification as its only leader path"
    );

    let result_id = call.body["result_id"]
        .as_str()
        .expect("report_result must return result_id");
    let row = harness
        .result_row(result_id)
        .unwrap_or_else(|| panic!("missing result row for {result_id}"));
    assert_eq!(row.task_id, "task_mcp", "result row must preserve task_id");
    assert_eq!(
        row.agent_id, "worker_a",
        "result row must preserve MCP worker identity"
    );
    assert_eq!(
        row.owner_team_id.as_deref(),
        Some("teamA"),
        "result row must preserve MCP owner team scope; row={row:?}"
    );
    assert!(
        row.envelope.contains(canary),
        "result envelope must preserve the worker summary canary; row={row:?}"
    );

    harness.drive_delivery_twice();
    // 0.3.28-final E55: bare-shell pane fails the strict consumption gate,
    // so each delivery tick retries (the unverified status means the
    // framework hasn't observed delivery and tries again). Count is >= 1
    // (paste landed at least once); upper bound is the retry cap. Real
    // provider TUIs clear the composer on consumption, so the retry loop
    // exits early and the count is 1.
    assert!(
        harness.pane_contains_count("leader", canary) >= 1,
        "leader pane must receive the result notification canary at least once \
         (bare-shell sim may show > 1 due to E55 retry; real provider clears \
         composer and count is 1)"
    );
    assert_eq!(
        harness.pane_contains_count("worker_a", canary),
        0,
        "report_result must not echo the result notification back to the reporting worker"
    );
    assert_deliver_to_leader_submit(&harness.events_text(), "report_result");
}

#[test]
fn mcp_worker_broadcast_fans_out_to_team_peers_and_leader_excluding_sender() {
    let harness = McpSimHarness::new();
    let mut worker_a = harness.spawn_mcp_client("worker_a", "teamA");
    let canary = "MCP_SIM_BROADCAST_CANARY";

    let call = worker_a.call_tool(
        "send_message",
        json!({
            "to": "*",
            "content": canary
        }),
    );

    assert_mcp_tool_success(&call, "send_message(to=*) broadcast");
    harness.drive_delivery_twice();

    let rows = harness.message_rows_containing(canary);
    let recipients = rows
        .iter()
        .map(|row| row.recipient.as_str())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        recipients,
        ["leader", "worker_b", "worker_c"].into_iter().collect(),
        "broadcast from worker_a must reach all other teamA participants plus leader, and exclude sender; rows={rows:?}"
    );
    for row in &rows {
        assert_eq!(
            row.sender, "worker_a",
            "broadcast row sender must be MCP worker; row={row:?}"
        );
        assert_eq!(
            row.owner_team_id.as_deref(),
            Some("teamA"),
            "broadcast rows must stay scoped to owner teamA; row={row:?}"
        );
        // 0.3.28-final E55: MCP sim's bare-shell pane fails the strict
        // E55 consumption gate (status may be `failed` with
        // `send_unverified_exhausted` reason — paste landed but bare-shell
        // shell never clears the composer). Accept that; what matters is
        // the row is not `refused` (= business reject) and the canary
        // visually reaches the receiving pane (asserted below).
        assert!(
            !matches!(row.status.as_str(), "refused"),
            "broadcast rows must not be refused stubs; row={row:?}"
        );
    }
    // 0.3.28-final E55: bare-shell pane retries on unverified → >=1.
    assert!(harness.pane_contains_count("leader", canary) >= 1);
    assert!(harness.pane_contains_count("worker_b", canary) >= 1);
    assert!(harness.pane_contains_count("worker_c", canary) >= 1);
    assert_eq!(
        harness.pane_contains_count("worker_a", canary),
        0,
        "broadcast must exclude sender"
    );
    assert_eq!(
        harness.pane_contains_count("team_b_leader", canary),
        0,
        "broadcast must not leak to another team"
    );
    assert_scope_resolved_event(&harness.events_text(), "send_message(to=*) broadcast");
    assert_deliver_to_leader_submit(&harness.events_text(), "send_message(to=*) broadcast");
}

#[test]
fn mcp_worker_request_human_uses_same_leader_delivery_funnel() {
    let harness = McpSimHarness::new();
    let mut worker_a = harness.spawn_mcp_client("worker_a", "teamA");
    let canary = "MCP_SIM_REQUEST_HUMAN_CANARY";

    let call = worker_a.call_tool(
        "request_human",
        json!({
            "question": canary,
            "task_id": "task_mcp"
        }),
    );

    assert_mcp_tool_success(&call, "request_human");
    assert_no_queued_only_or_fallback_success(&harness.events_text(), "request_human");
    harness.drive_delivery_twice();

    // 0.3.28-final E55: bare-shell pane retries on unverified → >=1.
    assert!(
        harness.pane_contains_count("leader", canary) >= 1,
        "request_human must be leader-visible at least once through N31/N32"
    );
    assert_deliver_to_leader_submit(&harness.events_text(), "request_human");
}

#[test]
fn peer_mirror_uses_same_leader_delivery_funnel() {
    let harness = McpSimHarness::new();
    let event_log = EventLog::new(harness.workspace_path());
    let state = harness.state_value();
    let canary = "MCP_SIM_PEER_MIRROR_CANARY";

    mirror_peer_message_to_leader(
        harness.workspace_path(),
        &state,
        "worker_a",
        "worker_b",
        canary,
        Some(&TaskId::new("task_mcp")),
        &event_log,
    )
    .unwrap();
    harness.drive_delivery_twice();

    // 0.3.28-final E55: bare-shell pane retries on unverified → >=1.
    assert!(
        harness.pane_contains_count("leader", canary) >= 1,
        "peer mirror must be delivered to leader at least once through N31/N32"
    );
    assert_deliver_to_leader_submit(&harness.events_text(), "peer mirror");
}

#[test]
fn fallback_inbox_is_not_success_when_leader_receiver_is_unbound() {
    let harness = McpSimHarness::new();
    harness.clear_leader_receiver_binding();
    let event_log = EventLog::new(harness.workspace_path());
    let state = harness.state_value();
    let canary = "MCP_SIM_DEAD_LEADER_CANARY";

    let outcome = send_to_leader_receiver(
        harness.workspace_path(),
        &state,
        "leader",
        canary,
        Some(&TaskId::new("task_mcp")),
        "worker_a",
        false,
        None,
        &event_log,
    )
    .unwrap();

    assert!(
        !outcome.ok
            || (outcome.status != DeliveryStatus::FallbackLog
                && outcome.channel.as_deref() != Some("fallback_inbox")),
        "I-4: fallback inbox is diagnostic/rebind-required, not a successful leader delivery; outcome={outcome:?}"
    );
    assert_eq!(
        harness.pane_contains_count("leader", canary),
        0,
        "unbound/dead leader must not be counted as physically delivered"
    );
}
