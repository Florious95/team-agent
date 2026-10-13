#![allow(clippy::unwrap_used, clippy::panic)]
use super::*;
use crate::model::enums::Provider;
use crate::transport::native_input::{NativeInputBinding, NativeInputRequest};
use std::sync::atomic::{AtomicUsize, Ordering};

struct Fixture {
    workspace: std::path::PathBuf,
    store: MessageStore,
    log: EventLog,
    evidence: NativeInputEvidence,
}
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let workspace = std::env::temp_dir().join(format!(
            "ta-enter-rescue-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir(&workspace).unwrap();
        let store = MessageStore::open(&workspace).unwrap();
        let log = EventLog::new(&workspace);
        let request = NativeInputRequest {
            message_id: "msg_one".into(),
            owner_team_id: "team".into(),
            recipient: "worker".into(),
            provider: Provider::Pi,
            pane_id: "%1".into(),
            pane_pid: 2,
            session: "session".into(),
            window: "worker".into(),
        };
        let evidence = NativeInputEvidence {
            request,
            binding: NativeInputBinding {
                endpoint: "/tmp/owned-test.sock".into(),
                pane_id: "%1".into(),
                pane_pid: 2,
                pane_height: 120,
                carrier_birth: "test-birth".into(),
                binding_nonce: "test-nonce".into(),
                session: "session".into(),
                window: "worker".into(),
            },
            payload_sha256: digest(&render_message("leader", None, "body", "msg_one")),
            ui_identity: "gpt-6-luna".into(),
            footer_sha256: digest(""),
            baseline_sha256: digest("baseline"),
            baseline_capsules: vec![],
            phase: NativeInputPhase::Submitted,
            capsule: None,
        };
        let fixture = Self {
            workspace,
            store,
            log,
            evidence,
        };
        let conn = fixture.conn();
        conn.execute("insert into messages(message_id,owner_team_id,sender,recipient,content,status,created_at,updated_at,delivery_attempts) values('msg_one','team','leader','worker','body','target_resolved','old','old',1)", []).unwrap();
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
    fn park(&self, age: i64) {
        park_at(
            &self.store,
            &self.evidence,
            1,
            "uncertain",
            &self.log,
            Utc::now() - Duration::seconds(age),
        )
        .unwrap();
    }
    fn job(&self) -> (i64, String, String) {
        self.conn()
            .query_row(
                "select id,payload_json,status from scheduled_events",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap()
    }
    fn message_status(&self) -> String {
        self.conn()
            .query_row(
                "select status from messages where message_id='msg_one'",
                [],
                |row| row.get(0),
            )
            .unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.workspace);
    }
}

#[test]
fn enter_rescue_first_successful_key_arms_original_turn_before_acceptance_poll() {
    let f = Fixture::new();
    f.park(95);
    super::super::delivery::with_rescue_submit_observer(
        &f.workspace,
        &f.store,
        &f.evidence,
        &f.log,
        |observer| {
            observer.after_physical_submit();
            let state = crate::state::persist::load_runtime_state(&f.workspace).unwrap();
            assert_eq!(
                state["agents"]["worker"]["current_turn_message_id"],
                "msg_one"
            );
            Ok(EnterRescueOutcome::Entered { accepted: false })
        },
    )
    .unwrap();
    assert_eq!(f.message_status(), "submitted_unverified");
}

#[test]
fn enter_rescue_post_key_owner_or_receiver_loss_is_unknown_without_cross_team_write() {
    for change in [
        "owner",
        "receiver",
        "pane",
        "pane_pid",
        "missing_pane_pid",
        "session",
        "stopped",
    ] {
        let f = Fixture::new();
        f.park(95);
        let (id, payload, _) = f.job();
        let effects = AtomicUsize::new(0);
        let path = crate::model::paths::runtime_dir(&f.workspace).join("state.json");
        let replacement = std::cell::RefCell::new(String::new());
        // Controlled post-key mutation; this is not a native TUI reproduction.
        fire_with(
            &f.workspace,
            &f.store,
            &f.log,
            id,
            &payload,
            &|evidence, _| {
                super::super::delivery::with_rescue_submit_observer(
                    &f.workspace,
                    &f.store,
                    evidence,
                    &f.log,
                    |observer| {
                        effects.fetch_add(1, Ordering::SeqCst);
                        let mut state =
                            crate::state::persist::load_runtime_state(&f.workspace).unwrap();
                        match change {
                            "owner" => {
                                state["team_key"] = "other".into();
                                state["active_team_key"] = "other".into();
                                state.as_object_mut().unwrap().remove("teams");
                            }
                            "receiver" => {
                                state["agents"].as_object_mut().unwrap().remove("worker");
                            }
                            "pane" => state["agents"]["worker"]["pane_id"] = "%2".into(),
                            "pane_pid" => state["agents"]["worker"]["pane_pid"] = 3.into(),
                            "missing_pane_pid" => {
                                state["agents"]["worker"]
                                    .as_object_mut()
                                    .unwrap()
                                    .remove("pane_pid");
                            }
                            "session" => state["session_name"] = "other-session".into(),
                            "stopped" => state["agents"]["worker"]["explicit_stop"] = true.into(),
                            _ => unreachable!(),
                        }
                        *replacement.borrow_mut() = state.to_string();
                        std::fs::write(&path, replacement.borrow().as_bytes()).unwrap();
                        observer.after_physical_submit();
                        Ok(EnterRescueOutcome::Entered { accepted: true })
                    },
                )
            },
        )
        .unwrap();
        // The rejected commit must not change even one byte of the replacement state.
        let expected = replacement.borrow().clone();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), expected);
        assert_eq!(f.job().2, "unknown", "{change}");
        assert_eq!(f.message_status(), "submitted_unverified");
        let state: serde_json::Value = serde_json::from_str(&expected).unwrap();
        assert!(
            state["agents"]["worker"]["current_turn_message_id"].is_null(),
            "{change}"
        );
        assert!(!fire_with(
            &f.workspace,
            &f.store,
            &f.log,
            id,
            &payload,
            &|_, _| panic!("post-key error replay")
        )
        .unwrap());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), expected);
        assert_eq!(effects.load(Ordering::SeqCst), 1);
    }
}

#[test]
fn enter_rescue_owner_loss_at_state_save_hook_refuses_without_default_fallback() {
    let f = Fixture::new();
    f.park(95);
    let path = crate::model::paths::runtime_dir(&f.workspace).join("state.json");
    let replacement = serde_json::json!({"team_key":"other","active_team_key":"other","session_name":"session","agents":{"worker":{"provider":"pi","status":"running","pane_id":"%1","pane_pid":2,"window":"worker"}}}).to_string();
    let hook_path = path.clone();
    let hook_state = replacement.clone();
    crate::state::repository::test_support::set_before_commit(move || {
        std::fs::write(hook_path, hook_state).unwrap();
    });
    assert!(super::super::delivery::with_rescue_submit_observer(
        &f.workspace,
        &f.store,
        &f.evidence,
        &f.log,
        |observer| {
            observer.after_physical_submit();
            Ok(EnterRescueOutcome::Entered { accepted: true })
        },
    )
    .is_err());
    assert_eq!(std::fs::read_to_string(path).unwrap(), replacement);
    assert_eq!(f.message_status(), "submitted_unverified");
}

#[test]
fn enter_rescue_switched_top_projection_updates_only_retained_original_owner() {
    let f = Fixture::new();
    f.park(95);
    super::super::delivery::with_rescue_submit_observer(
        &f.workspace, &f.store, &f.evidence, &f.log, |observer| {
            let original = crate::state::persist::load_runtime_state(&f.workspace).unwrap();
            let mut other = original.clone();
            other["team_key"] = "other".into();
            other["active_team_key"] = "other".into();
            other["session_name"] = "other-session".into();
            other["agents"]["worker"]["pane_id"] = "%2".into();
            other["agents"]["worker"]["pane_pid"] = 3.into();
            let mut state = other.clone();
            state["teams"] = serde_json::json!({"team": crate::state::projection::compact_team_state(&original), "other": crate::state::projection::compact_team_state(&other)});
            std::fs::write(crate::model::paths::runtime_dir(&f.workspace).join("state.json"), state.to_string()).unwrap();
            observer.after_physical_submit();
            Ok(EnterRescueOutcome::Entered { accepted: false })
        },
    ).unwrap();
    let state = crate::state::persist::load_runtime_state(&f.workspace).unwrap();
    assert_eq!(
        state["teams"]["team"]["agents"]["worker"]["current_turn_message_id"],
        "msg_one"
    );
    assert!(state["teams"]["other"]["agents"]["worker"]["current_turn_message_id"].is_null());
    assert!(state["agents"]["worker"]["current_turn_message_id"].is_null());
    assert_eq!(state["agents"]["worker"]["pane_id"], "%2");
}

#[test]
fn enter_rescue_atomic_park_and_due90_no_early_effect() {
    let f = Fixture::new();
    f.park(0);
    let (id, payload, status) = f.job();
    assert_eq!(status, "pending");
    assert_eq!(f.message_status(), "submitted_unverified");
    let times: (String, String, String) = f
        .conn()
        .query_row(
            "select owner_team_id,created_at,due_at from scheduled_events",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(times.0, "team");
    assert_eq!(
        DateTime::parse_from_rfc3339(&times.2).unwrap()
            - DateTime::parse_from_rfc3339(&times.1).unwrap(),
        Duration::seconds(90)
    );
    assert!(!fire_with(
        &f.workspace,
        &f.store,
        &f.log,
        id,
        &payload,
        &|_, _| panic!("not due")
    )
    .unwrap());
    assert!(park_at(&f.store, &f.evidence, 1, "again", &f.log, Utc::now()).is_err());
    assert_eq!(
        f.conn()
            .query_row("select count(*) from scheduled_events", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
}

#[test]
fn enter_rescue_insert_failure_rolls_back_original_uncertainty() {
    let f = Fixture::new();
    f.conn().execute_batch("create trigger refuse_job before insert on scheduled_events begin select raise(abort,'fixture'); end;").unwrap();
    assert!(park_at(&f.store, &f.evidence, 1, "uncertain", &f.log, Utc::now()).is_err());
    assert_eq!(f.message_status(), "target_resolved");
}

#[test]
fn enter_rescue_no_effect_deferral_releases_only_exact_unspent_claim() {
    let f = Fixture::new();
    release_no_effect(&f.store, "msg_one", 2).unwrap();
    assert_eq!(
        f.conn()
            .query_row("select delivery_attempts from messages", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
    release_no_effect(&f.store, "msg_one", 1).unwrap();
    assert_eq!(
        f.conn()
            .query_row("select delivery_attempts from messages", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert!(f.store.claim_for_delivery("msg_one").unwrap());
    assert_eq!(
        f.conn()
            .query_row("select delivery_attempts from messages", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
}

#[test]
fn enter_rescue_canonical_stopped_changed_consumed_expired_and_superseded_refuse() {
    for case in [
        "stopped",
        "explicit_stop",
        "content",
        "owner",
        "consumed",
        "expired",
        "superseded",
        "job_scope",
    ] {
        let f = Fixture::new();
        f.park(if case == "expired" { 121 } else { 95 });
        match case {
            "stopped" => f.state("stopped", false),
            "explicit_stop" => f.state("running", true),
            "content" => {
                f.conn()
                    .execute("update messages set content='changed'", [])
                    .unwrap();
            }
            "owner" => {
                f.conn()
                    .execute("update messages set owner_team_id='other'", [])
                    .unwrap();
            }
            "consumed" => {
                f.conn()
                    .execute("update messages set status='acknowledged'", [])
                    .unwrap();
            }
            "superseded" => {
                f.conn().execute("insert into messages(message_id,owner_team_id,recipient,status,created_at,delivery_attempts) values('msg_two','team','worker','submitted_unverified','z',1)",[]).unwrap();
            }
            "job_scope" => {
                f.conn()
                    .execute("update scheduled_events set owner_team_id='other'", [])
                    .unwrap();
            }
            "expired" => {}
            _ => unreachable!(),
        }
        let (id, payload, _) = f.job();
        assert!(fire_with(
            &f.workspace,
            &f.store,
            &f.log,
            id,
            &payload,
            &|_, _| panic!("{case}: forbidden effect")
        )
        .unwrap());
        assert_eq!(f.job().2, "refused", "{case}");
    }
}

#[test]
fn enter_rescue_no_effect_can_reobserve_but_unknown_or_claimed_never_replays() {
    let f = Fixture::new();
    f.park(95);
    let (id, payload, _) = f.job();
    assert!(fire_with(
        &f.workspace,
        &f.store,
        &f.log,
        id,
        &payload,
        &|_, authorize| {
            assert!(authorize());
            Ok(EnterRescueOutcome::NoEffect {
                reason: "busy",
                reobserve: true,
            })
        }
    )
    .unwrap());
    assert_eq!(f.job().2, "pending");
    assert!(!fire_with(
        &f.workspace,
        &f.store,
        &f.log,
        id,
        &payload,
        &|_, _| panic!("not yet due")
    )
    .unwrap());
    f.conn()
        .execute("update scheduled_events set due_at='0'", [])
        .unwrap();
    assert!(fire_with(
        &f.workspace,
        &f.store,
        &f.log,
        id,
        &payload,
        &|_, authorize| {
            assert!(authorize());
            Ok(EnterRescueOutcome::Entered { accepted: false })
        }
    )
    .unwrap());
    assert_eq!(f.job().2, "unknown");
    assert_eq!(f.message_status(), "submitted_unverified");
    assert!(!fire_with(
        &f.workspace,
        &f.store,
        &f.log,
        id,
        &payload,
        &|_, _| panic!("unknown replay")
    )
    .unwrap());
    f.conn()
        .execute("update scheduled_events set status='claimed'", [])
        .unwrap();
    assert!(!fire_with(
        &f.workspace,
        &f.store,
        &f.log,
        id,
        &payload,
        &|_, _| panic!("crash replay")
    )
    .unwrap());
}

#[test]
fn enter_rescue_concurrent_claim_is_one_effect_and_updates_original_only() {
    let f = Fixture::new();
    f.park(95);
    let (id, payload, _) = f.job();
    let effects = AtomicUsize::new(0);
    std::thread::scope(|scope| {
        for _ in 0..2 {
            scope.spawn(|| {
                fire_with(
                    &f.workspace,
                    &f.store,
                    &f.log,
                    id,
                    &payload,
                    &|_, authorize| {
                        assert!(authorize());
                        effects.fetch_add(1, Ordering::SeqCst);
                        std::thread::sleep(std::time::Duration::from_millis(20));
                        Ok(EnterRescueOutcome::Entered { accepted: true })
                    },
                )
                .unwrap()
            });
        }
    });
    assert_eq!(effects.load(Ordering::SeqCst), 1);
    assert_eq!(f.job().2, "done");
    assert_eq!(f.message_status(), "delivered");
    assert_eq!(
        f.conn()
            .query_row("select count(*) from messages", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        1
    );
}

#[test]
fn enter_rescue_revalidates_at_effect_and_keeps_unknown_errors_terminal() {
    let f = Fixture::new();
    f.park(95);
    let (id, payload, _) = f.job();
    fire_with(
        &f.workspace,
        &f.store,
        &f.log,
        id,
        &payload,
        &|_, authorize| {
            f.state("stopped", true);
            assert!(!authorize());
            Ok(EnterRescueOutcome::NoEffect {
                reason: "canonical_changed",
                reobserve: false,
            })
        },
    )
    .unwrap();
    assert_eq!(f.job().2, "refused");
    let other = Fixture::new();
    other.park(95);
    let (id, payload, _) = other.job();
    fire_with(
        &other.workspace,
        &other.store,
        &other.log,
        id,
        &payload,
        &|_, _| {
            Err(crate::transport::TransportError::Io(std::io::Error::other("may have sent")).into())
        },
    )
    .unwrap();
    assert_eq!(other.job().2, "unknown");
    assert!(!fire_with(
        &other.workspace,
        &other.store,
        &other.log,
        id,
        &payload,
        &|_, _| panic!("error replay")
    )
    .unwrap());
}
