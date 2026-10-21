#![allow(clippy::unwrap_used)]

use super::*;
use crate::transport::{native_input::InputFrame, test_support::OfflineTransport};
use std::sync::atomic::{AtomicU64, Ordering};

struct Fixture {
    workspace: std::path::PathBuf,
    store: MessageStore,
    state: serde_json::Value,
}
impl Fixture {
    fn new(provider: &str) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let workspace = std::env::temp_dir().join(format!(
            "ta-consumption-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&workspace).unwrap();
        let store = MessageStore::open(&workspace).unwrap();
        let conn = crate::db::schema::open_db(store.db_path()).unwrap();
        conn.execute("insert into messages(message_id,owner_team_id,sender,recipient,content,status,created_at,updated_at,delivery_attempts) values('msg_one','team','operator','worker','body','accepted','old','old',0)", []).unwrap();
        let state = serde_json::json!({"team_key":"team","session_name":"session","agents":{"worker":{
            "status":"running","provider":provider,"pane_id":"%1","pane_pid":2,"window":"worker"
        }}});
        Self {
            workspace,
            store,
            state,
        }
    }
    fn transport(&self) -> OfflineTransport {
        OfflineTransport::new()
            .with_session_present(true)
            .with_targets(vec![PaneInfo {
                pane_id: PaneId::new("%1"),
                session: SessionName::new("session"),
                window_index: Some(1),
                window_name: Some(WindowName::new("worker")),
                pane_index: Some(0),
                tty: None,
                current_command: None,
                current_path: Some(self.workspace.clone()),
                active: true,
                pane_pid: Some(2),
                leader_env: Default::default(),
            }])
            .with_default_liveness(crate::transport::PaneLiveness::Live)
    }
    fn deliver(&self, transport: &OfflineTransport) -> DeliveryOutcome {
        let message = message_for_delivery(&self.store, "msg_one")
            .unwrap()
            .unwrap();
        deliver_prepared_message(
            &self.workspace,
            &self.store,
            transport,
            "msg_one",
            &EventLog::new(&self.workspace),
            &self.state,
            message,
            PreparedMessageState {
                scoped: None,
                canonical_owner_team_id: Some("team".into()),
            },
        )
        .unwrap()
    }
    fn receipt(&self) -> (String, i64) {
        let conn = crate::db::schema::open_db(self.store.db_path()).unwrap();
        (
            conn.query_row(
                "select status from messages where message_id='msg_one'",
                [],
                |r| r.get(0),
            )
            .unwrap(),
            conn.query_row(
                "select count(*) from scheduled_events where kind='enter_rescue'",
                [],
                |r| r.get(0),
            )
            .unwrap(),
        )
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.workspace);
    }
}
fn frame(composer: &str, transcript: &str) -> InputFrame {
    InputFrame {
        composer: composer.into(),
        transcript: transcript.into(),
        identity: "known-current-ui".into(),
        footer: "owned cwd".into(),
        busy: false,
        capsule: None,
    }
}

#[test]
fn current_token_in_composer_is_not_duplicate_delivered_or_repasted() {
    let f = Fixture::new("cursor_agent");
    let marker = message_token_marker("msg_one");
    let t = f
        .transport()
        .with_capture_for_pane("%1", &marker)
        .with_native_input_observation(frame(&marker, &marker));
    let out = f.deliver(&t);
    assert!(!out.ok);
    assert_eq!(f.receipt(), ("submitted_unverified".into(), 1));
    assert!(t.inject_payloads().is_empty());
}

#[test]
fn successful_transport_report_does_not_override_unconsumed_composer() {
    for composer in [
        "[team-agent-token:msg_one]",
        "[Pasted text #1 +25 lines]",
        "foreign draft",
    ] {
        let mut f = Fixture::new("cursor_agent");
        #[cfg(unix)]
        let panes = vec![crate::mcp_server::readiness::test_ready_worker(
            &f.workspace,
            &mut f.state,
            "team",
            "worker",
            "%1",
        )];
        #[cfg(not(unix))]
        let panes = f.transport().list_targets().unwrap();
        let t = f
            .transport()
            .with_targets(panes)
            .with_native_input_observation(frame(composer, ""));
        let out = f.deliver(&t);
        assert!(!out.ok, "composer={composer}");
        assert_eq!(f.receipt(), ("submitted_unverified".into(), 1));
        assert_eq!(t.inject_payloads().len(), 1);
    }
}

#[test]
fn unknown_current_native_surface_is_unverified_not_success() {
    let f = Fixture::new("pi");
    let t = f
        .transport()
        .with_native_input_error("native_surface_unknown");
    assert!(!f.deliver(&t).ok);
    assert_eq!(f.receipt(), ("submitted_unverified".into(), 1));
}

#[test]
fn pane_visibility_without_an_observation_protocol_is_not_duplicate_acceptance() {
    let f = Fixture::new("fake");
    let t = f
        .transport()
        .with_capture_for_pane("%1", message_token_marker("msg_one"));
    assert!(!f.deliver(&t).ok);
    assert_eq!(f.receipt(), ("submitted_unverified".into(), 1));
    assert!(t.inject_payloads().is_empty());
}

#[test]
fn current_empty_composer_and_unique_transcript_token_prove_consumption() {
    let f = Fixture::new("cursor_agent");
    let marker = message_token_marker("msg_one");
    let t = f
        .transport()
        .with_capture_for_pane("%1", &marker)
        .with_native_input_observation(frame("", &marker));
    assert!(f.deliver(&t).ok);
    assert_eq!(f.receipt(), ("delivered".into(), 0));
    assert!(t.inject_payloads().is_empty());
}

#[test]
fn busy_answers_or_duplicate_history_tokens_do_not_prove_consumption() {
    let f = Fixture::new("cursor_agent");
    let target = Target::Pane(PaneId::new("%1"));
    for transcript in [
        "answer is 5535",
        "[team-agent-token:msg_one]\n[team-agent-token:msg_one]",
    ] {
        let mut view = frame("", transcript);
        view.busy = true;
        let t = f.transport().with_native_input_observation(view);
        assert_eq!(
            observed_token_consumed(&t, &f.state, "worker", &target, "msg_one"),
            Some(false)
        );
    }
}

#[test]
fn unsupported_observation_keeps_existing_positive_transport_receipt() {
    let f = Fixture::new("fake");
    assert!(f.deliver(&f.transport()).ok);
    assert_eq!(f.receipt(), ("delivered".into(), 0));
}

#[test]
fn mark_unverified_schedules_once_and_insert_failure_rolls_back_status() {
    let f = Fixture::new("fake");
    f.store
        .mark("msg_one", "submitted_unverified", Some("opaque no proof"))
        .unwrap();
    f.store
        .mark("msg_one", "submitted_unverified", Some("again"))
        .unwrap();
    assert_eq!(f.receipt(), ("submitted_unverified".into(), 1));
    let conn = crate::db::schema::open_db(f.store.db_path()).unwrap();
    let payload: String = conn
        .query_row("select payload_json from scheduled_events", [], |r| {
            r.get(0)
        })
        .unwrap();
    let job: serde_json::Value = serde_json::from_str(&payload).unwrap();
    assert!(job["context"]["target"].is_null());
    assert!(job["context"]["proof"].is_null());

    let g = Fixture::new("fake");
    let conn = crate::db::schema::open_db(g.store.db_path()).unwrap();
    conn.execute_batch("create trigger refuse_rescue before insert on scheduled_events begin select raise(abort,'fixture failure'); end;").unwrap();
    assert!(g
        .store
        .mark("msg_one", "submitted_unverified", None)
        .is_err());
    assert_eq!(g.receipt(), ("accepted".into(), 0));
}
