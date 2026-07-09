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
use crate::transport::{
    AttachOutcome, BackendKind, CaptureRange, CapturedText, InjectPayload, InjectReport,
    InjectStage, InjectVerification, Key, PaneField, PaneId, PaneInfo, PaneLiveness, SessionName,
    SetEnvOutcome, SpawnResult, SubmitVerification, Target, Transport, TransportError,
    TurnVerification, WindowName,
};
use serde_json::json;
use std::collections::BTreeSet;
use std::time::Duration;

#[allow(dead_code)]
struct HermeticTestEnv;

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

#[test]
fn breal_spawn_ownership_mismatch_fails_without_state_pollution() {
    let (workspace, _team) = breal_workspace();
    let transport = BRealTransport::misowned();

    let result = crate::lifecycle::reset_agent_with_transport(
        &workspace,
        &aid("w1"),
        true,
        false,
        Some("teamdir"),
        &transport,
    );

    assert!(
        matches!(result, Err(LifecycleError::RequirementUnmet(_))),
        "reset must fail closed when spawn returns another worker's live pane: {result:?}"
    );
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("spawned pane not owned") && err.contains("requested=team-phaseb:w1"),
        "error must name requested/observed ownership; got {err}"
    );
    let state = crate::state::projection::select_runtime_state(&workspace, Some("teamdir"))
        .expect("team state after failed reset");
    let w1 = state.pointer("/agents/w1").expect("w1 state");
    assert_ne!(
        w1.get("pane_id").and_then(serde_json::Value::as_str),
        Some("%5"),
        "failed reset must not write w2's pane into w1 state"
    );
    let started_true = read_events(&workspace).into_iter().any(|event| {
        event.get("event").and_then(serde_json::Value::as_str) == Some("reset_agent.complete")
            && event.get("agent_id").and_then(serde_json::Value::as_str) == Some("w1")
            && event.get("started").and_then(serde_json::Value::as_bool) == Some(true)
    });
    assert!(
        !started_true,
        "failed reset must not emit reset_agent.complete started=true"
    );
}

#[test]
fn breal_provider_missing_window_disappeared_fails_closed() {
    let (workspace, _team) = breal_workspace();
    let transport = BRealTransport::dead_after_spawn();

    let result = crate::lifecycle::reset_agent_with_transport(
        &workspace,
        &aid("w1"),
        true,
        false,
        Some("teamdir"),
        &transport,
    );

    assert!(
        matches!(result, Err(LifecycleError::RequirementUnmet(_))),
        "reset must return Err when provider exits and requested window disappears: {result:?}"
    );
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("window disappeared") || err.contains("spawned pane not owned"),
        "error must name window disappearance / ownership failure; got {err}"
    );
    let state = crate::state::projection::select_runtime_state(&workspace, Some("teamdir"))
        .expect("team state after failed reset");
    let w1 = state.pointer("/agents/w1").expect("w1 state");
    assert_ne!(
        w1.get("status").and_then(serde_json::Value::as_str),
        Some("running"),
        "failed reset must not mark w1 running"
    );
    assert_ne!(
        w1.get("pane_id").and_then(serde_json::Value::as_str),
        Some("%9"),
        "failed reset must not persist disappeared pane"
    );
}

#[test]
fn breal_reset_rehydrates_role_context_from_compiled_spec() {
    let (workspace, _team) = breal_workspace();
    strip_runtime_command_context(&workspace);
    let transport = BRealTransport::owned();

    crate::lifecycle::reset_agent_with_transport(
        &workspace,
        &aid("w1"),
        true,
        false,
        Some("teamdir"),
        &transport,
    )
    .expect("reset should use spec-rehydrated role context");

    let spawns = transport.spawn_records();
    let argv = spawns
        .last()
        .map(|record| record.join("\n"))
        .expect("reset must spawn w1");
    assert!(
        argv.contains("role `Phase B Real Worker One`")
            || argv.contains("role Phase B Real Worker One"),
        "spawn argv must carry role from compiled spec/role doc, got:\n{argv}"
    );
    assert!(
        !argv.contains("role `developer`") && !argv.contains("role developer"),
        "spawn argv must not fall back to generic developer, got:\n{argv}"
    );
}

#[test]
fn breal_restart_rehydrates_role_context_from_compiled_spec() {
    let (workspace, _team) = breal_one_worker_workspace();
    strip_runtime_command_context(&workspace);
    let transport = BRealTransport::owned();

    crate::lifecycle::restart_with_transport(&workspace, true, Some("teamdir"), &transport)
        .expect("restart should use spec-rehydrated role context");

    let spawns = transport.spawn_records();
    let argv = spawns
        .last()
        .map(|record| record.join("\n"))
        .expect("restart must spawn w1");
    assert!(
        argv.contains("role `Phase B Real Worker One`")
            || argv.contains("role Phase B Real Worker One"),
        "restart spawn argv must carry role from compiled spec/role doc, got:\n{argv}"
    );
    assert!(
        !argv.contains("role `developer`") && !argv.contains("role developer"),
        "restart spawn argv must not fall back to generic developer, got:\n{argv}"
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

fn breal_workspace() -> (PathBuf, PathBuf) {
    let w1 = "---\nname: w1\nrole: Phase B Real Worker One\nprovider: codex\nmodel: gpt-5.5\nauth_mode: subscription\ntools:\n  - mcp_team\n---\n\nWorker one body.\n";
    let w2 = "---\nname: w2\nrole: Phase B Real Worker Two\nprovider: codex\nmodel: gpt-5.5\nauth_mode: subscription\ntools:\n  - mcp_team\n---\n\nWorker two body.\n";
    let team = team_dir_with_roles(&[("w1.md", w1), ("w2.md", w2)]);
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
    .expect("quick-start breal fixture");
    (workspace, team)
}

fn breal_one_worker_workspace() -> (PathBuf, PathBuf) {
    let w1 = "---\nname: w1\nrole: Phase B Real Worker One\nprovider: codex\nmodel: gpt-5.5\nauth_mode: subscription\ntools:\n  - mcp_team\n---\n\nWorker one body.\n";
    let team = team_dir_with_roles(&[("w1.md", w1)]);
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
    .expect("quick-start breal one-worker fixture");
    (workspace, team)
}

fn strip_runtime_command_context(workspace: &std::path::Path) {
    let mut state = crate::state::projection::select_runtime_state(workspace, Some("teamdir"))
        .expect("team state");
    if let Some(agent) = state
        .pointer_mut("/agents/w1")
        .and_then(serde_json::Value::as_object_mut)
    {
        for field in [
            "role",
            "tools",
            "system_prompt",
            "output_contract",
            "model",
            "effort",
            "permission_mode",
        ] {
            agent.remove(field);
        }
    }
    crate::state::projection::save_team_scoped_state(workspace, &state)
        .expect("save stripped team state");
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

#[derive(Clone, Copy)]
enum BRealMode {
    Misowned,
    DeadAfterSpawn,
    Owned,
}

struct BRealTransport {
    mode: BRealMode,
    spawns: std::sync::Mutex<Vec<Vec<String>>>,
}

impl BRealTransport {
    fn misowned() -> Self {
        Self {
            mode: BRealMode::Misowned,
            spawns: std::sync::Mutex::new(Vec::new()),
        }
    }

    fn dead_after_spawn() -> Self {
        Self {
            mode: BRealMode::DeadAfterSpawn,
            spawns: std::sync::Mutex::new(Vec::new()),
        }
    }

    fn owned() -> Self {
        Self {
            mode: BRealMode::Owned,
            spawns: std::sync::Mutex::new(Vec::new()),
        }
    }

    fn spawn_records(&self) -> Vec<Vec<String>> {
        self.spawns.lock().unwrap().clone()
    }

    fn spawn_result(
        &self,
        session: &SessionName,
        window: &WindowName,
        argv: &[String],
    ) -> SpawnResult {
        self.spawns.lock().unwrap().push(argv.to_vec());
        let pane = match self.mode {
            BRealMode::Misowned => "%5",
            BRealMode::DeadAfterSpawn => "%9",
            BRealMode::Owned => "%10",
        };
        SpawnResult {
            pane_id: PaneId::new(pane),
            session: session.clone(),
            window: window.clone(),
            child_pid: None,
        }
    }
}

impl Transport for BRealTransport {
    fn kind(&self) -> BackendKind {
        BackendKind::Tmux
    }

    fn spawn_first(
        &self,
        session: &SessionName,
        window: &WindowName,
        argv: &[String],
        _cwd: &std::path::Path,
        _env: &std::collections::BTreeMap<String, String>,
    ) -> Result<SpawnResult, TransportError> {
        Ok(self.spawn_result(session, window, argv))
    }

    fn spawn_into(
        &self,
        session: &SessionName,
        window: &WindowName,
        argv: &[String],
        _cwd: &std::path::Path,
        _env: &std::collections::BTreeMap<String, String>,
    ) -> Result<SpawnResult, TransportError> {
        Ok(self.spawn_result(session, window, argv))
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
            inject_verification: InjectVerification::NoToken,
            submit_verification: SubmitVerification::EnterSentWithoutPlaceholderCheck,
            turn_verification: TurnVerification::NotRequired,
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
            text: "OpenAI Codex".to_string(),
            range,
        })
    }

    fn query(
        &self,
        _target: &Target,
        _field: PaneField,
    ) -> Result<Option<String>, TransportError> {
        Ok(Some("node".to_string()))
    }

    fn liveness(&self, pane: &PaneId) -> Result<PaneLiveness, TransportError> {
        if matches!(self.mode, BRealMode::DeadAfterSpawn) && pane.as_str() == "%9" {
            Ok(PaneLiveness::Dead)
        } else {
            Ok(PaneLiveness::Live)
        }
    }

    fn has_pane(&self, pane: &PaneId) -> Result<Option<bool>, TransportError> {
        Ok(Some(!matches!(
            (self.mode, pane.as_str()),
            (BRealMode::DeadAfterSpawn, "%9")
        )))
    }

    fn list_targets(&self) -> Result<Vec<PaneInfo>, TransportError> {
        let mut targets = Vec::new();
        if matches!(self.mode, BRealMode::Misowned) {
            targets.push(pane_info("team-phaseb", "w2", "%5", 502));
        }
        if matches!(self.mode, BRealMode::Owned) {
            targets.push(pane_info("team-phaseb", "w1", "%10", 501));
        }
        Ok(targets)
    }

    fn has_session(&self, _session: &SessionName) -> Result<bool, TransportError> {
        Ok(true)
    }

    fn list_windows(&self, _session: &SessionName) -> Result<Vec<WindowName>, TransportError> {
        Ok(vec![WindowName::new("w1"), WindowName::new("w2")])
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

    fn kill_pane(&self, _pane: &PaneId) -> Result<(), TransportError> {
        Ok(())
    }

    fn attach_session(&self, _session: &SessionName) -> Result<AttachOutcome, TransportError> {
        Ok(AttachOutcome::Attached)
    }
}

fn pane_info(session: &str, window: &str, pane: &str, pid: u32) -> PaneInfo {
    PaneInfo {
        pane_id: PaneId::new(pane),
        session: SessionName::new(session),
        window_index: None,
        window_name: Some(WindowName::new(window)),
        pane_index: None,
        tty: None,
        current_command: Some("node".to_string()),
        current_path: None,
        active: false,
        pane_pid: Some(pid),
        leader_env: std::collections::BTreeMap::new(),
    }
}
