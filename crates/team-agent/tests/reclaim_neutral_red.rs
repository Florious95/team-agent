// 0.5.x Windows portability Batch 5: entire file uses `TmuxCase`
// which builds tmux sessions with `std::os::unix::fs::symlink` +
// shell scripts. tmux is Unix-only conceptually, so cfg-gate the
// whole file rather than sprinkle cfg on every test.
#![cfg(unix)]
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

#[path = "support/hermetic.rs"]
mod hermetic_guard;
#[allow(dead_code)]
fn _hermetic_boundary_marker(_: &hermetic_guard::HermeticTestEnv) {}

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use mcp_sim_harness::McpSimHarness;
use serde_json::{json, Value};
use serial_test::{file_serial, serial};
use team_agent::messaging::MessageTarget;
use team_agent::station::send::Authority;
use team_agent::model::ids::{AgentId, TeamKey};
use team_agent::tmux_backend::TmuxBackend;
use team_agent::transport::{SessionName, Transport, WindowName};

#[test]
fn send_path_reclaim_neutral_static_guard_no_send_auto_reclaim_callsite() {
    let send = source("src/station/send.rs");
    assert!(
        !send.contains("auto_reclaim_from_caller_if_stale"),
        "I-RN/send neutral: station/send.rs must not call auto_reclaim_from_caller_if_stale; delivery is not an ownership side-effect"
    );
}

#[test]
#[ignore = "real-machine: live tmux/MCP-sim reclaim-neutral gate"]
#[serial(env)]
#[file_serial(tmux)]
fn send_path_reclaim_neutral_cli_broadcast_fanout_and_mcp_do_not_claim() {
    let mut failures = Vec::new();
    for (label, target) in [
        (
            "CLI-style single send",
            MessageTarget::Single("worker_a".to_string()),
        ),
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
    let envelope = result_envelope("res_report_neutral");
    let out = report_fixture_result(&case.workspace, &envelope);
    let after = runtime_state(&case.workspace);
    assert_eq!(binding_snapshot(&before), binding_snapshot(&after),
        "report_result must not claim or rewrite owner/receiver: {out}");
    assert_eq!(out["stored"], true);
    assert_eq!(out["live"], false);
    assert_ne!(out["delivery"], "physical");
    assert!(out["accepted"].is_null());
    assert!(out["delivery_error"].as_str().is_some_and(|error| !error.is_empty()),
        "dead receiver must produce a truthful delivery diagnostic: {out}");
    let inbox = leader_inbox(&case.workspace);
    assert_eq!(inbox.len(), 1, "result and notification share one durable record: {inbox:?}");
    assert_eq!(inbox[0]["kind"], "result");
    assert_eq!(inbox[0]["message_id"], out["message_id"]);
    for (key, value) in envelope.as_object().unwrap() {
        assert_eq!(&inbox[0]["envelope"][key], value, "result field {key} must survive");
    }
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
    seed_state(&case.workspace, Some(leader.as_str()), 4, &[("worker_a", "fake")]);
    set_agent_pane(&case.workspace, "worker_a", &worker);
    let before = runtime_state(&case.workspace);
    let send = || team_agent::station::send::execute(team_agent::station::send::Request {
        workspace: &case.workspace, team: Some("team-a"), to: "worker_a",
        content: "TRANSPORT_RECLAIM_NEUTRAL_CANARY", task_id: None,
        message_id: Some("transport-reclaim-neutral"), presentation: &Default::default(),
        mailbox: false, envelope: None,
    }, Authority::Operator { sender: "leader" }).expect("current worker send");
    let out = send();
    assert_eq!(out["stored"], true);
    assert_eq!(out["delivery"], "physical", "fixture must physically deliver: {out}");
    let retry = send();
    assert_eq!(retry["duplicate"], true);
    assert_eq!(retry["message_id"], out["message_id"]);
    assert_eq!(binding_snapshot(&before), binding_snapshot(&runtime_state(&case.workspace)),
        "physical delivery and retry must not rewrite ownership");
    let transport = TmuxBackend::for_workspace(&case.workspace);
    assert!(transport.capture(
        &team_agent::transport::Target::Pane(team_agent::transport::PaneId::new(&worker)),
        team_agent::transport::CaptureRange::Full,
    ).unwrap().text.contains("TRANSPORT_RECLAIM_NEUTRAL_CANARY"),
        "physical capture, not a fabricated status, must prove delivery");
}

#[test]
#[ignore = "real-machine: live tmux/MCP-sim reclaim-neutral gate"]
#[serial(env)]
#[file_serial(tmux)]
fn explicit_claim_unconditional_live_caller_claims_vacant_or_dead_and_requeues_same_message_id() {
    let case = TmuxCase::new("explicit-claim");
    seed_state(&case.workspace, None, 0, &[("worker_a", "codex")]);
    let out = report_fixture_result(&case.workspace, &result_envelope("res_claim_requeue"));
    assert_eq!(out["stored"], true);
    assert_ne!(out["delivery"], "physical");
    let before = leader_inbox(&case.workspace);
    assert_eq!(before.len(), 1);
    assert_eq!(before[0]["message_id"], out["message_id"]);
    let caller = case.spawn_plain_pane("ordinary-shell-caller");
    let _env = EnvGuard::set(&[("TMUX_PANE", caller.as_str())]);
    let result = team_agent::leader::claim_leader(&case.workspace, Some("team-a"), false)
        .expect("explicit claim must return a typed result");
    let state = runtime_state(&case.workspace);
    assert!(result.ok, "explicit live-caller claim must succeed: {result:?} state={state}");
    assert_eq!(binding_snapshot(&state).pane.as_deref(), Some(caller.as_str()));
    assert_eq!(binding_snapshot(&state).epoch, Some(1), "explicit claim must advance the epoch");
    let after = leader_inbox(&case.workspace);
    assert_eq!(after, before, "explicit binding must preserve the same durable record without requeue duplication or invented acceptance");
}

#[test]
#[ignore = "real-machine: live tmux/MCP-sim reclaim-neutral gate"]
#[serial(env)]
#[file_serial(tmux)]
fn explicit_takeover_live_owner_unconditional_replaces_live_owner_without_split_brain() {
    let case = TmuxCase::new("explicit-takeover");
    let old = case.spawn_leader_shaped_pane("old-live-leader");
    seed_state(
        &case.workspace,
        Some(old.as_str()),
        6,
        &[("worker_a", "codex")],
    );
    let caller = case.spawn_plain_pane("ordinary-takeover-caller");
    let _env = EnvGuard::set(&[("TMUX_PANE", caller.as_str())]);

    let out = team_agent::cli::leader_port::claim_leader(&case.workspace, Some("team-a"), true)
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
    let out = send_to_scope(&case.workspace, "team-a", "worker_a", "DEAD_LEADER_SEND_MUST_NOT_AUTOFIX",
        Authority::Operator { sender: "leader" }).unwrap();
    assert_eq!(binding_snapshot(&before), binding_snapshot(&runtime_state(&case.workspace)));
    assert!(!events(&case.workspace).contains("auto_reclaim_applied"));
    assert_eq!(out["stored"], true);
    assert_ne!(out["delivery"], "physical");
    assert!(out["delivery_error"].as_str().is_some_and(|error| !error.is_empty()),
        "unavailable endpoint must be diagnosed without a retired takeover hint: {out}");
}

#[test]
#[ignore = "real-machine: live tmux/MCP-sim reclaim-neutral gate"]
#[serial(env)]
#[file_serial(tmux)]
fn send_owner_gate_retreat_cases_do_not_auto_bind_or_open_cross_team_access() {
    let mut failures = Vec::new();
    if let Err(error) = assert_operator_without_pane_is_binding_neutral() {
        failures.push(error);
    }
    if let Err(error) = assert_same_team_other_pane_preserves_owner() {
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

fn assert_operator_without_pane_is_binding_neutral() -> Result<(), String> {
    let case = TmuxCase::new("owner-gate-no-caller");
    let owner = case.spawn_plain_pane("parent-owner");
    let _env = EnvGuard::set(&[]);
    seed_parent_child_state(&case.workspace, &owner, "%child-owner", "parent");
    let before = all_bindings(&case.workspace);
    let out = send_to_scope(&case.workspace, "parent", "subleader_w", "OPERATOR_WITHOUT_PANE_MUST_NOT_BIND",
        Authority::Operator { sender: "leader" })?;
    if before != all_bindings(&case.workspace) || out["stored"] != true || out["delivery"] == "physical" {
        return Err(format!("pane-less operator must store without taking ownership: {out} before={before} after={}", all_bindings(&case.workspace)));
    }
    Ok(())
}

fn assert_same_team_other_pane_preserves_owner() -> Result<(), String> {
    let case = TmuxCase::new("owner-gate-live-mismatch");
    let owner = case.spawn_plain_pane("parent-owner");
    let caller = case.spawn_plain_pane("other-live-caller");
    let _env = EnvGuard::set(&[("TMUX_PANE", caller.as_str())]);
    seed_parent_child_state(&case.workspace, &owner, "%child-owner", "parent");
    let before = all_bindings(&case.workspace);
    let out = send_to_scope(&case.workspace, "parent", "subleader_w", "OTHER_PANE_MUST_NOT_TAKE_OVER",
        Authority::Operator { sender: "leader" })?;
    if before != all_bindings(&case.workspace) || out["stored"] != true {
        return Err(format!("same-team send must leave the existing owner unchanged: {out} before={before} after={}", all_bindings(&case.workspace)));
    }
    Ok(())
}

fn assert_l1_send_to_child_live_owner_is_refused() -> Result<(), String> {
    let case = TmuxCase::new("owner-gate-l1-child-live");
    let l1 = case.spawn_plain_pane("l1-parent-owner");
    let child_owner = case.spawn_plain_pane("child-owner");
    let _env = EnvGuard::set(&[("TMUX_PANE", l1.as_str())]);
    seed_parent_child_state(&case.workspace, &l1, &child_owner, "parent");
    let before = all_bindings(&case.workspace);
    let out = send_to_scope(&case.workspace, "child", "child_worker", "PARENT_SEAT_MUST_NOT_WIDEN_SCOPE",
        Authority::Seat { sender: "leader", team: "parent" })?;
    if out["reason"] != "peer_not_in_scope" || out["stored"] != false
        || before != all_bindings(&case.workspace) || case.workspace.join(".team/inbox").exists() {
        return Err(format!("cross-team Seat refusal must be binding-neutral and zero-write: {out}"));
    }
    Ok(())
}

fn assert_l1_send_to_child_dead_owner_retreats_without_binding() -> Result<(), String> {
    let case = TmuxCase::new("owner-gate-l1-child-dead");
    let l1 = case.spawn_plain_pane("l1-parent-owner");
    let _env = EnvGuard::set(&[("TMUX_PANE", l1.as_str())]);
    seed_parent_child_state(&case.workspace, &l1, "%dead-child-owner", "parent");
    let before = all_bindings(&case.workspace);
    let out = send_to_scope(&case.workspace, "child", "child_worker", "DEAD_OWNER_DOES_NOT_WIDEN_SEAT_SCOPE",
        Authority::Seat { sender: "leader", team: "parent" })?;
    if out["reason"] != "peer_not_in_scope" || out["stored"] != false
        || before != all_bindings(&case.workspace) || case.workspace.join(".team/inbox").exists() {
        return Err(format!("dead child owner must not grant parent Seat authority or trigger writes: {out}"));
    }
    let after = team_binding_snapshot(&case.workspace, "child");
    if after.owner_pane.as_deref() == Some(l1.as_str()) || after.receiver_pane.as_deref() == Some(l1.as_str()) {
        return Err("parent pane must never auto-bind as child owner/receiver".into());
    }
    Ok(())
}

fn assert_worker_send_to_parent_dead_leader_retreats_without_binding_worker() -> Result<(), String> {
    let case = TmuxCase::new("owner-gate-worker-parent-dead");
    let worker = case.spawn_plain_pane("worker-pane");
    let _env = EnvGuard::set(&[("TMUX_PANE", worker.as_str()), ("TEAM_AGENT_ID", "subleader_w")]);
    seed_parent_child_state(&case.workspace, "%dead-parent-owner", "%child-owner", "parent");
    set_team_agent_pane(&case.workspace, "parent", "subleader_w", &worker);
    let before = all_bindings(&case.workspace);
    let out = send_to_scope(&case.workspace, "parent", "leader", "WORKER_MUST_NOT_SELF_PROMOTE",
        Authority::Seat { sender: "subleader_w", team: "parent" })?;
    if before != all_bindings(&case.workspace) || out["stored"] != true || out["delivery"] == "physical" {
        return Err(format!("worker result must remain durable/nonphysical without changing any binding: {out}"));
    }
    let after = team_binding_snapshot(&case.workspace, "parent");
    if after.owner_pane.as_deref() == Some(worker.as_str()) || after.receiver_pane.as_deref() == Some(worker.as_str()) {
        return Err("worker must never promote itself to parent owner/receiver".into());
    }
    let inbox = team_agent::inbox_store::read(&case.workspace, "leader", 10, Some("parent"))?;
    if !inbox["messages"].as_array().unwrap().iter().any(|record| record["message_id"] == out["message_id"]) {
        return Err("dead-receiver message must actually be durable".into());
    }
    Ok(())
}

fn assert_send_after_explicit_child_claim_succeeds() -> Result<(), String> {
    let case = TmuxCase::new("owner-gate-explicit-claim");
    let l1 = case.spawn_leader_shaped_pane("l1-explicit-child-claim");
    let worker = case.spawn_cat_pane("child-worker");
    // Explicit claim requires the real caller's absolute native tmux endpoint.
    let argv = TmuxBackend::argv_for_workspace(&case.workspace, &[
        "tmux".into(), "display-message".into(), "-p".into(), "-t".into(), l1.clone(),
        "#{socket_path},#{pid},0".into(),
    ]);
    let caller = std::process::Command::new(&argv[0]).args(&argv[1..]).output().map_err(|error| error.to_string())?;
    if !caller.status.success() { return Err(format!("read native caller tuple: {caller:?}")); }
    let tmux = String::from_utf8(caller.stdout).map_err(|error| error.to_string())?;
    let _env = EnvGuard::set(&[("TMUX_PANE", l1.as_str()), ("TMUX", tmux.trim())]);
    seed_parent_child_state(&case.workspace, &l1, "%dead-child-owner", "parent");
    set_team_agent_pane(&case.workspace, "child", "child_worker", &worker);
    // This is a real cat-based fake worker, not a native Codex seat without its required authority.
    let mut state = runtime_state(&case.workspace);
    state["teams"]["child"]["agents"]["child_worker"]["provider"] = json!("fake");
    team_agent::state::persist::save_runtime_state(&case.workspace, &state).unwrap();
    let before = team_binding_snapshot(&case.workspace, "child");
    let parent = team_binding_snapshot(&case.workspace, "parent");
    // Explicit confirmation is required when the old owner's liveness is not proven.
    let claim = team_agent::leader::claim_leader(&case.workspace, Some("child"), true)
        .map_err(|error| error.to_string())?;
    let claimed = team_binding_snapshot(&case.workspace, "child");
    if !claim.ok || claimed.owner_epoch != Some(before.owner_epoch.unwrap_or(0) + 1)
        || claimed.receiver_pane.as_deref() != Some(l1.as_str())
        || claimed.owner_pane.as_deref() != Some(l1.as_str())
        || parent != team_binding_snapshot(&case.workspace, "parent") {
        return Err(format!("explicit child claim must advance its epoch and bind only the caller: {claim:?} before={before:?} after={claimed:?}"));
    }
    let bindings = all_bindings(&case.workspace);
    let token = "SEND_AFTER_EXPLICIT_CHILD_CLAIM_MUST_SUCCEED";
    let out = send_to_scope(&case.workspace, "child", "child_worker", token,
        Authority::Operator { sender: "leader" })?;
    if out["stored"] != true || out["delivery"] != "physical" || bindings != all_bindings(&case.workspace) {
        return Err(format!("send after explicit claim must physically deliver without further ownership changes: {out}"));
    }
    let backend = TmuxBackend::for_workspace(&case.workspace);
    let target = team_agent::transport::Target::Pane(team_agent::transport::PaneId::new(&worker));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        let capture = backend.capture(&target, team_agent::transport::CaptureRange::Full).map_err(|error| error.to_string())?;
        if capture.text.contains(token) { break; }
        if std::time::Instant::now() >= deadline {
            return Err(format!("physical child-worker capture must prove submission: {:?}", capture.text));
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    Ok(())
}

fn assert_send_variant_is_reclaim_neutral(label: &str, target: MessageTarget) -> Result<(), String> {
    let case = TmuxCase::new(&format!("send-neutral-{}", label.replace(' ', "-")));
    let caller = case.spawn_plain_pane("ordinary-sender");
    let _env = EnvGuard::set(&[("TMUX_PANE", caller.as_str())]);
    seed_state(&case.workspace, Some("%dead-leader"), 3, &[("worker_a", "codex"), ("worker_b", "codex")]);
    let before = runtime_state(&case.workspace);
    let target = match target { MessageTarget::Single(id) => id, MessageTarget::Broadcast => "*".into(), MessageTarget::Fanout(ids) => ids.join(",") };
    let out = send_to_scope(&case.workspace, "team-a", &target, &format!("{label} RECLAIM_NEUTRAL_CANARY"),
        Authority::Operator { sender: "leader" })?;
    if out["ok"] != true || binding_snapshot(&before) != binding_snapshot(&runtime_state(&case.workspace)) {
        return Err(format!("{label}: current durable send must not mutate leader_receiver/team_owner/owner_epoch: {out}"));
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
            .or_else(|| {
                entry
                    .get("leader_receiver")
                    .and_then(|v| v.get("owner_epoch"))
            })
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
    let active_state = if active == "child" {
        child.clone()
    } else {
        parent.clone()
    };
    let mut state = active_state;
    state["active_team_key"] = json!(active);
    state["teams"] = json!({
        "parent": parent,
        "child": child,
    });
    team_agent::state::persist::save_runtime_state(workspace, &state).unwrap();
    // M4: no SQLite message store fixture (MessageStore::open is retired).
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

fn send_to_scope(workspace: &Path, team: &str, to: &str, content: &str, authority: Authority<'_>) -> Result<Value, String> {
    team_agent::station::send::execute(team_agent::station::send::Request {
        workspace, team: Some(team), to, content, task_id: None, message_id: None,
        presentation: &Default::default(), mailbox: false, envelope: None,
    }, authority)
}

fn report_fixture_result(workspace: &Path, envelope: &Value) -> Value {
    let out = team_agent::mcp_server::TeamOrchestratorTools::with_identity(
        workspace, Some(AgentId::new("worker_a")), Some(TeamKey::new("team-a")),
    ).report_result_with_presentation(Some(envelope), None, None, None, None, None, None, None, None, None, None)
        .expect("current scoped report_result");
    Value::Object(out.fields)
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
    // M4: no SQLite message store fixture (MessageStore::open is retired).
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
            .or_else(|| {
                state
                    .get("leader_receiver")
                    .and_then(|v| v.get("owner_epoch"))
            })
            .and_then(Value::as_u64),
    }
}

fn runtime_state(workspace: &Path) -> Value {
    team_agent::state::persist::load_runtime_state(workspace).unwrap()
}

fn events(workspace: &Path) -> String {
    std::fs::read_to_string(workspace.join(".team/logs/events.jsonl")).unwrap_or_default()
}

fn all_bindings(workspace: &Path) -> Value {
    let state = runtime_state(workspace);
    let teams: serde_json::Map<String, Value> = state["teams"].as_object().unwrap().iter()
        .map(|(key, entry)| (key.clone(), json!([entry["team_owner"], entry["leader_receiver"]]))).collect();
    json!({"root": [state["team_owner"], state["leader_receiver"]], "teams": teams})
}

fn leader_inbox(workspace: &Path) -> Vec<Value> {
    for path in [".team/team.db", ".team/runtime/team.db"] {
        assert!(!workspace.join(path).exists(), "current messaging must not create {path}");
    }
    team_agent::inbox_store::read(workspace, "leader", 1000, Some("team-a"))
        .unwrap()["messages"].as_array().unwrap().clone()
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
        self.spawn_pane(
            window,
            vec![self.fake.to_string_lossy().to_string(), "120".to_string()],
        )
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
