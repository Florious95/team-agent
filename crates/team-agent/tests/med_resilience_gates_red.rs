//! MED A-batch (2/3): semantics & resilience gates — slices A-2 / A-6 / A-8.
//!
//! Triage doc (sole basis): `.team/artifacts/med-triage-fixed-failure-sweep.md`.
//! Python truth source: 0.2.11.
//!
//! A-2 takeover reminder ignores arm_state — leader/takeover.rs:129-133 `let _ = arm_state`
//!     vs Python idle_predicate.py:20-75: an un-armed monitor (no worker turn-open since
//!     ack) must NEVER ping, reason "not_armed_no_worker_turn". The full Python state
//!     machine already exists in RS at provider/classify.rs:66+ (and is unit-locked in
//!     provider/tests/idle.rs); the leader facade simply never consults it. Debounce /
//!     episode-dedup tiers need time inputs the current facade signature lacks — those
//!     stay locked at the classify layer; this contract pins the facade's arm gate.
//!
//! A-6 resilience family:
//!   - watchers: retry_result_deliveries (watchers.rs:300-340) marks watchers `notified`
//!     WITHOUT performing any delivery. Python result_delivery.py:19-35 routes retries
//!     through notify_result_watchers which really delivers and only then records the
//!     notified message id; a failed delivery keeps the watcher retryable.
//!   - tick runtime approval: one agent's failed send_keys aborts the WHOLE tick
//!     (tick.rs:794 `?`). Python approvals/runtime_prompts.py:21-43 handles prompts
//!     per-agent via run_cmd(check=False) loops — one agent's tmux failure never kills
//!     the tick for the rest.
//!   - scheduler payload item from the triage was DOWNGRADED on Python grounding:
//!     Python scheduler.py:44 `json.loads(row["payload_json"] or "{}")` sits OUTSIDE the
//!     try block, so a corrupt payload aborts the Python batch identically (triage rule:
//!     same disease on both sides => not an A contract). Same for a corrupt result
//!     envelope inside the watcher retry (result_delivery.py:366 bare json.loads).
//!
//! A-8 upgrade-compat gate dead — Coordinator::schema_health (tick.rs:994-1001) is a
//!     hardcoded `ok:true` with zero db reads, while start() (tick.rs:942+) documents
//!     "schema 兼容门: 不可静默继续 (card §89)" and depends on it. Python
//!     coordinator/lifecycle.py:197+ message_store_schema_health really inspects team.db
//!     for missing required columns and refuses start.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use rusqlite::params;
use serde_json::{json, Value};
use team_agent::coordinator::{Coordinator, ErrorLists, ProviderRegistry, WorkspacePath};
use team_agent::event_log::EventLog;
use team_agent::leader::{evaluate_takeover_reminder, IdleNode, NodeRole};
use team_agent::message_store::MessageStore;
use team_agent::model::enums::Provider;
use team_agent::provider::{get_adapter, ProviderAdapter, TurnState};
use team_agent::state::persist::save_runtime_state;
use team_agent::transport::{
    AttachOutcome, BackendKind, CaptureRange, CapturedText, InjectPayload, InjectReport,
    InjectStage, InjectVerification, Key, PaneField, PaneId, PaneInfo, SessionName, SetEnvOutcome,
    SpawnResult, SubmitVerification, Target, Transport, TransportError, TurnVerification,
    WindowName,
};

/// A-2: all-idle nodes with an UN-ARMED arm_state must not ping
/// (Python idle_predicate.py:62 reason "not_armed_no_worker_turn").
#[test]
fn a2_all_idle_but_unarmed_must_not_ping() {
    let nodes = vec![idle_node("w1"), idle_node("w2")];
    let result = evaluate_takeover_reminder(&nodes, &json!({}))
        .expect("evaluate should succeed");
    assert!(
        !result.should_ping,
        "A-2: an un-armed monitor must never ping (Python C1: only a worker turn-open \
arms the watch, idle_predicate.py:55-62); got should_ping=true reason={:?}",
        result.reason
    );
    assert_eq!(
        result.reason.as_deref(),
        Some("not_armed_no_worker_turn"),
        "A-2: the no-ping reason must be the Python literal"
    );
}

/// A-2 green lock: a non-idle node still blocks the ping (current behavior, keep it).
#[test]
fn a2_green_lock_blocking_node_still_blocks() {
    let mut blocking = idle_node("w1");
    blocking.state = TurnState::Working;
    let result = evaluate_takeover_reminder(&[blocking, idle_node("w2")], &json!({}))
        .expect("evaluate should succeed");
    assert!(
        !result.should_ping && result.reason.as_deref() == Some("node_working"),
        "A-2 green lock: any non-idle node blocks the ping with reason node_<state>; got {result:?}"
    );
}

/// A-6 watchers: a notify_failed watcher must NOT flip to `notified` without a real
/// delivery (Python: notify_result_watchers really delivers; failure keeps it
/// retryable). Today retry_result_deliveries updates status='notified' unconditionally
/// and never delivers anything.
#[test]
fn a6_watcher_must_not_mark_notified_without_delivery() {
    let ws = tmp_ws("a6-watchers");
    // No leader_receiver in state => no delivery route exists at all.
    save_runtime_state(&ws, &json!({"session_name": "team-x", "agents": {}})).unwrap();
    let store = MessageStore::open(&ws).unwrap();
    let conn = team_agent::db::schema::open_db(store.db_path()).unwrap();
    conn.execute(
        "insert into results(result_id, owner_team_id, task_id, agent_id, envelope, status, created_at)
         values ('res-1', 'team-x', 't1', 'w1', ?1, 'stored', '2026-06-10T00:00:00+00:00')",
        params![json!({"task_id":"t1","agent_id":"w1","status":"success","summary":"s"}).to_string()],
    )
    .unwrap();
    conn.execute(
        "insert into result_watchers(watcher_id, owner_team_id, task_id, agent_id, leader_id, status, created_at, result_id)
         values ('wat-1', 'team-x', 't1', 'w1', 'leader', 'notify_failed', '2026-06-10T00:00:00+00:00', 'res-1')",
        [],
    )
    .unwrap();
    let event_log = EventLog::new(&ws);

    let _ = team_agent::messaging::watchers::retry_result_deliveries(&ws, &event_log)
        .expect("retry should not error");

    let (status, notified_message_id): (String, Option<String>) = conn
        .query_row(
            "select status, notified_message_id from result_watchers where watcher_id = 'wat-1'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert!(
        !(status == "notified" && notified_message_id.is_none()),
        "A-6: watcher flipped to `notified` with no notified_message_id and no delivery \
performed — Python only records notified after notify_result_watchers really delivers \
(result_delivery.py:19-35); status={status} notified_message_id={notified_message_id:?}"
    );
}

/// A-6 tick: a failing send_keys while auto-approving one agent's runtime approval
/// prompt must not abort the whole tick (Python approvals/runtime_prompts.py loops
/// per-agent with run_cmd(check=False)); the next agent must still be handled.
#[test]
fn a6_tick_runtime_approval_send_keys_failure_must_not_abort_tick() {
    let ws = tmp_ws("a6-tick");
    save_runtime_state(
        &ws,
        &json!({
            "session_name": "team-x",
            "active_team_key": "team-x",
            "agents": {
                "w1": {
                    "status": "running", "provider": "codex", "agent_id": "w1",
                    "window": "w1", "pane_id": "%11",
                    "effective_approval_policy": approval_policy(),
                },
                "w2": {
                    "status": "running", "provider": "codex", "agent_id": "w2",
                    "window": "w2", "pane_id": "%12",
                    "effective_approval_policy": approval_policy(),
                },
            },
        }),
    )
    .unwrap();
    let transport = ApprovalPromptTransport::new();
    let send_keys_targets = transport.send_keys_targets.clone();
    let coord = Coordinator::new(
        WorkspacePath::new(ws.clone()),
        Box::new(RealAdapterRegistry),
        Box::new(transport),
    );

    let report = coord.tick();

    let targets = send_keys_targets.lock().unwrap().clone();
    let mut failures = Vec::new();
    if report.is_err() {
        failures.push(format!(
            "A-6: tick aborted on the first agent's send_keys failure (tick.rs:794 `?`); \
Python isolates per agent; err={report:?}"
        ));
    }
    if targets.len() < 2 {
        failures.push(format!(
            "A-6: the second agent's approval prompt was never handled after the first \
agent's send_keys failed; send_keys targets={targets:?}"
        ));
    }
    assert!(
        failures.is_empty(),
        "A-6 tick approval resilience contract failed:\n{}",
        failures.join("\n")
    );
}

/// A-8: an old/incompatible team.db (existing table missing required columns) must make
/// Coordinator::start refuse with the schema gate (its own card §89 contract; Python
/// message_store_schema_health really diagnoses the table). Today schema_health is a
/// hardcoded ok:true (tick.rs:994-1001) and start() happily proceeds.
#[test]
fn a8_old_schema_db_must_refuse_coordinator_start() {
    let ws = tmp_ws("a8-schema");
    let runtime = ws.join(".team/runtime");
    std::fs::create_dir_all(&runtime).unwrap();
    let conn = rusqlite::Connection::open(runtime.join("team.db")).unwrap();
    // Pre-init incompatible shape: the messages table exists but carries none of the
    // required columns (Python lifecycle.py:197+ flags missing required columns and
    // refuses with an action hint).
    conn.execute("create table messages (id integer primary key)", []).unwrap();
    drop(conn);

    let coord = Coordinator::new(
        WorkspacePath::new(ws.clone()),
        Box::new(RealAdapterRegistry),
        Box::new(ApprovalPromptTransport::new()),
    );
    let report = coord.start().expect("start should return a typed report");
    assert!(
        !report.ok,
        "A-8: coordinator start must refuse on an incompatible team.db schema \
(card §89: 三元任一不匹配 → restart_incompatible, 不可静默继续); got report={report:?}"
    );
    assert!(
        report.schema_error.is_some(),
        "A-8: the refusal must carry a schema error (Python returns schema_error + \
action hint); report={report:?}"
    );
}

// ---------------------------------------------------------------------------
// fixtures
// ---------------------------------------------------------------------------

/// Real-machine field shape: `persist_effective_approval_policy` (launch.rs) writes this
/// exact object; runtime_config + explicit_yes_confirmed=true => auto-answer allowed.
fn approval_policy() -> Value {
    json!({
        "enabled": true,
        "source": "runtime_config",
        "inherited": false,
        "explicit_yes_confirmed": true,
        "provider": "codex",
        "flag": null,
        "worker_capability_above_leader": false,
    })
}

fn idle_node(id: &str) -> IdleNode {
    IdleNode {
        node_id: id.to_string(),
        role: NodeRole::Worker,
        state: TurnState::Idle,
        turn_id: None,
        annotations: Vec::new(),
        provider: Some(Provider::Codex),
        auth_mode: None,
        rollout_path: None,
    }
}

struct RealAdapterRegistry;

impl ProviderRegistry for RealAdapterRegistry {
    fn adapter_for(&self, provider: Provider) -> Box<dyn ProviderAdapter> {
        get_adapter(provider)
    }
    fn error_lists(&self, _provider: Provider) -> ErrorLists {
        ErrorLists { whitelist: Vec::new(), blacklist: Vec::new() }
    }
}

/// Transport whose pane captures always show a live allowlisted Team Agent MCP approval
/// prompt (same fixture text the #232 contracts use) and whose send_keys ALWAYS fails —
/// the deterministic per-agent failure injection for the tick resilience contract.
struct ApprovalPromptTransport {
    send_keys_targets: std::sync::Arc<Mutex<Vec<String>>>,
}

impl ApprovalPromptTransport {
    fn new() -> Self {
        Self {
            send_keys_targets: std::sync::Arc::new(Mutex::new(Vec::new())),
        }
    }
}

impl Transport for ApprovalPromptTransport {
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
        Ok(SpawnResult {
            pane_id: PaneId::new("%1"),
            session: session.clone(),
            window: window.clone(),
            child_pid: None,
        })
    }

    fn spawn_into(
        &self,
        session: &SessionName,
        window: &WindowName,
        argv: &[String],
        cwd: &Path,
        env: &BTreeMap<String, String>,
    ) -> Result<SpawnResult, TransportError> {
        self.spawn_first(session, window, argv, cwd, env)
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
        })
    }

    fn send_keys(&self, target: &Target, _keys: &[Key]) -> Result<(), TransportError> {
        self.send_keys_targets
            .lock()
            .unwrap()
            .push(format!("{target:?}"));
        Err(TransportError::Subprocess {
            argv: vec!["tmux".to_string(), "send-keys".to_string()],
            code: Some(1),
            stderr: "injected send-keys failure".to_string(),
        })
    }

    fn capture(&self, _target: &Target, range: CaptureRange) -> Result<CapturedText, TransportError> {
        Ok(CapturedText {
            text: "Allow the team_orchestrator MCP server to run tool \"report_result\"?\n  1. Allow\n  2. Deny\nEnter to submit | Esc to cancel\n".to_string(),
            range,
        })
    }

    fn query(&self, _target: &Target, field: PaneField) -> Result<Option<String>, TransportError> {
        match field {
            PaneField::PaneWidth => Ok(Some("120".to_string())),
            _ => Ok(None),
        }
    }

    fn liveness(
        &self,
        _pane: &PaneId,
    ) -> Result<team_agent::transport::PaneLiveness, TransportError> {
        Ok(team_agent::transport::PaneLiveness::Live)
    }

    fn list_targets(&self) -> Result<Vec<PaneInfo>, TransportError> {
        Ok(Vec::new())
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

    fn attach_session(&self, _session: &SessionName) -> Result<AttachOutcome, TransportError> {
        Ok(AttachOutcome::Attached)
    }
}

fn tmp_ws(tag: &str) -> PathBuf {
    static N: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "ta-rs-med-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::canonicalize(dir).unwrap()
}
