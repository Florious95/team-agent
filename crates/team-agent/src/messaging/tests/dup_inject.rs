//! ---
//! purpose: 消息双投三机制回归——claim 闸、observer 落盘失败、token 幂等
//! contract:
//!   provides:
//!     - name: dup-inject-unit
//!       what: 同 message_id 二次 deliver 不得再 inject；observer 失败不返 Err 重投
//!   requires:
//!     - name: recording-transport
//!       what: 捕获 pane 文本与 inject 次数
//! boundary:
//!   - 不作为活体验收；活体三臂装置在 scripts/repro/repro_dup_inject.sh
//! maturity: wired
//! ---

use super::*;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use crate::event_log::EventLog;
use crate::message_store::MessageStore;
use crate::messaging::leader_receiver::deliver_to_leader_fallback_pane;
use crate::transport::{
    AttachOutcome, BackendKind, CaptureRange, CapturedText, InjectPayload, InjectReport,
    InjectStage, InjectVerification, Key, PaneField, PaneId, PaneInfo, PaneLiveness, SessionName,
    SetEnvOutcome, SpawnResult, SubmitVerification, Target, Transport, TransportError,
    TurnVerification, WindowName,
};

#[test]
#[cfg(unix)]
fn eager_mcp_waits_without_spending_input_attempts_then_delivers_the_same_message() {
    let mut case = WorkerCase::new("mcp-handshake", "cursor_agent");
    let carrier = crate::mcp_server::readiness::test_carrier_pid().unwrap();
    let config = case.workspace.join(".team/runtime/mcp/w1.json");
    std::fs::create_dir_all(config.parent().unwrap()).unwrap();
    std::fs::write(&config, serde_json::to_vec(&serde_json::json!({
        "mcpServers": {"team_orchestrator": {
            "command": std::env::current_exe().unwrap().canonicalize().unwrap(),
            "env": {"TEAM_AGENT_WORKSPACE": case.workspace, "TEAM_AGENT_ID": "w1", "TEAM_AGENT_OWNER_TEAM_ID": "dup-team"}
        }}
    })).unwrap()).unwrap();
    case.state["team_key"] = serde_json::json!("dup-team");
    case.state["agents"]["w1"]["mcp_config"] = serde_json::json!(config);
    case.state["agents"]["w1"]["pane_pid"] = serde_json::json!(carrier);
    case.state["agents"]["w1"]["spawned_at"] = serde_json::json!((chrono::Utc::now() - chrono::Duration::seconds(10)).to_rfc3339());
    case.state["teams"] = serde_json::json!({"dup-team": case.state.clone()});
    crate::state::persist::save_runtime_state(&case.workspace, &case.state).unwrap();
    let transport = RecordingTransport { carrier: Some(carrier), ..RecordingTransport::new("") };
    let message_id = case.store.create_message(None, "leader", "w1", "handshake canary", None, false, Some("dup-team")).unwrap();
    for _ in 0..3 {
        let outcome = deliver_pending_message(&case.workspace, &case.store, &transport,
            &message_id, &case.event_log, &case.state).unwrap();
        assert!(!outcome.ok);
        assert!(outcome.verification.as_deref().unwrap().starts_with("mcp_handshake_"));
        assert_eq!(transport.inject_count(), 0);
    }
    let connection = rusqlite::Connection::open(case.store.db_path()).unwrap();
    let attempts: i64 = connection.query_row("select delivery_attempts from messages where message_id=?1", [&message_id], |row| row.get(0)).unwrap();
    assert_eq!(attempts, 0, "waiting is not a physical delivery attempt");
    crate::mcp_server::readiness::publish(&case.workspace, "dup-team", "w1", chrono::Utc::now()).unwrap();
    let outcome = deliver_pending_message(&case.workspace, &case.store, &transport,
        &message_id, &case.event_log, &case.state).unwrap();
    assert_eq!(outcome.message_id.as_deref(), Some(message_id.as_str()));
    assert_eq!(transport.inject_count(), 1, "same queued row proceeds after the real receipt");
}

#[test]
#[cfg(unix)]
fn issue372_bounded_handshake_wait_parks_notifies_once_then_publish_delivers() {
    let mut case = WorkerCase::new("mcp-handshake-park", "cursor_agent");
    let carrier = crate::mcp_server::readiness::test_carrier_pid().unwrap();
    let config = case.workspace.join(".team/runtime/mcp/w1.json");
    std::fs::create_dir_all(config.parent().unwrap()).unwrap();
    std::fs::write(&config, serde_json::to_vec(&serde_json::json!({
        "mcpServers": {"team_orchestrator": {
            "command": std::env::current_exe().unwrap().canonicalize().unwrap(),
            "env": {"TEAM_AGENT_WORKSPACE": case.workspace, "TEAM_AGENT_ID": "w1", "TEAM_AGENT_OWNER_TEAM_ID": "dup-team"}
        }}
    })).unwrap()).unwrap();
    let long_ago = (chrono::Utc::now() - chrono::Duration::seconds(400)).to_rfc3339();
    case.state["team_key"] = serde_json::json!("dup-team");
    case.state["agents"]["w1"]["mcp_config"] = serde_json::json!(config);
    case.state["agents"]["w1"]["pane_pid"] = serde_json::json!(carrier);
    case.state["agents"]["w1"]["spawned_at"] = serde_json::json!(long_ago);
    case.state["teams"] = serde_json::json!({"dup-team": case.state.clone()});
    crate::state::persist::save_runtime_state(&case.workspace, &case.state).unwrap();
    let transport = RecordingTransport { carrier: Some(carrier), ..RecordingTransport::new("") };
    let ids: Vec<String> = (0..2)
        .map(|n| case.store.create_message(None, "leader", "w1", &format!("parked canary {n}"), None, false, Some("dup-team")).unwrap())
        .collect();
    let connection = rusqlite::Connection::open(case.store.db_path()).unwrap();
    connection.execute("update messages set created_at = ?1", [&long_ago]).unwrap();
    for id in &ids {
        let outcome = deliver_pending_message(&case.workspace, &case.store, &transport,
            id, &case.event_log, &case.state).unwrap();
        assert_eq!(outcome.message_status.0, "queued_until_start", "{outcome:?}");
        assert!(outcome.verification.as_deref().unwrap().starts_with("mcp_handshake_timeout:mcp_handshake_"));
    }
    assert_eq!(transport.inject_count(), 0, "a parked wait never reaches input");
    let row = |id: &str| -> (String, Option<String>, i64) {
        connection.query_row(
            "select status, error, delivery_attempts from messages where message_id = ?1",
            [id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        ).unwrap()
    };
    for id in &ids {
        assert_eq!(row(id.as_str()), ("queued_until_start".to_string(), Some("mcp_handshake_timeout".to_string()), 0));
    }
    let events = case.events();
    assert_eq!(events.matches("\"delivery.mcp_handshake_parked\"").count(), 2, "{events}");
    assert_eq!(events.matches("\"delivery.mcp_handshake_parked_notification\"").count(), 1,
        "one leader notice per seat spawn: {events}");
    let notices: i64 = connection.query_row(
        "select count(*) from leader_notification_log where result_id like 'mcp_handshake.timeout:w1:%'",
        [], |r| r.get(0),
    ).unwrap();
    assert_eq!(notices, 1);

    crate::mcp_server::readiness::publish(&case.workspace, "dup-team", "w1", chrono::Utc::now()).unwrap();
    for id in &ids {
        assert_eq!(row(id.as_str()).0, "accepted", "a fresh receipt resumes parked rows");
    }
    let outcome = deliver_pending_message(&case.workspace, &case.store, &transport,
        &ids[0], &case.event_log, &case.state).unwrap();
    assert_eq!(outcome.message_id.as_deref(), Some(ids[0].as_str()));
    assert_eq!(transport.inject_count(), 1, "the same parked row is delivered after the receipt");
}

#[test]
#[cfg(unix)]
fn issue345_missing_eager_config_defers_without_input_or_attempts() {
    let mut case = WorkerCase::new("missing-mcp-config", "cursor_agent");
    let carrier = crate::mcp_server::readiness::test_carrier_pid().unwrap();
    case.state["team_key"] = serde_json::json!("dup-team");
    case.state["agents"]["w1"]["pane_pid"] = serde_json::json!(carrier);
    case.state["agents"]["w1"]["spawned_at"] = serde_json::json!(chrono::Utc::now().to_rfc3339());
    case.state["teams"] = serde_json::json!({"dup-team": case.state.clone()});
    crate::state::persist::save_runtime_state(&case.workspace, &case.state).unwrap();
    let transport = RecordingTransport { carrier: Some(carrier), ..RecordingTransport::new("") };
    let message_id = case.store.create_message(None, "leader", "w1", "missing config canary", None, false, Some("dup-team")).unwrap();
    for _ in 0..3 {
        let outcome = deliver_pending_message(&case.workspace, &case.store, &transport,
            &message_id, &case.event_log, &case.state).unwrap();
        assert!(!outcome.ok);
        assert!(outcome.verification.as_deref().unwrap().starts_with("mcp_handshake_"));
        assert_eq!(transport.inject_count(), 0, "missing metadata must never reach input");
    }
    let connection = rusqlite::Connection::open(case.store.db_path()).unwrap();
    let attempts: i64 = connection.query_row("select delivery_attempts from messages where message_id=?1", [&message_id], |row| row.get(0)).unwrap();
    assert_eq!(attempts, 0, "metadata wait is not an input attempt");
}

#[test]
fn error_retry_claim_is_single_winner_across_two_delivers() {
    let case = WorkerCase::new("claim-gate", "fake");
    let transport = RecordingTransport::new("");
    let message_id = case.seed_leader_to_worker("claim canary");
    case.store
        .mark(&message_id, "target_resolved", Some("probe_failed"))
        .unwrap();

    let first = deliver_pending_message(
        &case.workspace,
        &case.store,
        &transport,
        &message_id,
        &case.event_log,
        &case.state,
    )
    .expect("first retry after error");
    assert!(
        first.ok || first.status == DeliveryStatus::AlreadyDelivered,
        "{first:?}"
    );
    assert_eq!(
        transport.inject_count(),
        1,
        "first claim winner injects once"
    );

    case.store
        .mark(&message_id, "target_resolved", Some("probe_failed"))
        .unwrap();
    let second = deliver_pending_message(
        &case.workspace,
        &case.store,
        &transport,
        &message_id,
        &case.event_log,
        &case.state,
    )
    .expect("second retry after plant");
    assert_eq!(
        transport.inject_count(),
        1,
        "token already in pane must refuse the planted second inject: {second:?}"
    );
}

#[test]
fn observer_state_save_failure_after_enter_does_not_return_err() {
    let case = WorkerCase::new("observer-degrade", "fake");
    let transport = RecordingTransport::new("");
    let message_id = case.seed_leader_to_worker("observer canary");
    poison_state_file(&case.workspace);

    let outcome = deliver_pending_message(
        &case.workspace,
        &case.store,
        &transport,
        &message_id,
        &case.event_log,
        &case.state,
    )
    .expect("physical Enter already sent; observer save must not reverse that into Err");
    assert_eq!(transport.inject_count(), 1);
    assert!(
        case.events().contains("delivery.post_submit_state_degraded")
            || outcome.ok
            || outcome.status == DeliveryStatus::AlreadyDelivered,
        "observer/state save failure must degrade, not reopen inject. outcome={outcome:?} events={}",
        case.events()
    );
}

#[test]
fn pane_token_refuses_second_inject_for_claude_grok_cursor() {
    for provider in ["claude", "grok", "cursor_agent"] {
        let mut case = WorkerCase::new(&format!("token-{provider}"), provider);
        #[cfg(unix)]
        let carrier = crate::mcp_server::readiness::test_carrier_pid().unwrap();
        #[cfg(not(unix))]
        let carrier = std::process::id();
        let config = case.workspace.join(".team/runtime/mcp/w1.json");
        std::fs::create_dir_all(config.parent().unwrap()).unwrap();
        std::fs::write(&config, serde_json::to_vec(&serde_json::json!({
            "mcpServers": {"team_orchestrator": {
                "command": std::env::current_exe().unwrap().canonicalize().unwrap(),
                "env": {"TEAM_AGENT_WORKSPACE": case.workspace, "TEAM_AGENT_ID": "w1", "TEAM_AGENT_OWNER_TEAM_ID": "dup-team"}
            }}
        })).unwrap()).unwrap();
        case.state["team_key"] = serde_json::json!("dup-team");
        case.state["agents"]["w1"]["mcp_config"] = serde_json::json!(config);
        case.state["agents"]["w1"]["pane_pid"] = serde_json::json!(carrier);
        case.state["agents"]["w1"]["spawned_at"] = serde_json::json!((chrono::Utc::now() - chrono::Duration::seconds(10)).to_rfc3339());
        case.state["teams"] = serde_json::json!({"dup-team": case.state.clone()});
        crate::state::persist::save_runtime_state(&case.workspace, &case.state).unwrap();
        #[cfg(unix)]
        crate::mcp_server::readiness::publish(&case.workspace, "dup-team", "w1", chrono::Utc::now()).unwrap();
        let transport = RecordingTransport { carrier: Some(carrier), ..RecordingTransport::new("") };
        let message_id = case.store.create_message(None, "leader", "w1", &format!("{provider} canary"), None, false, Some("dup-team")).unwrap();
        let first = deliver_pending_message(
            &case.workspace,
            &case.store,
            &transport,
            &message_id,
            &case.event_log,
            &case.state,
        )
        .expect("first inject");
        assert_eq!(
            transport.inject_count(),
            1,
            "provider={provider} first inject failed: {first:?}"
        );

        case.store
            .mark(&message_id, "target_resolved", Some("probe_failed"))
            .unwrap();
        let second = deliver_pending_message(
            &case.workspace,
            &case.store,
            &transport,
            &message_id,
            &case.event_log,
            &case.state,
        )
        .expect("second deliver");
        assert_eq!(
            transport.inject_count(),
            1,
            "provider={provider} pane token must refuse second inject: {second:?}"
        );
        assert!(
            second.status == DeliveryStatus::AlreadyDelivered
                || second.reason == Some(DeliveryRefusal::MessageAlreadyClaimed)
                || case.events().contains("delivery.duplicate_token_refused"),
            "provider={provider} missing duplicate refuse. outcome={second:?} events={}",
            case.events()
        );
    }
}

#[test]
fn transcript_token_refuses_second_inject() {
    let case = WorkerCase::new("transcript", "claude");
    let message_id = case.seed_leader_to_worker("transcript canary");
    std::fs::write(
        &case.rollout,
        format!("user\n[team-agent-token:{message_id}]\n"),
    )
    .unwrap();
    let transport = RecordingTransport::new("unrelated pane text");
    case.store
        .mark(&message_id, "target_resolved", Some("probe_failed"))
        .unwrap();
    let outcome = deliver_pending_message(
        &case.workspace,
        &case.store,
        &transport,
        &message_id,
        &case.event_log,
        &case.state,
    )
    .expect("transcript hit");
    assert_eq!(
        transport.inject_count(),
        0,
        "transcript token must refuse inject: {outcome:?}"
    );
}

#[test]
fn fallback_already_delivered_uses_live_status_set() {
    let workspace = tmp_ws("fallback-status");
    let store = MessageStore::open(&workspace).unwrap();
    let mid = store
        .create_message(None, "w1", "leader", "fallback canary", None, false, None)
        .unwrap();
    store
        .mark(&mid, "submitted_pending_acceptance", None)
        .unwrap();

    let event_log = EventLog::new(&workspace);
    let state = serde_json::json!({
        "active_team_key": "t",
        "leader_receiver": {"pane_id": "%leader", "status": "attached"}
    });
    crate::state::persist::save_runtime_state(&workspace, &state).unwrap();
    let outcome = deliver_to_leader_fallback_pane(
        &workspace,
        &state,
        &mid,
        None,
        "fallback canary",
        false,
        Some("primary failed"),
        &event_log,
    )
    .expect("fallback short-circuit");
    assert_eq!(outcome.status, DeliveryStatus::AlreadyDelivered);
}

struct WorkerCase {
    workspace: PathBuf,
    rollout: PathBuf,
    store: MessageStore,
    event_log: EventLog,
    state: serde_json::Value,
}

impl WorkerCase {
    fn new(tag: &str, provider: &str) -> Self {
        let workspace = tmp_ws(&format!("dup-inject-{tag}"));
        let rollout = workspace.join(format!("{provider}-rollout.jsonl"));
        std::fs::write(&rollout, "").unwrap();
        let state = serde_json::json!({
            "active_team_key": "dup-team",
            "session_name": "team-dup",
            "agents": {
                "w1": {
                    "pane_id": "%w1",
                    "window": "w1",
                    "provider": provider,
                    "rollout_path": rollout,
                }
            }
        });
        crate::state::persist::save_runtime_state(&workspace, &state).unwrap();
        let store = MessageStore::open(&workspace).unwrap();
        let event_log = EventLog::new(&workspace);
        Self {
            workspace,
            rollout,
            store,
            event_log,
            state,
        }
    }

    fn seed_leader_to_worker(&self, content: &str) -> String {
        self.store
            .create_message(None, "leader", "w1", content, None, false, None)
            .unwrap()
    }

    fn events(&self) -> String {
        self.event_log
            .tail(0)
            .unwrap()
            .into_iter()
            .map(|event| event.to_string())
            .collect::<Vec<_>>()
            .join("\n")
    }
}

fn poison_state_file(workspace: &Path) {
    let path = crate::state::persist::runtime_state_path(workspace);
    let _ = std::fs::remove_file(&path);
    std::fs::create_dir_all(&path).unwrap();
}

#[derive(Clone)]
struct RecordingTransport {
    injects: Arc<AtomicUsize>,
    capture: Arc<Mutex<String>>,
    carrier: Option<u32>,
}

impl RecordingTransport {
    fn new(capture: &str) -> Self {
        Self {
            injects: Arc::new(AtomicUsize::new(0)),
            capture: Arc::new(Mutex::new(capture.to_string())),
            carrier: None,
        }
    }

    fn inject_count(&self) -> usize {
        self.injects.load(Ordering::Relaxed)
    }
}

impl Transport for RecordingTransport {
    fn kind(&self) -> BackendKind {
        BackendKind::Tmux
    }

    fn spawn_first(
        &self,
        _session: &SessionName,
        _window: &WindowName,
        _argv: &[String],
        _cwd: &Path,
        _env: &BTreeMap<String, String>,
    ) -> Result<SpawnResult, TransportError> {
        unreachable!("dup-inject tests do not spawn")
    }

    fn spawn_into(
        &self,
        _session: &SessionName,
        _window: &WindowName,
        _argv: &[String],
        _cwd: &Path,
        _env: &BTreeMap<String, String>,
    ) -> Result<SpawnResult, TransportError> {
        unreachable!("dup-inject tests do not spawn")
    }

    fn inject(
        &self,
        _target: &Target,
        payload: &InjectPayload,
        _submit: Key,
        _bracketed_paste: bool,
    ) -> Result<InjectReport, TransportError> {
        self.injects.fetch_add(1, Ordering::Relaxed);
        if let Some(text) = payload.text() {
            self.capture.lock().unwrap().push_str(text);
        }
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
            text: self.capture.lock().unwrap().clone(),
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
        Ok(vec![PaneInfo {
            pane_id: PaneId::new("%w1"),
            session: SessionName::new("team-dup"),
            window_index: None,
            window_name: Some(WindowName::new("w1")),
            pane_index: None,
            tty: None,
            current_command: Some("sleep".to_string()),
            current_path: None,
            active: true,
            pane_pid: self.carrier,
            leader_env: BTreeMap::new(),
        }])
    }

    fn has_session(&self, _session: &SessionName) -> Result<bool, TransportError> {
        Ok(true)
    }

    fn list_windows(&self, _session: &SessionName) -> Result<Vec<WindowName>, TransportError> {
        Ok(vec![WindowName::new("w1")])
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
