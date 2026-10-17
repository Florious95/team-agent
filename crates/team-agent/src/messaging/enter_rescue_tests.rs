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

#[test]
fn enter_rescue_first_successful_key_arms_original_turn_before_acceptance_poll() {
    let f = Fixture::new();
    f.schedule(95, true);
    super::super::delivery::with_rescue_submit_observer(
        &f.workspace,
        &f.store,
        &f.context,
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
        f.schedule(95, true);
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
            &|context, _| {
                super::super::delivery::with_rescue_submit_observer(
                    &f.workspace,
                    &f.store,
                    context,
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
        // A rejected post-key commit must preserve every byte of replacement state.
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
    f.schedule(95, true);
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
        &f.context,
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
    f.schedule(95, true);
    super::super::delivery::with_rescue_submit_observer(
        &f.workspace, &f.store, &f.context, &f.log, |observer| {
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
fn every_unverified_row_registers_independently_of_native_receipts() {
    let f = Fixture::new();
    f.schedule(0, false);
    let (_, payload, status) = f.job();
    let job: RescueJob = serde_json::from_str(&payload).unwrap();
    assert_eq!(status, "pending");
    assert!(job.context.target.is_none());
    assert!(job.context.proof.is_none());
    assert!(!payload.contains("evidence"));
    assert!(!payload.contains("NativeInput"));
    let due: String = f
        .conn()
        .query_row("select due_at from scheduled_events", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        timestamp(&due).unwrap() - timestamp(&job.registered_at).unwrap(),
        Duration::seconds(90)
    );
    assert_eq!(
        timestamp(&job.expires_at).unwrap() - timestamp(&job.registered_at).unwrap(),
        Duration::seconds(120)
    );
}

#[test]
fn even_missing_owner_or_recipient_gets_a_job_then_refuses_without_keys() {
    let f = Fixture::new();
    f.conn()
        .execute("update messages set owner_team_id=null,recipient=null", [])
        .unwrap();
    f.schedule(95, false);
    assert_eq!(f.job().2, "pending");
    assert!(f.fire(&|_, _| panic!("unowned registration is not key authority")));
    assert_eq!(f.job().2, "refused");
}

#[test]
fn registration_is_durable_and_never_resets_a_claimed_or_unknown_attempt() {
    for status in ["pending", "claimed", "unknown", "done", "refused"] {
        let f = Fixture::new();
        f.schedule(95, false);
        f.conn()
            .execute("update scheduled_events set status=?1", [status])
            .unwrap();
        let original = f.job();
        f.schedule(0, false);
        assert_eq!(f.job(), original);
        let count: i64 = f
            .conn()
            .query_row("select count(*) from scheduled_events", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);
    }
}

#[test]
fn unbound_job_is_observed_then_refused_without_executor() {
    let f = Fixture::new();
    f.schedule(95, false);
    assert!(f.fire(&|_, _| panic!("unbound job must not reach keys")));
    assert_eq!(f.job().2, "refused");
    assert_eq!(f.message_status(), "submitted_unverified");
    let result: String = f
        .conn()
        .query_row("select result_json from scheduled_events", [], |r| r.get(0))
        .unwrap();
    assert!(result.contains("proof_missing"));
}

#[test]
fn supplement_cannot_change_original_clock_scope_digest_or_bound_carrier() {
    let f = Fixture::new();
    f.schedule(95, false);
    let before: (String, String) = f
        .conn()
        .query_row("select due_at,created_at from scheduled_events", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap();
    assert!(bind_pending_context(&f.store, &f.context).unwrap());
    let after: (String, String) = f
        .conn()
        .query_row("select due_at,created_at from scheduled_events", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap();
    assert_eq!(before, after);
    for field in [
        "owner",
        "recipient",
        "digest",
        "pid",
        "birth",
        "nonce",
        "endpoint",
    ] {
        let mut other = f.context.clone();
        match field {
            "owner" => other.owner_team_id = "other".into(),
            "recipient" => other.recipient = "other".into(),
            "digest" => other.payload_sha256 = digest("different"),
            "pid" => other.target.as_mut().unwrap().pane_pid = 3,
            "birth" => other.target.as_mut().unwrap().carrier_birth = "reused".into(),
            "nonce" => other.target.as_mut().unwrap().binding_nonce = "replacement".into(),
            _ => other.target.as_mut().unwrap().endpoint = "/tmp/other.sock".into(),
        }
        assert!(!bind_pending_context(&f.store, &other).unwrap(), "{field}");
    }
}

#[test]
fn early_expired_or_extended_jobs_have_no_effect() {
    for (age, extend) in [(0, false), (121, false), (95, true)] {
        let f = Fixture::new();
        f.schedule(age, true);
        let (id, payload, _) = f.job();
        let payload = if extend {
            let mut j: RescueJob = serde_json::from_str(&payload).unwrap();
            j.expires_at = (Utc::now() + Duration::hours(1)).to_rfc3339();
            let p = serde_json::to_string(&j).unwrap();
            f.conn()
                .execute("update scheduled_events set payload_json=?1", [&p])
                .unwrap();
            p
        } else {
            payload
        };
        f.conn()
            .execute(
                "update scheduled_events set due_at='1970-01-01T00:00:00+00:00'",
                [],
            )
            .unwrap();
        assert!(fire_with(
            &f.workspace,
            &f.store,
            &f.log,
            id,
            &payload,
            &|_, _| panic!("invalid clock")
        )
        .unwrap());
        assert_eq!(f.job().2, "refused");
    }
}

#[test]
fn current_canonical_message_and_original_owner_recipient_remain_required() {
    for change in [
        "ack",
        "content",
        "owner",
        "recipient",
        "stop",
        "pid",
        "provider",
        "session",
        "window",
        "newer",
    ] {
        let f = Fixture::new();
        f.schedule(95, true);
        match change {
            "ack" => {
                f.conn()
                    .execute("update messages set status='acknowledged'", [])
                    .unwrap();
            }
            "content" => {
                f.conn()
                    .execute("update messages set content='edited'", [])
                    .unwrap();
            }
            "owner" => {
                f.conn()
                    .execute("update messages set owner_team_id='other'", [])
                    .unwrap();
            }
            "recipient" => {
                f.conn()
                    .execute("update messages set recipient='other'", [])
                    .unwrap();
            }
            "stop" => f.state("running", true),
            "newer" => {
                f.conn().execute("insert into messages(message_id,owner_team_id,recipient,created_at,delivery_attempts) values('msg_new','team','worker','z',1)",[]).unwrap();
            }
            field => {
                let p = crate::model::paths::runtime_dir(&f.workspace).join("state.json");
                let mut state: serde_json::Value =
                    serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
                match field {
                    "pid" => state["agents"]["worker"]["pane_pid"] = 3.into(),
                    "provider" => state["agents"]["worker"]["provider"] = "cursor_agent".into(),
                    "session" => state["session_name"] = "other".into(),
                    _ => state["agents"]["worker"]["window"] = "other".into(),
                };
                std::fs::write(p, state.to_string()).unwrap();
            }
        }
        assert!(f.fire(&|_, _| panic!("canonical change {change}")));
        assert_eq!(f.job().2, "refused", "{change}");
    }
}

#[test]
fn one_cas_and_ambiguous_effect_or_error_never_replay() {
    for error in [false, true] {
        let f = Fixture::new();
        f.schedule(95, true);
        let calls = Cell::new(0);
        assert!(f.fire(&|_, authorize| {
            calls.set(calls.get() + 1);
            assert!(authorize());
            if error {
                Err(MessagingError::Validation("may have sent".into()))
            } else {
                Ok(EnterRescueOutcome::Entered { accepted: false })
            }
        }));
        assert_eq!(f.job().2, "unknown");
        assert!(!f.fire(&|_, _| panic!("unknown replay")));
        assert_eq!(calls.get(), 1);
        assert_eq!(f.message_status(), "submitted_unverified");
    }
    let f = Fixture::new();
    f.schedule(95, true);
    f.conn()
        .execute("update scheduled_events set status='claimed'", [])
        .unwrap();
    assert!(!f.fire(&|_, _| panic!("crash replay")));
}

#[test]
fn no_effect_busy_reobserves_within_original_window_only() {
    let f = Fixture::new();
    f.schedule(95, true);
    assert!(f.fire(&|_, _| Ok(EnterRescueOutcome::NoEffect {
        reason: "busy",
        reobserve: true
    })));
    assert_eq!(f.job().2, "pending");
    let due: String = f
        .conn()
        .query_row("select due_at from scheduled_events", [], |r| r.get(0))
        .unwrap();
    assert!(timestamp(&due).unwrap() > Utc::now());
    let f = Fixture::new();
    f.schedule(119, true);
    assert!(f.fire(&|_, _| Ok(EnterRescueOutcome::NoEffect {
        reason: "busy",
        reobserve: true
    })));
    assert_eq!(f.job().2, "refused");
}

#[test]
fn consumed_or_single_accepted_enter_updates_only_original_unverified_row() {
    for consumed in [false, true] {
        let f = Fixture::new();
        f.schedule(95, true);
        assert!(f.fire(&|_, _| Ok(if consumed {
            EnterRescueOutcome::Consumed
        } else {
            EnterRescueOutcome::Entered { accepted: true }
        })));
        assert_eq!(f.job().2, "done");
        assert_eq!(f.message_status(), "delivered");
        assert!(!f.fire(&|_, _| panic!("done replay")));
    }
    let f = Fixture::new();
    f.schedule(95, true);
    assert!(f.fire(&|_, _| {
        f.conn()
            .execute("update messages set status='acknowledged'", [])
            .unwrap();
        Ok(EnterRescueOutcome::Entered { accepted: true })
    }));
    assert_eq!(f.message_status(), "acknowledged");
}

#[test]
fn final_authorizer_detects_canonical_changes_during_transport_checks() {
    let f = Fixture::new();
    f.schedule(95, true);
    assert!(f.fire(&|_, authorize| {
        assert!(authorize());
        f.state("stopped", true);
        assert!(!authorize());
        Ok(EnterRescueOutcome::NoEffect {
            reason: "canonical_changed",
            reobserve: false,
        })
    }));
    assert_eq!(f.job().2, "refused");
}

#[test]
fn revoked_scheduler_claim_is_not_key_authority() {
    let f = Fixture::new();
    f.schedule(95, true);
    assert!(f.fire(&|_, authorize| {
        assert!(authorize());
        f.conn()
            .execute("update scheduled_events set status='refused'", [])
            .unwrap();
        assert!(!authorize());
        Ok(EnterRescueOutcome::NoEffect {
            reason: "claim_revoked",
            reobserve: false,
        })
    }));
    assert_eq!(f.job().2, "refused");
}

#[test]
fn tampered_event_scope_or_untrusted_proof_cannot_execute() {
    for change in ["scope", "capsule", "capsule_scope", "missing"] {
        let f = Fixture::new();
        f.schedule(95, false);
        let mut context = f.context.clone();
        if change == "capsule" || change == "capsule_scope" {
            let scope_sha256 = if change == "capsule_scope" {
                digest("foreign receipt")
            } else {
                context.scope_sha256().unwrap()
            };
            let proof = context.proof.as_mut().unwrap();
            proof.visible_marker = false;
            proof.owned_capsule = Some(crate::transport::enter_rescue::OwnedCapsule {
                scope_sha256,
                label: "[paste #1 +30 lines]".into(),
                baseline_sha256: digest("baseline"),
                baseline_capsules: vec![],
                observed_after_owned_paste: change == "capsule_scope",
            });
        }
        if change == "missing" {
            context.proof = None;
        }
        assert!(bind_pending_context(&f.store, &context).unwrap());
        if change == "scope" {
            f.conn()
                .execute("update scheduled_events set owner_team_id='other'", [])
                .unwrap();
        }
        assert!(f.fire(&|_, _| panic!("untrusted job")));
        assert_eq!(f.job().2, "refused");
    }
}
