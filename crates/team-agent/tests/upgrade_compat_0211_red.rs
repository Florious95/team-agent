//! #249 Python 0.2.11 -> Rust 0.3.x local upgrade compatibility contracts.
//!
//! These are T0/T1 fixture-driven checks only: no provider subscription, no live
//! upgrade. They pin the state key, DB, delivery, restart, and MCP config seams
//! that must survive a Python 0.2.11 runtime handoff.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use rusqlite::params;
use serde_json::{json, Value};
use serial_test::serial;
use team_agent::cli::{
    cmd_collect_for_team, cmd_status_for_team, CollectArgs, CmdOutput, StatusArgs,
};
use team_agent::db::schema::{initialize_schema, open_db, table_layout, SCHEMA_VERSION};
use team_agent::event_log::EventLog;
use team_agent::lifecycle::restart::classify_restart_plan;
use team_agent::lifecycle::{restart_with_transport, ResumeDecision, StartMode};
use team_agent::message_store::MessageStore;
use team_agent::messaging::delivery::deliver_pending_message;
use team_agent::state::persist::save_runtime_state;
use team_agent::state::selector::{resolve_active_team, SelectorMode};
use team_agent::transport::{
    AttachOutcome, BackendKind, CaptureRange, CapturedText, InjectPayload, InjectReport,
    InjectStage, InjectVerification, Key, PaneField, PaneId, PaneInfo, PaneLiveness,
    SessionName, SetEnvOutcome, SpawnResult, SubmitVerification, Target, Transport,
    TransportError, TurnVerification, WindowName,
};

const TEAM_KEY: &str = "upgrade-key";
const LEGACY_NAME: &str = "legacy-name";
const SIBLING_KEY: &str = "sibling-key";

#[test]
#[serial(env)]
fn upgrade_0211_state_team_key_runtime_key_not_spec_name() {
    let fixture = UpgradeFixture::new("state-key");
    fixture.write_spec(TEAM_KEY, LEGACY_NAME);
    fixture.seed_dual_team_state(TEAM_KEY);

    let selected =
        resolve_active_team(&fixture.workspace, Some(TEAM_KEY), SelectorMode::RuntimeOnly)
            .expect("selector must accept the runtime team key");
    assert_eq!(
        selected.team_key, TEAM_KEY,
        "runtime team key comes from team_dir/spec_path state key, not spec.name"
    );
    assert_eq!(selected.state["active_team_key"], json!(TEAM_KEY));
    assert!(selected.state["agents"].get("upgrade_worker").is_some());

    let status = json_output(cmd_status_for_team(
        &StatusArgs {
            agent: None,
            workspace: fixture.workspace.clone(),
            detail: true,
            summary: false,
            json: true,
        },
        Some(TEAM_KEY),
    ));
    assert!(
        status.pointer("/agents/upgrade_worker").is_some(),
        "status --team upgrade-key must project the runtime-key team; status={status}"
    );
    assert!(
        status.pointer("/agents/sibling_worker").is_none(),
        "status --team upgrade-key must not collapse to sibling/spec-name state; status={status}"
    );
}

#[test]
#[serial(env)]
fn upgrade_0211_db_schema_v3_reads_and_preserves_rows() {
    let fixture = UpgradeFixture::new("db-v3");
    fixture.seed_python_v3_db();
    let before = fixture.table_counts(&["messages", "results", "agent_health"]);

    let store = MessageStore::open(&fixture.workspace).expect("Rust must open Python schema v3 DB");
    let after = fixture.table_counts(&["messages", "results", "agent_health"]);

    assert_eq!(before, after, "opening Python schema v3 must not drop rows");
    assert_eq!(
        pragma_user_version(store.db_path()),
        SCHEMA_VERSION,
        "schema version must remain readable as v3"
    );
}

#[test]
#[serial(env)]
fn upgrade_legacy_owner_team_id_delivery_projects_state_team() {
    let fixture = UpgradeFixture::new("delivery-runtime-key");
    fixture.seed_dual_team_state(SIBLING_KEY);
    let store = MessageStore::open(&fixture.workspace).unwrap();
    let message_id = store
        .create_message(None, "leader", "w1", "ping", None, false, Some(TEAM_KEY))
        .unwrap();
    let transport = RecordingTransport::new();
    let state = team_agent::state::persist::load_runtime_state(&fixture.workspace).unwrap();

    let outcome = deliver_pending_message(
        &fixture.workspace,
        &store,
        &transport,
        &message_id,
        &EventLog::new(&fixture.workspace),
        &state,
    )
    .expect("delivery should run");

    assert!(outcome.ok, "runtime-key owner row should deliver; outcome={outcome:?}");
    assert_eq!(
        transport.injected_windows(),
        vec![(format!("team-{TEAM_KEY}"), "upgrade-window".to_string())],
        "owner_team_id=upgrade-key must project state.teams[upgrade-key], not active sibling"
    );
}

#[test]
#[serial(env)]
fn upgrade_spec_name_owner_team_id_is_rejected_or_migrated() {
    let fixture = UpgradeFixture::new("delivery-legacy-name");
    fixture.seed_dual_team_state(SIBLING_KEY);
    let store = MessageStore::open(&fixture.workspace).unwrap();
    let message_id = store
        .create_message(None, "leader", "w1", "ping", None, false, Some(LEGACY_NAME))
        .unwrap();
    let transport = RecordingTransport::new();
    let state = team_agent::state::persist::load_runtime_state(&fixture.workspace).unwrap();

    let outcome = deliver_pending_message(
        &fixture.workspace,
        &store,
        &transport,
        &message_id,
        &EventLog::new(&fixture.workspace),
        &state,
    )
    .expect("delivery should return an explicit outcome");
    let owner_after = owner_team_id_for_message(&fixture.workspace, &message_id);
    let status_after = message_status(&fixture.workspace, &message_id);
    let migrated = owner_after.as_deref() == Some(TEAM_KEY);
    let explicitly_rejected = !outcome.ok && matches!(status_after.as_deref(), Some("failed" | "queued_until_rebind" | "rebind_required"));

    assert!(
        migrated || explicitly_rejected,
        "owner_team_id=legacy-name must be explicitly migrated/aliased to upgrade-key or rejected; \
         silent delivery is a misroute. outcome={outcome:?} owner_after={owner_after:?} \
         status_after={status_after:?} injected={:?}",
        transport.injected_windows()
    );
}

#[test]
#[serial(env)]
fn upgrade_status_collect_scope_by_selected_team_key() {
    let fixture = UpgradeFixture::new("status-collect-scope");
    fixture.write_spec(TEAM_KEY, LEGACY_NAME);
    fixture.seed_dual_team_state(TEAM_KEY);
    fixture.seed_python_v3_db();
    fixture.seed_result("res-upgrade", TEAM_KEY, "task-upgrade", "upgrade_worker", "success");
    fixture.seed_result("res-sibling", SIBLING_KEY, "task-sibling", "sibling_worker", "success");

    let status = json_output(cmd_status_for_team(
        &StatusArgs {
            agent: None,
            workspace: fixture.workspace.clone(),
            detail: true,
            summary: false,
            json: true,
        },
        Some(TEAM_KEY),
    ));
    assert_eq!(
        status.pointer("/results/total").and_then(Value::as_i64),
        Some(2),
        "status --team upgrade-key must count only upgrade-key results and exclude sibling rows; status={status}"
    );
    assert_eq!(
        status.pointer("/messages/accepted").and_then(Value::as_i64),
        Some(1),
        "status --team upgrade-key must count only upgrade-key messages; status={status}"
    );

    let collect = json_output(cmd_collect_for_team(
        &CollectArgs {
            workspace: fixture.workspace.clone(),
            result_file: None,
            json: true,
        },
        Some(TEAM_KEY),
    ));
    let empty = Vec::new();
    let collected_ids = collect
        .get("collected_results")
        .and_then(Value::as_array)
        .unwrap_or(&empty)
        .iter()
        .filter_map(|item| item.get("result_id").and_then(Value::as_str))
        .collect::<Vec<_>>();
    assert!(
        collected_ids.contains(&"res-existing-upgrade")
            && collected_ids.contains(&"res-upgrade")
            && !collected_ids.contains(&"res-sibling"),
        "collect --team upgrade-key must collect only selected runtime-key rows; collect={collect}"
    );
}

#[test]
fn upgrade_restart_plan_uses_persisted_session_id() {
    let dir = tmp_dir("restart-plan-rollout");
    let rollout = dir.join("rollout.jsonl");
    std::fs::write(&rollout, "{}\n").unwrap();
    let state = json!({
        "agents": {
            "w1": {
                "status": "running",
                "provider": "codex",
                "session_id": "sess-upgrade",
                "rollout_path": rollout.to_string_lossy(),
                "first_send_at": "2026-06-07T01:00:00+00:00"
            }
        }
    });

    let plan = classify_restart_plan(&state, false).expect("restart plan");
    assert_eq!(plan.decisions[0].decision, ResumeDecision::Resume);
    assert_eq!(plan.decisions[0].restart_mode, StartMode::Resumed);
    assert_eq!(plan.decisions[0].session_id.as_ref().map(|s| s.as_str()), Some("sess-upgrade"));
}

#[test]
fn upgrade_restart_refuses_interacted_worker_without_session_id() {
    let state = json!({
        "agents": {
            "w1": {
                "status": "running",
                "provider": "codex",
                "session_id": null,
                "first_send_at": "2026-06-07T01:00:00+00:00"
            }
        }
    });

    let refused = classify_restart_plan(&state, false).expect("restart plan");
    assert_eq!(refused.decisions[0].decision, ResumeDecision::Refuse);
    assert_eq!(refused.unresumable[0].reason, "no_persisted_session_id");

    let allowed = classify_restart_plan(&state, true).expect("restart plan with allow_fresh");
    assert_eq!(allowed.decisions[0].decision, ResumeDecision::FreshStart);
    assert!(
        allowed.unresumable.is_empty(),
        "allow_fresh is the explicit opt-in to fresh restart for interacted workers"
    );
}

#[test]
#[serial(env)]
fn upgrade_rust_mcp_config_uses_current_binary_and_runtime_key() {
    let fixture = UpgradeFixture::new("restart-mcp");
    fixture.write_spec(TEAM_KEY, LEGACY_NAME);
    fixture.seed_restartable_state();
    fixture.seed_healthy_coordinator();
    let transport = RecordingTransport::new().with_session_present(false);

    let report = restart_with_transport(&fixture.team_dir, false, Some(TEAM_KEY), &transport)
        .expect("restart should spawn resumable worker");
    let spawn = transport.single_spawn();
    let command_line = spawn.command_line();
    let current_exe = std::env::current_exe().unwrap().to_string_lossy().to_string();

    assert!(
        format!("{report:?}").contains("Resumed"),
        "restart report must show resume path for persisted session_id; report={report:?}"
    );
    assert!(
        command_line.contains(&current_exe),
        "restarted worker MCP command must use the current Rust candidate binary path; current_exe={current_exe} command_line={command_line:?}"
    );
    assert!(
        command_line.contains("TEAM_AGENT_OWNER_TEAM_ID=upgrade-key")
            && command_line.contains("TEAM_AGENT_OWNER_TEAM_ID=\"upgrade-key\""),
        "spawn env and MCP config must both use runtime team key upgrade-key; command_line={command_line:?}"
    );
    assert!(
        spawn
            .argv
            .windows(2)
            .any(|pair| pair[0] == "codex" && pair[1] == "resume")
            && spawn.argv.iter().any(|arg| arg == "sess-upgrade"),
        "restart must resume persisted provider session, not fresh start; command_line={command_line:?}"
    );
}

#[test]
#[serial(env)]
fn upgrade_db_layout_rebuild_preserves_owner_team_id() {
    let fixture = UpgradeFixture::new("db-rebuild-owner");
    fixture.seed_legacy_drift_db_owner_last();

    let before = distinct_owner_ids(&fixture.db_path(), "messages");
    let store = MessageStore::open(&fixture.workspace).expect("open drifted legacy DB");
    let after = distinct_owner_ids(store.db_path(), "messages");

    assert_eq!(before, vec![TEAM_KEY.to_string()]);
    assert_eq!(after, before, "layout rebuild must preserve owner_team_id values");
    assert_eq!(
        table_layout(&open_db(store.db_path()).unwrap(), "messages").unwrap()[..2],
        ["message_id".to_string(), "owner_team_id".to_string()],
        "messages layout must rebuild to canonical owner_team_id position"
    );
}

struct UpgradeFixture {
    workspace: PathBuf,
    team_dir: PathBuf,
}

impl UpgradeFixture {
    fn new(tag: &str) -> Self {
        let root = tmp_dir(tag);
        let workspace = root.join("ws");
        let team_dir = workspace.join(TEAM_KEY);
        std::fs::create_dir_all(&team_dir).unwrap();
        std::fs::create_dir_all(team_agent::model::paths::runtime_dir(&workspace)).unwrap();
        Self { workspace, team_dir }
    }

    fn write_spec(&self, team_key: &str, spec_name: &str) {
        std::fs::write(
            self.team_dir.join("TEAM.md"),
            format!(
                "---\nname: {spec_name}\nobjective: upgrade compat\nprovider: codex\n---\n\nTeam.\n"
            ),
        )
        .unwrap();
        std::fs::create_dir_all(self.team_dir.join("agents")).unwrap();
        std::fs::write(
            self.team_dir.join("agents").join("upgrade_worker.md"),
            "---\nname: upgrade_worker\nrole: Upgrade Worker\nprovider: codex\nmodel: gpt-5.5\nauth_mode: subscription\ntools:\n  - mcp_team\n---\n\nWorker.\n",
        )
        .unwrap();
        let spec = team_agent::compiler::compile_team(&self.team_dir).unwrap();
        assert_eq!(team_key, TEAM_KEY, "fixture uses constant runtime key");
        std::fs::write(
            self.team_dir.join("team.spec.yaml"),
            team_agent::model::yaml::dumps(&spec),
        )
        .unwrap();
    }

    fn seed_dual_team_state(&self, active: &str) {
        save_runtime_state(&self.workspace, &self.dual_team_state(active)).unwrap();
    }

    fn seed_restartable_state(&self) {
        std::fs::write(self.workspace.join("rollout.jsonl"), "{}\n").unwrap();
        save_runtime_state(
            &self.workspace,
            &json!({
                "active_team_key": TEAM_KEY,
                "team_dir": self.team_dir.to_string_lossy(),
                "spec_path": self.team_dir.join("team.spec.yaml").to_string_lossy(),
                "session_name": format!("team-{TEAM_KEY}"),
                "agents": {
                    "upgrade_worker": {
                        "status": "running",
                        "provider": "codex",
                        "role": "Upgrade Worker",
                        "tools": ["mcp_team"],
                        "window": "upgrade_worker",
                        "owner_team_id": TEAM_KEY,
                        "session_id": "sess-upgrade",
                        "first_send_at": "2026-06-07T01:00:00+00:00",
                        "rollout_path": self.workspace.join("rollout.jsonl").to_string_lossy(),
                        "spawn_cwd": self.team_dir.to_string_lossy()
                    }
                },
                "tasks": [],
                "teams": {
                    TEAM_KEY: {
                        "status": "alive",
                        "team_dir": self.team_dir.to_string_lossy(),
                        "spec_path": self.team_dir.join("team.spec.yaml").to_string_lossy(),
                        "session_name": format!("team-{TEAM_KEY}"),
                        "agents": {
                            "upgrade_worker": {
                                "status": "running",
                                "provider": "codex",
                                "role": "Upgrade Worker",
                                "tools": ["mcp_team"],
                                "window": "upgrade_worker",
                                "owner_team_id": TEAM_KEY,
                                "session_id": "sess-upgrade",
                                "first_send_at": "2026-06-07T01:00:00+00:00",
                                "rollout_path": self.workspace.join("rollout.jsonl").to_string_lossy(),
                                "spawn_cwd": self.team_dir.to_string_lossy()
                            }
                        },
                        "tasks": []
                    }
                }
            }),
        )
        .unwrap();
    }

    fn dual_team_state(&self, active: &str) -> Value {
        let upgrade = json!({
            "status": "alive",
            "team_dir": self.team_dir.to_string_lossy(),
            "spec_path": self.team_dir.join("team.spec.yaml").to_string_lossy(),
            "session_name": format!("team-{TEAM_KEY}"),
            "team": {"name": LEGACY_NAME},
            "agents": {
                "upgrade_worker": {"status": "running", "provider": "fake", "window": "upgrade-window", "owner_team_id": TEAM_KEY},
                "w1": {"status": "running", "provider": "fake", "window": "upgrade-window", "owner_team_id": TEAM_KEY}
            },
            "tasks": [{"id": "task-upgrade", "assignee": "upgrade_worker", "status": "pending"}]
        });
        let sibling = json!({
            "status": "alive",
            "team_dir": self.workspace.join(SIBLING_KEY).to_string_lossy(),
            "spec_path": self.workspace.join(SIBLING_KEY).join("team.spec.yaml").to_string_lossy(),
            "session_name": format!("team-{SIBLING_KEY}"),
            "agents": {
                "sibling_worker": {"status": "running", "provider": "fake", "window": "sibling-window", "owner_team_id": SIBLING_KEY},
                "w1": {"status": "running", "provider": "fake", "window": "sibling-window", "owner_team_id": SIBLING_KEY}
            },
            "tasks": [{"id": "task-sibling", "assignee": "sibling_worker", "status": "pending"}]
        });
        let active_state = if active == TEAM_KEY { upgrade.clone() } else { sibling.clone() };
        let mut state = active_state;
        let obj = state.as_object_mut().unwrap();
        obj.insert("active_team_key".to_string(), json!(active));
        obj.insert("teams".to_string(), json!({TEAM_KEY: upgrade, SIBLING_KEY: sibling}));
        state
    }

    fn db_path(&self) -> PathBuf {
        self.workspace.join(".team").join("runtime").join("team.db")
    }

    fn seed_python_v3_db(&self) {
        std::fs::create_dir_all(self.db_path().parent().unwrap()).unwrap();
        let conn = open_db(&self.db_path()).unwrap();
        initialize_schema(&conn, Some(&self.db_path())).unwrap();
        conn.execute_batch(&format!("pragma user_version = {SCHEMA_VERSION};")).unwrap();
        conn.execute(
            "insert into messages(message_id, owner_team_id, sender, recipient, requires_ack, status, content, artifact_refs, created_at, updated_at, delivery_attempts)
             values ('msg-upgrade', ?1, 'leader', 'upgrade_worker', 0, 'accepted', 'hello', '[]', '2026-06-07T00:00:00+00:00', '2026-06-07T00:00:00+00:00', 0),
                    ('msg-sibling', ?2, 'leader', 'sibling_worker', 0, 'accepted', 'hello', '[]', '2026-06-07T00:00:01+00:00', '2026-06-07T00:00:01+00:00', 0)",
            params![TEAM_KEY, SIBLING_KEY],
        )
        .unwrap();
        self.seed_result("res-existing-upgrade", TEAM_KEY, "task-upgrade", "upgrade_worker", "success");
        conn.execute(
            "insert or replace into agent_health(owner_team_id, agent_id, status, updated_at)
             values (?1, 'upgrade_worker', 'running', '2026-06-07T00:00:00+00:00'),
                    (?2, 'sibling_worker', 'running', '2026-06-07T00:00:00+00:00')",
            params![TEAM_KEY, SIBLING_KEY],
        )
        .unwrap();
    }

    fn seed_result(&self, result_id: &str, owner_team_id: &str, task_id: &str, agent_id: &str, status: &str) {
        let conn = open_db(&self.db_path()).unwrap();
        let envelope = json!({
            "schema_version": "result_envelope_v1",
            "result_id": result_id,
            "task_id": task_id,
            "agent_id": agent_id,
            "status": status,
            "summary": "done",
            "changes": [], "tests": [], "risks": [], "artifacts": [], "next_actions": []
        });
        conn.execute(
            "insert or replace into results(result_id, owner_team_id, task_id, agent_id, envelope, status, created_at)
             values (?1, ?2, ?3, ?4, ?5, ?6, '2026-06-07T00:00:00+00:00')",
            params![result_id, owner_team_id, task_id, agent_id, envelope.to_string(), status],
        )
        .unwrap();
    }

    fn seed_legacy_drift_db_owner_last(&self) {
        std::fs::create_dir_all(self.db_path().parent().unwrap()).unwrap();
        let conn = rusqlite::Connection::open(self.db_path()).unwrap();
        conn.execute_batch(
            "pragma user_version=1;
             create table messages (message_id text primary key, task_id text, sender text, recipient text, reply_to text, requires_ack integer, status text, content text, artifact_refs text, created_at text, updated_at text, delivered_at text, acknowledged_at text, error text, delivery_attempts integer not null default 0, owner_team_id text);
             create table results (result_id text primary key, task_id text not null, agent_id text not null, envelope text not null, status text not null, created_at text not null, owner_team_id text);
             insert into messages(message_id, status, owner_team_id) values ('msg-drift','accepted','upgrade-key');
             insert into results(result_id, task_id, agent_id, envelope, status, created_at, owner_team_id) values ('res-drift','task-upgrade','upgrade_worker','{}','success','2026-06-07T00:00:00+00:00','upgrade-key');",
        )
        .unwrap();
    }

    fn seed_healthy_coordinator(&self) {
        let workspace = team_agent::coordinator::WorkspacePath::new(self.workspace.clone());
        std::fs::create_dir_all(team_agent::model::paths::runtime_dir(workspace.as_path())).unwrap();
        let _ = MessageStore::open(workspace.as_path()).unwrap();
        let pid = team_agent::coordinator::Pid::new(std::process::id());
        team_agent::coordinator::write_coordinator_metadata(
            &workspace,
            pid,
            team_agent::coordinator::MetadataSource::Boot,
        )
        .unwrap();
        std::fs::write(team_agent::coordinator::coordinator_pid_path(&workspace), pid.to_string()).unwrap();
    }

    fn table_counts(&self, tables: &[&str]) -> BTreeMap<String, i64> {
        let conn = open_db(&self.db_path()).unwrap();
        tables
            .iter()
            .map(|table| {
                let count = conn
                    .query_row(&format!("select count(*) from {table}"), [], |row| row.get(0))
                    .unwrap();
                ((*table).to_string(), count)
            })
            .collect()
    }
}

#[derive(Debug, Clone)]
struct RecordedSpawn {
    argv: Vec<String>,
    env: BTreeMap<String, String>,
}

impl RecordedSpawn {
    fn command_line(&self) -> String {
        let mut parts = self
            .env
            .iter()
            .map(|(key, value)| format!("{key}={value}"))
            .collect::<Vec<_>>();
        parts.extend(self.argv.iter().cloned());
        parts.join(" ")
    }
}

#[derive(Debug, Default)]
struct RecordingTransport {
    spawns: Mutex<Vec<RecordedSpawn>>,
    injected: Mutex<Vec<Target>>,
    session_present: bool,
}

impl RecordingTransport {
    fn new() -> Self {
        Self::default()
    }

    fn with_session_present(mut self, present: bool) -> Self {
        self.session_present = present;
        self
    }

    fn single_spawn(&self) -> RecordedSpawn {
        let spawns = self.spawns.lock().unwrap();
        assert_eq!(spawns.len(), 1, "expected one spawn; spawns={spawns:?}");
        spawns[0].clone()
    }

    fn injected_windows(&self) -> Vec<(String, String)> {
        self.injected
            .lock()
            .unwrap()
            .iter()
            .filter_map(|target| match target {
                Target::SessionWindow { session, window } => {
                    Some((session.as_str().to_string(), window.as_str().to_string()))
                }
                Target::Pane(pane) => Some(("pane".to_string(), pane.as_str().to_string())),
            })
            .collect()
    }

    fn spawn_result(
        &self,
        session: &SessionName,
        window: &WindowName,
        argv: &[String],
        env: &BTreeMap<String, String>,
    ) -> SpawnResult {
        let mut spawns = self.spawns.lock().unwrap();
        spawns.push(RecordedSpawn {
            argv: argv.to_vec(),
            env: env.clone(),
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
        argv: &[String],
        _cwd: &Path,
        env: &BTreeMap<String, String>,
    ) -> Result<SpawnResult, TransportError> {
        Ok(self.spawn_result(session, window, argv, env))
    }

    fn spawn_into(
        &self,
        session: &SessionName,
        window: &WindowName,
        argv: &[String],
        _cwd: &Path,
        env: &BTreeMap<String, String>,
    ) -> Result<SpawnResult, TransportError> {
        Ok(self.spawn_result(session, window, argv, env))
    }

    fn inject(
        &self,
        target: &Target,
        _payload: &InjectPayload,
        _submit: Key,
        _bracketed: bool,
    ) -> Result<InjectReport, TransportError> {
        self.injected.lock().unwrap().push(target.clone());
        Ok(InjectReport {
            stage_reached: InjectStage::Submit,
            inject_verification: InjectVerification::CaptureContainsToken,
            submit_verification: SubmitVerification::EnterSentWithoutPlaceholderCheck,
            turn_verification: TurnVerification::NotYetObserved,
            attempts: 1,
        })
    }

    fn send_keys(&self, _target: &Target, _keys: &[Key]) -> Result<(), TransportError> {
        Ok(())
    }

    fn capture(&self, _target: &Target, range: CaptureRange) -> Result<CapturedText, TransportError> {
        Ok(CapturedText { text: String::new(), range })
    }

    fn query(&self, _target: &Target, field: PaneField) -> Result<Option<String>, TransportError> {
        match field {
            PaneField::PaneWidth => Ok(Some("120".to_string())),
            _ => Ok(None),
        }
    }

    fn liveness(&self, _pane: &PaneId) -> Result<PaneLiveness, TransportError> {
        Ok(PaneLiveness::Live)
    }

    fn has_pane(&self, pane: &PaneId) -> Result<Option<bool>, TransportError> {
        let spawns = self.spawns.lock().unwrap();
        Ok(Some(spawns.iter().enumerate().any(|(idx, _)| {
            pane.as_str() == format!("%{}", idx + 1)
        })))
    }

    fn list_targets(&self) -> Result<Vec<PaneInfo>, TransportError> {
        Ok(Vec::new())
    }

    fn has_session(&self, _session: &SessionName) -> Result<bool, TransportError> {
        Ok(self.session_present || !self.spawns.lock().unwrap().is_empty())
    }

    fn list_windows(&self, _session: &SessionName) -> Result<Vec<WindowName>, TransportError> {
        Ok(Vec::new())
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

fn json_output(result: Result<team_agent::cli::CmdResult, team_agent::cli::CliError>) -> Value {
    match result.expect("CLI command should return a result").output {
        CmdOutput::Json(value) => value,
        other => panic!("expected JSON output, got {other:?}"),
    }
}

fn owner_team_id_for_message(workspace: &Path, message_id: &str) -> Option<String> {
    let store = MessageStore::open(workspace).unwrap();
    let conn = open_db(store.db_path()).unwrap();
    conn.query_row(
        "select owner_team_id from messages where message_id=?1",
        params![message_id],
        |row| row.get(0),
    )
    .unwrap()
}

fn message_status(workspace: &Path, message_id: &str) -> Option<String> {
    let store = MessageStore::open(workspace).unwrap();
    let conn = open_db(store.db_path()).unwrap();
    conn.query_row(
        "select status from messages where message_id=?1",
        params![message_id],
        |row| row.get(0),
    )
    .unwrap()
}

fn distinct_owner_ids(db_path: &Path, table: &str) -> Vec<String> {
    let conn = open_db(db_path).unwrap();
    let mut stmt = conn
        .prepare(&format!(
            "select distinct owner_team_id from {table} where owner_team_id is not null order by owner_team_id"
        ))
        .unwrap();
    stmt.query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
}

fn pragma_user_version(db_path: &Path) -> i64 {
    open_db(db_path)
        .unwrap()
        .query_row("pragma user_version", [], |row| row.get(0))
        .unwrap()
}

fn tmp_dir(tag: &str) -> PathBuf {
    static N: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "ta-rs-upgrade-compat-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::canonicalize(dir).unwrap()
}
