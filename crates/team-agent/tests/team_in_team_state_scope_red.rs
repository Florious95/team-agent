//! Team-in-team Bug 1/2 RED contracts.
//!
//! Evidence anchor:
//! `.team/artifacts/macmini-e2e/rs235-stageA-20260605T195214Z/rs235-a-evid-rs235-stageA-20260605T195214Z/`.
//!
//! User-facing contract:
//! - Starting a child team must preserve every running team in `state.teams`.
//! - A first-layer leader operating on `--team parent` may manage its parent worker
//!   `subleader_w`, even when the same pane is also the child team's leader.
//! - The same first-layer leader must not manage child-team workers unless it has
//!   explicitly claimed that child team.
//!
//! Mac mini follow-up, not enforced by this cargo file:
//! recovered subleader pane -> `claim-leader --team child` -> L3 worker result appears
//! on the recovered subleader pane.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[path = "support/hermetic.rs"]
mod hermetic_guard;
#[allow(dead_code)]
fn _hermetic_boundary_marker(_: &hermetic_guard::HermeticTestEnv) {}

#[path = "support/composite_source.rs"]
mod composite_source;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use serde_json::{json, Value};
use serial_test::serial;
use team_agent::lifecycle::{
    quick_start_with_transport, reset_agent_with_transport, start_agent_with_transport,
    stop_agent_with_transport,
};
use team_agent::messaging::{send_message, MessageTarget, SendOptions, TrustedSender};
use team_agent::model::ids::{AgentId, TeamKey};
use team_agent::state::persist::{load_runtime_state, save_runtime_state};
use team_agent::transport::{
    AttachOutcome, BackendKind, CaptureRange, CapturedText, InjectPayload, InjectReport,
    InjectStage, InjectVerification, Key, PaneField, PaneId, PaneInfo, PaneLiveness, SessionName,
    SetEnvOutcome, SpawnResult, SubmitVerification, Target, Transport, TransportError,
    TurnVerification, WindowName,
};

#[test]
#[ignore = "real-machine: nested quick-start/start-agent lifecycle gate"]
#[serial]
fn bug1_quick_start_child_preserves_runtime_teams_and_status_bindings() {
    let root = tmp_dir("bug1-state");
    let parent = team_dir(&root, "parent", &[("subleader_w", "Parent subleader")]);
    let child = team_dir(&root, "child", &[("child_worker", "Child worker")]);
    let transport = RecordingTransport::new();

    quick_start_with_transport(&parent, None, true, Some("parent"), &transport)
        .expect("fixture: parent quick-start should complete with recording transport");
    seed_canonical_state(
        &root,
        vec![team_entry(
            "parent",
            &parent,
            "%2",
            1,
            &[(
                "subleader_w",
                "running",
                json!(["mcp_team", "dangerous_auto_approve"]),
            )],
        )],
        "parent",
    );

    quick_start_with_transport(&child, None, true, Some("child"), &transport)
        .expect("Bug 1 trigger: child quick-start should run without a real provider");
    let after_child = load_runtime_state(&root).expect("state after child quick-start");
    let child_failures = missing_team_binding_failures(&after_child, &["parent", "child"]);

    let status = team_agent::cli::status_port::status(&root, false, true)
        .expect("status --json should be available for command-substitute validation");
    let status_failures = missing_status_team_binding_failures(&status, &["parent", "child"]);

    let mut failures = Vec::new();
    failures.extend(child_failures);
    failures.extend(status_failures);
    assert!(
        failures.is_empty(),
        "Bug 1 contract: quick-start must upsert launched parent/child teams into state.teams and status --json must expose every allowed team owner/receiver. Grandchild depth refusal is covered by sweep_new_reds_254_red::quick_start_grandchild_depth_limit_refuses_before_state_or_spawn.\nstate_after_child={after_child}\nstatus={status}\nfailures:\n{}",
        failures.join("\n")
    );
}

#[test]
#[ignore = "real-machine: nested quick-start/start-agent lifecycle gate"]
#[serial]
fn bug2_owner_gate_is_target_team_and_target_role_scoped_for_subleader_dual_identity() {
    let mut failures = Vec::new();

    for case in [
        OwnerGateCase::positive(
            "start-agent --team parent subleader_w",
            "parent",
            "subleader_w",
            Operation::Start,
        ),
        OwnerGateCase::positive(
            "restart-agent/resume --team parent subleader_w",
            "parent",
            "subleader_w",
            Operation::Resume,
        ),
        OwnerGateCase::positive(
            "stop-agent --team parent subleader_w",
            "parent",
            "subleader_w",
            Operation::Stop,
        ),
        OwnerGateCase::positive(
            "reset-agent --team parent subleader_w",
            "parent",
            "subleader_w",
            Operation::Reset,
        ),
        OwnerGateCase::positive(
            "send --team parent --to subleader_w",
            "parent",
            "subleader_w",
            Operation::Send,
        ),
    ] {
        if let Err(err) = run_owner_gate_case(case) {
            failures.push(err);
        }
    }

    for case in [
        OwnerGateCase::negative(
            "start-agent --team child child_worker",
            "child",
            "child_worker",
            Operation::Start,
        ),
        OwnerGateCase::negative(
            "stop-agent --team child child_worker",
            "child",
            "child_worker",
            Operation::Stop,
        ),
        OwnerGateCase::negative(
            "reset-agent --team child child_worker",
            "child",
            "child_worker",
            Operation::Reset,
        ),
        OwnerGateCase::negative(
            "send --team child --to child_worker",
            "child",
            "child_worker",
            Operation::Send,
        ),
    ] {
        if let Err(err) = run_owner_gate_case(case) {
            failures.push(err);
        }
    }

    if let Err(err) = assert_owner_gate_static_guard() {
        failures.push(err);
    }

    assert!(
        failures.is_empty(),
        "Bug 2 contract: owner gate must key on (target_team, target_role), not pane id alone; it must not let the L1 leader operate on child team workers without explicit child claim.\n{}",
        failures.join("\n\n")
    );
}

#[test]
#[ignore = "real-machine: nested quick-start/start-agent lifecycle gate"]
#[serial]
fn bug2_owner_binding_is_team_scoped_for_claim_projection_quick_start_and_parent_start_agent() {
    let mut failures = Vec::new();

    if let Err(err) = assert_claim_leader_writes_selected_team_owner_not_top_level() {
        failures.push(err);
    }
    if let Err(err) = assert_projection_does_not_borrow_top_level_owner_for_other_team() {
        failures.push(err);
    }
    if let Err(err) = assert_child_quick_start_does_not_inherit_parent_owner() {
        failures.push(err);
    }
    if let Err(err) = assert_parent_start_agent_ignores_child_owner_projection() {
        failures.push(err);
    }

    assert!(
        failures.is_empty(),
        "Bug 2 residual contract: owner bindings are team-scoped. claim-leader --team T writes teams[T], team projection never borrows another team's top-level owner, child quick-start does not inherit parent owner, and start-agent --team parent must not see child owner/sticky_bind_collision.\n{}",
        failures.join("\n\n")
    );
}

#[test]
#[ignore = "real-machine: nested quick-start/start-agent lifecycle gate"]
#[serial]
fn bug2_full_parent_child_sequence_keeps_owner_writeback_team_scoped() {
    let mut failures = Vec::new();
    for parent_quick_start_caller in [Some("%2"), None] {
        if let Err(err) = run_full_parent_child_owner_writeback_sequence(parent_quick_start_caller)
        {
            failures.push(err);
        }
    }

    assert!(
        failures.is_empty(),
        "Bug 2 full-sequence contract: parent quick-start must never persist an empty-pane fake owner; claim-leader --team parent must write teams.parent even when parent is active; child quick-start must not stale parent owner; start-agent --team parent must not read empty/stale parent owner or raise sticky_bind_collision.\n{}",
        failures.join("\n\n")
    );
}

#[derive(Clone, Copy)]
enum Operation {
    Start,
    Resume,
    Stop,
    Reset,
    Send,
}

struct OwnerGateCase {
    label: &'static str,
    team: &'static str,
    agent: &'static str,
    operation: Operation,
    should_allow: bool,
}

impl OwnerGateCase {
    fn positive(
        label: &'static str,
        team: &'static str,
        agent: &'static str,
        operation: Operation,
    ) -> Self {
        Self {
            label,
            team,
            agent,
            operation,
            should_allow: true,
        }
    }

    fn negative(
        label: &'static str,
        team: &'static str,
        agent: &'static str,
        operation: Operation,
    ) -> Self {
        Self {
            label,
            team,
            agent,
            operation,
            should_allow: false,
        }
    }
}

fn run_owner_gate_case(case: OwnerGateCase) -> Result<(), String> {
    let fixture = ScopeFixture::new(case.label);
    let _env = EnvGuard::set("TMUX_PANE", "%2");
    let before = load_runtime_state(&fixture.root).map_err(|e| e.to_string())?;
    let before_child_receiver = before
        .pointer("/teams/child/leader_receiver/pane_id")
        .cloned();
    let before_capability = before
        .pointer("/teams/parent/agents/subleader_w/tools")
        .cloned();

    let result = match case.operation {
        Operation::Start => start_agent_with_transport(
            &fixture.root,
            &AgentId::new(case.agent),
            true,
            false,
            true,
            Some(case.team),
            &fixture.transport,
        )
        .map(|_| json!({"ok": true}))
        .map_err(|e| e.to_string()),
        Operation::Resume => start_agent_with_transport(
            &fixture.root,
            &AgentId::new(case.agent),
            false,
            false,
            true,
            Some(case.team),
            &fixture.transport,
        )
        .map(|_| json!({"ok": true}))
        .map_err(|e| e.to_string()),
        Operation::Stop => stop_agent_with_transport(
            &fixture.root,
            &AgentId::new(case.agent),
            Some(case.team),
            &fixture.transport,
        )
        .map(|_| json!({"ok": true}))
        .map_err(|e| e.to_string()),
        Operation::Reset => reset_agent_with_transport(
            &fixture.root,
            &AgentId::new(case.agent),
            true,
            false,
            Some(case.team),
            &fixture.transport,
        )
        .map(|_| json!({"ok": true}))
        .map_err(|e| e.to_string()),
        Operation::Send => {
            let opts = SendOptions {
                sender: TrustedSender::leader(),
                team: Some(TeamKey::new(case.team.to_string())),
                requires_ack: false,
                wait_visible: false,
                route_task_id: false,
                ..SendOptions::default()
            };
            send_message(
                &fixture.root,
                &MessageTarget::Single(case.agent.to_string()),
                "TEAM_IN_TEAM_SCOPE_CANARY",
                &opts,
            )
            .map(|out| json!({"ok": out.ok, "status": format!("{:?}", out.status), "reason": format!("{:?}", out.reason)}))
            .map_err(|e| e.to_string())
        }
    };

    let after = load_runtime_state(&fixture.root).map_err(|e| e.to_string())?;
    let after_child_receiver = after
        .pointer("/teams/child/leader_receiver/pane_id")
        .cloned();
    let after_capability = after
        .pointer("/teams/parent/agents/subleader_w/tools")
        .cloned();
    if before_child_receiver != after_child_receiver {
        return Err(format!(
            "{}: L1 operation on parent subleader must not rewrite child leader_receiver; before={before_child_receiver:?} after={after_child_receiver:?}",
            case.label
        ));
    }
    if before_capability != after_capability {
        return Err(format!(
            "{}: L1 resume/operation must not change subleader_w capability/MUST-16 safety; before={before_capability:?} after={after_capability:?}",
            case.label
        ));
    }

    match (case.should_allow, result) {
        (true, Ok(value)) if value.get("ok").and_then(Value::as_bool) == Some(true) => Ok(()),
        (true, Ok(value)) => Err(format!("{}: expected allow for parent-team subleader operation, got {value}", case.label)),
        (true, Err(error)) => Err(format!("{}: expected allow for parent-team subleader operation, got error={error}", case.label)),
        (false, Ok(value)) if value.get("ok").and_then(Value::as_bool) == Some(false) => Ok(()),
        (false, Err(error)) if owner_refusal_like(&error) => Ok(()),
        (false, Ok(value)) => Err(format!(
            "{}: expected refusal before explicit claim-leader --team child, got allowed value={value}",
            case.label
        )),
        (false, Err(error)) => Err(format!(
            "{}: expected owner-gate refusal before explicit claim-leader --team child, got unrelated error={error}",
            case.label
        )),
    }
}

fn assert_owner_gate_static_guard() -> Result<(), String> {
    let restart_agent = source("src/lifecycle/restart/agent.rs");
    let launch = source("src/lifecycle/launch.rs");
    let send = source("src/messaging/send.rs");
    let mut failures = Vec::new();
    if restart_agent.contains("ensure_owner_allowed(workspace)") {
        failures.push(
            "restart/agent.rs must not call raw ensure_owner_allowed(workspace); lifecycle owner gate must receive selected target_team/target_role state".to_string(),
        );
    }
    if !launch.contains("ensure_owner_allowed_for")
        && !restart_agent.contains("ensure_owner_allowed_for")
    {
        failures.push(
            "lifecycle needs an owner-gate API that accepts selected target team state plus target role; raw pane-id-only owner gate is insufficient".to_string(),
        );
    }
    if send.contains("let state = crate::state::persist::load_runtime_state(workspace)?;")
        && send.contains(".get(\"agents\")")
    {
        failures.push(
            "messaging/send.rs must resolve opts.team into that team's agents before membership/owner checks; raw top-level agents make send --team parent/child scope drift".to_string(),
        );
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("\n"))
    }
}

fn assert_claim_leader_writes_selected_team_owner_not_top_level() -> Result<(), String> {
    let root = tmp_dir("bug2-owner-claim-scoped");
    let parent = team_dir(&root, "parent", &[("subleader_w", "Parent subleader")]);
    let child = team_dir(&root, "child", &[("child_worker", "Child worker")]);
    seed_canonical_state(
        &root,
        vec![
            team_entry(
                "parent",
                &parent,
                "%2",
                7,
                &[(
                    "subleader_w",
                    "running",
                    json!(["mcp_team", "dangerous_auto_approve"]),
                )],
            ),
            team_entry(
                "child",
                &child,
                "%4",
                3,
                &[("child_worker", "running", json!(["mcp_team"]))],
            ),
        ],
        "parent",
    );

    let mut state = load_runtime_state(&root).map_err(|e| e.to_string())?;
    let event_log = team_agent::event_log::EventLog::new(&root);
    let result = team_agent::leader::claim_lease_no_incident(
        &root,
        &mut state,
        Some("child"),
        &TeamKey::new("child"),
        &PaneId::new("%6"),
        true,
        &event_log,
        &AlwaysLive,
    )
    .map_err(|e| e.to_string())?;
    if !result.ok {
        return Err(format!(
            "claim-leader --team child command-substitute should succeed, got {result:?}"
        ));
    }

    let persisted = load_runtime_state(&root).map_err(|e| e.to_string())?;
    let mut failures = Vec::new();
    if pane_at(&persisted, "/teams/child/team_owner/pane_id") != Some("%6") {
        failures.push(format!(
            "claim-leader --team child must write teams.child.team_owner.pane_id=%6; state={persisted}"
        ));
    }
    if pane_at(&persisted, "/teams/child/leader_receiver/pane_id") != Some("%6") {
        failures.push(format!(
            "claim-leader --team child must write teams.child.leader_receiver.pane_id=%6; state={persisted}"
        ));
    }
    if pane_at(&persisted, "/teams/parent/team_owner/pane_id") != Some("%2") {
        failures.push(format!(
            "claim-leader --team child must not rewrite teams.parent.team_owner; state={persisted}"
        ));
    }
    if pane_at(&persisted, "/team_owner/pane_id") == Some("%6") {
        failures.push(format!(
            "claim-leader --team child must not bind the workspace top-level owner to the child caller; state={persisted}"
        ));
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("\n"))
    }
}

fn assert_projection_does_not_borrow_top_level_owner_for_other_team() -> Result<(), String> {
    let state = json!({
        "active_team_key": "child",
        "session_name": "team-child",
        "team_owner": owner("%4", "uuid-child", 5),
        "leader_receiver": receiver("%4", "uuid-child", 5),
        "teams": {
            "parent": {
                "status": "alive",
                "session_name": "team-parent",
                "team_dir": "/tmp/team-in-team-parent",
                "agents": {"subleader_w": {"status": "running"}}
            },
            "child": {
                "status": "alive",
                "session_name": "team-child",
                "team_dir": "/tmp/team-in-team-child",
                "team_owner": owner("%4", "uuid-child", 5),
                "leader_receiver": receiver("%4", "uuid-child", 5),
                "agents": {"child_worker": {"status": "running"}}
            }
        }
    });
    let projected = team_agent::state::projection::project_top_level_view(&state, "parent");
    let mut failures = Vec::new();
    if pane_at(&projected, "/team_owner/pane_id").is_some() {
        failures.push(format!(
            "project_top_level_view(parent) must not fallback to top-level child team_owner; projected={projected}"
        ));
    }
    if pane_at(&projected, "/leader_receiver/pane_id").is_some() {
        failures.push(format!(
            "project_top_level_view(parent) must not fallback to top-level child leader_receiver; projected={projected}"
        ));
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("\n"))
    }
}

fn assert_child_quick_start_does_not_inherit_parent_owner() -> Result<(), String> {
    let root = tmp_dir("bug2-child-quick-start-owner-scope");
    let parent = team_dir(&root, "parent", &[("subleader_w", "Parent subleader")]);
    let child = team_dir(&root, "child", &[("child_worker", "Child worker")]);
    seed_canonical_state(
        &root,
        vec![team_entry(
            "parent",
            &parent,
            "%2",
            11,
            &[(
                "subleader_w",
                "running",
                json!(["mcp_team", "dangerous_auto_approve"]),
            )],
        )],
        "parent",
    );
    quick_start_with_transport(
        &child,
        None,
        true,
        Some("child"),
        &RecordingTransport::new(),
    )
    .map_err(|e| e.to_string())?;
    let after = load_runtime_state(&root).map_err(|e| e.to_string())?;
    let mut failures = Vec::new();
    if pane_at(&after, "/teams/child/team_owner/pane_id") == Some("%2") {
        failures.push(format!(
            "child quick-start must not copy parent owner into teams.child.team_owner; state={after}"
        ));
    }
    if pane_at(&after, "/team_owner/pane_id") == Some("%2")
        && after.get("active_team_key").and_then(Value::as_str) == Some("child")
    {
        failures.push(format!(
            "child quick-start projected top-level active child must not carry parent owner; state={after}"
        ));
    }
    if pane_at(&after, "/teams/parent/team_owner/pane_id") != Some("%2") {
        failures.push(format!(
            "child quick-start must preserve parent owner under teams.parent; state={after}"
        ));
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("\n"))
    }
}

fn assert_parent_start_agent_ignores_child_owner_projection() -> Result<(), String> {
    let root = tmp_dir("bug2-parent-start-agent-owner-scope");
    let parent = team_dir(&root, "parent", &[("subleader_w", "Parent subleader")]);
    let child = team_dir(&root, "child", &[("child_worker", "Child worker")]);
    seed_canonical_state(
        &root,
        vec![team_entry_without_owner(
            "parent",
            &parent,
            &[(
                "subleader_w",
                "running",
                json!(["mcp_team", "dangerous_auto_approve"]),
            )],
        )],
        "parent",
    );
    let mut state = load_runtime_state(&root).map_err(|e| e.to_string())?;
    state["active_team_key"] = json!("child");
    state["session_name"] = json!("team-child");
    state["team_owner"] = owner("%4", "uuid-child", 9);
    state["leader_receiver"] = receiver("%4", "uuid-child", 9);
    state["teams"]["child"] = team_entry(
        "child",
        &child,
        "%4",
        9,
        &[("child_worker", "running", json!(["mcp_team"]))],
    )
    .1;
    save_runtime_state(&root, &state).map_err(|e| e.to_string())?;

    let transport = RecordingTransport::new()
        .with_session_present(true)
        .with_windows(vec![WindowName::new("subleader_w")]);
    let _pane = EnvGuard::set("TMUX_PANE", "%2");
    let _uuid = EnvGuard::set("TEAM_AGENT_LEADER_SESSION_UUID", "uuid-child");
    let result = start_agent_with_transport(
        &root,
        &AgentId::new("subleader_w"),
        false,
        false,
        true,
        Some("parent"),
        &transport,
    );
    match result {
        Ok(_) => Ok(()),
        Err(error) => {
            let error = error.to_string();
            if error.contains("sticky_bind_collision") || error.contains("team_owner_mismatch") {
                Err(format!(
                    "start-agent --team parent subleader_w must use parent team-scoped projection and must not see child owner/sticky_bind_collision; got error={error}"
                ))
            } else {
                Err(format!(
                    "start-agent --team parent subleader_w should succeed in this command-substitute fixture; got unrelated error={error}"
                ))
            }
        }
    }
}

fn run_full_parent_child_owner_writeback_sequence(
    parent_quick_start_caller: Option<&str>,
) -> Result<(), String> {
    let label = match parent_quick_start_caller {
        Some(_) => "with caller pane",
        None => "without caller pane",
    };
    let root = tmp_dir(&format!("bug2-full-owner-writeback-{label}"));
    let parent = team_dir(&root, "parent", &[("subleader_w", "Parent subleader")]);
    let child = team_dir(&root, "child", &[("child_worker", "Child worker")]);
    let quick_start_transport = RecordingTransport::new();
    let start_transport = RecordingTransport::new()
        .with_session_present(true)
        .with_windows(vec![
            WindowName::new("subleader_w"),
            WindowName::new("child_worker"),
        ]);
    let mut failures = Vec::new();

    {
        let _pane = match parent_quick_start_caller {
            Some(pane) => EnvGuard::set("TMUX_PANE", pane),
            None => EnvGuard::remove("TMUX_PANE"),
        };
        let _leader_pane = EnvGuard::remove("TEAM_AGENT_LEADER_PANE_ID");
        let _uuid = EnvGuard::remove("TEAM_AGENT_LEADER_SESSION_UUID");
        quick_start_with_transport(&parent, None, true, Some("parent"), &quick_start_transport)
            .map_err(|e| format!("{label}: parent quick-start failed: {e}"))?;
    }
    let after_parent = load_runtime_state(&root).map_err(|e| e.to_string())?;
    failures.extend(empty_owner_failures(
        &after_parent,
        &format!("{label}: after parent quick-start"),
    ));
    if let Some(pane) = parent_quick_start_caller {
        if pane_at(&after_parent, "/team_owner/pane_id") != Some(pane) {
            failures.push(format!(
                "{label}: parent quick-start with caller pane must bind top-level owner to {pane}; state={after_parent}"
            ));
        }
        if pane_at(&after_parent, "/teams/parent/team_owner/pane_id") != Some(pane) {
            failures.push(format!(
                "{label}: parent quick-start with caller pane must bind teams.parent owner to {pane}; state={after_parent}"
            ));
        }
    }

    let mut claim_state = load_runtime_state(&root).map_err(|e| e.to_string())?;
    let event_log = team_agent::event_log::EventLog::new(&root);
    let claim = team_agent::leader::claim_lease_no_incident(
        &root,
        &mut claim_state,
        Some("parent"),
        &TeamKey::new("parent"),
        &PaneId::new("%3"),
        true,
        &event_log,
        &AlwaysLive,
    )
    .map_err(|e| format!("{label}: claim-leader --team parent failed: {e}"))?;
    if !claim.ok {
        failures.push(format!(
            "{label}: claim-leader --team parent should succeed, got {claim:?}"
        ));
    }
    let after_claim = load_runtime_state(&root).map_err(|e| e.to_string())?;
    failures.extend(empty_owner_failures(
        &after_claim,
        &format!("{label}: after claim-leader --team parent"),
    ));
    if pane_at(&after_claim, "/team_owner/pane_id") != Some("%3") {
        failures.push(format!(
            "{label}: claim-leader --team parent should update active top-level owner to %3; state={after_claim}"
        ));
    }
    if pane_at(&after_claim, "/teams/parent/team_owner/pane_id") != Some("%3") {
        failures.push(format!(
            "{label}: claim-leader --team parent must update teams.parent.team_owner even when parent is active; state={after_claim}"
        ));
    }
    if pane_at(&after_claim, "/teams/parent/leader_receiver/pane_id") != Some("%3") {
        failures.push(format!(
            "{label}: claim-leader --team parent must update teams.parent.leader_receiver even when parent is active; state={after_claim}"
        ));
    }

    {
        let _pane = EnvGuard::set("TMUX_PANE", "%4");
        let _leader_pane = EnvGuard::remove("TEAM_AGENT_LEADER_PANE_ID");
        let _uuid = EnvGuard::remove("TEAM_AGENT_LEADER_SESSION_UUID");
        quick_start_with_transport(&child, None, true, Some("child"), &quick_start_transport)
            .map_err(|e| format!("{label}: child quick-start failed: {e}"))?;
    }
    let after_child = load_runtime_state(&root).map_err(|e| e.to_string())?;
    failures.extend(empty_owner_failures(
        &after_child,
        &format!("{label}: after child quick-start"),
    ));
    if pane_at(&after_child, "/teams/parent/team_owner/pane_id") != Some("%3") {
        failures.push(format!(
            "{label}: child quick-start must preserve claimed parent owner in teams.parent=%3; state={after_child}"
        ));
    }
    if pane_at(&after_child, "/teams/child/team_owner/pane_id") != Some("%4") {
        failures.push(format!(
            "{label}: child quick-start with caller pane must bind teams.child owner to %4 and not inherit parent; state={after_child}"
        ));
    }

    let parent_uuid = after_child
        .pointer("/teams/parent/team_owner/leader_session_uuid")
        .and_then(Value::as_str)
        .unwrap_or("");
    let _pane = EnvGuard::set("TMUX_PANE", "%3");
    let _leader_pane = EnvGuard::remove("TEAM_AGENT_LEADER_PANE_ID");
    let _uuid = if parent_uuid.is_empty() {
        EnvGuard::remove("TEAM_AGENT_LEADER_SESSION_UUID")
    } else {
        EnvGuard::set("TEAM_AGENT_LEADER_SESSION_UUID", parent_uuid)
    };
    let start = start_agent_with_transport(
        &root,
        &AgentId::new("subleader_w"),
        false,
        false,
        true,
        Some("parent"),
        &start_transport,
    );
    if let Err(error) = start {
        let error = error.to_string();
        if error.contains("sticky_bind_collision")
            || error.contains("team_owner_mismatch")
            || error.contains("\"pane_id\":\"\"")
        {
            failures.push(format!(
                "{label}: start-agent --team parent must not read stale/empty teams.parent owner or raise sticky_bind_collision; got error={error}"
            ));
        } else {
            failures.push(format!(
                "{label}: start-agent --team parent failed unexpectedly: {error}"
            ));
        }
    }

    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("\n"))
    }
}

fn owner_refusal_like(error: &str) -> bool {
    error.contains("team_owner_mismatch")
        || error.contains("owner_takeover_required")
        || error.contains("not_owner")
        || error.contains("refused")
}

struct ScopeFixture {
    root: PathBuf,
    transport: RecordingTransport,
}

impl ScopeFixture {
    fn new(label: &str) -> Self {
        let root = tmp_dir(label);
        let parent = team_dir(&root, "parent", &[("subleader_w", "Parent subleader")]);
        let child = team_dir(&root, "child", &[("child_worker", "Child worker")]);
        std::fs::write(
            root.join("team.spec.yaml"),
            std::fs::read_to_string(parent.join("team.spec.yaml")).unwrap(),
        )
        .unwrap();
        seed_canonical_state(
            &root,
            vec![
                team_entry(
                    "parent",
                    &parent,
                    "%2",
                    1,
                    &[(
                        "subleader_w",
                        "running",
                        json!(["mcp_team", "dangerous_auto_approve"]),
                    )],
                ),
                team_entry(
                    "child",
                    &child,
                    "%4",
                    1,
                    &[("child_worker", "running", json!(["mcp_team"]))],
                ),
            ],
            "child",
        );
        Self {
            root,
            transport: RecordingTransport::new()
                .with_session_present(true)
                .with_windows(vec![
                    WindowName::new("subleader_w"),
                    WindowName::new("child_worker"),
                ]),
        }
    }
}

fn seed_canonical_state(root: &Path, entries: Vec<(&'static str, Value)>, active: &str) {
    let mut teams = serde_json::Map::new();
    for (key, entry) in entries {
        teams.insert(key.to_string(), entry);
    }
    let active_entry = teams.get(active).cloned().unwrap_or_else(|| json!({}));
    let mut root_state = active_entry.as_object().cloned().unwrap_or_default();
    root_state.insert("active_team_key".to_string(), json!(active));
    root_state.insert("teams".to_string(), Value::Object(teams));
    save_runtime_state(root, &Value::Object(root_state)).unwrap();
    let _ = team_agent::message_store::MessageStore::open(root).unwrap();
}

fn team_entry(
    key: &'static str,
    team_dir: &Path,
    leader_pane: &str,
    owner_epoch: u64,
    agents: &[(&str, &str, Value)],
) -> (&'static str, Value) {
    let mut agent_map = serde_json::Map::new();
    for (agent_id, status, tools) in agents {
        agent_map.insert(
            (*agent_id).to_string(),
            json!({
                "agent_id": agent_id,
                "status": status,
                "provider": "codex",
                "auth_mode": "subscription",
                "role": agent_id,
                "window": agent_id,
                "owner_team_id": key,
                "session_id": format!("sess-{key}-{agent_id}"),
                "rollout_path": team_dir.join(format!("{agent_id}.jsonl")).to_string_lossy().to_string(),
                "tools": tools,
            }),
        );
    }
    (
        key,
        json!({
            "status": "alive",
            "active_team_key": key,
            "team_dir": team_dir.to_string_lossy().to_string(),
            "spec_path": team_dir.join("team.spec.yaml").to_string_lossy().to_string(),
            "workspace": team_dir.parent().unwrap().to_string_lossy().to_string(),
            "session_name": format!("team-{key}"),
            "leader": {"id": "leader"},
            "leader_receiver": {
                "pane_id": leader_pane,
                "provider": "codex",
                "leader_session_uuid": format!("uuid-{key}"),
                "owner_epoch": owner_epoch
            },
            "team_owner": {
                "pane_id": leader_pane,
                "provider": "codex",
                "machine_fingerprint": "",
                "leader_session_uuid": format!("uuid-{key}"),
                "owner_epoch": owner_epoch,
                "claimed_at": "2026-06-06T00:00:00Z",
                "claimed_via": "claim-leader",
                "os_user": "alauda"
            },
            "agents": Value::Object(agent_map),
            "tasks": [],
        }),
    )
}

fn team_entry_without_owner(
    key: &'static str,
    team_dir: &Path,
    agents: &[(&str, &str, Value)],
) -> (&'static str, Value) {
    let (_, mut entry) = team_entry(key, team_dir, "%unused", 0, agents);
    if let Some(obj) = entry.as_object_mut() {
        obj.remove("team_owner");
        obj.remove("leader_receiver");
    }
    (key, entry)
}

fn owner(pane_id: &str, uuid: &str, owner_epoch: u64) -> Value {
    json!({
        "pane_id": pane_id,
        "provider": "codex",
        "machine_fingerprint": "",
        "leader_session_uuid": uuid,
        "owner_epoch": owner_epoch,
        "claimed_at": "2026-06-06T00:00:00Z",
        "claimed_via": "claim-leader",
        "os_user": "alauda"
    })
}

fn receiver(pane_id: &str, uuid: &str, owner_epoch: u64) -> Value {
    json!({
        "pane_id": pane_id,
        "provider": "codex",
        "leader_session_uuid": uuid,
        "owner_epoch": owner_epoch
    })
}

fn pane_at<'a>(value: &'a Value, pointer: &str) -> Option<&'a str> {
    value.pointer(pointer).and_then(Value::as_str)
}

fn empty_owner_failures(state: &Value, context: &str) -> Vec<String> {
    let mut failures = Vec::new();
    for pointer in [
        "/team_owner/pane_id",
        "/leader_receiver/pane_id",
        "/teams/parent/team_owner/pane_id",
        "/teams/parent/leader_receiver/pane_id",
        "/teams/child/team_owner/pane_id",
        "/teams/child/leader_receiver/pane_id",
    ] {
        if state
            .pointer(pointer)
            .is_some_and(|value| value.as_str() == Some(""))
        {
            failures.push(format!(
                "{context}: {pointer} must never be an empty pane_id fake owner/receiver; state={state}"
            ));
        }
    }
    failures
}

fn missing_team_binding_failures(state: &Value, teams: &[&str]) -> Vec<String> {
    teams
        .iter()
        .filter_map(|team| {
            let entry = state.pointer(&format!("/teams/{team}"));
            match entry {
                None => Some(format!("missing state.teams.{team}")),
                Some(entry) => {
                    let mut fields = Vec::new();
                    for field in ["session_name", "agents", "leader_receiver", "team_owner"] {
                        if entry.get(field).is_none() {
                            fields.push(field);
                        }
                    }
                    let epoch = entry
                        .pointer("/team_owner/owner_epoch")
                        .or_else(|| entry.pointer("/leader_receiver/owner_epoch"));
                    if epoch.is_none() {
                        fields.push("owner_epoch");
                    }
                    (!fields.is_empty())
                        .then(|| format!("state.teams.{team} missing fields {fields:?}: {entry}"))
                }
            }
        })
        .collect()
}

fn missing_status_team_binding_failures(status: &Value, teams: &[&str]) -> Vec<String> {
    teams
        .iter()
        .filter_map(|team| {
            let entry = status.pointer(&format!("/teams/{team}"));
            match entry {
                None => Some(format!("status --json missing teams.{team} binding view")),
                Some(entry) if entry.get("leader_receiver").is_none() || entry.get("team_owner").is_none() => {
                    Some(format!("status --json teams.{team} must include leader_receiver and team_owner: {entry}"))
                }
                Some(_) => None,
            }
        })
        .collect()
}

fn team_dir(root: &Path, name: &str, agents: &[(&str, &str)]) -> PathBuf {
    let team = root.join(name);
    std::fs::create_dir_all(team.join("agents")).unwrap();
    std::fs::write(
        team.join("TEAM.md"),
        format!(
            "---\nname: {name}\nobjective: Team-in-team contract fixture.\nprovider: codex\n---\n\n{name} team.\n"
        ),
    )
    .unwrap();
    for (agent_id, role) in agents {
        std::fs::write(
            team.join("agents").join(format!("{agent_id}.md")),
            role_doc(agent_id, role),
        )
        .unwrap();
    }
    let spec = team_agent::compiler::compile_team(&team).unwrap();
    std::fs::write(
        team.join("team.spec.yaml"),
        team_agent::model::yaml::dumps(&spec),
    )
    .unwrap();
    team
}

fn role_doc(name: &str, role: &str) -> String {
    format!(
        "---\nname: {name}\nrole: {role}\nprovider: codex\nmodel: gpt-5.5\nauth_mode: subscription\ntools:\n  - mcp_team\n---\n\n{role}.\n"
    )
}

fn tmp_dir(tag: &str) -> PathBuf {
    static N: AtomicU64 = AtomicU64::new(0);
    let sanitized = tag
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect::<String>();
    let dir = std::env::temp_dir().join(format!(
        "ta-rs-team-in-team-{sanitized}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::canonicalize(dir).unwrap()
}

fn source(rel: &str) -> String {
    composite_source::composite_source(rel)
}

struct EnvGuard {
    key: &'static str,
    previous: Option<String>,
}

impl EnvGuard {
    fn set(key: &'static str, value: &str) -> Self {
        let previous = std::env::var(key).ok();
        unsafe {
            std::env::set_var(key, value);
        }
        Self { key, previous }
    }

    fn remove(key: &'static str) -> Self {
        let previous = std::env::var(key).ok();
        unsafe {
            std::env::remove_var(key);
        }
        Self { key, previous }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        unsafe {
            if let Some(previous) = self.previous.take() {
                std::env::set_var(self.key, previous);
            } else {
                std::env::remove_var(self.key);
            }
        }
    }
}

struct AlwaysLive;

impl team_agent::state::owner_gate::PaneLivenessProbe for AlwaysLive {
    fn liveness(&self, _pane_id: &str) -> PaneLiveness {
        PaneLiveness::Live
    }
}

#[derive(Debug)]
struct RecordedSpawn {
    kind: &'static str,
    session: String,
    window: String,
}

#[derive(Debug, Default)]
struct RecordingTransport {
    spawns: Mutex<Vec<RecordedSpawn>>,
    session_present: bool,
    windows: Vec<WindowName>,
}

impl RecordingTransport {
    fn new() -> Self {
        Self::default()
    }

    fn with_session_present(mut self, present: bool) -> Self {
        self.session_present = present;
        self
    }

    fn with_windows(mut self, windows: Vec<WindowName>) -> Self {
        self.windows = windows;
        self
    }

    fn record_spawn(
        &self,
        kind: &'static str,
        session: &SessionName,
        window: &WindowName,
    ) -> SpawnResult {
        let mut spawns = self.spawns.lock().unwrap();
        spawns.push(RecordedSpawn {
            kind,
            session: session.as_str().to_string(),
            window: window.as_str().to_string(),
        });
        SpawnResult {
            pane_id: PaneId::new(format!("%{}", spawns.len())),
            session: session.clone(),
            window: window.clone(),
            child_pid: None,
        }
    }
}

impl Transport for RecordingTransport {
    fn kind(&self) -> BackendKind {
        BackendKind::Tmux
    }

    fn spawn_first(
        &self,
        session: &SessionName,
        window: &WindowName,
        _argv: &[String],
        _cwd: &Path,
        _env: &BTreeMap<String, String>,
    ) -> Result<SpawnResult, TransportError> {
        Ok(self.record_spawn("spawn_first", session, window))
    }

    fn spawn_into(
        &self,
        session: &SessionName,
        window: &WindowName,
        _argv: &[String],
        _cwd: &Path,
        _env: &BTreeMap<String, String>,
    ) -> Result<SpawnResult, TransportError> {
        Ok(self.record_spawn("spawn_into", session, window))
    }

    fn inject(
        &self,
        _target: &Target,
        _payload: &InjectPayload,
        _submit: Key,
        _bracketed: bool,
    ) -> Result<InjectReport, TransportError> {
        Ok(InjectReport {
            stage_reached: InjectStage::Submit,
            inject_verification: InjectVerification::CaptureContainsToken,
            submit_verification: SubmitVerification::EnterSentWithoutPlaceholderCheck,
            turn_verification: TurnVerification::NotYetObserved,
            attempts: 1,
            submit_diagnostics: None,
        })
    }

    fn send_keys(&self, _target: &Target, _keys: &[Key]) -> Result<(), TransportError> {
        Ok(())
    }

    fn capture(
        &self,
        _target: &Target,
        range: CaptureRange,
    ) -> Result<CapturedText, TransportError> {
        Ok(CapturedText {
            text: String::new(),
            range,
        })
    }

    fn query(&self, _target: &Target, _field: PaneField) -> Result<Option<String>, TransportError> {
        Ok(None)
    }

    fn liveness(&self, _pane: &PaneId) -> Result<PaneLiveness, TransportError> {
        Ok(PaneLiveness::Live)
    }

    fn list_targets(&self) -> Result<Vec<PaneInfo>, TransportError> {
        Ok(Vec::new())
    }

    fn has_session(&self, _session: &SessionName) -> Result<bool, TransportError> {
        Ok(self.session_present)
    }

    fn list_windows(&self, _session: &SessionName) -> Result<Vec<WindowName>, TransportError> {
        Ok(self.windows.clone())
    }

    fn set_session_env(
        &self,
        _session: &SessionName,
        _key: &str,
        _value: &str,
    ) -> Result<SetEnvOutcome, TransportError> {
        Ok(SetEnvOutcome::Applied)
    }

    fn kill_session(&self, _session: &SessionName) -> Result<(), TransportError> {
        Ok(())
    }

    fn kill_window(&self, _target: &Target) -> Result<(), TransportError> {
        Ok(())
    }

    fn attach_session(&self, _session: &SessionName) -> Result<AttachOutcome, TransportError> {
        Ok(AttachOutcome::Attached)
    }
}
