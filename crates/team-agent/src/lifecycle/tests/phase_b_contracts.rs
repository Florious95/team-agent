use super::agent_ops::lanea_team_ws;
use super::lane_ops::{fork_ws, LaneTransport};
use super::launch_spawn::{
    quick_start_team_dir, seed_healthy_coordinator, DELEG_ROLE_ALPHA, QS_VALID_ROLE,
};
use super::*;
use crate::lifecycle::lock::{
    acquire_agent_lifecycle_lock_for_test, override_agent_lifecycle_lock_deadlines_for_test,
    LifecycleLockRequest,
};
use crate::transport::test_support::OfflineTransport;
use crate::transport::WindowName;
use serde_json::json;
use std::collections::BTreeSet;
use std::time::Duration;

#[test]
fn concurrent_reset_discard_session_serializes() {
    let ids = (1..=6).map(|n| format!("w{n}")).collect::<Vec<_>>();
    let role_docs = ids
        .iter()
        .map(|id| (format!("{id}.md"), role_doc(id)))
        .collect::<Vec<_>>();
    let role_refs = role_docs
        .iter()
        .map(|(file, doc)| (file.as_str(), doc.as_str()))
        .collect::<Vec<_>>();
    let team = team_dir_with_roles(&role_refs);
    let workspace = team.parent().expect("team workspace").to_path_buf();
    seed_healthy_coordinator(&workspace);

    let launch_transport = codex_ready_transport();
    let quick_start = quick_start_with_transport_in_workspace_with_display(
        &workspace,
        &team,
        None,
        true,
        None,
        &launch_transport,
        false,
    )
    .expect("quick-start fixture");
    assert!(
        matches!(quick_start, QuickStartReport::Ready { .. }),
        "fixture must launch: {quick_start:?}"
    );

    let reset_transport = codex_ready_transport()
        .with_session_present(true)
        .with_windows(ids.iter().map(|id| WindowName::new(id.as_str())).collect());
    let outcomes = ids
        .iter()
        .map(|id| {
            crate::lifecycle::reset_agent_with_transport(
                &workspace,
                &aid(id),
                true,
                false,
                Some("teamdir"),
                &reset_transport,
            )
        })
        .collect::<Vec<_>>();
    for (id, outcome) in ids.iter().zip(outcomes.iter()) {
        assert!(
            matches!(outcome, Ok(ResetAgentOutcome::Reset { .. })),
            "reset-agent --discard-session must complete for {id}: {outcome:?}"
        );
    }

    let state = crate::state::projection::select_runtime_state(&workspace, Some("teamdir"))
        .expect("team state");
    let agents = state
        .get("agents")
        .and_then(serde_json::Value::as_object)
        .expect("agents map");
    assert_eq!(agents.len(), ids.len(), "one roster row per worker");
    let mut windows = BTreeSet::new();
    let mut panes = BTreeSet::new();
    for id in &ids {
        let agent = agents.get(id).unwrap_or_else(|| panic!("missing {id}"));
        assert_eq!(
            agent.get("status").and_then(|v| v.as_str()),
            Some("running")
        );
        let window = agent
            .get("window")
            .and_then(|v| v.as_str())
            .unwrap_or_else(|| panic!("{id} missing window"));
        assert!(
            windows.insert(window.to_string()),
            "duplicate window {window}"
        );
        let pane = agent
            .get("pane_id")
            .and_then(|v| v.as_str())
            .unwrap_or_else(|| panic!("{id} missing pane_id"));
        assert!(panes.insert(pane.to_string()), "duplicate pane_id {pane}");
    }

    let reset_events = read_events(&workspace)
        .into_iter()
        .filter(|event| {
            event.get("event").and_then(serde_json::Value::as_str) == Some("reset_agent.complete")
        })
        .collect::<Vec<_>>();
    assert_eq!(
        reset_events.len(),
        ids.len(),
        "each reset must emit reset_agent.complete"
    );
    let completed = reset_events
        .iter()
        .filter_map(|event| event.get("agent_id").and_then(serde_json::Value::as_str))
        .collect::<BTreeSet<_>>();
    let expected = ids.iter().map(String::as_str).collect::<BTreeSet<_>>();
    assert_eq!(
        completed, expected,
        "complete events must cover all workers"
    );
}

#[test]
fn add_fork_remove_share_lifecycle_lock_behavior() {
    let (add_team, add_workspace, add_role, add_transport) = add_fixture();
    let add_agent = aid("w2");
    let add_state_before =
        crate::state::projection::select_runtime_state(&add_workspace, Some("teamdir")).unwrap();
    let add_spec_before = std::fs::read_to_string(crate::model::paths::runtime_spec_path(
        &add_workspace,
        "teamdir",
    ))
    .unwrap();
    let held = hold_lifecycle_lock(&add_workspace, "add-agent", Some(&add_agent));
    let override_guard = override_agent_lifecycle_lock_deadlines_for_test(
        Duration::from_millis(120),
        Duration::from_secs(5),
    );
    assert_lock_timeout(
        crate::lifecycle::add_agent_with_transport(
            &add_team,
            &add_agent,
            &add_role,
            false,
            None,
            &add_transport,
        ),
        "add-agent",
    );
    assert!(
        add_transport.spawn_records().is_empty(),
        "blocked add-agent must not spawn a window"
    );
    assert_eq!(
        crate::state::projection::select_runtime_state(&add_workspace, Some("teamdir")).unwrap(),
        add_state_before,
        "blocked add-agent must not add a state row"
    );
    assert_eq!(
        std::fs::read_to_string(crate::model::paths::runtime_spec_path(
            &add_workspace,
            "teamdir",
        ))
        .unwrap(),
        add_spec_before,
        "blocked add-agent must not mutate spec"
    );
    drop(override_guard);
    drop(held);
    crate::lifecycle::add_agent_with_transport(
        &add_team,
        &add_agent,
        &add_role,
        false,
        None,
        &add_transport,
    )
    .expect("add-agent succeeds after lock release");
    assert!(
        crate::state::projection::select_runtime_state(&add_workspace, Some("teamdir"))
            .unwrap()
            .pointer("/agents/w2")
            .is_some(),
        "released add-agent must add the worker"
    );

    let fork_workspace = fork_ws(DELEG_ROLE_ALPHA);
    let fork_transport = LaneTransport::new("team-laneateam", &[]);
    let fork_spec_before = std::fs::read_to_string(fork_workspace.join("team.spec.yaml")).unwrap();
    let fork_state_before = crate::state::persist::load_runtime_state(&fork_workspace).unwrap();
    let fork_agent = aid("newfork");
    let held = hold_lifecycle_lock(&fork_workspace, "fork-agent", Some(&fork_agent));
    let override_guard = override_agent_lifecycle_lock_deadlines_for_test(
        Duration::from_millis(120),
        Duration::from_secs(5),
    );
    assert_lock_timeout(
        crate::lifecycle::fork_agent_with_transport(
            &fork_workspace,
            &aid("alpha"),
            &fork_agent,
            None,
            false,
            None,
            &fork_transport,
        ),
        "fork-agent",
    );
    assert!(
        fork_transport.spawns().is_empty(),
        "blocked fork-agent must not spawn a window"
    );
    assert_eq!(
        crate::state::persist::load_runtime_state(&fork_workspace).unwrap(),
        fork_state_before,
        "blocked fork-agent must not add a state row"
    );
    assert_eq!(
        std::fs::read_to_string(fork_workspace.join("team.spec.yaml")).unwrap(),
        fork_spec_before,
        "blocked fork-agent must not mutate spec"
    );
    drop(override_guard);
    drop(held);
    crate::lifecycle::fork_agent_with_transport(
        &fork_workspace,
        &aid("alpha"),
        &fork_agent,
        None,
        false,
        None,
        &fork_transport,
    )
    .expect("fork-agent succeeds after lock release");
    assert!(
        crate::state::persist::load_runtime_state(&fork_workspace)
            .unwrap()
            .pointer("/agents/newfork")
            .is_some(),
        "released fork-agent must add the fork row"
    );

    let remove_workspace = lanea_team_ws("stopped");
    let remove_transport = LaneTransport::new("team-laneateam", &[]);
    let remove_spec_before =
        std::fs::read_to_string(remove_workspace.join("team.spec.yaml")).unwrap();
    let remove_state_before = crate::state::persist::load_runtime_state(&remove_workspace).unwrap();
    let remove_agent = aid("alpha");
    let held = hold_lifecycle_lock(&remove_workspace, "remove-agent", Some(&remove_agent));
    let override_guard = override_agent_lifecycle_lock_deadlines_for_test(
        Duration::from_millis(120),
        Duration::from_secs(5),
    );
    assert_lock_timeout(
        crate::lifecycle::remove_agent_with_transport(
            &remove_workspace,
            &remove_agent,
            true,
            true,
            None,
            &remove_transport,
        ),
        "remove-agent",
    );
    assert!(
        remove_transport.killed().is_empty() && remove_transport.spawns().is_empty(),
        "blocked remove-agent must not touch tmux"
    );
    assert_eq!(
        crate::state::persist::load_runtime_state(&remove_workspace).unwrap(),
        remove_state_before,
        "blocked remove-agent must not remove a state row"
    );
    assert_eq!(
        std::fs::read_to_string(remove_workspace.join("team.spec.yaml")).unwrap(),
        remove_spec_before,
        "blocked remove-agent must not mutate spec"
    );
    drop(override_guard);
    drop(held);
    assert!(
        matches!(
            crate::lifecycle::remove_agent_with_transport(
                &remove_workspace,
                &remove_agent,
                true,
                true,
                None,
                &remove_transport,
            ),
            Ok(RemoveAgentOutcome::Removed { .. })
        ),
        "remove-agent succeeds after lock release"
    );
    assert!(
        crate::state::persist::load_runtime_state(&remove_workspace)
            .unwrap()
            .pointer("/agents/alpha")
            .is_none(),
        "released remove-agent must remove alpha"
    );
}

fn team_dir_with_roles(role_docs: &[(&str, &str)]) -> PathBuf {
    let team = temp_ws().join("teamdir");
    std::fs::create_dir_all(team.join("agents")).unwrap();
    std::fs::write(
        team.join("TEAM.md"),
        "---\nname: phaseb\nobjective: Phase B.\nprovider: codex\n---\n\nPhase B team.\n",
    )
    .unwrap();
    for (file, role_doc) in role_docs {
        std::fs::write(team.join("agents").join(file), role_doc).unwrap();
    }
    team
}

fn role_doc(id: &str) -> String {
    format!(
        "---\nname: {id}\nrole: {id} Worker\nprovider: codex\nmodel: gpt-5.5\nauth_mode: subscription\ntools:\n  - mcp_team\n---\n\n{id} worker.\n"
    )
}

fn add_fixture() -> (PathBuf, PathBuf, PathBuf, OfflineTransport) {
    let team = quick_start_team_dir(QS_VALID_ROLE);
    let workspace = team.parent().expect("team workspace").to_path_buf();
    seed_healthy_coordinator(&workspace);
    let launch_transport = codex_ready_transport();
    quick_start_with_transport_in_workspace_with_display(
        &workspace,
        &team,
        None,
        true,
        None,
        &launch_transport,
        false,
    )
    .expect("quick-start add fixture");
    let role = workspace.join("w2-role.md");
    std::fs::write(&role, role_doc("w2")).unwrap();
    (
        team,
        workspace,
        role,
        codex_ready_transport().with_session_present(true),
    )
}

fn codex_ready_transport() -> OfflineTransport {
    let mut transport = OfflineTransport::new();
    for pane in 0..32 {
        transport = transport.with_capture_for_pane(format!("%{pane}"), "OpenAI Codex");
    }
    transport
}

fn hold_lifecycle_lock(
    workspace: &std::path::Path,
    operation: &'static str,
    agent_id: Option<&AgentId>,
) -> crate::lifecycle::lock::LifecycleLockGuard {
    acquire_agent_lifecycle_lock_for_test(
        LifecycleLockRequest {
            workspace,
            operation,
            team: None,
            agent_id,
        },
        Duration::from_millis(200),
        Duration::from_secs(5),
    )
    .expect("hold lifecycle lock")
}

fn assert_lock_timeout<T: std::fmt::Debug>(
    result: Result<T, LifecycleError>,
    expected_operation: &str,
) {
    assert!(
        matches!(
            result,
            Err(LifecycleError::LifecycleLockTimeout { ref operation, .. })
                if operation == expected_operation
        ),
        "expected {expected_operation} lifecycle lock timeout, got {result:?}"
    );
}

fn read_events(workspace: &std::path::Path) -> Vec<serde_json::Value> {
    let path = crate::model::paths::logs_dir(workspace).join("events.jsonl");
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}
