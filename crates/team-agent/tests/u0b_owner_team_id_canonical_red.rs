#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[path = "support/hermetic.rs"]
mod hermetic_guard;
#[allow(dead_code)]
fn _hermetic_boundary_marker(_: &hermetic_guard::HermeticTestEnv) {}

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use rusqlite::params;
use serde_json::{json, Value};
use serial_test::serial;
use team_agent::event_log::EventLog;
use team_agent::message_store::MessageStore;
use team_agent::messaging::deliver_pending_message;
use team_agent::messaging::results::collect_for_team;
use team_agent::state::persist::{load_runtime_state, save_runtime_state};
use team_agent::tmux_backend::TmuxBackend;

#[test]
#[serial(env)]
fn launch_writes_worker_owner_team_id_from_runtime_spec_key_not_stale_active_team() {
    let case = RealLaunchCase::new("u0b-owner-canonical");
    let _path = install_codex_stub(&case.root);
    seed_stale_active_multiteam_state(&case.root);
    let spec_path = write_runtime_spec(&case.root, "bravo", "worker_a");

    team_agent::lifecycle::launch_with_transport_in_workspace(
        &case.root,
        &spec_path,
        false,
        true,
        true,
        &case.backend,
    )
    .expect("real launch should spawn the bravo worker");

    let state = load_runtime_state(&case.root).expect("runtime state after launch");
    let mcp_config = state
        .pointer("/teams/bravo/agents/worker_a/mcp_config")
        .and_then(Value::as_str)
        .unwrap_or_else(|| {
            panic!("bravo worker must be persisted under teams.bravo; state={state}")
        });
    let owner = worker_mcp_owner_team_id(Path::new(mcp_config));
    assert_eq!(
        owner.as_deref(),
        Some("bravo"),
        "worker MCP env must use the launched runtime team key, not stale active_team_key=alpha; state={state}"
    );

    insert_result_with_owner(&case.root, "res-u0b-bravo", owner.as_deref().unwrap());
    collect_for_team(&case.root, None, false, Some("bravo"))
        .expect("collect scoped by canonical bravo should see the worker result");
    assert_eq!(
        result_status(&case.root, "res-u0b-bravo").as_deref(),
        Some("collected"),
        "result written through worker MCP owner_team_id must be collectible by canonical key"
    );
}

#[test]
fn legacy_alias_projection_is_read_only_and_does_not_rewrite_owner_team_rows() {
    let root = tmp_dir("u0b-readonly-alias");
    seed_unambiguous_alias_state(&root);
    let store = MessageStore::open(&root).unwrap();
    let message_id = store
        .create_message(
            None,
            "leader",
            "worker_a",
            "alias ping",
            None,
            false,
            Some("legacy-name"),
        )
        .unwrap();
    let transport = AliasDeliveryTransport;
    let state = load_runtime_state(&root).unwrap();

    let outcome = deliver_pending_message(
        &root,
        &store,
        &transport,
        &message_id,
        &EventLog::new(&root),
        &state,
    )
    .expect("legacy alias delivery should return an explicit outcome");

    assert!(
        outcome.ok,
        "legacy alias should still project to its canonical team; outcome={outcome:?}"
    );
    assert_eq!(
        message_owner_team_id(&root, &message_id).as_deref(),
        Some("legacy-name"),
        "read-side compatibility must not UPDATE owner_team_id rows; the write side is the canonical source"
    );
    assert!(
        events_text(&root).contains("owner_team_id.compatibility_alias_detected"),
        "read-only alias handling must leave an audit event instead of silently mutating DB rows"
    );
}

struct RealLaunchCase {
    root: PathBuf,
    backend: TmuxBackend,
}

impl RealLaunchCase {
    fn new(tag: &str) -> Self {
        let root = tmp_dir(tag);
        let backend = TmuxBackend::for_workspace(&root);
        Self { root, backend }
    }
}

impl Drop for RealLaunchCase {
    fn drop(&mut self) {
        let _ = self.backend.kill_server();
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn tmp_dir(tag: &str) -> PathBuf {
    static N: AtomicU64 = AtomicU64::new(0);
    let path = std::env::temp_dir().join(format!(
        "ta-rs-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).unwrap();
    std::fs::canonicalize(path).unwrap()
}

fn seed_stale_active_multiteam_state(root: &Path) {
    let state = json!({
        "active_team_key": "alpha",
        "session_name": "team-alpha",
        "agents": {},
        "tasks": [{"id": "task_initial", "assignee": "worker_a", "status": "pending", "summary": "Initial task"}],
        "teams": {
            "alpha": {
                "status": "alive",
                "session_name": "team-alpha",
                "spec_name": "shared-template",
                "legacy_aliases": ["shared-template"],
                "agents": {},
                "tasks": [{"id": "task_initial", "assignee": "worker_a", "status": "pending", "summary": "Initial task"}]
            },
            "bravo": {
                "status": "alive",
                "session_name": "team-bravo",
                "spec_name": "shared-template",
                "legacy_aliases": ["shared-template"],
                "agents": {},
                "tasks": [{"id": "task_initial", "assignee": "worker_a", "status": "pending", "summary": "Initial task"}]
            }
        }
    });
    save_runtime_state(root, &state).unwrap();
    let _ = MessageStore::open(root).unwrap();
}

fn seed_unambiguous_alias_state(root: &Path) {
    let state = json!({
        "active_team_key": "teamA",
        "session_name": "team-teamA",
        "agents": {
            "worker_a": {"status": "running", "provider": "fake", "window": "worker_a"}
        },
        "tasks": [],
        "teams": {
            "teamA": {
                "status": "alive",
                "session_name": "team-teamA",
                "spec_name": "legacy-name",
                "legacy_aliases": ["legacy-name"],
                "agents": {
                    "worker_a": {"status": "running", "provider": "fake", "window": "worker_a"}
                },
                "tasks": []
            }
        }
    });
    save_runtime_state(root, &state).unwrap();
}

fn write_runtime_spec(root: &Path, team_key: &str, agent_id: &str) -> PathBuf {
    let spec_path = root
        .join(".team")
        .join("runtime")
        .join(team_key)
        .join("team.spec.yaml");
    std::fs::create_dir_all(spec_path.parent().unwrap()).unwrap();
    let spec_text = format!(
        r#"version: 1
team:
  name: "shared-template"
  mode: "supervisor_worker"
  objective: "U0B owner-team canonical contract"
  workspace: "{workspace}"
leader:
  id: "leader"
  role: "leader"
  provider: "codex"
  tools: ["mcp_team"]
agents:
  - id: "{agent_id}"
    role: "Worker"
    provider: "codex"
    auth_mode: "subscription"
    working_directory: "{workspace}"
    system_prompt:
      inline: "Worker."
      file: null
    tools: ["mcp_team"]
    permission_mode: "restricted"
routing:
  default_assignee: "{agent_id}"
  rules:
    - id: "route-{agent_id}"
      match:
        assignee: ["{agent_id}"]
      assign_to: "{agent_id}"
      priority: 10
communication:
  protocol: "mcp_inbox"
  topology: "leader_centered"
  worker_to_worker: true
  result_format: "result_envelope_v1"
  message_store:
    sqlite: ".team/runtime/team.db"
runtime:
  backend: "tmux"
  display_backend: "none"
  session_name: "team-{team_key}"
  auto_launch: true
  startup_order: ["{agent_id}"]
  dangerous_auto_approve: false
tasks:
  - id: "task_initial"
    title: "Initial task"
    type: "implementation"
    assignee: "{agent_id}"
    status: "pending"
    acceptance: ["Worker reports valid result_envelope_v1"]
"#,
        workspace = root.display(),
    );
    std::fs::write(&spec_path, &spec_text).unwrap();
    std::fs::write(root.join("team.spec.yaml"), &spec_text).unwrap();
    spec_path
}

fn install_codex_stub(root: &Path) -> EnvGuard {
    let bin = root.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let codex = bin.join("codex");
    std::fs::write(
        &codex,
        "#!/bin/sh\nstty -echo 2>/dev/null || true\nexec cat\n",
    )
    .unwrap();
    let mut perms = std::fs::metadata(&codex).unwrap().permissions();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        perms.set_mode(0o755);
    }
    std::fs::set_permissions(&codex, perms).unwrap();
    let old = std::env::var("PATH").ok();
    let mut next = bin.to_string_lossy().to_string();
    if let Some(old_path) = old.as_deref().filter(|path| !path.is_empty()) {
        next.push(':');
        next.push_str(old_path);
    }
    std::env::set_var("PATH", next);
    EnvGuard { key: "PATH", old }
}

struct EnvGuard {
    key: &'static str,
    old: Option<String>,
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        if let Some(value) = &self.old {
            std::env::set_var(self.key, value);
        } else {
            std::env::remove_var(self.key);
        }
    }
}

fn worker_mcp_owner_team_id(path: &Path) -> Option<String> {
    let value: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    value
        .pointer("/mcpServers/team_orchestrator/env/TEAM_AGENT_OWNER_TEAM_ID")
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn insert_result_with_owner(root: &Path, result_id: &str, owner_team_id: &str) {
    let store = MessageStore::open(root).unwrap();
    let conn = team_agent::db::schema::open_db(store.db_path()).unwrap();
    let envelope = json!({
        "schema_version": "result_envelope_v1",
        "result_id": result_id,
        "task_id": "task_initial",
        "agent_id": "worker_a",
        "status": "success",
        "summary": "done",
        "changes": [],
        "tests": [],
        "risks": [],
        "artifacts": [],
        "next_actions": []
    });
    conn.execute(
        "insert into results(result_id, owner_team_id, task_id, agent_id, envelope, status, created_at)
         values (?1, ?2, 'task_initial', 'worker_a', ?3, 'success', ?4)",
        params![result_id, owner_team_id, envelope.to_string(), chrono::Utc::now().to_rfc3339()],
    )
    .unwrap();
}

fn result_status(root: &Path, result_id: &str) -> Option<String> {
    let store = MessageStore::open(root).unwrap();
    let conn = team_agent::db::schema::open_db(store.db_path()).unwrap();
    conn.query_row(
        "select status from results where result_id = ?1",
        [result_id],
        |row| row.get(0),
    )
    .ok()
}

fn message_owner_team_id(root: &Path, message_id: &str) -> Option<String> {
    let store = MessageStore::open(root).unwrap();
    let conn = team_agent::db::schema::open_db(store.db_path()).unwrap();
    conn.query_row(
        "select owner_team_id from messages where message_id = ?1",
        [message_id],
        |row| row.get(0),
    )
    .ok()
}

fn events_text(root: &Path) -> String {
    std::fs::read_to_string(root.join(".team").join("logs").join("events.jsonl"))
        .unwrap_or_default()
}

struct AliasDeliveryTransport;

impl team_agent::transport::Transport for AliasDeliveryTransport {
    fn kind(&self) -> team_agent::transport::BackendKind {
        team_agent::transport::BackendKind::Tmux
    }

    fn spawn_first(
        &self,
        _session: &team_agent::transport::SessionName,
        _window: &team_agent::transport::WindowName,
        _argv: &[String],
        _cwd: &Path,
        _env: &BTreeMap<String, String>,
    ) -> Result<team_agent::transport::SpawnResult, team_agent::transport::TransportError> {
        unreachable!("alias delivery test does not spawn")
    }

    fn spawn_into(
        &self,
        _session: &team_agent::transport::SessionName,
        _window: &team_agent::transport::WindowName,
        _argv: &[String],
        _cwd: &Path,
        _env: &BTreeMap<String, String>,
    ) -> Result<team_agent::transport::SpawnResult, team_agent::transport::TransportError> {
        unreachable!("alias delivery test does not spawn")
    }

    fn inject(
        &self,
        _target: &team_agent::transport::Target,
        _payload: &team_agent::transport::InjectPayload,
        _submit: team_agent::transport::Key,
        _bracketed: bool,
    ) -> Result<team_agent::transport::InjectReport, team_agent::transport::TransportError> {
        Ok(team_agent::transport::InjectReport {
            stage_reached: team_agent::transport::InjectStage::Submit,
            inject_verification: team_agent::transport::InjectVerification::CaptureContainsToken,
            submit_verification:
                team_agent::transport::SubmitVerification::EnterSentWithoutPlaceholderCheck,
            turn_verification: team_agent::transport::TurnVerification::NotYetObserved,
            attempts: 1,
            submit_diagnostics: None,
        })
    }

    fn send_keys(
        &self,
        _target: &team_agent::transport::Target,
        _keys: &[team_agent::transport::Key],
    ) -> Result<(), team_agent::transport::TransportError> {
        Ok(())
    }

    fn capture(
        &self,
        _target: &team_agent::transport::Target,
        range: team_agent::transport::CaptureRange,
    ) -> Result<team_agent::transport::CapturedText, team_agent::transport::TransportError> {
        Ok(team_agent::transport::CapturedText {
            text: String::new(),
            range,
        })
    }

    fn query(
        &self,
        _target: &team_agent::transport::Target,
        _field: team_agent::transport::PaneField,
    ) -> Result<Option<String>, team_agent::transport::TransportError> {
        Ok(None)
    }

    fn liveness(
        &self,
        _pane: &team_agent::transport::PaneId,
    ) -> Result<team_agent::transport::PaneLiveness, team_agent::transport::TransportError> {
        Ok(team_agent::transport::PaneLiveness::Live)
    }

    fn has_session(
        &self,
        _session: &team_agent::transport::SessionName,
    ) -> Result<bool, team_agent::transport::TransportError> {
        Ok(true)
    }

    fn list_targets(
        &self,
    ) -> Result<Vec<team_agent::transport::PaneInfo>, team_agent::transport::TransportError> {
        Ok(vec![team_agent::transport::PaneInfo {
            pane_id: team_agent::transport::PaneId::new("%u0b"),
            pane_pid: Some(std::process::id()),
            session: team_agent::transport::SessionName::new("team-teamA"),
            window_index: Some(0),
            window_name: Some(team_agent::transport::WindowName::new("worker_a")),
            pane_index: Some(0),
            tty: None,
            current_command: Some("fake".to_string()),
            current_path: None,
            active: true,
            leader_env: BTreeMap::new(),
        }])
    }

    fn list_windows(
        &self,
        _session: &team_agent::transport::SessionName,
    ) -> Result<Vec<team_agent::transport::WindowName>, team_agent::transport::TransportError> {
        Ok(vec![team_agent::transport::WindowName::new("worker_a")])
    }

    fn set_session_env(
        &self,
        _session: &team_agent::transport::SessionName,
        _key: &str,
        _value: &str,
    ) -> Result<team_agent::transport::SetEnvOutcome, team_agent::transport::TransportError> {
        Ok(team_agent::transport::SetEnvOutcome::Applied)
    }

    fn kill_session(
        &self,
        _session: &team_agent::transport::SessionName,
    ) -> Result<(), team_agent::transport::TransportError> {
        Ok(())
    }

    fn kill_window(
        &self,
        _target: &team_agent::transport::Target,
    ) -> Result<(), team_agent::transport::TransportError> {
        Ok(())
    }

    fn attach_session(
        &self,
        _session: &team_agent::transport::SessionName,
    ) -> Result<team_agent::transport::AttachOutcome, team_agent::transport::TransportError> {
        Ok(team_agent::transport::AttachOutcome::Attached)
    }
}
