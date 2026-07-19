#[path = "support/hermetic.rs"]
mod hermetic_guard;
#[allow(dead_code)]
fn _hermetic_boundary_marker(_: &hermetic_guard::HermeticTestEnv) {}

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use rusqlite::params;
use serde_json::Value;
use team_agent::event_log::EventLog;
use team_agent::message_store::MessageStore;
use team_agent::messaging::{
    deliver_pending_messages, send_message, DeliveryStatus, MessageTarget, SendOptions,
};
use team_agent::transport::{
    AttachOutcome, BackendKind, CaptureRange, CapturedText, InjectPayload, InjectReport,
    InjectStage, InjectVerification, Key, PaneField, PaneId, PaneInfo, PaneLiveness, SessionName,
    SetEnvOutcome, SpawnResult, SubmitVerification, Target, Transport, TransportError,
    TurnVerification, WindowName,
};

#[test]
fn c3_worker_send_accepted_row_and_delivered_event_keep_one_message_id_across_state_persist() {
    let ws = tmp_ws("c3-chain");
    let state = serde_json::json!({
        "session_name": "team-c3",
        "agents": {
            "w1": {
                "agent_id": "w1",
                "provider": "codex",
                "status": "running",
                "window": "w1"
            }
        }
    });
    team_agent::state::persist::save_runtime_state(&ws, &state).unwrap();
    let opts = SendOptions {
        route_task_id: false,
        block_until_delivered: false,
        message_id: Some("msg-cr-c3-chain".to_string()),
        ..SendOptions::default()
    };

    let accepted = send_message(
        &ws,
        &MessageTarget::Single("w1".to_string()),
        "prove event chain",
        &opts,
    )
    .unwrap();
    let message_id = accepted
        .message_id
        .clone()
        .expect("accepted send returns durable message_id");
    assert_eq!(message_id, "msg-cr-c3-chain");
    assert_eq!(accepted.status, DeliveryStatus::Queued);
    assert_eq!(accepted.message_status.0, "accepted");
    assert_eq!(
        message_status(&ws, &message_id).as_deref(),
        Some("accepted"),
        "precondition: send accepted edge is the durable DB row keyed by message_id"
    );

    let delivered = deliver_pending_messages(&ws, &state, &DeliverOkTransport, &EventLog::new(&ws))
        .expect("accepted row should deliver through the normal delivery kernel");
    assert_eq!(delivered, vec![message_id.clone()]);
    assert_eq!(
        message_status(&ws, &message_id).as_deref(),
        Some("delivered")
    );

    let before_persist = EventLog::new(&ws).tail(0).unwrap();
    assert_delivered_event_traces_to_message_id(&before_persist, &message_id);

    let mut after_state = state.clone();
    after_state["coordinator"] = serde_json::json!({"last_test_tick": 1});
    team_agent::state::persist::save_runtime_state(&ws, &after_state).unwrap();
    let after_persist = EventLog::new(&ws).tail(0).unwrap();
    assert_eq!(
        after_persist, before_persist,
        "CR C-3 / Phase C: state persist/merge must not rewrite or drift the event chain"
    );
}

#[test]
fn c3_source_guard_keeps_message_id_on_accepted_queued_and_delivered_edges() {
    let send = source("src/messaging/send.rs");
    // Car-C arch (consume persisted message truth): the accepted/queued edge no
    // longer hard-codes the "accepted" string; the presenter consumes the real
    // persisted row status. Trace-integrity intent is unchanged — the queued
    // edge stays keyed by the created message_id.
    assert!(
        send.contains("status: DeliveryStatus::Queued")
            && send.contains("MessageStatusShadow(persisted.row_status.as_str().to_string())")
            && send.contains("message_id: Some(message_id)"),
        "worker send accepted/queued edge must consume persisted.row_status and stay keyed by the created message_id"
    );

    let leader_receiver = source("src/messaging/leader_receiver.rs");
    assert!(
        leader_receiver.contains("\"leader_receiver.queued\"")
            && leader_receiver.contains("\"message_id\": message_id")
            && leader_receiver.contains("\"owner_team_id\": owner_team"),
        "leader queued edge must keep owner_team_id/message_id as the trace key"
    );

    let delivery = source("src/messaging/delivery.rs");
    assert!(
        delivery.contains("\"message.delivered\"")
            && delivery.contains("serde_json::json!({\"message_id\": message_id})"),
        "message.delivered event must carry the same message_id used by send accepted/queued"
    );

    let persist = source("src/state/persist.rs");
    for event_name in [
        "message.delivered",
        "send.accepted",
        "send.queued",
        "leader_receiver.queued",
    ] {
        assert!(
            !persist.contains(event_name),
            "state persist/merge must not synthesize or rewrite delivery event-chain event {event_name}"
        );
    }
}

struct DeliverOkTransport;

impl Transport for DeliverOkTransport {
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
        unimplemented!("not reached")
    }

    fn spawn_into(
        &self,
        _session: &SessionName,
        _window: &WindowName,
        _argv: &[String],
        _cwd: &Path,
        _env: &BTreeMap<String, String>,
    ) -> Result<SpawnResult, TransportError> {
        unimplemented!("not reached")
    }

    fn inject(
        &self,
        _target: &Target,
        _payload: &InjectPayload,
        _submit: Key,
        _bracketed_paste: bool,
    ) -> Result<InjectReport, TransportError> {
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
            text: String::new(),
            range,
        })
    }

    fn query(&self, _target: &Target, _field: PaneField) -> Result<Option<String>, TransportError> {
        Ok(None)
    }

    fn liveness(&self, _pane: &PaneId) -> Result<PaneLiveness, TransportError> {
        Ok(PaneLiveness::Unknown)
    }

    fn list_targets(&self) -> Result<Vec<PaneInfo>, TransportError> {
        Ok(Vec::new())
    }

    fn has_session(&self, _session: &SessionName) -> Result<bool, TransportError> {
        Ok(true)
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

fn assert_delivered_event_traces_to_message_id(events: &[Value], message_id: &str) {
    let delivered = events.iter().position(|event| {
        event.get("event").and_then(Value::as_str) == Some("message.delivered")
            && event.get("message_id").and_then(Value::as_str) == Some(message_id)
    });
    assert!(
        delivered.is_some(),
        "message.delivered must be present and keyed by the accepted message_id={message_id}; events={events:?}"
    );
}

fn message_status(workspace: &Path, message_id: &str) -> Option<String> {
    let store = MessageStore::open(workspace).unwrap();
    let conn = team_agent::db::schema::open_db(store.db_path()).unwrap();
    conn.query_row(
        "select status from messages where message_id = ?1",
        params![message_id],
        |row| row.get::<_, String>(0),
    )
    .ok()
}

fn source(relative: &str) -> String {
    std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(relative))
        .unwrap_or_else(|error| panic!("read {relative}: {error}"))
}

fn tmp_ws(tag: &str) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "team-agent-event-chain-{tag}-{}-{n}",
        std::process::id()
    ));
    std::fs::create_dir_all(&path).unwrap();
    path
}
