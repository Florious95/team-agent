use super::launch_spawn::{seed_healthy_coordinator, DELEG_ROLE_ALPHA, DELEG_ROLE_BRAVO};
use super::*;

// ═════════════════════════════════════════════════════════════════════════

const DELEG_ROLE_ALPHA_COMPAT: &str = "---\nname: alpha\nrole: Alpha Worker\nprovider: codex\nmodel: gpt-5.5\nauth_mode: compatible_api\nprofile: alpha-compat\ntools:\n  - mcp_team\n---\n\nAlpha.\n";

type LaneKills = std::sync::Arc<std::sync::Mutex<Vec<String>>>;
pub(super) type LaneSpawns = std::sync::Arc<std::sync::Mutex<Vec<(String, Vec<String>)>>>;

/// Recording transport for Lane-A v2: `list_windows`/`list_targets` answer from a configurable window
/// set (golden's `_tmux_window_exists` primitive = `tmux list-windows`); `kill_window` + spawn_first/into
/// are RECORDED. Every other method returns a benign Ok (never panics) so stop/reset/remove/fork run
/// end-to-end in-process.
pub(super) struct LaneTransport {
    session: String,
    windows: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    foreign: std::sync::Arc<std::sync::Mutex<Vec<(String, String)>>>,
    killed: LaneKills,
    spawns: LaneSpawns,
}
impl LaneTransport {
    pub(super) fn new(session: &str, windows: &[&str]) -> Self {
        Self {
            session: session.to_string(),
            windows: std::sync::Arc::new(std::sync::Mutex::new(
                windows.iter().map(|w| (*w).to_string()).collect(),
            )),
            foreign: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
            killed: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
            spawns: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
        }
    }
    pub(super) fn killed(&self) -> Vec<String> {
        self.killed.lock().unwrap().clone()
    }
    pub(super) fn spawns(&self) -> Vec<(String, Vec<String>)> {
        self.spawns.lock().unwrap().clone()
    }
    fn add_foreign(&self, session: &str, window: &str) {
        self.foreign
            .lock()
            .unwrap()
            .push((session.to_string(), window.to_string()));
    }
}
impl crate::transport::Transport for LaneTransport {
    fn kind(&self) -> crate::transport::BackendKind {
        crate::transport::BackendKind::Tmux
    }
    fn spawn_first(
        &self,
        session: &crate::transport::SessionName,
        window: &crate::transport::WindowName,
        argv: &[String],
        _c: &std::path::Path,
        _e: &std::collections::BTreeMap<String, String>,
    ) -> Result<crate::transport::SpawnResult, crate::transport::TransportError> {
        self.spawns
            .lock()
            .unwrap()
            .push(("spawn_first".to_string(), argv.to_vec()));
        if !self
            .windows
            .lock()
            .unwrap()
            .iter()
            .any(|name| name == window.as_str())
        {
            self.windows
                .lock()
                .unwrap()
                .push(window.as_str().to_string());
        }
        Ok(crate::transport::SpawnResult {
            pane_id: crate::transport::PaneId::new(format!("%{}", window.as_str())),
            session: session.clone(),
            window: window.clone(),
            child_pid: None,
        })
    }
    fn spawn_into(
        &self,
        session: &crate::transport::SessionName,
        window: &crate::transport::WindowName,
        argv: &[String],
        _c: &std::path::Path,
        _e: &std::collections::BTreeMap<String, String>,
    ) -> Result<crate::transport::SpawnResult, crate::transport::TransportError> {
        self.spawns
            .lock()
            .unwrap()
            .push(("spawn_into".to_string(), argv.to_vec()));
        if !self
            .windows
            .lock()
            .unwrap()
            .iter()
            .any(|name| name == window.as_str())
        {
            self.windows
                .lock()
                .unwrap()
                .push(window.as_str().to_string());
        }
        Ok(crate::transport::SpawnResult {
            pane_id: crate::transport::PaneId::new(format!("%{}", window.as_str())),
            session: session.clone(),
            window: window.clone(),
            child_pid: None,
        })
    }
    fn inject(
        &self,
        _t: &crate::transport::Target,
        _p: &crate::transport::InjectPayload,
        _s: crate::transport::Key,
        _b: bool,
    ) -> Result<crate::transport::InjectReport, crate::transport::TransportError> {
        unimplemented!("LaneTransport::inject not reached by stop/reset/remove/fork")
    }
    fn send_keys(
        &self,
        _t: &crate::transport::Target,
        _k: &[crate::transport::Key],
    ) -> Result<(), crate::transport::TransportError> {
        Ok(())
    }
    fn capture(
        &self,
        _t: &crate::transport::Target,
        r: crate::transport::CaptureRange,
    ) -> Result<crate::transport::CapturedText, crate::transport::TransportError> {
        Ok(crate::transport::CapturedText {
            text: String::new(),
            range: r,
        })
    }
    fn query(
        &self,
        _t: &crate::transport::Target,
        _f: crate::transport::PaneField,
    ) -> Result<Option<String>, crate::transport::TransportError> {
        Ok(None)
    }
    fn liveness(
        &self,
        _p: &crate::transport::PaneId,
    ) -> Result<crate::model::enums::PaneLiveness, crate::transport::TransportError> {
        Ok(crate::model::enums::PaneLiveness::Unknown)
    }
    fn list_targets(
        &self,
    ) -> Result<Vec<crate::transport::PaneInfo>, crate::transport::TransportError> {
        let mut panes: Vec<_> = self
            .windows
            .lock()
            .unwrap()
            .iter()
            .map(|w| crate::transport::PaneInfo {
                pane_id: crate::transport::PaneId::new(format!("%{w}")),
                session: crate::transport::SessionName::new(&self.session),
                window_index: None,
                window_name: Some(crate::transport::WindowName::new(w)),
                pane_index: None,
                tty: None,
                current_command: None,
                current_path: None,
                active: false,
                pane_pid: None,
                leader_env: std::collections::BTreeMap::new(),
            })
            .collect();
        panes.extend(
            self.foreign
                .lock()
                .unwrap()
                .iter()
                .map(|(session, window)| crate::transport::PaneInfo {
                    pane_id: crate::transport::PaneId::new(format!("%{session}-{window}")),
                    session: crate::transport::SessionName::new(session),
                    window_index: None,
                    window_name: Some(crate::transport::WindowName::new(window)),
                    pane_index: None,
                    tty: None,
                    current_command: None,
                    current_path: None,
                    active: false,
                    pane_pid: None,
                    leader_env: std::collections::BTreeMap::new(),
                }),
        );
        Ok(panes)
    }
    fn has_session(
        &self,
        _s: &crate::transport::SessionName,
    ) -> Result<bool, crate::transport::TransportError> {
        Ok(true)
    }
    fn list_windows(
        &self,
        s: &crate::transport::SessionName,
    ) -> Result<Vec<crate::transport::WindowName>, crate::transport::TransportError> {
        if s.as_str() == self.session {
            Ok(self
                .windows
                .lock()
                .unwrap()
                .iter()
                .map(|w| crate::transport::WindowName::new(w.as_str()))
                .collect())
        } else {
            Ok(Vec::new())
        }
    }
    fn set_session_env(
        &self,
        _s: &crate::transport::SessionName,
        _k: &str,
        _v: &str,
    ) -> Result<crate::transport::SetEnvOutcome, crate::transport::TransportError> {
        Ok(crate::transport::SetEnvOutcome::Applied)
    }
    fn kill_session(
        &self,
        _s: &crate::transport::SessionName,
    ) -> Result<(), crate::transport::TransportError> {
        Ok(())
    }
    fn kill_window(
        &self,
        t: &crate::transport::Target,
    ) -> Result<(), crate::transport::TransportError> {
        let (name, window) = match t {
            crate::transport::Target::Pane(p) => (p.as_str().to_string(), None),
            crate::transport::Target::SessionWindow { session, window } => (
                format!("{}:{}", session.as_str(), window.as_str()),
                Some(window.as_str()),
            ),
        };
        self.killed.lock().unwrap().push(name);
        if let crate::transport::Target::Pane(pane) = t {
            if let Some(window) = pane.as_str().strip_prefix('%') {
                self.windows
                    .lock()
                    .unwrap()
                    .retain(|candidate| candidate != window);
                self.foreign
                    .lock()
                    .unwrap()
                    .retain(|(session, candidate)| format!("{session}-{candidate}") != window);
            }
        } else if let Some(window) = window {
            self.windows
                .lock()
                .unwrap()
                .retain(|candidate| candidate != window);
        }
        Ok(())
    }
    fn attach_session(
        &self,
        _s: &crate::transport::SessionName,
    ) -> Result<crate::transport::AttachOutcome, crate::transport::TransportError> {
        Ok(crate::transport::AttachOutcome::Attached)
    }
}

/// 2-agent (alpha, bravo) compiled spec + custom `state.agents` map. session_name = "team-laneateam"
/// (the compiled runtime.session_name). ensure_owner_allowed passes (no team_owner).
fn lanea_ws_agents(agents: serde_json::Value) -> PathBuf {
    lanea_ws_agents_at(&temp_ws(), agents)
}

fn lanea_ws_agents_at(root: &std::path::Path, agents: serde_json::Value) -> PathBuf {
    let ws = root.join("laneav2");
    std::fs::create_dir_all(ws.join("agents")).unwrap();
    std::fs::write(
        ws.join("TEAM.md"),
        "---\nname: laneateam\nobjective: Lane A v2 probe.\nprovider: codex\n---\n\nteam.\n",
    )
    .unwrap();
    std::fs::write(ws.join("agents").join("alpha.md"), DELEG_ROLE_ALPHA).unwrap();
    std::fs::write(ws.join("agents").join("bravo.md"), DELEG_ROLE_BRAVO).unwrap();
    let spec = crate::compiler::compile_team(&ws).expect("compile lane-A v2 team");
    // Make `alpha` REMOVABLE (golden-valid): compile_team auto-wires EVERY agent into routing/tasks
    // (default_assignee + route-<id>.assign_to + task.assignee), but golden remove_agent removes only from
    // agents + startup_order then validate_spec RAISES on dangling refs (agents.py:94 / spec.py:341-346) —
    // so a routed agent is NOT removable in golden. Re-point the *validated* refs at the STAYING agent
    // `bravo`, so removing `alpha` (an unrouted, dynamic-style worker — the real fork->remove case) passes
    // validate. (match-block `assignee` lists are not validated, so they may keep referencing alpha.)
    let yaml = crate::model::yaml::dumps(&spec)
        .replace("default_assignee: \"alpha\"", "default_assignee: \"bravo\"")
        .replace("assign_to: \"alpha\"", "assign_to: \"bravo\"")
        .replace("assignee: \"alpha\"", "assignee: \"bravo\"");
    assert!(
        !yaml.contains("default_assignee: \"alpha\""),
        "fixture unroute: default_assignee still alpha"
    );
    assert!(
        !yaml.contains("assign_to: \"alpha\""),
        "fixture unroute: a routing rule still assign_to alpha"
    );
    assert!(
        !yaml.contains("assignee: \"alpha\""),
        "fixture unroute: task still assignee alpha"
    );
    std::fs::write(ws.join("team.spec.yaml"), yaml).unwrap();
    crate::state::persist::save_runtime_state(
        &ws,
        &json!({ "session_name": "team-laneateam", "agents": agents }),
    )
    .unwrap();
    ws
}

/// SINGLE-worker compiled spec (alpha only). Removing alpha leaves `agents: []` -> validate_spec FAILS
/// ("/agents: must be a non-empty list", spec.rs:273) -> a deterministic IN-TRY mid-remove failure that
/// drives the rollback path (golden agents.py:110 except).
fn lanea_one_agent_ws(alpha_status: &str) -> PathBuf {
    let ws = temp_ws().join("lanea1");
    std::fs::create_dir_all(ws.join("agents")).unwrap();
    std::fs::write(
        ws.join("TEAM.md"),
        "---\nname: laneateam\nobjective: Lane A one-agent probe.\nprovider: codex\n---\n\nteam.\n",
    )
    .unwrap();
    std::fs::write(ws.join("agents").join("alpha.md"), DELEG_ROLE_ALPHA).unwrap();
    let spec = crate::compiler::compile_team(&ws).expect("compile 1-agent team");
    std::fs::write(ws.join("team.spec.yaml"), crate::model::yaml::dumps(&spec)).unwrap();
    crate::state::persist::save_runtime_state(&ws, &json!({ "session_name": "team-laneateam", "agents": { "alpha": { "status": alpha_status, "provider": "codex", "window": "alpha" } } })).unwrap();
    ws
}

fn selected_spec_path(ws: &std::path::Path) -> std::path::PathBuf {
    crate::state::selector::resolve_active_team(
        ws,
        None,
        crate::state::selector::SelectorMode::RequireSpec,
    )
    .unwrap()
    .spec_workspace
    .unwrap_or_else(|| ws.to_path_buf())
    .join("team.spec.yaml")
}

fn remove_agent_from_fixture_spec(ws: &std::path::Path, agent_id: &str) {
    let path = selected_spec_path(ws);
    let spec = crate::model::yaml::loads(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let removed = crate::lifecycle::restart::remove::spec_without_agent(&spec, &aid(agent_id));
    std::fs::write(path, crate::model::yaml::dumps(&removed)).unwrap();
}

fn remove_agent_from_fixture_state(ws: &std::path::Path, agent_id: &str) {
    let mut state = crate::state::persist::load_runtime_state(ws).unwrap();
    state
        .get_mut("agents")
        .and_then(serde_json::Value::as_object_mut)
        .unwrap()
        .remove(agent_id);
    crate::state::persist::save_runtime_state_with_deleted_agents(ws, &state, &[agent_id]).unwrap();
}

#[test]
fn force_remove_reconciles_all_seat_quadrants_without_cross_team_same_name_kill() {
    for quadrant in ["state-only", "spec-only", "physical-only", "mixed"] {
        let ws = lanea_ws_agents(json!({
            "alpha": { "status": "stopped", "provider": "codex", "window": "alpha" },
            "bravo": { "status": "running", "provider": "codex", "window": "bravo" }
        }));
        let local_physical = matches!(quadrant, "physical-only");
        let tx = LaneTransport::new(
            "team-laneateam",
            if local_physical { &["alpha"][..] } else { &[] },
        );
        tx.add_foreign("team-other", "alpha");
        match quadrant {
            "state-only" => remove_agent_from_fixture_spec(&ws, "alpha"),
            "spec-only" => remove_agent_from_fixture_state(&ws, "alpha"),
            "physical-only" => {
                remove_agent_from_fixture_spec(&ws, "alpha");
                remove_agent_from_fixture_state(&ws, "alpha");
            }
            "mixed" => {}
            _ => unreachable!(),
        }

        for attempt in 0..2 {
            let result = remove_agent_with_transport(&ws, &aid("alpha"), true, true, None, &tx);
            assert!(
                matches!(result, Ok(RemoveAgentOutcome::Removed { .. })),
                "{quadrant} force-remove attempt {attempt} must be idempotent: {result:?}"
            );
        }
        let state = crate::state::persist::load_runtime_state(&ws).unwrap();
        assert!(
            state["agents"].get("alpha").is_none(),
            "{quadrant}: state absent"
        );
        let spec =
            crate::model::yaml::loads(&std::fs::read_to_string(selected_spec_path(&ws)).unwrap())
                .unwrap();
        assert!(
            spec.get("agents")
                .and_then(crate::model::yaml::Value::as_list)
                .is_none_or(|agents| agents.iter().all(|agent| {
                    agent.get("id").and_then(crate::model::yaml::Value::as_str) != Some("alpha")
                })),
            "{quadrant}: spec absent"
        );
        let live = crate::transport::Transport::list_targets(&tx).unwrap();
        assert!(
            live.iter()
                .all(|pane| pane.session.as_str() != "team-laneateam"
                    || pane
                        .window_name
                        .as_ref()
                        .is_none_or(|window| window.as_str() != "alpha")),
            "{quadrant}: selected team physical absent"
        );
        assert!(
            live.iter().any(|pane| pane.session.as_str() == "team-other"
                && pane
                    .window_name
                    .as_ref()
                    .is_some_and(|window| window.as_str() == "alpha")),
            "{quadrant}: cross-team same-name pane must survive"
        );
        assert!(
            !tx.killed().iter().any(|pane| pane == "%team-other-alpha"),
            "{quadrant}: cross-team pane must never be selected"
        );
    }
}

#[test]
fn remove_persists_retired_tombstone_and_explicit_readd_clears_it() {
    let ws = lanea_ws_agents(json!({
        "alpha": { "status": "stopped", "provider": "codex", "window": "alpha" },
        "bravo": { "status": "running", "provider": "codex", "window": "bravo" }
    }));
    let mut seeded = crate::state::persist::load_runtime_state(&ws).unwrap();
    seeded["team_key"] = json!("laneateam");
    seeded["active_team_key"] = json!("laneateam");
    let mut nested = crate::state::projection::compact_team_state(&seeded);
    nested["status"] = json!("alive");
    seeded["teams"] = json!({
        "laneateam": nested,
        "sibling": {
            "team_key": "sibling",
            "status": "shutdown",
            "agents": {},
            "agent_lifecycle": {
                "alpha": {
                    "state": "retired",
                    "changed_at": "2026-07-20T00:00:00Z",
                    "reason": "sibling retirement"
                }
            }
        }
    });
    crate::state::persist::save_runtime_state(&ws, &seeded).unwrap();
    let stale_tick = crate::state::projection::select_runtime_state(&ws, Some("laneateam"))
        .expect("capture the coordinator projection from before remove");
    let tx = LaneTransport::new("team-laneateam", &[]);
    let role = ws.join("agents").join("alpha.md");

    remove_agent_with_transport(&ws, &aid("alpha"), true, true, None, &tx).unwrap();
    let removed: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(crate::state::persist::runtime_state_path(&ws)).unwrap(),
    )
    .unwrap();
    assert_eq!(
        removed["teams"]["laneateam"]["agent_lifecycle"]["alpha"]["state"],
        "retired",
        "the first successful remove must directly persist the canonical tombstone"
    );
    assert!(
        removed["teams"]["laneateam"]["agents"]
            .get("alpha")
            .is_none(),
        "the first successful remove must directly delete the canonical agent row"
    );
    crate::state::repository::StateRepository::new(&ws)
        .save(
            crate::state::repository::StateWriteIntent::CoordinatorTick {
                team_key: "laneateam",
            },
            &stale_tick,
        )
        .unwrap();
    let after_stale_tick: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(crate::state::persist::runtime_state_path(&ws)).unwrap(),
    )
    .unwrap();
    assert_eq!(
        after_stale_tick["teams"]["laneateam"]["agent_lifecycle"]["alpha"]["state"],
        "retired",
        "a stale coordinator save must not erase the first remove's tombstone"
    );
    assert!(
        after_stale_tick["teams"]["laneateam"]["agents"]
            .get("alpha")
            .is_none(),
        "a stale coordinator save must not resurrect the first removed agent"
    );

    crate::lifecycle::add_agent_with_transport(&ws, &aid("alpha"), &role, false, None, &tx)
        .unwrap();
    let readded = crate::state::persist::load_runtime_state(&ws).unwrap();
    assert!(
        readded["teams"]["laneateam"]["agent_lifecycle"]
            .get("alpha")
            .is_none(),
        "explicit re-add must consume the retired tombstone"
    );
    assert!(readded["teams"]["laneateam"]["agents"]
        .get("alpha")
        .is_some());
    assert_eq!(
        readded["teams"]["sibling"]["agent_lifecycle"]["alpha"]["state"],
        "retired",
        "selected-team re-add must not clear a sibling team's same-id retirement"
    );
}

const CHARLIE_ROLE: &str = "---\nname: charlie\nrole: Charlie Worker\nprovider: codex\nmodel: gpt-5.5\nauth_mode: subscription\ntools:\n  - mcp_team\n---\n\nCharlie.\n";

struct HermeticTestEnv {
    root: std::path::PathBuf,
}

impl HermeticTestEnv {
    fn enter(tag: &str) -> Self {
        let root = temp_ws().join(format!("hermetic-{tag}"));
        std::fs::create_dir_all(&root).unwrap();
        Self { root }
    }

    fn workspace_root(&self) -> &std::path::Path {
        &self.root
    }
}

impl Drop for HermeticTestEnv {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

struct ForceRecreateFailureGuard(Option<String>);

impl ForceRecreateFailureGuard {
    fn after_spawn(reason: &str) -> Self {
        const KEY: &str = "TEAM_AGENT_TEST_FAIL_FORCE_RECREATE_AFTER_SPAWN";
        let previous = std::env::var(KEY).ok();
        unsafe { std::env::set_var(KEY, reason) };
        Self(previous)
    }
}

impl Drop for ForceRecreateFailureGuard {
    fn drop(&mut self) {
        const KEY: &str = "TEAM_AGENT_TEST_FAIL_FORCE_RECREATE_AFTER_SPAWN";
        unsafe {
            if let Some(previous) = self.0.take() {
                std::env::set_var(KEY, previous);
            } else {
                std::env::remove_var(KEY);
            }
        }
    }
}

fn seed_agent_health(ws: &std::path::Path, team: &str, agent: &str, status: &str) {
    let store = crate::message_store::MessageStore::open(ws).unwrap();
    let conn = crate::db::schema::open_db(store.db_path()).unwrap();
    conn.execute(
        "insert into agent_health(owner_team_id, agent_id, status, last_output_at, context_usage_pct, current_task_id, updated_at) values (?1, ?2, ?3, 'before-output', 37, 'task-before', '2026-07-19T00:00:00Z')",
        rusqlite::params![team, agent, status],
    )
    .unwrap();
}

fn seed_charlie(ws: &std::path::Path, tx: &LaneTransport) -> std::path::PathBuf {
    let role_dir = ws.join("role-masters");
    std::fs::create_dir_all(&role_dir).unwrap();
    let role = role_dir.join("charlie.md");
    std::fs::write(&role, CHARLIE_ROLE).unwrap();
    crate::lifecycle::add_agent_with_transport(ws, &aid("charlie"), &role, false, None, tx)
        .unwrap();
    role
}

#[test]
#[serial_test::serial(env)]
fn force_recreate_is_one_lock_transaction_for_all_seat_quadrants_and_rolls_back_failure() {
    for quadrant in ["state-only", "spec-only", "physical-only", "mixed"] {
        let ws = lanea_ws_agents(json!({
            "alpha": { "status": "stopped", "provider": "codex", "window": "alpha" },
            "bravo": { "status": "running", "provider": "codex", "window": "bravo" }
        }));
        let tx = LaneTransport::new("team-laneateam", &[]);
        seed_healthy_coordinator(&ws);
        let role = seed_charlie(&ws, &tx);
        match quadrant {
            "state-only" => {
                remove_agent_from_fixture_spec(&ws, "charlie");
                crate::transport::Transport::kill_pane(
                    &tx,
                    &crate::transport::PaneId::new("%charlie"),
                )
                .unwrap();
            }
            "spec-only" => {
                remove_agent_from_fixture_state(&ws, "charlie");
                crate::transport::Transport::kill_pane(
                    &tx,
                    &crate::transport::PaneId::new("%charlie"),
                )
                .unwrap();
            }
            "physical-only" => {
                remove_agent_from_fixture_spec(&ws, "charlie");
                remove_agent_from_fixture_state(&ws, "charlie");
            }
            "mixed" => {
                crate::transport::Transport::kill_pane(
                    &tx,
                    &crate::transport::PaneId::new("%charlie"),
                )
                .unwrap();
            }
            _ => unreachable!(),
        }
        crate::lifecycle::add_agent_with_transport_force(
            &ws,
            &aid("charlie"),
            &role,
            false,
            None,
            true,
            &tx,
        )
        .unwrap_or_else(|error| panic!("{quadrant} force-recreate failed: {error}"));
        let state = crate::state::persist::load_runtime_state(&ws).unwrap();
        assert!(
            state["agents"].get("charlie").is_some(),
            "{quadrant}: state restored"
        );
        assert!(
            state["agent_lifecycle"].get("charlie").is_none(),
            "{quadrant}: successful force-recreate consumes its interim tombstone"
        );
        let spec_text = std::fs::read_to_string(selected_spec_path(&ws)).unwrap();
        assert_eq!(
            spec_text.matches("id: \"charlie\"").count(),
            1,
            "{quadrant}: exactly one spec row"
        );
        assert_eq!(
            crate::transport::Transport::list_targets(&tx)
                .unwrap()
                .iter()
                .filter(|pane| pane.session.as_str() == "team-laneateam"
                    && pane
                        .window_name
                        .as_ref()
                        .is_some_and(|window| window.as_str() == "charlie"))
                .count(),
            1,
            "{quadrant}: exactly one live pane"
        );
        assert!(role.exists(), "{quadrant}: external role preserved");
    }

    let ws = lanea_ws_agents(json!({
        "alpha": { "status": "stopped", "provider": "codex", "window": "alpha" },
        "bravo": { "status": "running", "provider": "codex", "window": "bravo" }
    }));
    let tx = LaneTransport::new("team-laneateam", &[]);
    seed_healthy_coordinator(&ws);
    let role = seed_charlie(&ws, &tx);
    let missing = ws.join("role-masters").join("missing-charlie.md");
    let error = crate::lifecycle::add_agent_with_transport_force(
        &ws,
        &aid("charlie"),
        &missing,
        false,
        None,
        true,
        &tx,
    )
    .expect_err("missing replacement role must fail and roll back");
    assert!(error.to_string().contains("role file not found"));
    let state = crate::state::persist::load_runtime_state(&ws).unwrap();
    assert!(
        state["agents"].get("charlie").is_some(),
        "rollback restores state"
    );
    assert!(
        state["agent_lifecycle"].get("charlie").is_none(),
        "preflight failure must not create a retirement tombstone"
    );
    assert!(
        std::fs::read_to_string(selected_spec_path(&ws))
            .unwrap()
            .contains("id: \"charlie\""),
        "rollback restores spec"
    );
    assert!(
        crate::transport::Transport::list_targets(&tx)
            .unwrap()
            .iter()
            .any(|pane| {
                pane.session.as_str() == "team-laneateam"
                    && pane
                        .window_name
                        .as_ref()
                        .is_some_and(|window| window.as_str() == "charlie")
            }),
        "rollback restores physical seat"
    );
    assert!(role.exists(), "rollback never mutates external role source");
}

#[test]
#[serial_test::serial(env)]
fn force_recreate_post_consumption_failure_restores_before_tuple_and_health() {
    let hermetic = HermeticTestEnv::enter("force-recreate-rollback");
    let ws = lanea_ws_agents_at(
        hermetic.workspace_root(),
        json!({
            "alpha": { "status": "stopped", "provider": "codex", "window": "alpha" },
            "bravo": { "status": "running", "provider": "codex", "window": "bravo" }
        }),
    );
    let tx = LaneTransport::new("team-laneateam", &[]);
    seed_healthy_coordinator(&ws);
    let role = seed_charlie(&ws, &tx);
    seed_agent_health(&ws, "team-laneateam", "charlie", "IDLE");
    seed_agent_health(&ws, "team-other", "charlie", "WORKING");
    let selected_health = crate::db::agent_health_capture::select_agent_health(
        &ws,
        "team-laneateam",
        &aid("charlie"),
    )
    .unwrap();
    let sibling_health =
        crate::db::agent_health_capture::select_agent_health(&ws, "team-other", &aid("charlie"))
            .unwrap();
    let _failure = ForceRecreateFailureGuard::after_spawn("post-consumption seam");
    let error = crate::lifecycle::add_agent_with_transport_force(
        &ws,
        &aid("charlie"),
        &role,
        false,
        None,
        true,
        &tx,
    )
    .expect_err("post-spawn failure must roll back a consumed seat");
    assert!(error.to_string().contains("post-consumption seam"));
    assert_eq!(
        tx.killed()
            .iter()
            .filter(|pane| pane.as_str() == "%charlie")
            .count(),
        2,
        "rollback kills the old pane during remove and the transaction-created pane before restore"
    );
    assert_eq!(
        crate::transport::Transport::list_targets(&tx)
            .unwrap()
            .iter()
            .filter(|pane| pane.session.as_str() == "team-laneateam"
                && pane
                    .window_name
                    .as_ref()
                    .is_some_and(|window| window.as_str() == "charlie"))
            .count(),
        1,
        "rollback restores exactly one physical seat; error={error}; spawns={:?}; killed={:?}",
        tx.spawns(),
        tx.killed(),
    );
    assert_eq!(
        crate::db::agent_health_capture::select_agent_health(
            &ws,
            "team-laneateam",
            &aid("charlie"),
        )
        .unwrap(),
        selected_health,
        "rollback restores the selected team's captured health row"
    );
    assert_eq!(
        crate::db::agent_health_capture::select_agent_health(&ws, "team-other", &aid("charlie"),)
            .unwrap(),
        sibling_health,
        "rollback never changes the sibling team's same-agent health row"
    );
    let state = crate::state::persist::load_runtime_state(&ws).unwrap();
    assert!(
        state["agent_lifecycle"].get("charlie").is_none(),
        "post-consumption rollback restores the before-state without a new tombstone"
    );
}

#[test]
fn force_remove_scopes_agent_health_to_canonical_team_key() {
    let hermetic = HermeticTestEnv::enter("force-remove-health-scope");
    let ws = lanea_ws_agents_at(
        hermetic.workspace_root(),
        json!({
            "alpha": { "status": "stopped", "provider": "codex", "window": "alpha" },
            "bravo": { "status": "running", "provider": "codex", "window": "bravo" }
        }),
    );
    let tx = LaneTransport::new("team-laneateam", &[]);
    seed_agent_health(&ws, "team-laneateam", "alpha", "IDLE");
    seed_agent_health(&ws, "team-other", "alpha", "WORKING");
    let sibling_before =
        crate::db::agent_health_capture::select_agent_health(&ws, "team-other", &aid("alpha"))
            .unwrap();

    remove_agent_with_transport(&ws, &aid("alpha"), true, true, None, &tx).unwrap();

    assert!(crate::db::agent_health_capture::select_agent_health(
        &ws,
        "team-laneateam",
        &aid("alpha"),
    )
    .unwrap()
    .is_none());
    assert_eq!(
        crate::db::agent_health_capture::select_agent_health(&ws, "team-other", &aid("alpha"),)
            .unwrap(),
        sibling_before,
        "force-remove must not delete the sibling owner_team_id row"
    );
}

/// Rewrite the compiled spec's `context.state_file` (default `team_state.md`) to `name`, so the
/// rollback team-state path divergence (capture honors spec.context.state_file; restore hardcodes
/// team_state.md) is observable. Asserts the rewrite took, so a format change can't silently mis-test.
fn set_context_state_file(ws: &std::path::Path, name: &str) {
    let p = ws.join("team.spec.yaml");
    let text = std::fs::read_to_string(&p).unwrap();
    let replaced = text.replace(
        "state_file: \"team_state.md\"",
        &format!("state_file: \"{name}\""),
    );
    assert_ne!(
        replaced, text,
        "expected to rewrite context.state_file in the compiled spec; got:\n{text}"
    );
    std::fs::write(&p, replaced).unwrap();
}

/// Fork workspace: source `alpha` (role doc = `alpha_role`) + `bravo`, alpha seeded RUNNING with a
/// source `session_id` (so fork reaches the spec-mutation / native-fork gate). Seeded-healthy coordinator
/// (fork's start_coordinator -> AlreadyRunning, no real daemon). session_name = "team-laneateam".
pub(super) fn fork_ws(alpha_role: &str) -> PathBuf {
    let ws = temp_ws().join("forkv2");
    std::fs::create_dir_all(ws.join("agents")).unwrap();
    std::fs::write(
        ws.join("TEAM.md"),
        "---\nname: laneateam\nobjective: Fork v2 probe.\nprovider: codex\n---\n\nteam.\n",
    )
    .unwrap();
    std::fs::write(ws.join("agents").join("alpha.md"), alpha_role).unwrap();
    std::fs::write(ws.join("agents").join("bravo.md"), DELEG_ROLE_BRAVO).unwrap();
    let spec = crate::compiler::compile_team(&ws).expect("compile fork team");
    std::fs::write(ws.join("team.spec.yaml"), crate::model::yaml::dumps(&spec)).unwrap();
    // 0.4.6 tuple-atomic contract: fork now requires the complete source
    // tuple (session_id + rollout_path + captured_at + captured_via). These
    // fork-mechanics tests previously only seeded session_id; seed a real
    // rollout file + the rest of the tuple so the source backing guard
    // passes and the test exercises the fork mechanics being asserted.
    use std::sync::atomic::{AtomicU64, Ordering};
    static FORK_ROLLOUT_SEQ: AtomicU64 = AtomicU64::new(0);
    let n = FORK_ROLLOUT_SEQ.fetch_add(1, Ordering::Relaxed);
    let rollout = std::env::temp_dir().join(format!(
        "ta-fork-ws-rollout-{}-{}.jsonl",
        std::process::id(),
        n
    ));
    std::fs::write(&rollout, b"{}\n").expect("seed fork source rollout");
    crate::state::persist::save_runtime_state(
        &ws,
        &json!({
            "session_name": "team-laneateam",
            "agents": {
                "alpha": {
                    "status": "running",
                    "provider": "codex",
                    "window": "alpha",
                    "session_id": "sess-a",
                    "rollout_path": rollout.to_string_lossy(),
                    "captured_at": "2026-06-25T10:00:00+00:00",
                    "captured_via": "session.captured"
                },
                "bravo": { "status": "running", "provider": "codex", "window": "bravo" }
            }
        }),
    )
    .unwrap();
    seed_healthy_coordinator(&ws);
    ws
}

// ── STOP #1 (stop-window-gate-1) [RED] — window ABSENT => stopped=false + NO kill ────────────────────
// Golden operations.py:81-99 gates `tmux kill-window` behind `if _tmux_window_exists(session, window)`;
// an absent window (already-stopped / never-started spec-known agent) skips the kill and returns
// {ok:true, status:"stopped", stopped:FALSE}. Rust kills UNCONDITIONALLY (restart.rs:181-183) and
// hardcodes stopped:true (:192). RED: LaneTransport with NO windows -> the porter's list_windows gate
// must skip the kill (killed empty) and set stopped=false; today kill_window IS called + stopped=true.
#[test]
fn lanea_stop_window_absent_returns_stopped_false_no_kill() {
    let ws = lanea_ws_agents(
        json!({ "alpha": { "status": "running", "provider": "codex", "window": "alpha" } }),
    );
    let tx = LaneTransport::new("team-laneateam", &[]); // alpha's window is ABSENT
    let report = stop_agent_with_transport(&ws, &aid("alpha"), None, &tx)
        .expect("window-absent stop is a clean Ok, not an Err (golden returns stopped:false)");
    assert!(
        !report.stopped,
        "golden operations.py:81-99: an ABSENT tmux window => stopped=FALSE (kill skipped); Rust hardcodes stopped:true"
    );
    assert!(
        tx.killed().is_empty(),
        "golden gates kill on _tmux_window_exists; an absent window must NOT be killed; Rust kills unconditionally, killed={:?}",
        tx.killed()
    );
}

// ── STOP #1 companion [LOCK] — window PRESENT => stopped=true + kill the golden target ───────────────
// When the selected team's exact `(session, window, pane)` tuple exists, stop kills that pane and
// returns stopped:true. Pane identity avoids ambiguous same-name windows on shared endpoints.
#[test]
fn lanea_stop_window_present_kills_and_stopped_true() {
    let ws = lanea_ws_agents(
        json!({ "alpha": { "status": "running", "provider": "codex", "window": "alpha" } }),
    );
    let tx = LaneTransport::new("team-laneateam", &["alpha"]); // window present
    let report =
        stop_agent_with_transport(&ws, &aid("alpha"), None, &tx).expect("present-window stop ok");
    assert!(
        report.stopped,
        "a present window must be killed => stopped=true"
    );
    assert_eq!(
        tx.killed(),
        vec!["%alpha".to_string()],
        "stop must kill the selected team's exact pane tuple; killed={:?}",
        tx.killed()
    );
}

// ── STOP #2 (stop-display-noop-2) [RED] — close ghostty_workspace slot: persist display.status/pane_title
// Golden operations.py:88-92 -> display/close.py:84-85 relabels the slot: display["status"]="stopped",
// display["pane_title"]=f"stopped: {agent_id}", written back into the persisted agent entry. Rust never
// touches the display (mark_agent_stopped leaves it as-is) and hardcodes display_closed:false. RED: the
// persisted display.status/pane_title are the in-process observable.
#[test]
fn lanea_stop_ghostty_workspace_relabels_slot_to_stopped() {
    let ws = lanea_ws_agents(json!({
        "alpha": { "status": "running", "provider": "codex", "window": "alpha",
                   "display": { "backend": "ghostty_workspace", "pane_id": "%5", "linked_session": "disp-alpha", "status": "running", "pane_title": "alpha" } }
    }));
    let tx = LaneTransport::new("team-laneateam", &["alpha"]);
    let _ = stop_agent_with_transport(&ws, &aid("alpha"), None, &tx).expect("stop ok");
    let state = crate::state::persist::load_runtime_state(&ws).expect("load state");
    assert_eq!(
        state.pointer("/agents/alpha/display/status").and_then(serde_json::Value::as_str),
        Some("stopped"),
        "stop must relabel a ghostty_workspace slot: display.status='stopped' (close.py:84); Rust leaves it 'running'"
    );
    assert_eq!(
        state.pointer("/agents/alpha/display/pane_title").and_then(serde_json::Value::as_str),
        Some("stopped: alpha"),
        "stop must set display.pane_title='stopped: <id>' (close.py:85); Rust never touches the display"
    );
}

// ── RESET #3 (reset-paused-restart-2) [RED] — reset of a PAUSED agent returns ok=true (NOT an Err) ───
// Golden operations.py:126-140: after discard, reset re-spawns via start_agent(force,allow_fresh).
// discard does NOT clear `paused`, so start_agent returns the refusal-shaped {ok:False,status:paused,
// reason:agent_paused} (start.py:101) WITHOUT raising; reset embeds it as `started` and returns the
// success envelope {ok:True, status:"running", started:{ok:False,...}, coordinator:None}. Rust maps
// StartAgentOutcome::Paused -> Err(RequirementUnmet "agent ... is paused") (restart.rs:251-253) — a hard
// error instead of golden's ok=true. RED: reset of a paused agent must be Ok, not Err.
#[test]
fn lanea_reset_paused_agent_returns_ok_not_err() {
    let ws = lanea_ws_agents(json!({
        "alpha": { "status": "running", "provider": "codex", "window": "alpha", "paused": true, "session_id": "sess-a" }
    }));
    let tx = LaneTransport::new("team-laneateam", &["alpha"]);
    let result = reset_agent_with_transport(&ws, &aid("alpha"), true, false, None, &tx);
    assert!(
        result.is_ok(),
        "golden operations.py:133-140: reset of a PAUSED agent returns the ok=true success envelope embedding \
         started={{ok:false,status:paused,reason:agent_paused}}; Rust raises RequirementUnmet. Porter must add a \
         ResetAgentOutcome variant carrying the paused `started` result. got {result:?}"
    );
    assert!(
        !matches!(result, Ok(ResetAgentOutcome::Refused { .. })),
        "discard_session=true was passed -> the DiscardSessionRequired refusal must NOT fire; got {result:?}"
    );
}

// ── REMOVE #4 (remove-dynamic-agent-refused-1) [RED] — dynamic agent removable WITHOUT from_spec ─────
// Golden agents.py:50-54: dynamic_agent = bool(agent_state.dynamic_role_file OR agent.forked_from);
// it only refuses when `not dynamic_agent and not (from_spec and confirm)`. A dynamic/forked agent is
// removable with from_spec=false. Rust unconditionally `if !from_spec -> RefusedFromSpecConfirm`
// (restart.rs:285) BEFORE even loading the spec/state -> wrongly refuses the dynamic agent. RED.
#[test]
fn lanea_remove_dynamic_agent_removable_without_from_spec() {
    let ws = lanea_ws_agents(json!({
        "alpha": { "status": "stopped", "provider": "codex", "window": "alpha", "dynamic_role_file": ".team/dynamic-role-files/alpha.md" },
        "bravo": { "status": "stopped", "provider": "codex", "window": "bravo" }
    }));
    // make the dynamic role file exist (so removal resolves+deletes it cleanly under either path policy).
    let dyn_dir = ws.join(".team").join("dynamic-role-files");
    std::fs::create_dir_all(&dyn_dir).unwrap();
    std::fs::write(dyn_dir.join("alpha.md"), "dynamic alpha role\n").unwrap();
    let tx = LaneTransport::new("team-laneateam", &[]); // alpha not running
    let result = remove_agent_with_transport(&ws, &aid("alpha"), false, true, None, &tx); // from_spec=FALSE
    assert!(
        !matches!(result, Ok(RemoveAgentOutcome::RefusedFromSpecConfirm { .. })),
        "golden agents.py:50-54: a DYNAMIC agent (state.dynamic_role_file) is removable with from_spec=false; \
         Rust wrongly returns RefusedFromSpecConfirm; got {result:?}"
    );
    let state = crate::state::persist::load_runtime_state(&ws).expect("load state");
    assert!(
        state
            .get("agents")
            .and_then(serde_json::Value::as_object)
            .is_some_and(|a| !a.contains_key("alpha")),
        "the dynamic agent must actually be removed from state.agents; got {result:?}"
    );
}

#[test]
fn remove_preserves_external_role_source_for_both_flag_forms() {
    for from_spec in [false, true] {
        let ws = lanea_ws_agents(json!({
            "alpha": { "status": "stopped", "provider": "codex", "window": "alpha" },
            "bravo": { "status": "stopped", "provider": "codex", "window": "bravo" }
        }));
        let external_dir = ws.join("role-masters");
        std::fs::create_dir_all(&external_dir).unwrap();
        let external = external_dir.join("alpha.md");
        let original = b"external alpha role\n";
        std::fs::write(&external, original).unwrap();
        let mut state = crate::state::persist::load_runtime_state(&ws).unwrap();
        state["agents"]["alpha"]["dynamic_role_file"] =
            serde_json::json!(external.to_string_lossy());
        crate::state::persist::save_runtime_state(&ws, &state).unwrap();

        let result = remove_agent_with_transport(
            &ws,
            &aid("alpha"),
            from_spec,
            true,
            None,
            &LaneTransport::new("team-laneateam", &[]),
        );
        assert!(result.is_ok(), "remove failed: {result:?}");
        assert_eq!(
            std::fs::read(&external).unwrap(),
            original,
            "external role source changed for from_spec={from_spec}"
        );
    }
}

#[cfg(unix)]
#[test]
fn remove_preserves_managed_path_symlink_escape() {
    use std::os::unix::fs::symlink;

    let ws = lanea_ws_agents(json!({
        "alpha": { "status": "stopped", "provider": "codex", "window": "alpha", "dynamic_role_file": ".team/dynamic-role-files/alpha.md" },
        "bravo": { "status": "stopped", "provider": "codex", "window": "bravo" }
    }));
    let external = ws.join("role-masters").join("alpha.md");
    std::fs::create_dir_all(external.parent().unwrap()).unwrap();
    std::fs::write(&external, b"external target\n").unwrap();
    let managed = ws.join(".team").join("dynamic-role-files").join("alpha.md");
    std::fs::create_dir_all(managed.parent().unwrap()).unwrap();
    symlink(&external, &managed).unwrap();

    let result = remove_agent_with_transport(
        &ws,
        &aid("alpha"),
        false,
        true,
        None,
        &LaneTransport::new("team-laneateam", &[]),
    );
    assert!(result.is_ok(), "remove failed: {result:?}");
    assert_eq!(std::fs::read(&external).unwrap(), b"external target\n");
    assert!(managed.exists(), "symlink escape must be preserved");
}

#[test]
fn force_remove_state_only_seat_is_idempotent() {
    let ws = lanea_ws_agents(json!({
        "alpha": { "status": "stopped", "provider": "codex", "window": "alpha" },
        "bravo": { "status": "stopped", "provider": "codex", "window": "bravo" }
    }));
    let spec_path = ws.join("team.spec.yaml");
    let spec = crate::model::yaml::loads(&std::fs::read_to_string(&spec_path).unwrap()).unwrap();
    std::fs::write(
        &spec_path,
        crate::model::yaml::dumps(&crate::lifecycle::restart::remove::spec_without_agent(
            &spec,
            &aid("alpha"),
        )),
    )
    .unwrap();
    let transport = LaneTransport::new("team-laneateam", &[]);
    let first = remove_agent_with_transport(&ws, &aid("alpha"), true, true, None, &transport);
    let second = remove_agent_with_transport(&ws, &aid("alpha"), true, true, None, &transport);
    assert!(first.is_ok(), "state-only force-remove failed: {first:?}");
    assert!(
        second.is_ok(),
        "second force-remove must be idempotent: {second:?}"
    );
}

// ── REMOVE #5 (remove-unknown-precedence-2) [RED] — unknown-worker raised BEFORE from_spec refusal ───
// Golden agents.py:41-54 loads the spec and runs _find_worker (raising "unknown worker agent id: <id>")
// BEFORE the from_spec/confirm refusal at :53. Rust checks `!from_spec` FIRST (restart.rs:285) and
// returns RefusedFromSpecConfirm without ever loading the spec -> a nonexistent agent is mis-reported as
// a from_spec refusal. RED: an unknown agent with from_spec=false must surface "unknown worker".
#[test]
fn lanea_remove_unknown_agent_precedes_from_spec_refusal() {
    let ws = lanea_ws_agents(
        json!({ "alpha": { "status": "stopped", "provider": "codex", "window": "alpha" } }),
    );
    let tx = LaneTransport::new("team-laneateam", &[]);
    let text = format!(
        "{:?}",
        remove_agent_with_transport(&ws, &aid("ghost"), false, false, None, &tx)
    );
    assert!(
        text.contains("unknown worker"),
        "golden agents.py:41-54: the unknown-worker check precedes the from_spec refusal; an unknown agent must \
         raise 'unknown worker agent id: ghost', NOT RefusedFromSpecConfirm; got {text}"
    );
}

// ── REMOVE #8 (remove-team-state-md-content-5) [RED] — team_state.md is golden MARKDOWN, not JSON ────
// Golden state.py:625-686 write_team_state builds a Markdown doc ("# Team State", "## Objective",
// "## Agents" with one "- {id}: {role} on {provider} ({status})" per spec agent, etc.) from removed_spec.
// Rust write_team_state (restart.rs:925-941) writes serde_json::to_string_pretty(state) — raw JSON — and
// passes the ORIGINAL spec (so the removed agent would not be excluded). RED: after a successful remove,
// team_state.md must be the Markdown doc, list the REMAINING agent (bravo) and NOT the removed one.
#[test]
fn lanea_remove_writes_markdown_team_state_not_json() {
    let ws = lanea_ws_agents(json!({
        "alpha": { "status": "stopped", "provider": "codex", "window": "alpha" },
        "bravo": { "status": "running", "provider": "codex", "window": "bravo" }
    }));
    let tx = LaneTransport::new("team-laneateam", &[]);
    let _ =
        remove_agent_with_transport(&ws, &aid("alpha"), true, true, None, &tx).expect("remove ok");
    let team_state =
        std::fs::read_to_string(ws.join("team_state.md")).expect("team_state.md written");
    assert!(
        team_state.starts_with("# Team State"),
        "golden write_team_state emits a Markdown document starting '# Team State'; Rust dumps JSON; got:\n{team_state}"
    );
    assert!(
        !team_state.trim_start().starts_with('{'),
        "team_state.md must NOT be a JSON dump of runtime state; got:\n{team_state}"
    );
    assert!(
        team_state.contains("## Agents"),
        "golden has a '## Agents' section; got:\n{team_state}"
    );
    assert!(
        team_state.contains("bravo: Bravo Worker on codex"),
        "the '## Agents' section must list the remaining agent bravo (golden '- {{id}}: {{role}} on {{provider}} ({{status}})'); got:\n{team_state}"
    );
    assert!(
        !team_state.contains("alpha: Alpha Worker"),
        "the removed agent (alpha) must be EXCLUDED — golden writes removed_spec, not the original spec; got:\n{team_state}"
    );
}

// ── REMOVE #10 (remove-is-running-no-tmux-fallback-7) [RED] — is_running honors the tmux-window fallback
// Golden agents.py:247-252 _is_running returns True if status in {running,busy} OR
// (session_name AND _tmux_window_exists(session, window)). Rust agent_is_running (restart.rs:689-700)
// only checks status -> an agent with a STALE status ('idle') whose tmux window is still live is treated
// as not-running, so removal without --force is wrongly ALLOWED. RED: such an agent removed without
// force must be RefusedForceRequired (golden), not Removed.
#[test]
fn lanea_remove_is_running_honors_tmux_window_fallback() {
    let ws = lanea_ws_agents(json!({
        "alpha": { "status": "idle", "provider": "codex", "window": "alpha" }, // stale status, but window is live
        "bravo": { "status": "stopped", "provider": "codex", "window": "bravo" }
    }));
    let tx = LaneTransport::new("team-laneateam", &["alpha"]); // alpha's tmux window EXISTS
    let result = remove_agent_with_transport(&ws, &aid("alpha"), true, false, None, &tx); // force=FALSE
    assert!(
        matches!(result, Ok(RemoveAgentOutcome::RefusedForceRequired { .. })),
        "golden agents.py:247-252: a stale-status agent whose tmux window is LIVE counts as running -> removal \
         without --force is RefusedForceRequired; Rust drops the tmux fallback and allows it. Porter must thread \
         the transport into agent_is_running. got {result:?}"
    );
}

// ── REMOVE #7 (remove-rollback-no-restart-running-4) [RED] — rollback RESTARTS the force-stopped worker
// Golden agents.py:78,219-223: after force-stopping a running worker, rollback (on any in-try failure)
// sets restore_running and calls start_agent(force=True, allow_fresh=True) to bring the worker back.
// Rust rollback (restart.rs:1023-1067) has no restore_running and never restarts -> a force-remove that
// stops a running agent then fails leaves it DEAD. RED: drive a deterministic in-try failure
// (1-worker team: removing alpha -> empty agents -> validate_spec fails AFTER the force-stop) and assert
// the transport recorded a re-spawn during rollback (the golden worker restart). Today: zero spawns.
#[test]
fn lanea_remove_rollback_restarts_force_stopped_worker() {
    let ws = lanea_one_agent_ws("running"); // removing alpha -> agents:[] -> validate_spec FAILS post-stop
    let tx = LaneTransport::new("team-laneateam", &["alpha"]);
    let result = remove_agent_with_transport(&ws, &aid("alpha"), true, true, None, &tx); // from_spec+force
    assert!(
        result.is_err(),
        "precondition: removing the only worker makes removed_spec invalid (validate_spec) -> the remove fails \
         after the force-stop, triggering rollback; got {result:?}"
    );
    assert!(
        tx.killed().contains(&"%alpha".to_string()),
        "precondition: the running worker's exact pane was killed before the failure; killed={:?}",
        tx.killed()
    );
    assert!(
        !tx.spawns().is_empty(),
        "golden agents.py:219-223: rollback must RESTART the force-stopped worker (start_agent force=True) -> a \
         re-spawn; Rust rollback never restarts, leaving the worker dead. Porter must thread the transport into \
         RemoveRollback::restore. spawns={:?}",
        tx.spawns()
    );
    let restored = crate::state::persist::load_runtime_state(&ws).unwrap();
    assert!(
        restored["agent_lifecycle"].get("alpha").is_none(),
        "failed remove must roll back the transaction-created tombstone"
    );
}

// ── REMOVE #11 (remove-dynamic-role-path-and-required-8, warn) [RED] — missing REQUIRED role file raises
// Golden agents.py:255-261 _remove_dynamic_role_file(path, required=True) RAISES "dynamic role file
// missing: <path>" when the state recorded a dynamic_role_file but it is absent. Rust hardcodes the
// default path and returns Ok(false) silently (restart.rs:951-953), losing the hard-fail+rollback. RED:
// a dynamic agent whose recorded role file is MISSING must raise, not silently complete the removal.
#[test]
fn lanea_remove_dynamic_role_file_missing_raises() {
    let ws = lanea_ws_agents(json!({
        "alpha": { "status": "stopped", "provider": "codex", "window": "alpha", "dynamic_role_file": ".team/dynamic-role-files/custom.md" }, // file NOT created
        "bravo": { "status": "stopped", "provider": "codex", "window": "bravo" }
    }));
    let tx = LaneTransport::new("team-laneateam", &[]);
    let text = format!(
        "{:?}",
        remove_agent_with_transport(&ws, &aid("alpha"), true, true, None, &tx)
    );
    assert!(
        text.contains("dynamic role file missing"),
        "golden agents.py:259-260: a state-recorded dynamic_role_file that is MISSING must RAISE 'dynamic role \
         file missing: <path>' (required=true); Rust returns Ok(false) silently and completes the remove. got {text}"
    );
}

// ── REMOVE #9 (remove-rollback-team-state-path-6) [RED] — rollback restores via spec.context.state_file
// Golden agents.py:181-204 derives team_state_path = workspace / spec.context.state_file once and uses
// it for BOTH capture and restore. Rust capture honors spec.context.state_file (restart.rs:994-999) but
// restore HARDCODES workspace/team_state.md (:1031). With a custom state_file, rollback writes the
// captured content to the WRONG file. RED (deterministic in-try failure via the 1-worker validate-fail):
// after rollback, a SPURIOUS team_state.md must NOT exist (golden restores the custom file, never creates
// team_state.md). Today the hardcoded restore creates team_state.md.
#[test]
fn lanea_remove_rollback_restores_via_spec_state_file_path() {
    let ws = lanea_one_agent_ws("stopped");
    set_context_state_file(&ws, "custom_state.md");
    std::fs::write(ws.join("custom_state.md"), "ORIGINAL CUSTOM TEAM STATE\n").unwrap(); // capture reads this
    let tx = LaneTransport::new("team-laneateam", &[]);
    let result = remove_agent_with_transport(&ws, &aid("alpha"), true, true, None, &tx);
    assert!(
        result.is_err(),
        "precondition: 1-worker removal -> validate_spec fails -> rollback runs; got {result:?}"
    );
    assert!(
        !ws.join("team_state.md").exists(),
        "golden agents.py:200-204: rollback restores the spec-derived state_file (custom_state.md), it must NOT \
         create the hardcoded team_state.md. Porter must capture team_state_path on the rollback struct and reuse \
         it in restore."
    );
}

// ── FORK (fork-dup-guard-misses-leader) [RED] — forking ONTO the leader id is 'already exists' ───────
// Golden operations.py:301-302 uses _find_agent (matches agents AND the leader, runtime.py:1055), so
// forking to as_agent_id == leader.id raises 'agent id already exists: <id>'. Rust find_spec_agent
// short-circuits to None for the leader id (launch.rs:507-515) -> the duplicate guard is SKIPPED and the
// fork proceeds against the leader id. RED: fork target == "leader" must be 'already exists'.
#[test]
fn lanea_fork_dup_target_leader_id_is_already_exists() {
    let ws = fork_ws(DELEG_ROLE_ALPHA);
    let tx = LaneTransport::new("team-laneateam", &[]);
    let text = format!(
        "{:?}",
        fork_agent_with_transport(&ws, &aid("alpha"), &aid("leader"), None, false, None, &tx)
    );
    assert!(
        text.contains("already exists"),
        "golden operations.py:301-302 (_find_agent matches the leader): forking ONTO the leader id must raise \
         'agent id already exists: leader'; Rust skips the dup guard for the leader id and proceeds. got {text}"
    );
}

// ── FORK (fork-missing-tmux-window-guard) [RED] — window-already-exists guard BEFORE spec mutation ────
// Golden operations.py:310-312: after the session_id guard and BEFORE mutating the spec, raise
// 'tmux window already exists for fork target: {session}:{as_agent_id}' if the window exists. Rust has no
// such guard (launch.rs:439-440) -> it appends + writes the spec regardless. RED: a pre-existing window
// for the target must (a) raise that exact message and (b) leave the spec UNMUTATED (no fork agent).
#[test]
fn lanea_fork_window_already_exists_guard_before_spec_mutation() {
    let ws = fork_ws(DELEG_ROLE_ALPHA);
    let tx = LaneTransport::new("team-laneateam", &["newfork"]); // the target window already exists
    let text = format!(
        "{:?}",
        fork_agent_with_transport(&ws, &aid("alpha"), &aid("newfork"), None, false, None, &tx)
    );
    assert!(
        text.contains("tmux window already exists for fork target: team-laneateam:newfork"),
        "golden operations.py:310-312: a pre-existing target window must raise 'tmux window already exists for \
         fork target: team-laneateam:newfork' BEFORE spec mutation; Rust has no guard. got {text}"
    );
    let spec_text = std::fs::read_to_string(ws.join("team.spec.yaml")).unwrap();
    assert!(
        !spec_text.contains("newfork"),
        "the guard must fire BEFORE the spec is mutated; the spec must NOT contain the fork agent 'newfork'"
    );
}

// ── FORK (fork-gate-error-text) [RED] + (fork-incomplete-rollback, adapter arm) — golden gate text + spec rollback
// Golden operations.py:329-330 raises f"{provider} does not support native session fork" when the native
// fork gate fails (auth_mode==compatible_api). Rust relies on adapter.fork_plan() -> CapabilityUnsupported
// ("Codex:fork") (adapter.rs:310) -> a different observable. AND golden wraps the post-spec-write steps
// in try/except restoring the spec on ANY failure (operations.py:384-394); Rust writes the spec
// (launch.rs:443) then errors at adapter.fork_plan (458-460) WITHOUT restoring it. RED on both: the message
// text AND the spec must be rolled back to not contain the fork agent.
#[test]
fn lanea_fork_gate_error_text_and_spec_rollback_on_adapter_arm() {
    let ws = fork_ws(DELEG_ROLE_ALPHA_COMPAT); // source alpha auth_mode=compatible_api -> native fork unsupported
    let tx = LaneTransport::new("team-laneateam", &[]);
    let result =
        fork_agent_with_transport(&ws, &aid("alpha"), &aid("newfork"), None, false, None, &tx);
    let text = format!("{result:?}");
    assert!(
        text.contains("codex does not support native session fork"),
        "golden operations.py:329-330: the native-fork gate must raise 'codex does not support native session \
         fork'; Rust surfaces the generic 'capability unsupported: Codex:fork'. got {text}"
    );
    let spec_text = std::fs::read_to_string(ws.join("team.spec.yaml")).unwrap();
    assert!(
        !spec_text.contains("newfork"),
        "golden operations.py:384-394: on the gate failure the spec must be ROLLED BACK; Rust writes the spec \
         then errors at adapter.fork_plan without restoring it, leaving the fork agent 'newfork' in the spec"
    );
}

// ── FORK (fork-report-session-id-is-pane-id) [RED] — report session_id is the captured id / None, not pane
// Golden operations.py:399,408 returns state['agents'][as_agent_id].get('session_id') — the captured
// provider session id (or None if capture missed, raise_on_missed=False). Rust sets
// session_id: Some(SessionId::new(spawn.pane_id)) (launch.rs:502) — the tmux pane id ('%newfork'),
// a different value kind. RED: the report session_id must NOT be the pane id (None, since the Rust fork
// path performs no session capture).
#[test]
fn lanea_fork_report_session_id_is_not_pane_id() {
    let ws = fork_ws(DELEG_ROLE_ALPHA); // codex+subscription -> native fork supported -> full success path
    let tx = LaneTransport::new("team-laneateam", &[]);
    let report =
        fork_agent_with_transport(&ws, &aid("alpha"), &aid("newfork"), None, false, None, &tx)
            .expect("fork ok (codex subscription supports fork)");
    assert_ne!(
        report.session_id,
        Some(crate::provider::SessionId::new("%newfork")),
        "golden operations.py:399,408: report.session_id is the captured provider session id / None, NEVER the \
         tmux pane id; Rust returns Some(pane_id='%newfork')"
    );
    assert_eq!(
        report.session_id, None,
        "the Rust fork path captures no session -> report.session_id must be None (golden capture-missed), not the pane id"
    );
}

// ── REMOVE #6/#12 (remove-rollback-no-agent-health-3 / remove-rollback-health-1) [SEAM #[ignore]] ────
// Golden _RemoveRollback captures `self.health = copy.deepcopy(store.agent_health().get(agent_id))`
// (agents.py:185) and restore() re-upserts it via _restore_agent_health (agents.py:215-218,268-278). The
// Rust RemoveRollback has NO health field and never restores it (restart.rs:972-1067). This is only
// observable when a step AFTER the agent_health delete fails — but Rust's only post-delete step is the
// snapshot, which golden runs OUTSIDE the rollback-protected region (agents.py:135). Exercising it
// golden-faithfully needs a production failure-injection seam at an in-try step after the delete (mirror
// the coordinator SaveHook). PORTER: add `health: Option<Value>` to RemoveRollback (capture the row
// before delete; restore re-upserts status||"IDLE"/last_output_at/context_usage_pct/current_task_id, or
// deletes if None) AND move save_team_runtime_snapshot OUTSIDE the rollback region (golden agents.py:135).
#[test]
#[ignore = "seam: agent_health rollback restore needs a failure-injection hook (post-delete, in-try) to \
            exercise in-process; golden agents.py:185/215-218/268-278. Porter adds RemoveRollback.health + \
            moves save_team_runtime_snapshot outside the rollback region."]
fn lanea_remove_rollback_restores_agent_health() {
    // Golden contract (verified by reading agents.py): on a mid-remove failure after the agent_health
    // row is deleted, rollback re-upserts the captured row so the health history is not lost.
}

// ── FORK (fork-incomplete-rollback) [SEAM #[ignore]] — post-spawn rollback arms ─────────────────────
// Golden operations.py:384-394 wraps spec-mutation..start_coordinator in try/except; on ANY failure it
// (1) kills the spawned tmux window if present, (2) adapter.cleanup_mcp, (3) restores old spec text, and
// (4) restores prior state. Rust only restores the spec on the spawn_into arm (launch.rs:481); the
// save_runtime_state (486-487) and start_coordinator (488-493) failure arms leave the spec mutated, the
// already-spawned window un-killed, and the state un-rolled-back; install_mcp/cleanup_mcp are absent.
// The adapter.fork_plan arm IS covered HARD above (lanea_fork_gate_error_text_and_spec_rollback_on_adapter_arm).
// The post-SPAWN arms need a failure-injection seam after spawn_into (codex+subscription forks past
// adapter.fork_plan, so the spawn succeeds and there is no in-process way to fail save/coordinator cleanly).
// PORTER: a Drop guard armed after the spec write, disarmed on success — kills the window, restores spec
// + state, runs cleanup_mcp on every post-write error arm.
#[test]
#[ignore = "seam: fork post-spawn rollback arms (save_runtime_state / start_coordinator failure) need a \
            failure-injection hook after spawn_into; golden operations.py:384-394. Porter wires a Drop \
            guard (kill window + restore spec/state + cleanup_mcp) armed after the spec write."]
fn lanea_fork_rollback_complete_on_post_spawn_failure() {
    // Golden contract (operations.py:384-394): a post-spawn failure kills the spawned window, restores
    // the old spec text + prior state, and runs cleanup_mcp before re-raising.
}
