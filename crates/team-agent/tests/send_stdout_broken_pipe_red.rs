//! Closing stdout must not undo or repeat physical delivery / inbox append.
#![cfg(unix)]
#![allow(clippy::unwrap_used)]

#[path = "support/hermetic.rs"]
mod hermetic;

#[path = "support/direct_fixture.rs"]
mod direct_fixture;
use direct_fixture::DirectFixture;
use std::os::fd::OwnedFd;
use std::os::unix::net::UnixStream;
use std::process::{Command, Stdio};

fn send_with_closed_stdout(fixture: &DirectFixture, mailbox: bool) {
    let (reader, writer) = UnixStream::pair().unwrap();
    drop(reader); // EPIPE is guaranteed before the CLI begins, not a scheduling race.
    let writer: OwnedFd = writer.into();
    let mut command = Command::new(env!("CARGO_BIN_EXE_team-agent"));
    command
        .args([
            "send",
            "worker-1",
            "closed stdout token",
            "--json",
            "--workspace",
            fixture.root.to_str().unwrap(),
        ])
        .env_remove("TEAM_AGENT_ID")
        .env_remove("TEAM_AGENT_AGENT_ID")
        .env_remove("TEAM_AGENT_WORKSPACE")
        .env_remove("TEAM_AGENT_OWNER_TEAM_ID")
        .env_remove("TEAM_AGENT_TEAM_ID")
        .stdout(Stdio::from(writer))
        .stderr(Stdio::piped());
    if mailbox {
        command.arg("--mailbox");
    }
    let output = command.spawn().unwrap().wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "stdout failure must not imply send failure: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!String::from_utf8_lossy(&output.stderr).contains("panicked"));
}

#[test]
#[serial_test::serial(env)]
fn physical_send_completes_once_when_stdout_is_closed() {
    let _env = hermetic::HermeticTestEnv::enter("physical_send_completes_once_when_stdout_is_closed");
    let fixture = DirectFixture::new();
    send_with_closed_stdout(&fixture, false);
    fixture.received("Team Agent message from leader:\n\nclosed stdout token");
    assert_eq!(
        std::fs::read_to_string(fixture.root.join("received"))
            .unwrap()
            .matches("closed stdout token")
            .count(),
        1
    );
    // R-M1a: M1 stores before submitting, so a physical send leaves exactly one durable record
    // (exactly-once holds for both the keystrokes and the inbox).
    let records = fixture.inbox("worker-1");
    assert_eq!(records.len(), 1, "{records:?}");
    assert_eq!(records[0]["content"], "closed stdout token");
    assert!(!fixture.root.join(".team/runtime/team.db").exists());
}

#[test]
#[serial_test::serial(env)]
fn mailbox_remains_pullable_once_when_stdout_is_closed() {
    let _env = hermetic::HermeticTestEnv::enter("mailbox_remains_pullable_once_when_stdout_is_closed");
    let fixture = DirectFixture::new();
    send_with_closed_stdout(&fixture, true);
    let records = fixture.inbox("worker-1");
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["content"], "closed stdout token");
    assert!(std::fs::read(fixture.root.join("received"))
        .unwrap()
        .is_empty());
    assert!(!fixture.root.join(".team/runtime/team.db").exists());
}
