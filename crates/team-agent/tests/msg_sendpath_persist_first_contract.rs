//! Track 3 entrypoint parity: physical send does not persist or recover a queue.
#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

#[path = "support/hermetic.rs"]
mod hermetic;

#[path = "support/direct_fixture.rs"]
mod direct_fixture;
#[path = "support/mcp_sim_harness.rs"]
#[allow(dead_code)]
mod mcp_sim_harness;

use direct_fixture::DirectFixture;
use serde_json::{json, Value};
use std::process::{Command, Output};

fn cli(fixture: &DirectFixture, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_team-agent"))
        .arg("send")
        .args(args)
        .args(["--workspace", fixture.root.to_str().unwrap(), "--json"])
        .env_remove("TEAM_AGENT_ID")
        .env_remove("TEAM_AGENT_AGENT_ID")
        .env_remove("TEAM_AGENT_WORKSPACE")
        .env_remove("TEAM_AGENT_OWNER_TEAM_ID")
        .env_remove("TEAM_AGENT_TEAM_ID")
        .output()
        .unwrap()
}

fn sent(output: Output) -> Value {
    assert!(
        output.status.success(),
        "{} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["status"], "sent");
    assert_eq!(value["delivery"], "physical");
    assert_eq!(value["accepted"], Value::Null);
    // R-M1a (M1 store-first): the physical receipt names its durable inbox record.
    assert!(value["message_id"].as_str().is_some_and(|id| !id.is_empty()), "{value}");
    assert_eq!(value["stored"], true, "{value}");
    assert!(value.get("delivery_pending").is_none());
    value
}

/// Inbox records of `recipient` (team `current`) whose JSON mentions `text`.
fn inbox_hits(fixture: &DirectFixture, recipient: &str, text: &str) -> usize {
    fixture.inbox(recipient).iter().filter(|record| record.to_string().contains(text)).count()
}

#[test]
#[serial_test::serial(env)]
fn c1_dead_coordinator_is_ignored_and_snapshot_is_immutable() {
    let _env = hermetic::HermeticTestEnv::enter("c1_dead_coordinator_is_ignored_and_snapshot_is_immutable");
    let fixture = DirectFixture::new();
    let ws = team_agent::coordinator::WorkspacePath::new(fixture.root.clone());
    let pid = team_agent::coordinator::Pid::new(99_999_999);
    team_agent::coordinator::write_coordinator_metadata(
        &ws,
        pid,
        team_agent::coordinator::MetadataSource::Boot,
    )
    .unwrap();
    let pid_path = team_agent::coordinator::coordinator_pid_path(&ws);
    std::fs::write(&pid_path, pid.to_string()).unwrap();
    let state_path = fixture.root.join(".team/runtime/state.json");
    let before = std::fs::read(&state_path).unwrap();
    sent(cli(&fixture, &["worker-1", "without coordinator"]));
    fixture.received("Team Agent message from leader:\n\nwithout coordinator");
    assert_eq!(std::fs::read(state_path).unwrap(), before);
    // M5 upgrade cleanup may retire stale legacy coordinator metadata, but never rewrites it.
    if let Ok(recorded) = std::fs::read_to_string(&pid_path) {
        assert_eq!(recorded, pid.to_string());
    }
    assert!(!fixture.root.join(".team/runtime/team.db").exists());
    // R-M1a: the message is stored once in the recipient's inbox.
    assert_eq!(inbox_hits(&fixture, "worker-1", "without coordinator"), 1);
}

#[test]
#[serial_test::serial(env)]
fn c2_positional_alias_and_real_mcp_share_physical_delivery() {
    let _env = hermetic::HermeticTestEnv::enter("c2_positional_alias_and_real_mcp_share_physical_delivery");
    let fixture = DirectFixture::new();
    sent(cli(&fixture, &["worker-1", "positional"]));
    sent(cli(&fixture, &["--to-name", "worker-1", "alias"]));
    let mut worker = mcp_sim_harness::spawn_mcp_client(&fixture.root, "worker-1", "current");
    let report = worker.call_tool(
        "send_message",
        json!({"to": "worker-1", "content": "MCP physical"}),
    );
    assert!(!report.is_error, "{}", report.body);
    assert_eq!(report.body["delivery"], "physical");
    // R-M1a: MCP shares the CLI store-first core, so its receipt names the inbox record too.
    assert!(report.body["message_id"].as_str().is_some_and(|id| !id.is_empty()), "{}", report.body);
    for (sender, content) in [
        ("leader", "positional"),
        ("leader", "alias"),
        ("worker-1", "MCP physical"),
    ] {
        fixture.received(&format!("Team Agent message from {sender}:\n\n{content}"));
        // R-M1a: every entrypoint stores exactly one record for its message.
        assert_eq!(inbox_hits(&fixture, "worker-1", content), 1, "{content}");
    }
    assert!(!fixture.root.join(".team/runtime/team.db").exists());
}

#[test]
#[serial_test::serial(env)]
fn c3_unresolved_recipients_refuse_without_delivery_or_persistence() {
    let _env = hermetic::HermeticTestEnv::enter("c3_unresolved_recipients_refuse_without_delivery_or_persistence");
    let fixture = DirectFixture::new();
    for args in [
        vec!["unknown", "must not arrive"],
        vec!["--to-name", "unknown/agent", "must not arrive"],
        vec!["--to", "worker-1,unknown", "must not arrive"],
    ] {
        let output = cli(&fixture, &args);
        assert!(
            !output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
    }
    sent(cli(&fixture, &["worker-1", "positive barrier"]));
    fixture.received("Team Agent message from leader:\n\npositive barrier");
    assert!(!std::fs::read_to_string(fixture.root.join("received"))
        .unwrap()
        .contains("must not arrive"));
    assert!(!fixture.root.join(".team/runtime/team.db").exists());
    assert!(!fixture.root.join(".team/inbox").exists());
}

#[test]
#[serial_test::serial(env)]
fn c4_failed_physical_send_is_not_queued_and_explicit_retry_delivers_once() {
    let _env = hermetic::HermeticTestEnv::enter("c4_failed_physical_send_is_not_queued_and_explicit_retry_delivers_once");
    let fixture = DirectFixture::new();
    let original = std::fs::read(fixture.root.join(".team/runtime/state.json")).unwrap();
    let mut state: Value = serde_json::from_slice(&original).unwrap();
    state["teams"]["current"]["agents"]["worker-1"]["pane_id"] = json!("%999999");
    fixture.set_state(&state);
    let failed = cli(&fixture, &["worker-1", "retry payload"]);
    // R-M1b: a non-live pane is stored with an explanatory delivery_error, never typed, never queued.
    assert!(failed.status.success(), "{}", String::from_utf8_lossy(&failed.stdout));
    let receipt: Value = serde_json::from_slice(&failed.stdout).unwrap();
    assert_eq!((receipt["ok"].clone(), receipt["status"].clone(), receipt["live"].clone()), (json!(true), json!("stored"), json!(false)), "{receipt}");
    assert!(receipt["delivery_error"].as_str().is_some_and(|e| e.starts_with("pane_gone")), "{receipt}");
    assert!(!std::fs::read_to_string(fixture.root.join("received")).unwrap().contains("retry payload"));
    std::fs::write(fixture.root.join(".team/runtime/state.json"), original).unwrap();
    sent(cli(&fixture, &["worker-1", "retry payload"]));
    fixture.received("Team Agent message from leader:\n\nretry payload");
    assert_eq!(
        std::fs::read_to_string(fixture.root.join("received"))
            .unwrap()
            .matches("retry payload")
            .count(),
        1
    );
    assert!(!fixture.root.join(".team/runtime/team.db").exists());
    // R-M1a: the failed attempt and the explicit retry are two stored records; only the retry was typed.
    assert_eq!(inbox_hits(&fixture, "worker-1", "retry payload"), 2);
}

#[test]
#[serial_test::serial(env)]
fn c5_fanout_reports_physical_failure_without_manufacturing_queued_obligations() {
    let _env = hermetic::HermeticTestEnv::enter("c5_fanout_reports_physical_failure_without_manufacturing_queued_obligations");
    let fixture = DirectFixture::new();
    fixture.set_state(&json!({"teams": {"current": {
        "tmux_socket": fixture.socket, "agents": {
            // M4 send authority: legacy rows publish their lifecycle status.
            "worker-1": {"pane_id": fixture.pane, "status": "running"}, "worker-2": {"pane_id": "%999999", "status": "running"}
        }
    }}}));
    let output = cli(&fixture, &["--to", "worker-1,worker-2", "partial fanout"]);
    fixture.received("Team Agent message from leader:\n\npartial fanout");
    // R-M1b + per-recipient receipts: a runtime failure of one recipient never aborts the batch and
    // never manufactures a queued obligation. (The receipt-list key itself is pinned by the
    // D-M5-2b oracles, so it is not asserted here.)
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    let receipts = value["results"].as_array().or_else(|| value["messages"].as_array()).cloned().unwrap_or_default();
    let of = |name: &str| receipts.iter().find(|r| r["recipient"] == name).cloned().unwrap_or(Value::Null);
    assert_eq!(of("worker-1")["live"], true, "{value}");
    assert_eq!(of("worker-2")["live"], false, "{value}");
    assert!(of("worker-2")["delivery_error"].as_str().is_some(), "{value}");
    assert!(receipts.iter().all(|r| r.get("delivery_pending").is_none()), "{value}");
    assert!(!fixture.root.join(".team/runtime/team.db").exists());
}
