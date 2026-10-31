#![allow(clippy::unwrap_used, clippy::panic)]
use super::*;
use crate::model::enums::Provider;
use crate::transport::enter_rescue::{EnterRescueProof, EnterRescueTarget};
use std::cell::Cell;
use std::sync::atomic::{AtomicUsize, Ordering};

struct Fixture {
    workspace: std::path::PathBuf,
    store: MessageStore,
    log: EventLog,
    context: EnterRescueContext,
}
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let workspace = std::env::temp_dir().join(format!(
            "ta-independent-rescue-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir(&workspace).unwrap();
        let store = MessageStore::open(&workspace).unwrap();
        let log = EventLog::new(&workspace);
        let context = EnterRescueContext {
            message_id: "msg_one".into(),
            owner_team_id: "team".into(),
            recipient: "worker".into(),
            payload_sha256: digest(&render_message("leader", None, "body", "msg_one")),
            target: Some(EnterRescueTarget {
                provider: Provider::Pi,
                endpoint: "/tmp/owned.sock".into(),
                pane_id: "%1".into(),
                pane_pid: 2,
                pane_height: 120,
                carrier_birth: "original-birth".into(),
                binding_nonce: "original-nonce".into(),
                session: "session".into(),
                window: "worker".into(),
            }),
            proof: Some(EnterRescueProof {
                ui_identity: "pi-model".into(),
                footer_sha256: digest("footer"),
                visible_marker: true,
                transcript_marker_absent: true,
                owned_capsule: None,
            }),
        };
        let fixture = Self {
            workspace,
            store,
            log,
            context,
        };
        fixture.conn().execute("insert into messages(message_id,owner_team_id,sender,recipient,content,status,created_at,updated_at,delivery_attempts) values('msg_one','team','leader','worker','body','submitted_unverified','old','old',1)",[]).unwrap();
        fixture.state("running", false);
        fixture
    }
    fn conn(&self) -> rusqlite::Connection {
        crate::db::schema::open_db(self.store.db_path()).unwrap()
    }
    fn state(&self, status: &str, stopped: bool) {
        let state = serde_json::json!({"team_key":"team","session_name":"session","agents":{"worker":{"status":status,"explicit_stop":stopped,"provider":"pi","pane_id":"%1","pane_pid":2,"window":"worker"}}});
        std::fs::write(
            crate::model::paths::runtime_dir(&self.workspace).join("state.json"),
            state.to_string(),
        )
        .unwrap();
    }
    fn schedule(&self, age: i64, bound: bool) {
        let mut conn = self.conn();
        let tx = conn.transaction().unwrap();
        schedule_in_tx(&tx, "msg_one", Utc::now() - Duration::seconds(age)).unwrap();
        tx.commit().unwrap();
        if bound {
            assert!(bind_pending_context(&self.store, &self.context).unwrap());
        }
    }
    fn job(&self) -> (i64, String, String) {
        self.conn()
            .query_row(
                "select id,payload_json,status from scheduled_events",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap()
    }
    fn message_status(&self) -> String {
        self.conn()
            .query_row(
                "select status from messages where message_id='msg_one'",
                [],
                |r| r.get(0),
            )
            .unwrap()
    }
    fn fire(&self, execute: &Executor<'_>) -> bool {
        let (id, payload, _) = self.job();
        fire_with(
            &self.workspace,
            &self.store,
            &self.log,
            id,
            &payload,
            execute,
        )
        .unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.workspace);
    }
}

















