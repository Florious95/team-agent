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








