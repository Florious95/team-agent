//! #235 reclaim-neutral delivery contracts.
//!
//! User-visible contract: delivery/report/retry paths never claim or take over a leader
//! receiver. Ownership changes only through explicit `claim-leader` / `takeover`, and
//! those explicit commands are unconditional for any live caller pane.
//!
//! Mac mini / subscription follow-up, not enforced here:
//! - subleader resume -> explicit `claim-leader --team child` -> child dispatch succeeds;
//! - leader dead -> real teammate worker-to-worker messages do not self-promote.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[path = "support/mcp_sim_harness.rs"]
mod mcp_sim_harness;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use mcp_sim_harness::McpSimHarness;
use rusqlite::{params, OptionalExtension};
use serde_json::{json, Value};
use serial_test::{file_serial, serial};
use team_agent::event_log::EventLog;
use team_agent::message_store::MessageStore;
use team_agent::messaging::results::report_result;
use team_agent::messaging::{
    deliver_pending_messages, send_message, DeliveryRefusal, DeliveryStatus, MessageTarget,
    SendOptions,
};
use team_agent::model::ids::TeamKey;
use team_agent::tmux_backend::TmuxBackend;
use team_agent::transport::{SessionName, Transport, WindowName};

#[test]
fn send_path_reclaim_neutral_static_guard_no_send_auto_reclaim_callsite() {
    let send = source("src/messaging/send.rs");
    assert!(
        !send.contains("auto_reclaim_from_caller_if_stale"),
        "I-RN/send neutral: messaging/send.rs must not call auto_reclaim_from_caller_if_stale; delivery is not an ownership side-effect"
    );
}

#[test]
#[ignore = "real-machine: live tmux/MCP-sim reclaim-neutral gate"]
#[serial(env)]
#[file_serial(tmux)]
fn send_path_reclaim_neutral_cli_broadcast_fanout_and_mcp_do_not_claim() {
    let mut failures = Vec::new();
    for (label, target) in [
        ("CLI-style single send", MessageTarget::Single("worker_a".to_string())),
        ("broadcast send", MessageTarget::Broadcast),
        (
            "fanout send",
            MessageTarget::Fanout(vec!["worker_a".to_string(), "worker_b".to_string()]),
        ),
    ] {
        if let Err(error) = assert_send_variant_is_reclaim_neutral(label, target) {
            failures.push(error);
        }
    }

    let harness = McpSimHarness::new();
    harness.clear_leader_receiver_binding();
    let before = harness.state_value();
    let mut worker = harness.spawn_mcp_client("worker_a", "teamA");
    let call = worker.call_tool(
        "send_message",
        json!({"to": "leader", "content": "MCP_RECLAIM_NEUTRAL_TO_LEADER"}),
    );
    let after = harness.state_value();
    if binding_snapshot(&before) != binding_snapshot(&after) {
        failures.push(format!(
            "MCP send(to=leader) must not claim or rewrite leader binding; call_body={} before={} after={}",
            call.body, before, after
        ));
    }

    assert!(
        failures.is_empty(),
        "send_path_reclaim_neutral violations:\n{}",
        failures.join("\n\n")
    );
}

#[test]
#[ignore = "real-machine: live tmux/MCP-sim reclaim-neutral gate"]
#[serial(env)]
#[file_serial(tmux)]
fn report_result_reclaim_neutral_preserves_result_and_message_but_not_owner() {
    let case = TmuxCase::new("report-neutral");
    seed_state(&case.workspace, Some("%dead-leader"), 9, &[("worker_a", "codex")]);
    let before = runtime_state(&case.workspace);

    let out = report_result(&case.workspace, &result_envelope("res_report_neutral"))
        .expect("report_result should persist the result even when leader is dead");
    let after = runtime_state(&case.workspace);
    let events = events(&case.workspace);

    assert_eq!(
        binding_snapshot(&before),
        binding_snapshot(&after),
        "report_result is worker-origin delivery; it must not claim or rewrite owner/receiver. out={out} events={events}"
    );
    assert!(
        result_exists(&case.workspace, "res_report_neutral"),
        "report_result must preserve the result row while waiting for explicit claim"
    );
    assert!(
        out.get("notification_status").and_then(Value::as_str) == Some("rebind_required")
            || events.contains("rebind_required")
            || events.contains("leader_not_attached"),
        "leader-dead report_result must surface rebind/claim-required, not pretend leader delivery succeeded; out={out} events={events}"
    );
    assert!(
        out.to_string().contains("claim-leader") || events.contains("claim-leader"),
        "leader-dead report_result must give the user an explicit claim-leader recovery hint; out={out} events={events}"
    );
}

#[test]
#[ignore = "real-machine: live tmux/MCP-sim reclaim-neutral gate"]
#[serial(env)]
#[file_serial(tmux)]
fn transport_retry_reclaim_neutral_injection_and_pending_paths_do_not_write_owner() {
    let case = TmuxCase::new("transport-neutral");
    let leader = case.spawn_leader_shaped_pane("leader");
    let worker = case.spawn_cat_pane("worker-a-pane");
    let _env = EnvGuard::set(&[("TMUX_PANE", leader.as_str())]);
    seed_state(&case.workspace, Some(leader.as_str()), 4, &[("worker_a", "codex")]);
    set_agent_pane(&case.workspace, "worker_a", &worker);
    let before = runtime_state(&case.workspace);

    let out = send_message(
        &case.workspace,
        &MessageTarget::Single("worker_a".to_string()),
        "TRANSPORT_RECLAIM_NEUTRAL_CANARY",
        &send_opts("leader", Some("team-a")),
    )
    .expect("fixture send should queue a worker message");
    assert_eq!(out.status, DeliveryStatus::Queued);

    let transport = TmuxBackend::for_workspace(&case.workspace);
    let delivered = deliver_pending_messages(
        &case.workspace,
        &runtime_state(&case.workspace),
        &transport,
        &EventLog::new(&case.workspace),
    )
    .expect("offline transport delivery should run locally");
    let after = runtime_state(&case.workspace);

    assert_eq!(
        binding_snapshot(&before),
        binding_snapshot(&after),
        "transport delivery/retry/fallback layer must only inject or update message state; it must not write owner/receiver"
    );
    assert_eq!(delivered.len(), 1, "fixture should physically inject one pending message");
    assert!(
        transport
            .capture(
                &team_agent::transport::Target::Pane(team_agent::transport::PaneId::new(&worker)),
                team_agent::transport::CaptureRange::Full,
            )
            .unwrap()
            .text
            .contains("TRANSPORT_RECLAIM_NEUTRAL_CANARY"),
        "transport path should prove delivery by physical inject into the worker pane, not by ownership mutation"
    );
}

#[test]
#[ignore = "real-machine: live tmux/MCP-sim reclaim-neutral gate"]
#[serial(env)]
#[file_serial(tmux)]
fn explicit_claim_unconditional_live_caller_claims_vacant_or_dead_and_requeues_same_message_id() {
    let case = TmuxCase::new("explicit-claim");
    seed_state(&case.workspace, None, 0, &[("worker_a", "codex")]);
    let blocked = report_result(&case.workspace, &result_envelope("res_claim_requeue"))
        .expect("blocked result fixture should persist");
    let blocked_message_id = blocked
        .get("notification_message_id")
        .and_then(Value::as_str)
        .expect("blocked report_result must expose a message id")
        .to_string();
    assert_eq!(
        message_status(&case.workspace, &blocked_message_id).as_deref(),
        Some("failed"),
        "fixture sanity: result notification starts blocked"
    );

    let caller = case.spawn_plain_pane("ordinary-shell-caller");
    let _env = EnvGuard::set(&[("TMUX_PANE", caller.as_str())]);
    let result = team_agent::leader::claim_leader(&case.workspace, Some("team-a"), false)
        .expect("claim-leader command should return a typed result");
    let state = runtime_state(&case.workspace);

    assert!(
        result.ok,
        "explicit claim-leader is unconditional for any live caller pane; no TEAM_AGENT_ID/owner/created_by/parent-chain or leader-shaped command gate may refuse it. result={result:?} state={state}"
    );
    assert_eq!(
        binding_snapshot(&state).pane.as_deref(),
        Some(caller.as_str()),
        "explicit claim must bind the caller pane as the only leader receiver"
    );
    assert_eq!(
        binding_snapshot(&state).epoch,
        Some(1),
        "explicit claim from vacant/dead state must advance owner_epoch monotonically"
    );
    assert!(
        matches!(
            message_status(&case.workspace, &blocked_message_id).as_deref(),
            Some("accepted" | "pending" | "target_resolved" | "injected" | "visible" | "submitted" | "submitted_unverified" | "delivered")
        ),
        "explicit claim must reuse requeue_after_claim_leader and requeue the SAME blocked message_id; status={:?}",
        message_status(&case.workspace, &blocked_message_id)
    );
    assert_eq!(
        result_notification_message_count(&case.workspace, "res_claim_requeue"),
        1,
        "explicit claim replay must not create a second message for the same result_id"
    );
    assert_eq!(
        leader_notification_log_count(&case.workspace, "res_claim_requeue"),
        1,
        "explicit claim replay must preserve leader_notification_log exactly-once for the same result_id"
    );
}

#[test]
#[ignore = "real-machine: live tmux/MCP-sim reclaim-neutral gate"]
#[serial(env)]
#[file_serial(tmux)]
fn explicit_takeover_live_owner_unconditional_replaces_live_owner_without_split_brain() {
    let case = TmuxCase::new("explicit-takeover");
    let old = case.spawn_leader_shaped_pane("old-live-leader");
    seed_state(&case.workspace, Some(old.as_str()), 6, &[("worker_a", "codex")]);
    let caller = case.spawn_plain_pane("ordinary-takeover-caller");
    let _env = EnvGuard::set(&[("TMUX_PANE", caller.as_str())]);

    let out = team_agent::cli::leader_port::takeover(&case.workspace, Some("team-a"), true)
        .expect("takeover command should return JSON");
    let state = runtime_state(&case.workspace);
    let binding = binding_snapshot(&state);

    assert!(
        out.get("ok").and_then(Value::as_bool) == Some(true),
        "explicit takeover --team T is unconditional even when the old owner is live; out={out} state={state}"
    );
    assert_eq!(
        binding.pane.as_deref(),
        Some(caller.as_str()),
        "takeover must leave exactly one active receiver: the caller pane"
    );
    assert_eq!(
        binding.epoch,
        Some(7),
        "takeover must advance owner_epoch monotonically from the old live owner"
    );
    assert_ne!(
        binding.pane.as_deref(),
        Some(old.as_str()),
        "old live owner must be unbound after explicit takeover"
    );
}

#[test]
#[ignore = "real-machine: live tmux/MCP-sim reclaim-neutral gate"]
#[serial(env)]
#[file_serial(tmux)]
fn dead_leader_send_does_not_autofix_and_tells_user_to_claim_or_takeover() {
    let case = TmuxCase::new("dead-send-no-autofix");
    let caller = case.spawn_plain_pane("ordinary-sender");
    let _env = EnvGuard::set(&[("TMUX_PANE", caller.as_str())]);
    seed_state(&case.workspace, Some("%dead-leader"), 12, &[("worker_a", "codex")]);
    let before = runtime_state(&case.workspace);

    let out = send_message(
        &case.workspace,
        &MessageTarget::Single("worker_a".to_string()),
        "DEAD_LEADER_SEND_MUST_NOT_AUTOFIX",
        &send_opts("leader", Some("team-a")),
    )
    .expect("ordinary send should return a recoverable outcome, not crash");
    let after = runtime_state(&case.workspace);
    let events = events(&case.workspace);

    assert_eq!(
        binding_snapshot(&before),
        binding_snapshot(&after),
        "ordinary send after dead leader must not auto-bind any pane; out={out:?} events={events}"
    );
    assert!(
        !events.contains("auto_reclaim_applied"),
        "dead leader send is reclaim-neutral; auto_reclaim_applied must not be emitted. events={events}"
    );
    assert!(
        format!("{out:?}").contains("claim-leader")
            || format!("{out:?}").contains("takeover")
            || events.contains("claim-leader")
            || events.contains("takeover"),
        "dead leader send must tell the user to run explicit claim/takeover; out={out:?} events={events}"
    );
}

#[test]
#[ignore = "real-machine: live tmux/MCP-sim reclaim-neutral gate"]
#[serial(env)]
#[file_serial(tmux)]
fn send_owner_gate_retreat_cases_do_not_auto_bind_or_open_cross_team_access() {
    let mut failures = Vec::new();
    if let Err(error) = assert_caller_pane_missing_is_hard_refused() {
        failures.push(error);
    }
    if let Err(error) = assert_same_team_live_owner_mismatch_requires_takeover() {
        failures.push(error);
    }
    if let Err(error) = assert_l1_send_to_child_live_owner_is_refused() {
        failures.push(error);
    }
    if let Err(error) = assert_l1_send_to_child_dead_owner_retreats_without_binding() {
        failures.push(error);
    }
    if let Err(error) = assert_worker_send_to_parent_dead_leader_retreats_without_binding_worker() {
        failures.push(error);
    }
    if let Err(error) = assert_send_after_explicit_child_claim_succeeds() {
        failures.push(error);
    }

    assert!(
        failures.is_empty(),
        "send owner-gate retreat contract failed:\n{}",
        failures.join("\n\n")
    );
}

fn assert_caller_pane_missing_is_hard_refused() -> Result<(), String> {
    let case = TmuxCase::new("owner-gate-no-caller");
    let owner = case.spawn_plain_pane("parent-owner");
    let _env = EnvGuard::set(&[]);
    seed_parent_child_state(&case.workspace, &owner, "%child-owner", "parent");
    let before = team_binding_snapshot(&case.workspace, "parent");

    let out = send_message(
        &case.workspace,
        &MessageTarget::Single("subleader_w".to_string()),
        "NO_CALLER_PANE_MUST_HARD_REFUSE",
        &send_opts("leader", Some("parent")),
    )
    .map_err(|err| format!("caller_pane_missing: send errored: {err}"))?;
    let after = team_binding_snapshot(&case.workspace, "parent");

    if before != after {
        return Err(format!(
            "caller_pane_missing must not mutate parent owner/receiver; out={out:?} before={before:?} after={after:?}"
        ));
    }
    let out_debug = format!("{out:?}");
    if out.status != DeliveryStatus::Refused
        || !(out_debug.contains("NoCallerPane") || out_debug.contains("no_caller_pane"))
    {
        return Err(format!(
            "caller_pane_missing must be a hard NoCallerPane refusal (MUST-11/N26), not rebind/autoclaim; got {out:?}"
        ));
    }
    Ok(())
}

fn assert_same_team_live_owner_mismatch_requires_takeover() -> Result<(), String> {
    let case = TmuxCase::new("owner-gate-live-mismatch");
    let owner = case.spawn_plain_pane("parent-owner");
    let caller = case.spawn_plain_pane("other-live-caller");
    let _env = EnvGuard::set(&[("TMUX_PANE", caller.as_str())]);
    seed_parent_child_state(&case.workspace, &owner, "%child-owner", "parent");

    let out = send_message(
        &case.workspace,
        &MessageTarget::Single("subleader_w".to_string()),
        "LIVE_MISMATCH_MUST_REQUIRE_TAKEOVER",
        &send_opts("leader", Some("parent")),
    )
    .map_err(|err| format!("same_team_owner_live_mismatch: send errored: {err}"))?;

    if out.status != DeliveryStatus::Refused || out.reason != Some(DeliveryRefusal::TeamOwnerMismatch) {
        return Err(format!(
            "same_team_owner_live_mismatch must return TeamOwnerMismatch, got {out:?}"
        ));
    }
    if !out.verification.as_deref().unwrap_or("").contains("takeover") {
        return Err(format!(
            "same_team_owner_live_mismatch must hint explicit takeover; got {out:?}"
        ));
    }
    Ok(())
}

fn assert_l1_send_to_child_live_owner_is_refused() -> Result<(), String> {
    let case = TmuxCase::new("owner-gate-l1-child-live");
    let l1 = case.spawn_plain_pane("l1-parent-owner");
    let child_owner = case.spawn_plain_pane("child-owner");
    let _env = EnvGuard::set(&[("TMUX_PANE", l1.as_str())]);
    seed_parent_child_state(&case.workspace, &l1, &child_owner, "parent");

    let out = send_message(
        &case.workspace,
        &MessageTarget::Single("child_worker".to_string()),
        "L1_MUST_NOT_OPERATE_CHILD_WHILE_CHILD_OWNER_LIVE",
        &send_opts("leader", Some("child")),
    )
    .map_err(|err| format!("l1_send_to_c_owner_live: send errored: {err}"))?;

    if out.status != DeliveryStatus::Refused || out.reason != Some(DeliveryRefusal::TeamOwnerMismatch) {
        return Err(format!(
            "l1_send_to_c_owner_live must return TeamOwnerMismatch; got {out:?}"
        ));
    }
    Ok(())
}

fn assert_l1_send_to_child_dead_owner_retreats_without_binding() -> Result<(), String> {
    let case = TmuxCase::new("owner-gate-l1-child-dead");
    let l1 = case.spawn_plain_pane("l1-parent-owner");
    let dead_child_owner = "%dead-child-owner";
    let _env = EnvGuard::set(&[("TMUX_PANE", l1.as_str())]);
    seed_parent_child_state(&case.workspace, &l1, dead_child_owner, "parent");
    let before_child = team_binding_snapshot(&case.workspace, "child");

    let out = send_message(
        &case.workspace,
        &MessageTarget::Single("child_worker".to_string()),
        "L1_MUST_RETREAT_NOT_AUTOBIND_CHILD",
        &send_opts("leader", Some("child")),
    )
    .map_err(|err| format!("l1_send_to_c_owner_dead: send errored: {err}"))?;
    let after_child = team_binding_snapshot(&case.workspace, "child");

    if before_child != after_child {
        return Err(format!(
            "l1_send_to_c_owner_dead must retreat without rewriting state.teams.child owner/receiver; out={out:?} before={before_child:?} after={after_child:?}"
        ));
    }
    if after_child.owner_pane.as_deref() == Some(l1.as_str())
        || after_child.receiver_pane.as_deref() == Some(l1.as_str())
    {
        return Err(format!(
            "l1_send_to_c_owner_dead must not auto-bind L1 pane as child owner/receiver; out={out:?} child={after_child:?}"
        ));
    }
    if out.status != DeliveryStatus::Blocked || out.channel.as_deref() != Some("rebind_required") {
        return Err(format!(
            "l1_send_to_c_owner_dead must return rebind_required, not TeamOwnerMismatch/autoclaim; got {out:?}"
        ));
    }
    if !out.verification.as_deref().unwrap_or("").contains("claim-leader --team child") {
        return Err(format!(
            "l1_send_to_c_owner_dead must hint explicit `claim-leader --team child`; got {out:?}"
        ));
    }
    Ok(())
}

fn assert_worker_send_to_parent_dead_leader_retreats_without_binding_worker() -> Result<(), String> {
    let case = TmuxCase::new("owner-gate-worker-parent-dead");
    let worker = case.spawn_plain_pane("worker-pane");
    let _env = EnvGuard::set(&[("TMUX_PANE", worker.as_str()), ("TEAM_AGENT_ID", "subleader_w")]);
    seed_parent_child_state(&case.workspace, "%dead-parent-owner", "%child-owner", "parent");
    set_team_agent_pane(&case.workspace, "parent", "subleader_w", &worker);
    let before_parent = team_binding_snapshot(&case.workspace, "parent");

    let out = send_message(
        &case.workspace,
        &MessageTarget::Single("leader".to_string()),
        "WORKER_MUST_NOT_SELF_PROMOTE_WHEN_PARENT_LEADER_DEAD",
        &send_opts("subleader_w", Some("parent")),
    )
    .map_err(|err| format!("worker_send_to_p_leader_dead: send errored: {err}"))?;
    let after_parent = team_binding_snapshot(&case.workspace, "parent");

    if before_parent != after_parent {
        return Err(format!(
            "worker_send_to_p_leader_dead must not rewrite parent owner/receiver; out={out:?} before={before_parent:?} after={after_parent:?}"
        ));
    }
    if after_parent.owner_pane.as_deref() == Some(worker.as_str())
        || after_parent.receiver_pane.as_deref() == Some(worker.as_str())
    {
        return Err(format!(
            "worker_send_to_p_leader_dead must not write worker pane as parent leader_receiver; out={out:?} parent={after_parent:?}"
        ));
    }
    if out.status != DeliveryStatus::Blocked || out.channel.as_deref() != Some("rebind_required") {
        return Err(format!(
            "worker_send_to_p_leader_dead must return rebind_required while preserving message/retry path; got {out:?}"
        ));
    }
    Ok(())
}

fn assert_send_after_explicit_child_claim_succeeds() -> Result<(), String> {
    let case = TmuxCase::new("owner-gate-explicit-claim");
    let l1 = case.spawn_plain_pane("l1-explicit-child-claim");
    let _env = EnvGuard::set(&[("TMUX_PANE", l1.as_str())]);
    seed_parent_child_state(&case.workspace, &l1, "%dead-child-owner", "parent");

    let claim = team_agent::leader::claim_leader(&case.workspace, Some("child"), false)
        .map_err(|err| format!("explicit child claim errored: {err}"))?;
    if !claim.ok {
        return Err(format!(
            "explicit claim-leader --team child is the recovery path and must succeed; got {claim:?}"
        ));
    }
    let out = send_message(
        &case.workspace,
        &MessageTarget::Single("child_worker".to_string()),
        "SEND_AFTER_EXPLICIT_CHILD_CLAIM_MUST_SUCCEED",
        &send_opts("leader", Some("child")),
    )
    .map_err(|err| format!("send_after_explicit_claim_succeeds: send errored: {err}"))?;
    if !out.ok || !matches!(out.status, DeliveryStatus::Queued | DeliveryStatus::Delivered) {
        return Err(format!(
            "send_after_explicit_claim_succeeds must queue/deliver after explicit child claim; got {out:?}"
        ));
    }
    Ok(())
}

fn assert_send_variant_is_reclaim_neutral(
    label: &str,
    target: MessageTarget,
) -> Result<(), String> {
    let case = TmuxCase::new(&format!("send-neutral-{}", label.replace(' ', "-")));
    let caller = case.spawn_plain_pane("ordinary-sender");
    let _env = EnvGuard::set(&[("TMUX_PANE", caller.as_str())]);
    seed_state(&case.workspace, Some("%dead-leader"), 3, &[("worker_a", "codex"), ("worker_b", "codex")]);
    let before = runtime_state(&case.workspace);
    let out = send_message(
        &case.workspace,
        &target,
        &format!("{label} RECLAIM_NEUTRAL_CANARY"),
        &send_opts("leader", Some("team-a")),
    )
    .map_err(|err| format!("{label}: send_message errored: {err}"))?;
    let after = runtime_state(&case.workspace);
    if binding_snapshot(&before) != binding_snapshot(&after) {
        return Err(format!(
            "{label}: send path must not mutate leader_receiver/team_owner/owner_epoch; out={out:?} before={before} after={after} events={}",
            events(&case.workspace)
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TeamBindingSnapshot {
    receiver_pane: Option<String>,
    owner_pane: Option<String>,
    owner_epoch: Option<u64>,
}

fn team_binding_snapshot(workspace: &Path, team: &str) -> TeamBindingSnapshot {
    let state = runtime_state(workspace);
    let entry = state
        .get("teams")
        .and_then(|teams| teams.get(team))
        .unwrap_or(&Value::Null);
    TeamBindingSnapshot {
        receiver_pane: entry
            .get("leader_receiver")
            .and_then(|v| v.get("pane_id"))
            .and_then(Value::as_str)
            .map(ToString::to_string),
        owner_pane: entry
            .get("team_owner")
            .and_then(|v| v.get("pane_id"))
            .and_then(Value::as_str)
            .map(ToString::to_string),
        owner_epoch: entry
            .get("team_owner")
            .and_then(|v| v.get("owner_epoch"))
            .or_else(|| entry.get("leader_receiver").and_then(|v| v.get("owner_epoch")))
            .and_then(Value::as_u64),
    }
}

fn seed_parent_child_state(workspace: &Path, parent_owner: &str, child_owner: &str, active: &str) {
    let parent = team_state(
        workspace,
        "parent",
        parent_owner,
        1,
        &[("subleader_w", "codex")],
    );
    let child = team_state(
        workspace,
        "child",
        child_owner,
        1,
        &[("child_worker", "codex")],
    );
    let active_state = if active == "child" { child.clone() } else { parent.clone() };
    let mut state = active_state;
    state["active_team_key"] = json!(active);
    state["teams"] = json!({
        "parent": parent,
        "child": child,
    });
    team_agent::state::persist::save_runtime_state(workspace, &state).unwrap();
    let _ = MessageStore::open(workspace).unwrap();
}

fn team_state(
    workspace: &Path,
    team: &str,
    owner_pane: &str,
    owner_epoch: u64,
    agents: &[(&str, &str)],
) -> Value {
    let mut agents_json = serde_json::Map::new();
    for (agent_id, provider) in agents {
        agents_json.insert(
            (*agent_id).to_string(),
            json!({
                "provider": provider,
                "status": "running",
                "window": agent_id,
                "owner_team_id": team
            }),
        );
    }
    let receiver = json!({
        "mode": "direct_tmux",
        "status": "attached",
        "provider": "codex",
        "pane_id": owner_pane,
        "owner_epoch": owner_epoch,
        "leader_session_uuid": format!("uuid-{team}")
    });
    let owner = json!({
        "provider": "codex",
        "pane_id": owner_pane,
        "owner_epoch": owner_epoch,
        "leader_session_uuid": format!("uuid-{team}"),
        "machine_fingerprint": "test-machine",
        "os_user": "test-user"
    });
    json!({
        "active_team_key": team,
        "team_dir": workspace.to_string_lossy().to_string(),
        "spec_path": workspace.join("team.spec.yaml").to_string_lossy().to_string(),
        "session_name": format!("team-{team}"),
        "leader": { "id": "leader" },
        "leader_receiver": receiver,
        "team_owner": owner,
        "agents": agents_json,
        "tasks": [
            { "id": "task_1", "assignee": agents.first().map(|(id, _)| *id).unwrap_or("worker"), "title": "task", "status": "pending" }
        ]
    })
}

fn set_team_agent_pane(workspace: &Path, team: &str, agent_id: &str, pane_id: &str) {
    let mut state = runtime_state(workspace);
    if let Some(agent) = state
        .get_mut("teams")
        .and_then(Value::as_object_mut)
        .and_then(|teams| teams.get_mut(team))
        .and_then(|entry| entry.get_mut("agents"))
        .and_then(Value::as_object_mut)
        .and_then(|agents| agents.get_mut(agent_id))
        .and_then(Value::as_object_mut)
    {
        agent.insert("pane_id".to_string(), json!(pane_id));
    }
    if state.get("active_team_key").and_then(Value::as_str) == Some(team) {
        state["agents"][agent_id]["pane_id"] = json!(pane_id);
    }
    team_agent::state::persist::save_runtime_state(workspace, &state).unwrap();
}

fn source(rel: &str) -> String {
    std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(rel)).unwrap()
}

fn send_opts(sender: &str, team: Option<&str>) -> SendOptions {
    SendOptions {
        sender: sender.to_string(),
        team: team.map(TeamKey::new),
        route_task_id: false,
        block_until_delivered: false,
        requires_ack: false,
        ..SendOptions::default()
    }
}

fn result_envelope(result_id: &str) -> Value {
    json!({
        "schema_version": "result_envelope_v1",
        "result_id": result_id,
        "task_id": "task_1",
        "agent_id": "worker_a",
        "status": "success",
        "summary": "result while leader binding is stale",
        "artifacts": [],
        "changes": [],
        "tests": [],
        "risks": [],
        "next_actions": []
    })
}

fn seed_state(workspace: &Path, receiver_pane: Option<&str>, epoch: u64, agents: &[(&str, &str)]) {
    let mut agents_json = serde_json::Map::new();
    let mut team_agents_json = serde_json::Map::new();
    for (agent_id, provider) in agents {
        agents_json.insert(
            (*agent_id).to_string(),
            json!({
                "provider": provider,
                "status": "running",
                "window": agent_id,
                "owner_team_id": "team-a"
            }),
        );
        team_agents_json.insert((*agent_id).to_string(), json!({"status": "running"}));
    }
    let mut state = json!({
        "active_team_key": "team-a",
        "team_dir": workspace.to_string_lossy().to_string(),
        "spec_path": workspace.join("team.spec.yaml").to_string_lossy().to_string(),
        "session_name": "team-reclaim-neutral",
        "leader": { "id": "leader" },
        "agents": agents_json,
        "teams": {
            "team-a": {
                "agents": team_agents_json,
                "tasks": [
                    { "id": "task_1", "assignee": "worker_a", "title": "task", "status": "pending" }
                ]
            }
        },
        "tasks": [
            { "id": "task_1", "assignee": "worker_a", "title": "task", "status": "pending" }
        ]
    });
    if let Some(pane) = receiver_pane {
        let receiver = json!({
            "mode": "direct_tmux",
            "status": "attached",
            "provider": "fake",
            "pane_id": pane,
            "owner_epoch": epoch
        });
        let owner = json!({
            "provider": "fake",
            "pane_id": pane,
            "owner_epoch": epoch,
            "machine_fingerprint": "test-machine",
            "os_user": "test-user"
        });
        state["leader_receiver"] = receiver.clone();
        state["team_owner"] = owner.clone();
        state["teams"]["team-a"]["leader_receiver"] = receiver;
        state["teams"]["team-a"]["team_owner"] = owner;
    }
    team_agent::state::persist::save_runtime_state(workspace, &state).unwrap();
    let _ = MessageStore::open(workspace).unwrap();
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct BindingSnapshot {
    pane: Option<String>,
    owner_pane: Option<String>,
    epoch: Option<u64>,
}

fn binding_snapshot(state: &Value) -> BindingSnapshot {
    BindingSnapshot {
        pane: state
            .get("leader_receiver")
            .and_then(|v| v.get("pane_id"))
            .and_then(Value::as_str)
            .map(ToString::to_string),
        owner_pane: state
            .get("team_owner")
            .and_then(|v| v.get("pane_id"))
            .and_then(Value::as_str)
            .map(ToString::to_string),
        epoch: state
            .get("team_owner")
            .and_then(|v| v.get("owner_epoch"))
            .or_else(|| state.get("leader_receiver").and_then(|v| v.get("owner_epoch")))
            .and_then(Value::as_u64),
    }
}

fn runtime_state(workspace: &Path) -> Value {
    team_agent::state::persist::load_runtime_state(workspace).unwrap()
}

fn events(workspace: &Path) -> String {
    std::fs::read_to_string(workspace.join(".team/logs/events.jsonl")).unwrap_or_default()
}

fn message_status(workspace: &Path, message_id: &str) -> Option<String> {
    let store = MessageStore::open(workspace).unwrap();
    let conn = team_agent::db::schema::open_db(store.db_path()).unwrap();
    conn.query_row(
        "select status from messages where message_id = ?1",
        params![message_id],
        |row| row.get(0),
    )
    .optional()
    .unwrap()
}

fn result_exists(workspace: &Path, result_id: &str) -> bool {
    let store = MessageStore::open(workspace).unwrap();
    let conn = team_agent::db::schema::open_db(store.db_path()).unwrap();
    let count: i64 = conn
        .query_row(
            "select count(*) from results where result_id = ?1",
            [result_id],
            |row| row.get(0),
        )
        .unwrap();
    count == 1
}

fn result_notification_message_count(workspace: &Path, result_id: &str) -> i64 {
    let store = MessageStore::open(workspace).unwrap();
    let conn = team_agent::db::schema::open_db(store.db_path()).unwrap();
    conn.query_row(
        "select count(*) from messages where content like ?1",
        [format!("%{result_id}%")],
        |row| row.get(0),
    )
    .unwrap()
}

fn leader_notification_log_count(workspace: &Path, result_id: &str) -> i64 {
    let store = MessageStore::open(workspace).unwrap();
    let conn = team_agent::db::schema::open_db(store.db_path()).unwrap();
    conn.query_row(
        "select count(*) from leader_notification_log where result_id = ?1",
        [result_id],
        |row| row.get(0),
    )
    .unwrap()
}

struct TmuxCase {
    workspace: PathBuf,
    fake: PathBuf,
}

impl TmuxCase {
    fn new(tag: &str) -> Self {
        let workspace = tmp_dir(tag);
        std::fs::write(
            workspace.join("team.spec.yaml"),
            "name: team-a\nobjective: reclaim neutral contract\n",
        )
        .unwrap();
        let bin = workspace.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let fake = bin.join("fake");
        std::os::unix::fs::symlink("/bin/sleep", &fake).unwrap();
        Self { workspace, fake }
    }

    fn spawn_leader_shaped_pane(&self, window: &str) -> String {
        self.spawn_pane(window, vec![self.fake.to_string_lossy().to_string(), "120".to_string()])
    }

    fn spawn_plain_pane(&self, window: &str) -> String {
        self.spawn_pane(window, vec!["/bin/sleep".to_string(), "120".to_string()])
    }

    fn spawn_cat_pane(&self, window: &str) -> String {
        self.spawn_pane(
            window,
            vec![
                "sh".to_string(),
                "-lc".to_string(),
                "stty -echo 2>/dev/null; exec cat".to_string(),
            ],
        )
    }

    fn spawn_pane(&self, window: &str, argv: Vec<String>) -> String {
        let backend = TmuxBackend::for_workspace(&self.workspace);
        let session = SessionName::new("ta-reclaim-neutral");
        let env = BTreeMap::new();
        let result = if backend.has_session(&session).unwrap_or(false) {
            backend.spawn_into(
                &session,
                &WindowName::new(window),
                &argv,
                &self.workspace,
                &env,
            )
        } else {
            backend.spawn_first(
                &session,
                &WindowName::new(window),
                &argv,
                &self.workspace,
                &env,
            )
        }
        .expect("tmux must be available for reclaim-neutral contract");
        std::thread::sleep(std::time::Duration::from_millis(100));
        result.pane_id.as_str().to_string()
    }
}

fn set_agent_pane(workspace: &Path, agent_id: &str, pane_id: &str) {
    let mut state = runtime_state(workspace);
    state["agents"][agent_id]["pane_id"] = json!(pane_id);
    team_agent::state::persist::save_runtime_state(workspace, &state).unwrap();
}

impl Drop for TmuxCase {
    fn drop(&mut self) {
        TmuxBackend::for_workspace(&self.workspace).kill_server();
        let _ = std::fs::remove_dir_all(&self.workspace);
    }
}

fn tmp_dir(tag: &str) -> PathBuf {
    static N: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "ta-rs-reclaim-neutral-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::canonicalize(dir).unwrap()
}

struct EnvGuard {
    previous: Vec<(String, Option<String>)>,
}

impl EnvGuard {
    fn set(values: &[(&str, &str)]) -> Self {
        let mut previous = Vec::new();
        for (key, value) in values {
            previous.push(((*key).to_string(), std::env::var(key).ok()));
            unsafe {
                std::env::set_var(key, value);
            }
        }
        for key in [
            "TEAM_AGENT_ID",
            "TEAM_AGENT_LEADER_PANE_ID",
            "TEAM_AGENT_OWNER_TEAM_ID",
            "TMUX",
            "TMUX_PANE",
        ] {
            if !values.iter().any(|(set_key, _)| *set_key == key) {
                previous.push((key.to_string(), std::env::var(key).ok()));
                unsafe {
                    std::env::remove_var(key);
                }
            }
        }
        Self { previous }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (key, value) in self.previous.drain(..).rev() {
            unsafe {
                if let Some(value) = value {
                    std::env::set_var(key, value);
                } else {
                    std::env::remove_var(key);
                }
            }
        }
    }
}
