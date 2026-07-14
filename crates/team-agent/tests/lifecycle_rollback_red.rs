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
use team_agent::lifecycle::{fork_agent_with_transport, remove_agent_with_transport};
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

#[test]
#[serial(env)]
fn remove_agent_rollback_restores_agent_health_after_post_delete_failure() {
    let _env = EnvGuard::set(FAIL_REMOVE_AFTER_HEALTH_DELETE, "rollback-contract");
    let fixture = RollbackFixture::new("remove-health");
    fixture.seed_team();
    fixture.seed_agent_health(
        "rollbackteam",
        "alpha",
        "BUSY",
        Some("2026-06-09T01:02:03Z"),
        Some(77),
        Some("task-alpha"),
    );
    let before = fixture.agent_health_row("alpha");
    let transport = RecordingTransport::new("team-rollbackteam", &["bravo"]);

    let result =
        remove_agent_with_transport(&fixture.team, &aid("alpha"), true, false, None, &transport);
    let after = fixture.agent_health_row("alpha");

    assert!(
        result.is_err(),
        "failure-injection hook {FAIL_REMOVE_AFTER_HEALTH_DELETE}=rollback-contract must fail after deleting agent_health so rollback is exercised; result={result:?} before={before:?} after={after:?}"
    );
    assert_eq!(
        after, before,
        "remove-agent rollback must restore the exact pre-operation agent_health row, including owner_team_id/status/last_output/context/task; result={result:?}"
    );
}

#[test]
#[serial(env)]
fn fork_agent_rollback_cleans_spawned_window_spec_state_and_mcp_after_post_spawn_failure() {
    let _env = EnvGuard::set(FAIL_FORK_AFTER_SPAWN, "save_runtime_state");
    let fixture = RollbackFixture::new("fork-post-spawn");
    fixture.seed_team();
    fixture.seed_healthy_coordinator();
    let spec_before = fixture.spec_text();
    let state_before = load_runtime_state(&fixture.team).unwrap();
    let transport = RecordingTransport::new("team-rollbackteam", &["alpha", "bravo"]);

    let result = fork_agent_with_transport(
        &fixture.team,
        &aid("alpha"),
        &aid("newfork"),
        None,
        false,
        None,
        &transport,
    );
    let spec_after = fixture.spec_text();
    let state_after = load_runtime_state(&fixture.team).unwrap();
    let mcp_path = fixture.team.join(".team/runtime/mcp/newfork.json");

    assert!(
        result.is_err(),
        "failure-injection hook {FAIL_FORK_AFTER_SPAWN}=save_runtime_state must fail after spawn so post-spawn rollback is exercised; result={result:?}"
    );
    assert!(
        transport
            .killed()
            .contains(&"team-rollbackteam:newfork".to_string()),
        "fork post-spawn rollback must kill the already-spawned window; killed={:?} result={result:?}",
        transport.killed()
    );
    assert_eq!(
        spec_after, spec_before,
        "fork post-spawn rollback must restore team.spec.yaml byte-for-byte; result={result:?}"
    );
    assert_eq!(
        state_after, state_before,
        "fork post-spawn rollback must restore runtime state and leave no half-created agent; result={result:?}"
    );
    assert!(
        !mcp_path.exists(),
        "fork post-spawn rollback must cleanup the MCP config written for the half-created agent; leaked={}",
        mcp_path.display()
    );
}

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
