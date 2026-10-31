//! Detached leader replies are stored, never represented as queued delivery.
#![allow(clippy::unwrap_used)]

#[path = "support/hermetic.rs"]
mod hermetic;

#[path = "support/direct_fixture.rs"]
mod direct_fixture;
use direct_fixture::DirectFixture;
use serde_json::{json, Value};
use team_agent::mcp_server::TeamOrchestratorTools;
use team_agent::messaging::MessageTarget;
use team_agent::model::ids::{AgentId, TeamKey};

#[test]
#[serial_test::serial(env)]
fn never_attached_leader_reply_is_stored_with_honest_physical_failure() {
    let _env = hermetic::HermeticTestEnv::enter("never_attached_leader_reply_is_stored_with_honest_physical_failure");
    let fixture = DirectFixture::new();
    fixture.set_state(&json!({"teams": {"current": {"agents": {"worker-1": {}}}}}));
    let tools = TeamOrchestratorTools::with_identity(
        &fixture.root,
        Some(AgentId::new("worker-1")),
        Some(TeamKey::new("current")),
    );
    let value = tools
        .send_message(
            &MessageTarget::Single("leader".into()),
            "offline reply",
            None,
            None,
            None,
        )
        .unwrap()
        .to_value();
    assert_eq!(value["status"], "stored");
    assert!(value["delivery_error"].as_str().unwrap().contains("leader"));
    for retired in [
        "delivery_pending",
        "poll_via",
        "message_status",
        "deferred_reason",
    ] {
        assert!(
            value.get(retired).is_none(),
            "stored is not a promise of future delivery"
        );
    }
    let records = fixture.inbox("leader");
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["message_id"], value["message_id"]);
    assert_eq!(records[0]["content"], "offline reply");
    assert!(!fixture.root.join(".team/runtime/team.db").exists());
}

#[test]
#[serial_test::serial(env)]
fn explicit_mailbox_needs_no_leader_endpoint_and_does_not_attempt_delivery() {
    let _env = hermetic::HermeticTestEnv::enter("explicit_mailbox_needs_no_leader_endpoint_and_does_not_attempt_delivery");
    let fixture = DirectFixture::new();
    fixture.set_state(&json!({"teams": {"current": {"agents": {"worker-1": {}}}}}));
    let tools = TeamOrchestratorTools::with_identity(
        &fixture.root,
        Some(AgentId::new("worker-1")),
        Some(TeamKey::new("current")),
    );
    let value = tools
        .send_message_with_presentation(
            &MessageTarget::Single("leader".into()),
            "mailbox reply",
            None,
            None,
            None,
            Some(&json!(true)),
            None,
        )
        .unwrap()
        .to_value();
    assert_eq!(value["status"], "stored");
    // R-M1a/M1b: a mailbox send is a durable record that never attempts live delivery
    // (`stored: true`, `live: false`); the receipt has no separate `mailbox` key.
    assert_eq!(value["stored"], true, "{value}");
    assert_eq!(value["live"], false, "{value}");
    assert!(value.get("delivery_error").is_none());
    assert_eq!(
        fixture.inbox("leader")[0]["message_id"],
        value["message_id"]
    );
    assert!(std::fs::read(fixture.root.join("received"))
        .unwrap()
        .is_empty());
    assert!(!fixture.root.join(".team/runtime/team.db").exists());
}

#[test]
#[serial_test::serial(env)]
fn attaching_a_leader_does_not_replay_old_inbox_records() {
    let _env = hermetic::HermeticTestEnv::enter("attaching_a_leader_does_not_replay_old_inbox_records");
    let fixture = DirectFixture::new();
    let original: Value = serde_json::from_slice(
        &std::fs::read(fixture.root.join(".team/runtime/state.json")).unwrap(),
    )
    .unwrap();
    fixture.set_state(&json!({"teams": {"current": {"agents": {"worker-1": {}}}}}));
    let tools = TeamOrchestratorTools::with_identity(
        &fixture.root,
        Some(AgentId::new("worker-1")),
        Some(TeamKey::new("current")),
    );
    tools
        .send_message(
            &MessageTarget::Single("leader".into()),
            "old offline reply",
            None,
            None,
            None,
        )
        .unwrap();
    // R-M1c: compare every inbox file (segment layout is the store's choice).
    let before = fixture.inbox_bytes();
    assert!(!before.is_empty());
    fixture.set_state(&original);
    let live = tools
        .send_message(
            &MessageTarget::Single("leader".into()),
            "new live reply",
            None,
            None,
            None,
        )
        .unwrap()
        .to_value();
    assert_eq!(live["status"], "sent");
    fixture.received("Team Agent message from worker-1:\n\nnew live reply");
    assert!(!std::fs::read_to_string(fixture.root.join("received"))
        .unwrap()
        .contains("old offline reply"));
    let after = fixture.inbox_bytes();
    assert!(before.iter().all(|(path, bytes)| after.get(path).is_some_and(|later| later.starts_with(bytes))),
        "attaching must not rewrite or replay earlier inbox records");
    assert_eq!(fixture.inbox("leader").len(), 2);
    assert!(!fixture.root.join(".team/runtime/team.db").exists());
}
