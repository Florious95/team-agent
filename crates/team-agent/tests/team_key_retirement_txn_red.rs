//! team-key slice2b · L1 — retirement tombstone TRANSACTION (RED).
//!
//! From owner-b's real product shape (no new public API; a single remove.rs
//! tombstone primitive writes/clears `agent_lifecycle.<id>.state=retired` inside
//! the SAME selected-state save, reusing add --force's single-lock
//! snapshot->rollback fact) + locate §6.2, NOT the root-cause reasoning. On the
//! public injected-transport surface (pure state, no real tmux). Reads go through
//! the CANONICAL team-scoped projection, and remove asserts at the OUTCOME enum
//! level, so no case reds for a fixture artifact (refused-not-executed / nested-
//! read-null). Two ACTIVE L1 REDs + one candidate regression guard; two scenarios
//! deferred to reachable layers (see inline NOTEs):
//!   1. [RED] remove leaves `state=retired` for the id;
//!   2. [RED] a plain re-add clears the tombstone (transition pinned: tombstone
//!      must EXIST after remove, then be cleared — no vacuous terminal-None pass);
//!   -  [deferred] failed PLAIN re-add rollback: unreachable at L1 (no plain-add
//!      fail seam) -> L3 real-machine re-add case;
//!   3. [regression guard, green at baseline] a force remove->add whose post-
//!      consumption add fails restores the seat AND leaks no stray tombstone;
//!   -  [deferred] same-id sibling non-interference: cannot red for the right
//!      reason at a no-semantics baseline -> L3 + owner-b scoped src unit.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[path = "support/hermetic.rs"]
mod hermetic_guard;
#[allow(dead_code)]
fn _hermetic_boundary_marker(_: &hermetic_guard::HermeticTestEnv) {}

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use rusqlite::params;
use serde_json::json;
use serial_test::serial;
use team_agent::coordinator::{
    coordinator_pid_path, write_coordinator_metadata, MetadataSource, Pid, WorkspacePath,
};
use team_agent::db::schema::open_db;
use team_agent::lifecycle::{
    add_agent_with_transport, add_agent_with_transport_force, fork_agent_with_transport,
    remove_agent_with_transport, RemoveAgentOutcome,
};
use team_agent::message_store::MessageStore;
use team_agent::model::ids::AgentId;
use team_agent::state::persist::{load_runtime_state, save_runtime_state};
use team_agent::transport::{
    AttachOutcome, BackendKind, CaptureRange, CapturedText, InjectPayload, InjectReport,
    InjectStage, InjectVerification, Key, PaneField, PaneId, PaneInfo, PaneLiveness, SessionName,
    SetEnvOutcome, SpawnResult, SubmitVerification, Target, Transport, TransportError,
    TurnVerification, WindowName,
};

const FAIL_REMOVE_AFTER_HEALTH_DELETE: &str =
    "TEAM_AGENT_TEST_FAIL_REMOVE_AFTER_AGENT_HEALTH_DELETE";
const FAIL_FORK_AFTER_SPAWN: &str = "TEAM_AGENT_TEST_FAIL_FORK_AFTER_SPAWN";
const FAIL_FORCE_RECREATE_AFTER_SPAWN: &str = "TEAM_AGENT_TEST_FAIL_FORCE_RECREATE_AFTER_SPAWN";

/// Read the retirement tombstone via the CANONICAL team-scoped projection, not a
/// hard-coded `teams.<key>` nested path: the legacy single-team seed keeps agents
/// at state root and the scoped writer's compat leg does not physicalize `teams`,
/// so a nested read would always be None regardless of product correctness.
fn retired_state(ws: &Path, key: &str, id: &str) -> Option<String> {
    team_agent::state::projection::select_runtime_state(ws, Some(key))
        .ok()?
        .get("agent_lifecycle")?
        .get(id)?
        .get("state")?
        .as_str()
        .map(str::to_string)
}

fn role_path(fixture: &RollbackFixture, id: &str) -> PathBuf {
    let p = fixture.team.join(format!("agents/{id}.md"));
    std::fs::write(&p, agent_doc(id, &format!("{id} worker"))).unwrap();
    p
}

#[test]
#[serial(env)]
fn remove_writes_retired_tombstone() {
    let fixture = RollbackFixture::new("txn-remove");
    fixture.seed_team();
    // alpha is a state+spec seat with NO live pane (transport carries only bravo),
    // so from_spec=true alone removes it cleanly. Assert at the OUTCOME enum level
    // so an `Ok(Refused*)` cannot be swallowed as success (fixture-artifact red).
    let transport = RecordingTransport::new("team-rollbackteam", &["bravo"]);
    let outcome =
        remove_agent_with_transport(&fixture.team, &aid("alpha"), true, false, None, &transport)
            .expect("remove alpha must not error");
    assert!(
        matches!(outcome, RemoveAgentOutcome::Removed { .. }),
        "remove must actually execute (Removed), not refuse; got {outcome:?}"
    );
    assert_eq!(
        retired_state(&fixture.team, "rollbackteam", "alpha").as_deref(),
        Some("retired"),
        "a successful remove must write agent_lifecycle.alpha.state=retired in the same save"
    );
}

#[test]
#[serial(env)]
fn plain_re_add_clears_the_tombstone() {
    let fixture = RollbackFixture::new("txn-readd");
    fixture.seed_team();
    let transport = RecordingTransport::new("team-rollbackteam", &["bravo"]);
    let outcome =
        remove_agent_with_transport(&fixture.team, &aid("alpha"), true, false, None, &transport)
            .expect("remove alpha must not error");
    assert!(
        matches!(outcome, RemoveAgentOutcome::Removed { .. }),
        "remove must actually execute (Removed) before re-add; got {outcome:?}"
    );
    // Pin the STATE TRANSITION, not just the terminal None: the tombstone must
    // first EXIST after remove (else the final `None` passes vacuously on a
    // no-tombstone baseline = false green), then be cleared by the re-add.
    assert_eq!(
        retired_state(&fixture.team, "rollbackteam", "alpha").as_deref(),
        Some("retired"),
        "precondition: remove must have written the retired tombstone to clear"
    );
    let role = role_path(&fixture, "alpha");
    add_agent_with_transport(&fixture.team, &aid("alpha"), &role, false, None, &transport)
        .expect("re-add alpha");
    assert_eq!(
        retired_state(&fixture.team, "rollbackteam", "alpha"),
        None,
        "a successful re-add must clear the retired tombstone for the id"
    );
}

// NOTE (honest reachability): a "failed PLAIN re-add restores the tombstone"
// scenario is NOT reachable at this L1 pure-transport surface. The only failure
// seam after spawn (`TEAM_AGENT_TEST_FAIL_FORCE_RECREATE_AFTER_SPAWN`) is read
// ONLY on the force-recreate path (`force_recreate_with_transport_locked`), so a
// plain `add_agent_with_transport` cannot be made to fail here — injecting that
// env would be a no-op and the case would red for the wrong reason (fixture
// artifact, not the guarded rollback fault). The failed-re-add rollback that
// restores the tombstone is contracted where a real add failure is reachable:
// the L3 real-machine re-add case. The reachable single-lock rollback at L1 is
// the force remove->add case below.

/// Candidate-side REGRESSION GUARD (not a RED): this case is GREEN at baseline
/// on purpose. The force-recreate snapshot restore predates slice2b (the seat
/// already survives a failed post-consumption add), and with no tombstone
/// semantics there is nothing to leak — so neither assert can red at baseline.
/// Its job is to pin, once the candidate DOES write tombstones, that a rolled-
/// back force transaction leaves no STRAY retired tombstone. The two active L1
/// REDs are `remove_writes_retired_tombstone` and `plain_re_add_clears_the_tombstone`.
#[test]
#[serial(env)]
fn force_remove_add_failed_post_consumption_restores_seat_without_new_tombstone() {
    let fixture = RollbackFixture::new("txn-force-fail");
    fixture.seed_team();
    let transport = RecordingTransport::new("team-rollbackteam", &["alpha", "bravo"]);
    let _env = EnvGuard::set(FAIL_FORCE_RECREATE_AFTER_SPAWN, "post-consumption");
    let role = role_path(&fixture, "alpha");
    let _ = add_agent_with_transport_force(
        &fixture.team,
        &aid("alpha"),
        &role,
        false,
        None,
        true,
        &transport,
    );
    // Read the restored seat via the CANONICAL projection (not a hard-coded
    // `teams.<key>` nested path, which is null under the legacy single-team seed).
    // The pre-existing force-recreate snapshot restore already survives the seat;
    // the slice2b-NEW invariant this case pins is the second assert: the rolled-
    // back transaction must NOT leak a stray retired tombstone.
    let restored =
        team_agent::state::projection::select_runtime_state(&fixture.team, Some("rollbackteam"))
            .unwrap();
    let agents = restored
        .get("agents")
        .and_then(serde_json::Value::as_object);
    assert!(
        agents.is_some_and(|m| m.contains_key("alpha")),
        "force remove->add with a failed post-consumption must restore the original alpha seat"
    );
    assert_eq!(
        retired_state(&fixture.team, "rollbackteam", "alpha"),
        None,
        "a rolled-back force recreate must not leave a stray retired tombstone"
    );
}

// NOTE (honest boundary): the "same-id sibling team lifecycle is untouched"
// scenario is NOT a sound L1 RED at this baseline. Without any tombstone
// semantics, a hand-seeded sibling agent_lifecycle is dropped by the remove's
// selected-state save regardless of cross-team scoping — so it reds for the
// wrong reason (a fixture artifact, not the guarded cross-team-damage fault).
// Cross-team non-interference is contracted where it can red for the right
// reason: the L3 real-machine cross-team case and owner-b's scoped src unit on
// the tombstone primitive. Fabricating it here would be a false red.

fn aid(id: &str) -> AgentId {
    AgentId::new(id)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct HealthRow {
    owner_team_id: Option<String>,
    status: String,
    last_output_at: Option<String>,
    context_usage_pct: Option<i64>,
    current_task_id: Option<String>,
}

struct RollbackFixture {
    team: PathBuf,
}

impl RollbackFixture {
    fn new(label: &str) -> Self {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "ta_lifecycle_rollback_{label}_{}_{}",
            std::process::id(),
            n
        ));
        if root.exists() {
            let _ = std::fs::remove_dir_all(&root);
        }
        std::fs::create_dir_all(&root).unwrap();
        Self {
            team: root.join("team"),
        }
    }

    fn seed_team(&self) {
        std::fs::create_dir_all(self.team.join("agents")).unwrap();
        std::fs::write(
            self.team.join("TEAM.md"),
            "---\nname: rollbackteam\nobjective: Lifecycle rollback contract.\nprovider: codex\n---\n\nRollback team.\n",
        )
        .unwrap();
        std::fs::write(
            self.team.join("agents/alpha.md"),
            agent_doc("alpha", "Alpha worker"),
        )
        .unwrap();
        std::fs::write(
            self.team.join("agents/bravo.md"),
            agent_doc("bravo", "Bravo worker"),
        )
        .unwrap();
        let spec = team_agent::compiler::compile_team(&self.team).expect("compile rollback team");
        let yaml = team_agent::model::yaml::dumps(&spec)
            .replace("default_assignee: \"alpha\"", "default_assignee: \"bravo\"")
            .replace("assign_to: \"alpha\"", "assign_to: \"bravo\"")
            .replace("assignee: \"alpha\"", "assignee: \"bravo\"");
        assert!(
            yaml.contains("id: \"alpha\"") && yaml.contains("id: \"bravo\""),
            "fixture must compile agent ids alpha/bravo before lifecycle calls; spec={yaml}"
        );
        std::fs::write(self.team.join("team.spec.yaml"), yaml).unwrap();
        // 0.4.6: seed real rollout files so fork tuple guard passes.
        std::fs::write(self.team.join("alpha-rollout.jsonl"), b"{}\n").unwrap();
        std::fs::write(self.team.join("bravo-rollout.jsonl"), b"{}\n").unwrap();
        save_runtime_state(
            &self.team,
            &json!({
                "session_name": "team-rollbackteam",
                "active_team_key": "rollbackteam",
                // 0.4.6 tuple-atomic contract: fork now requires complete
                // source tuple. Seed real rollout files + captured_at/via
                // so the fork-source backing guard passes and the test
                // reaches the rollback behaviour it actually asserts.
                "agents": {
                    "alpha": {
                        "status": "stopped",
                        "provider": "codex",
                        "auth_mode": "subscription",
                        "window": "alpha",
                        "session_id": "sess-alpha",
                        "rollout_path": self.team.join("alpha-rollout.jsonl").to_string_lossy(),
                        "captured_at": "2026-06-25T10:00:00+00:00",
                        "captured_via": "session.captured",
                        "owner_team_id": "rollbackteam"
                    },
                    "bravo": {
                        "status": "running",
                        "provider": "codex",
                        "auth_mode": "subscription",
                        "window": "bravo",
                        "session_id": "sess-bravo",
                        "rollout_path": self.team.join("bravo-rollout.jsonl").to_string_lossy(),
                        "captured_at": "2026-06-25T10:00:00+00:00",
                        "captured_via": "session.captured",
                        "owner_team_id": "rollbackteam"
                    }
                }
            }),
        )
        .unwrap();
        let _ = MessageStore::open(&self.team).unwrap();
    }

    fn seed_healthy_coordinator(&self) {
        let ws = WorkspacePath::new(self.team.clone());
        let pid = Pid::new(std::process::id());
        if let Some(parent) = coordinator_pid_path(&ws).parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(coordinator_pid_path(&ws), pid.to_string()).unwrap();
        write_coordinator_metadata(&ws, pid, MetadataSource::Start).unwrap();
    }

    fn seed_sibling_team_with_same_fork_id(&self) {
        let mut state = load_runtime_state(&self.team).unwrap();
        let selected = state.clone();
        state.as_object_mut().unwrap().insert(
            "teams".to_string(),
            json!({
                "rollbackteam": selected,
                "sibling": {
                    "agents": {
                        "newfork": {
                            "status": "running",
                            "provider": "codex",
                            "pane_id": "%sibling-newfork",
                            "owner_team_id": "sibling"
                        }
                    }
                }
            }),
        );
        save_runtime_state(&self.team, &state).unwrap();
        let selected =
            team_agent::state::projection::select_runtime_state(&self.team, Some("rollbackteam"))
                .unwrap();
        team_agent::state::projection::save_team_scoped_state(&self.team, &selected).unwrap();
    }

    fn seed_agent_health(
        &self,
        owner_team_id: &str,
        agent_id: &str,
        status: &str,
        last_output_at: Option<&str>,
        context_usage_pct: Option<i64>,
        current_task_id: Option<&str>,
    ) {
        let store = MessageStore::open(&self.team).unwrap();
        let conn = open_db(store.db_path()).unwrap();
        conn.execute(
            "insert or replace into agent_health(owner_team_id, agent_id, status, last_output_at, context_usage_pct, current_task_id, updated_at)
             values (?1, ?2, ?3, ?4, ?5, ?6, '2026-06-09T00:00:00Z')",
            params![
                owner_team_id,
                agent_id,
                status,
                last_output_at,
                context_usage_pct,
                current_task_id
            ],
        )
        .unwrap();
    }

    fn agent_health_row(&self, agent_id: &str) -> Option<HealthRow> {
        let store = MessageStore::open(&self.team).unwrap();
        let conn = open_db(store.db_path()).unwrap();
        conn.query_row(
            "select owner_team_id, status, last_output_at, context_usage_pct, current_task_id
             from agent_health where agent_id = ?1",
            [agent_id],
            |row| {
                Ok(HealthRow {
                    owner_team_id: row.get(0)?,
                    status: row.get(1)?,
                    last_output_at: row.get(2)?,
                    context_usage_pct: row.get(3)?,
                    current_task_id: row.get(4)?,
                })
            },
        )
        .ok()
    }

    fn spec_text(&self) -> String {
        std::fs::read_to_string(self.team.join("team.spec.yaml")).unwrap()
    }
}

fn agent_doc(name: &str, role: &str) -> String {
    format!(
        "---\nname: {name}\nrole: {role}\nprovider: codex\nauth_mode: subscription\ntools:\n  - mcp_team\n---\n\n{role}.\n"
    )
}

struct EnvGuard {
    key: &'static str,
    old: Option<String>,
}

impl EnvGuard {
    fn set(key: &'static str, value: &str) -> Self {
        let old = std::env::var(key).ok();
        std::env::set_var(key, value);
        Self { key, old }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        match &self.old {
            Some(value) => std::env::set_var(self.key, value),
            None => std::env::remove_var(self.key),
        }
    }
}

#[derive(Clone)]
struct RecordingTransport {
    session: String,
    windows: Arc<Mutex<BTreeSet<String>>>,
    killed: Arc<Mutex<Vec<String>>>,
}

impl RecordingTransport {
    fn new(session: &str, windows: &[&str]) -> Self {
        Self {
            session: session.to_string(),
            windows: Arc::new(Mutex::new(
                windows.iter().map(|window| (*window).to_string()).collect(),
            )),
            killed: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn killed(&self) -> Vec<String> {
        self.killed.lock().unwrap().clone()
    }

    fn record_spawn(
        &self,
        session: &SessionName,
        window: &WindowName,
    ) -> Result<SpawnResult, TransportError> {
        self.windows
            .lock()
            .unwrap()
            .insert(window.as_str().to_string());
        Ok(SpawnResult {
            pane_id: PaneId::new(format!("%{}", window.as_str())),
            session: session.clone(),
            window: window.clone(),
            child_pid: Some(4242),
        })
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
        self.record_spawn(session, window)
    }

    fn spawn_into(
        &self,
        session: &SessionName,
        window: &WindowName,
        _argv: &[String],
        _cwd: &Path,
        _env: &BTreeMap<String, String>,
    ) -> Result<SpawnResult, TransportError> {
        self.record_spawn(session, window)
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
            inject_verification: InjectVerification::EmptyTextSendKeys,
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
            text: String::new(),
            range,
        })
    }

    fn query(&self, _target: &Target, field: PaneField) -> Result<Option<String>, TransportError> {
        Ok(match field {
            PaneField::PaneId => Some("%0".to_string()),
            PaneField::PaneMode => None,
            PaneField::PaneWidth => Some("120".to_string()),
            PaneField::PaneCurrentPath => None,
            PaneField::PaneCurrentCommand => None,
            PaneField::SessionName => Some(self.session.clone()),
            PaneField::PaneTty => None,
        })
    }

    fn liveness(&self, _pane: &PaneId) -> Result<PaneLiveness, TransportError> {
        Ok(PaneLiveness::Live)
    }

    fn list_targets(&self) -> Result<Vec<PaneInfo>, TransportError> {
        Ok(self
            .windows
            .lock()
            .unwrap()
            .iter()
            .map(|window| PaneInfo {
                pane_id: PaneId::new(format!("%{window}")),
                session: SessionName::new(self.session.clone()),
                window_index: None,
                window_name: Some(WindowName::new(window.clone())),
                pane_index: None,
                tty: None,
                current_command: None,
                current_path: None,
                active: false,
                pane_pid: Some(4242),
                leader_env: BTreeMap::new(),
            })
            .collect())
    }

    fn has_session(&self, session: &SessionName) -> Result<bool, TransportError> {
        Ok(session.as_str() == self.session)
    }

    fn list_windows(&self, session: &SessionName) -> Result<Vec<WindowName>, TransportError> {
        if session.as_str() != self.session {
            return Ok(Vec::new());
        }
        Ok(self
            .windows
            .lock()
            .unwrap()
            .iter()
            .map(|window| WindowName::new(window.clone()))
            .collect())
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

    fn kill_window(&self, target: &Target) -> Result<(), TransportError> {
        let label = match target {
            Target::Pane(pane) => pane.as_str().to_string(),
            Target::SessionWindow { session, window } => {
                format!("{}:{}", session.as_str(), window.as_str())
            }
        };
        if let Some(window) = label.split(':').next_back() {
            self.windows.lock().unwrap().remove(window);
        }
        self.killed.lock().unwrap().push(label);
        Ok(())
    }

    fn attach_session(&self, _session: &SessionName) -> Result<AttachOutcome, TransportError> {
        Ok(AttachOutcome::Attached)
    }
}
