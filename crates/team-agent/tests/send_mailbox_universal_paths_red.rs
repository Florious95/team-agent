//! Mailbox sends append per-recipient inbox records without physical injection.
//! Ordinary sends use the same addressing but deliver directly to the PTY.
#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

#[path = "support/direct_fixture.rs"]
mod direct_fixture;

use direct_fixture::DirectFixture;
use serde_json::json;
use team_agent::mcp_server::TeamOrchestratorTools;
use team_agent::messaging::MessageTarget;
use team_agent::model::ids::{AgentId, TeamKey};

fn tools(fixture: &DirectFixture) -> TeamOrchestratorTools {
    TeamOrchestratorTools::with_identity(
        &fixture.root,
        Some(AgentId::new("leader")),
        Some(TeamKey::new("current")),
    )
}

#[test]
fn mailbox_to_worker_is_pullable_but_never_physically_injected() {
    let fixture = DirectFixture::new();
    let tools = tools(&fixture);
    let mailbox = tools
        .send_message_with_presentation(
            &MessageTarget::Single("worker-1".into()),
            "worker mailbox token",
            None,
            None,
            None,
            Some(&json!(true)),
            None,
        )
        .unwrap()
        .to_value();
    assert_eq!(mailbox["status"], "stored");
    assert_eq!(mailbox["mailbox"], true);
    assert!(mailbox.get("delivery").is_none());
    let records = fixture.inbox("worker-1");
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["message_id"], mailbox["message_id"]);
    assert_eq!(records[0]["content"], "worker mailbox token");
    assert_eq!(records[0]["sender"], "leader");
    assert_eq!(records[0]["team"], "current");

    let path = fixture.root.join(".team/inbox/worker-1.jsonl");
    let before = std::fs::read(&path).unwrap();
    for _ in 0..2 {
        let pulled =
            team_agent::inbox_store::read(&fixture.root, "worker-1", 20, Some("current")).unwrap();
        assert_eq!(pulled["messages"], json!(records));
    }
    let ordinary = tools
        .send_message(
            &MessageTarget::Single("worker-1".into()),
            "ordinary positive control",
            None,
            None,
            None,
        )
        .unwrap()
        .to_value();
    assert_eq!(ordinary["status"], "sent");
    assert_eq!(ordinary["delivery"], "physical");
    assert!(ordinary.get("message_id").is_none());
    fixture.received("Team Agent message from leader:\n\nordinary positive control");
    assert!(!std::fs::read_to_string(fixture.root.join("received"))
        .unwrap()
        .contains("worker mailbox token"));
    assert_eq!(
        std::fs::read(path).unwrap(),
        before,
        "reads and ordinary sends cannot mutate the inbox"
    );
    assert!(!fixture.root.join(".team/runtime/team.db").exists());
}

#[test]
fn mailbox_broadcast_appends_to_every_published_worker_except_sender() {
    let fixture = DirectFixture::new();
    fixture.set_state(&json!({"teams": {"current": {
        "tmux_socket": fixture.socket,
        "leader": {"pane_id": fixture.pane},
        "agents": {
            "sender": {}, "worker-1": {"pane_id": fixture.pane},
            "worker-2": {"pane_id": fixture.pane}
        }
    }}}));
    let tools = TeamOrchestratorTools::with_identity(
        &fixture.root,
        Some(AgentId::new("sender")),
        Some(TeamKey::new("current")),
    );
    let mailbox = tools
        .send_message_with_presentation(
            &MessageTarget::Broadcast,
            "broadcast mailbox token",
            None,
            None,
            None,
            Some(&json!(true)),
            None,
        )
        .unwrap()
        .to_value();
    let receipts = mailbox["results"].as_array().unwrap();
    assert_eq!(receipts.len(), 2);
    assert_ne!(receipts[0]["message_id"], receipts[1]["message_id"]);
    for receipt in receipts {
        assert_eq!(receipt["status"], "stored");
        assert_eq!(receipt["mailbox"], true);
        let recipient = receipt["recipient"].as_str().unwrap();
        let records = fixture.inbox(recipient);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0]["message_id"], receipt["message_id"]);
        assert_eq!(records[0]["content"], "broadcast mailbox token");
    }
    assert!(!fixture.root.join(".team/inbox/sender.jsonl").exists());
    assert!(
        !fixture.root.join(".team/inbox/leader.jsonl").exists(),
        "broadcast enumerates workers, not an implicit leader destination"
    );
    let ordinary = tools
        .send_message(
            &MessageTarget::Broadcast,
            "broadcast physical control",
            None,
            None,
            None,
        )
        .unwrap()
        .to_value();
    let results = ordinary["results"].as_array().unwrap();
    assert_eq!(results.len(), 2);
    assert!(results
        .iter()
        .all(|receipt| receipt["delivery"] == "physical" && receipt.get("message_id").is_none()));
    fixture.received("Team Agent message from sender:\n\nbroadcast physical control");
    assert!(!std::fs::read_to_string(fixture.root.join("received"))
        .unwrap()
        .contains("broadcast mailbox token"));
    for recipient in ["worker-1", "worker-2"] {
        assert_eq!(fixture.inbox(recipient).len(), 1);
    }
    assert!(!fixture.root.join(".team/runtime/team.db").exists());
}
