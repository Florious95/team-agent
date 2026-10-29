//! Explicit offline mailbox is an append-only inbox, never an automatic replay queue.
//! Cross-workspace names must preserve target scope and ordinary sends must use real PTY input.
#![cfg(unix)]
#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

#[path = "support/direct_fixture.rs"]
mod direct_fixture;
#[path = "support/hermetic.rs"]
mod hermetic_guard;
use hermetic_guard::HermeticTestEnv;
use direct_fixture::DirectFixture;
use serde_json::{json, Value};
use std::fs;
use std::process::{Command, Output};

fn send(sender: &DirectFixture, target: &DirectFixture, content: &str, mailbox: bool) -> Output {
    let address = format!("{}::current/leader", target.root.display());
    let mut command = Command::new(env!("CARGO_BIN_EXE_team-agent"));
    command.args(["send", "--to-name", &address, content, "--workspace"])
        .arg(&sender.root).args(["--json"]);
    if mailbox { command.arg("--mailbox"); }
    command.output().expect("run candidate CLI")
}

fn body(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!("invalid CLI JSON: {error}; stdout={:?}; stderr={:?}", output.stdout, output.stderr)
    })
}

#[test]
#[serial_test::serial(env)]
fn explicit_cross_workspace_mailbox_is_lossless_and_does_not_replay_on_attach() {
    let _env = HermeticTestEnv::enter("e6-direct-mailbox");
    let sender = DirectFixture::new();
    let target = DirectFixture::new();
    // Seed current identity keys so the read-only check excludes legacy migration.
    let mut state = json!({"team_key": "current", "active_team_key": "current", "teams": {"current": {
        "team_key": "current", "team_id": "current", "status": "stopped", "tmux_socket": target.socket,
        "leader": {}, "agents": {"worker-1": {"status": "dead", "pane_id": target.pane}}
    }}});
    team_agent::state::persist::save_runtime_state(&target.root, &state).unwrap();
    let state_path = target.root.join(".team/runtime/state.json");
    let before_state = fs::read(&state_path).unwrap();
    let payload = format!("OFFLINE_MAILBOX_{}\n中文 🚀\n{}", std::process::id(), "lossless-αβγ-".repeat(64));

    let ordinary = send(&sender, &target, &payload, false);
    assert!(!ordinary.status.success(), "unbound ordinary send must not claim delivery: {}", body(&ordinary));
    assert!(!target.root.join(".team/inbox/leader.jsonl").exists(), "no implicit mailbox fallback");
    assert!(fs::read(target.root.join("received")).unwrap().is_empty());

    let mailbox = send(&sender, &target, &payload, true);
    let accepted = body(&mailbox);
    assert!(mailbox.status.success(), "explicit mailbox: {accepted}");
    assert_eq!(accepted["status"], "stored");
    assert_eq!(accepted["mailbox"], true);
    let records = target.inbox("leader");
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["message_id"], accepted["message_id"]);
    assert_eq!(records[0]["content"], payload);
    assert_eq!(records[0]["recipient"], "leader");
    assert_eq!(records[0]["team"], "current");
    assert_eq!(records[0]["kind"], "message");
    assert!(!sender.root.join(".team/inbox/leader.jsonl").exists(), "no sender-workspace leak");
    let inbox_path = target.root.join(".team/inbox/leader.jsonl");
    let inbox_bytes = fs::read(&inbox_path).unwrap();
    for _ in 0..2 {
        let read = Command::new(env!("CARGO_BIN_EXE_team-agent"))
            .args(["inbox", "leader", "--workspace"]).arg(&target.root)
            .args(["--team", "current", "--json"]).output().unwrap();
        assert!(read.status.success(), "{}", body(&read));
        assert_eq!(body(&read)["messages"], json!(records));
    }
    assert_eq!(fs::read(&inbox_path).unwrap(), inbox_bytes, "inbox reads do not consume");
    assert_eq!(fs::read(&state_path).unwrap(), before_state, "send/read do not mutate runtime state");

    // Publish the existing owned PTY as the leader endpoint. No coordinator runs,
    // and observing/attaching the endpoint cannot turn inbox records into deliveries.
    state["teams"]["current"]["leader"]["pane_id"] = json!(target.pane);
    team_agent::state::persist::save_runtime_state(&target.root, &state).unwrap();
    assert!(fs::read(target.root.join("received")).unwrap().is_empty());
    assert_eq!(fs::read(&inbox_path).unwrap(), inbox_bytes);
    assert!(!target.root.join(".team/runtime/team.db").exists());
    assert!(!target.root.join(".team/runtime/coordinator.pid").exists());

    let physical = send(&sender, &target, &payload, false);
    let submitted = body(&physical);
    assert!(physical.status.success(), "{submitted}");
    assert_eq!(submitted["delivery"], "physical");
    assert_eq!(submitted["pane_id"], target.pane);
    assert!(submitted["accepted"].is_null(), "submit is not native acceptance");
    target.wait_for(|| fs::read(target.root.join("received")).unwrap()
        .windows(payload.len()).any(|window| window == payload.as_bytes()));
    let bytes = fs::read(target.root.join("received")).unwrap();
    assert_eq!(bytes.windows(payload.len()).filter(|window| *window == payload.as_bytes()).count(), 1);
    assert_eq!(bytes.windows(6).filter(|window| *window == b"\x1b[200~").count(), 1);
    assert!(bytes.ends_with(b"\x1b[201~\r"), "exactly one physical paste and Enter: {bytes:?}");
    assert!(fs::read(sender.root.join("received")).unwrap().is_empty());
    assert_eq!(fs::read(&inbox_path).unwrap(), inbox_bytes, "ordinary send does not replay or append mailbox records");
}
