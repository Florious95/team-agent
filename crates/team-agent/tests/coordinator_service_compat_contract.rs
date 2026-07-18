//! 0.5.23 MCP cross-version coordinator compatibility RED contracts (te-owned).
//!
//! References:
//! - `.team/artifacts/mcp-crossversion-daemon-locate.md` §8.
//! - RED1: old MCP caller + newer live daemon with compatible protocol/schema
//!   must queue work and emit binary-drift diagnostics, not `coordinator_unavailable`.
//! - RED2: protocol/schema incompatibility remains fail-closed with no row and
//!   no coordinator start/stop side effect.
//! - RED3/RED5: old start/lifecycle callers must not downgrade a newer daemon.
//! - RED4: current/newer callers still rotate older daemons loudly.

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

#[path = "support/hermetic.rs"]
mod hermetic_guard;

use std::path::{Path, PathBuf};
use std::process::{Child, Command};

use serde_json::{json, Value};
use serial_test::serial;
use team_agent::coordinator::{
    coordinator_meta_path, coordinator_pid_path, start_coordinator_with_team, stop_coordinator,
    Pid, StartOutcome, WorkspacePath, PROTOCOL_VERSION,
};
use team_agent::db::schema::open_db;
use team_agent::event_log::EventLog;
use team_agent::mcp_server::{SendOutcome, TeamOrchestratorTools};
use team_agent::message_store::MessageStore;
use team_agent::messaging::MessageTarget;
use team_agent::model::ids::{AgentId, TeamKey};
use team_agent::state::persist::save_runtime_state;

const TEAM: &str = "compat-team";
const SENDER: &str = "backend";
const WORKER: &str = "fe-dev";
const OLD_CALLER_VERSION: &str = "0.5.21";
const CURRENT_DAEMON_VERSION: &str = "0.5.22";
const OLDER_DAEMON_VERSION: &str = "0.5.21";
const CALLER_IDENTITY_ENV: &str = "TEAM_AGENT_TEST_CALLER_BINARY_IDENTITY";

#[test]
#[serial(env)]
fn mcp_send_binary_newer_daemon_is_service_compatible() {
    let mut fixture = CompatFixture::old_mcp_caller("red1-service-compatible");
    let daemon_pid = fixture.spawn_daemon_metadata(
        CURRENT_DAEMON_VERSION,
        PROTOCOL_VERSION,
        team_agent::db::schema::SCHEMA_VERSION,
    );

    let outcome = fixture.mcp_send("RED1_CROSS_VERSION");

    let message_id = match outcome {
        SendOutcome::WorkerAccepted { message_id, .. } => message_id,
        other => panic!(
            "RED1: old MCP caller talking to newer compatible daemon must return WorkerAccepted/queued, not unavailable; outcome={other:?} events={:?}",
            fixture.events_tail()
        ),
    };
    assert_eq!(
        fixture.message_rows_for("RED1_CROSS_VERSION"),
        1,
        "RED1: compatible binary drift must still create exactly one DB row; message_id={message_id} events={:?}",
        fixture.events_tail()
    );
    assert!(
        !fixture.has_event("send.coordinator_unavailable"),
        "RED1: binary-only drift must not be reported as coordinator_unavailable; events={:?}",
        fixture.events_tail()
    );
    assert!(
        fixture.has_event("send.coordinator_binary_identity_drift_ignored"),
        "RED1: service-compatible binary drift must be explicit via send.coordinator_binary_identity_drift_ignored with caller=0.5.21 daemon=0.5.22; events={:?}",
        fixture.events_tail()
    );
    assert_eq!(
        fixture.pid_file_value(),
        Some(daemon_pid),
        "RED1: MCP send must not rotate/replace the newer daemon; events={:?}",
        fixture.events_tail()
    );
}

#[test]
#[serial(env)]
fn mcp_send_protocol_mismatch_fails_closed_without_row_or_side_effects() {
    let mut fixture = CompatFixture::old_mcp_caller("red2-protocol-mismatch");
    let daemon_pid = fixture.spawn_daemon_metadata(
        CURRENT_DAEMON_VERSION,
        PROTOCOL_VERSION + 1,
        team_agent::db::schema::SCHEMA_VERSION,
    );

    let outcome = fixture.mcp_send("RED2_PROTOCOL_MISMATCH");

    assert!(
        !matches!(outcome, SendOutcome::WorkerAccepted { .. }),
        "RED2: incompatible protocol/schema must fail closed, never WorkerAccepted; outcome={outcome:?}"
    );
    assert_eq!(
        fixture.message_rows_for("RED2_PROTOCOL_MISMATCH"),
        0,
        "RED2: protocol/schema-incompatible daemon must not create a DB row; events={:?}",
        fixture.events_tail()
    );
    assert_eq!(
        fixture.pid_file_value(),
        Some(daemon_pid),
        "RED2: protocol/schema refusal must not stop or replace the daemon"
    );
    assert!(
        fixture.event_field_equals(
            "send.coordinator_unavailable",
            "metadata_mismatch_reason",
            "protocol_version_mismatch"
        ),
        "RED2: fail-closed response/event must name protocol_version_mismatch rather than a generic unavailable; events={:?}",
        fixture.events_tail()
    );
    assert!(
        !fixture.has_event("coordinator.rotation_required")
            && !fixture.has_event("coordinator.ensure_restarted"),
        "RED2: incompatible old caller must not start/stop/ensure the daemon; events={:?}",
        fixture.events_tail()
    );
}

#[test]
#[serial(env)]
fn old_caller_start_coordinator_does_not_downgrade_newer_daemon() {
    let mut fixture = CompatFixture::old_mcp_caller("red3-no-downgrade-start");
    let daemon_pid = fixture.spawn_daemon_metadata(
        CURRENT_DAEMON_VERSION,
        PROTOCOL_VERSION,
        team_agent::db::schema::SCHEMA_VERSION,
    );

    let report =
        start_coordinator_with_team(&fixture.workspace, Some(TEAM)).expect("start coordinator");

    assert!(
        report.ok,
        "RED3: old caller should treat newer compatible daemon as usable, not failed unavailable; report={report:?}"
    );
    assert_eq!(
        fixture.pid_file_value(),
        Some(daemon_pid),
        "RED3: old caller must not stop or replace newer daemon; report={report:?} events={:?}",
        fixture.events_tail()
    );
    assert!(
        fixture.pid_alive(daemon_pid),
        "RED3: newer daemon pid must remain alive after old caller start_coordinator; report={report:?}"
    );
    assert!(
        report.rotation_reason.as_deref() == Some("daemon_newer_than_caller")
            || format!("{:?}", report.status).contains("NewerDaemon"),
        "RED3: non-rotating report must explicitly say daemon_newer_than_caller / AlreadyRunningNewerDaemon; report={report:?}"
    );
    assert!(
        fixture.has_event("coordinator.newer_daemon_preserved"),
        "RED3: preserving a newer daemon must be auditable; events={:?}",
        fixture.events_tail()
    );
}

#[test]
#[serial(env)]
fn new_caller_start_coordinator_still_rotates_older_daemon_guard() {
    let mut fixture = CompatFixture::current_caller("red4-new-caller-rotates-old");
    let old_pid = fixture.spawn_daemon_metadata(
        OLDER_DAEMON_VERSION,
        PROTOCOL_VERSION,
        team_agent::db::schema::SCHEMA_VERSION,
    );

    let report =
        start_coordinator_with_team(&fixture.workspace, Some(TEAM)).expect("start coordinator");

    assert_eq!(
        report.status,
        StartOutcome::StartedAfterRotation,
        "RED4 guard: current/newer caller must keep rotating older daemon; report={report:?}"
    );
    assert_eq!(
        report.rotation_reason.as_deref(),
        Some("binary_version_mismatch"),
        "RED4 guard: rotation must still name binary_version_mismatch; report={report:?}"
    );
    assert_ne!(
        report.pid.map(Pid::get),
        Some(old_pid),
        "RED4 guard: rotation must replace the old daemon pid; report={report:?}"
    );
    assert!(
        !fixture.pid_alive(old_pid),
        "RED4 guard: old daemon pid must be stopped by the authorized newer caller"
    );
}

#[test]
#[serial(env)]
fn mcp_lifecycle_reset_cannot_rotate_newer_daemon_down() {
    let mut fixture = CompatFixture::old_mcp_caller("red5-lifecycle-no-downgrade");
    let daemon_pid = fixture.spawn_daemon_metadata(
        CURRENT_DAEMON_VERSION,
        PROTOCOL_VERSION,
        team_agent::db::schema::SCHEMA_VERSION,
    );

    let reset = fixture
        .tools()
        .reset_agent(WORKER, true)
        .map(|ok| Value::Object(ok.fields))
        .unwrap_or_else(|error| error.to_envelope());

    assert_eq!(
        fixture.pid_file_value(),
        Some(daemon_pid),
        "RED5: MCP reset/fork lifecycle path from old caller must not downgrade newer daemon; reset={reset} events={:?}",
        fixture.events_tail()
    );
    assert!(
        fixture.pid_alive(daemon_pid),
        "RED5: newer daemon process must remain alive after old MCP lifecycle call; reset={reset}"
    );
    assert!(
        !fixture.has_event("coordinator.rotation_required")
            && !fixture.has_event("coordinator.ensure_restarted"),
        "RED5: old MCP lifecycle call must not enter rotation/ensure path for a newer daemon; reset={reset} events={:?}",
        fixture.events_tail()
    );
    assert!(
        fixture.has_event("coordinator.newer_daemon_preserved")
            || reset
                .pointer("/coordinator_binary_relation")
                .and_then(Value::as_str)
                == Some("daemon_newer_than_caller"),
        "RED5: lifecycle tool result/event must expose daemon_newer_than_caller instead of silently continuing; reset={reset} events={:?}",
        fixture.events_tail()
    );
}

struct CompatFixture {
    _env: hermetic_guard::HermeticTestEnv,
    _binary_match_env: hermetic_guard::EnvOverride,
    _caller_identity_env: Option<hermetic_guard::EnvOverride>,
    root: PathBuf,
    workspace: WorkspacePath,
    children: Vec<Child>,
}

impl CompatFixture {
    fn old_mcp_caller(tag: &str) -> Self {
        Self::with_caller(tag, Some(OLD_CALLER_VERSION))
    }

    fn current_caller(tag: &str) -> Self {
        Self::with_caller(tag, None)
    }

    fn with_caller(tag: &str, caller_version: Option<&str>) -> Self {
        let env = hermetic_guard::HermeticTestEnv::enter(tag);
        let binary_match = cli_binary_path();
        let binary_match_env =
            env.with_env("TEAM_AGENT_TEST_HARNESS_BINARY_PATH_MATCH", &binary_match);
        let caller_identity_env = caller_version.map(|version| {
            env.with_env(
                CALLER_IDENTITY_ENV,
                &json!({
                    "binary_path": cli_binary_path(),
                    "binary_version": version
                })
                .to_string(),
            )
        });
        let root = env.workspace(tag);
        std::fs::create_dir_all(team_agent::model::paths::runtime_dir(&root))
            .expect("create runtime dir");
        let _ = MessageStore::open(&root).expect("create message store");
        save_runtime_state(&root, &runtime_state(&root)).expect("save runtime state");
        Self {
            _env: env,
            _binary_match_env: binary_match_env,
            _caller_identity_env: caller_identity_env,
            workspace: WorkspacePath::new(root.clone()),
            root,
            children: Vec::new(),
        }
    }

    fn tools(&self) -> TeamOrchestratorTools {
        TeamOrchestratorTools::with_identity(
            &self.root,
            Some(AgentId::new(SENDER.to_string())),
            Some(TeamKey::new(TEAM.to_string())),
        )
    }

    fn mcp_send(&self, content: &str) -> SendOutcome {
        self.tools()
            .send_message(
                &MessageTarget::Single(WORKER.to_string()),
                content,
                None,
                Some(true),
                None,
            )
            .unwrap_or_else(|error| {
                panic!(
                    "MCP send_message must return a tool outcome, not ToolError; error={} events={:?}",
                    error.to_envelope(),
                    self.events_tail()
                )
            })
    }

    fn spawn_daemon_metadata(
        &mut self,
        binary_version: &str,
        protocol_version: u32,
        schema_version: i64,
    ) -> u32 {
        let child = Command::new("sleep")
            .arg("60")
            .spawn()
            .expect("spawn fixture daemon process");
        let pid = child.id();
        self.children.push(child);
        write_raw_metadata(
            &self.workspace,
            json!({
                "pid": pid,
                "protocol_version": protocol_version,
                "message_store_schema_version": schema_version,
                "binary_path": cli_binary_path(),
                "binary_version": binary_version,
                "source": "boot",
                "updated_at": "2026-07-10T00:00:00Z"
            }),
        );
        pid
    }

    fn message_rows_for(&self, content: &str) -> i64 {
        let store = MessageStore::open(&self.root).expect("open message store");
        let conn = open_db(store.db_path()).expect("open db");
        conn.query_row(
            "select count(*) from messages where sender = ?1 and recipient = ?2 and content = ?3",
            rusqlite::params![SENDER, WORKER, content],
            |row| row.get(0),
        )
        .expect("count messages")
    }

    fn pid_file_value(&self) -> Option<u32> {
        std::fs::read_to_string(coordinator_pid_path(&self.workspace))
            .ok()
            .and_then(|text| text.trim().parse::<u32>().ok())
    }

    fn pid_alive(&self, pid: u32) -> bool {
        Command::new("kill")
            .arg("-0")
            .arg(pid.to_string())
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    }

    fn has_event(&self, event_name: &str) -> bool {
        self.events()
            .iter()
            .any(|event| event.get("event").and_then(Value::as_str) == Some(event_name))
    }

    fn event_field_equals(&self, event_name: &str, field: &str, expected: &str) -> bool {
        self.events().iter().any(|event| {
            event.get("event").and_then(Value::as_str) == Some(event_name)
                && event.get(field).and_then(Value::as_str) == Some(expected)
        })
    }

    fn events(&self) -> Vec<Value> {
        let path = self.root.join(".team/logs/events.jsonl");
        std::fs::read_to_string(path)
            .unwrap_or_default()
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .collect()
    }

    fn events_tail(&self) -> Vec<Value> {
        EventLog::new(&self.root).tail(50).expect("tail events")
    }
}

impl Drop for CompatFixture {
    fn drop(&mut self) {
        let _ = stop_coordinator(&self.workspace);
        for child in &mut self.children {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn runtime_state(root: &Path) -> Value {
    json!({
        "session_name": "compat-session",
        "active_team_key": TEAM,
        "team_key": TEAM,
        "tmux_socket": null,
        "tmux_endpoint": null,
        "agents": {
            SENDER: worker_state(SENDER),
            WORKER: worker_state(WORKER)
        },
        "teams": {
            TEAM: {
                "status": "alive",
                "team_dir": root.join("team-dir"),
                "session_name": "compat-session",
                "agents": {
                    SENDER: worker_state(SENDER),
                    WORKER: worker_state(WORKER)
                }
            }
        }
    })
}

fn worker_state(agent_id: &str) -> Value {
    json!({
        "status": "running",
        "agent_id": agent_id,
        "provider": "fake",
        "window": agent_id
    })
}

fn write_raw_metadata(workspace: &WorkspacePath, value: Value) {
    let pid = value
        .get("pid")
        .and_then(Value::as_u64)
        .expect("metadata pid") as u32;
    std::fs::write(coordinator_pid_path(workspace), pid.to_string()).expect("write pid");
    std::fs::write(
        coordinator_meta_path(workspace),
        serde_json::to_string_pretty(&value).expect("serialize metadata"),
    )
    .expect("write metadata");
}

fn cli_binary_path() -> String {
    std::fs::canonicalize(env!("CARGO_BIN_EXE_team-agent"))
        .unwrap_or_else(|_| PathBuf::from(env!("CARGO_BIN_EXE_team-agent")))
        .to_string_lossy()
        .to_string()
}
