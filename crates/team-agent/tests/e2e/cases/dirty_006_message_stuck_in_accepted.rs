//! E2E-DIRTY-006 Coordinator-unavailable message remains durable and undelivered.
//!
//! ---
//! purpose: Test-only durable non-delivery contract for unavailable coordinators.
//! contract:
//!   provides:
//!     - name: dirty_006_message_stuck_in_accepted_is_not_false_delivered
//!       what: Binds one message id to its queued row, blocker, and no-delivery evidence.
//!   depends:
//!     - e2e::framework::TestWorkspace
//!     - sqlite messages store
//!     - events.jsonl
//! boundary:
//!   - Test-only fixture and assertions; no messaging product behavior.
//! maturity: wired
//! ---

use crate::framework::*;

#[test]
fn dirty_006_message_stuck_in_accepted_is_not_false_delivered() {
    let team_id = "dirty006";
    let ws = TestWorkspace::new(team_id).with_fake_spec(&["a"]);
    let qs = quick_start_fake(&ws, team_id);
    assert!(quick_start_launched(&qs), "quick-start: {}", qs.stdout);

    let stopped = run_ta(
        &ws,
        &[
            "stop-agent",
            "a",
            "--workspace",
            ws.path().to_str().unwrap(),
            "--json",
        ],
    );
    assert!(stopped.is_success(), "stop-agent stderr={}", stopped.stderr);

    // Make the existing coordinator pid look live but unverifiable to the send
    // preflight. The real pid is restored before teardown, so the fixture never
    // loses ownership of its coordinator process.
    let coordinator_pid_file = ws.coordinator_pid_file();
    let coordinator_pid = std::fs::read_to_string(&coordinator_pid_file)
        .expect("quick-start must leave a coordinator pid file");
    std::fs::write(&coordinator_pid_file, std::process::id().to_string())
        .expect("write coordinator-unavailable fixture pid");

    let out = run_ta(
        &ws,
        &[
            "send",
            "a",
            "queued for stopped worker",
            "--workspace",
            ws.path().to_str().unwrap(),
            "--json",
        ],
    );
    std::fs::write(&coordinator_pid_file, coordinator_pid)
        .expect("restore coordinator pid before teardown");
    assert!(out.is_success(), "send stderr={}", out.stderr);
    let j = out.json();
    assert!(j.pointer("/message_id").and_then(|v| v.as_str()).is_some());
    let message_id = j["message_id"].as_str().unwrap();
    assert_json_field_eq_str(&j, "/status", "blocked");
    assert_json_field_eq_str(&j, "/delivery_status", "blocked");
    assert_json_field_eq_str(&j, "/message_status", "queued_coordinator_unavailable");
    assert_json_field_eq_bool(&j, "/delivered", false);
    assert_json_field_eq_str(&j, "/reason", "coordinator_unavailable");
    assert_json_field_eq_str(&j, "/channel", "coordinator_unavailable");

    let db = rusqlite::Connection::open(ws.path().join(".team/runtime/team.db"))
        .expect("open message store");
    let row: (i64, String, String, Option<String>, Option<String>) = db
        .query_row(
            "select count(*), recipient, status, error, delivered_at from messages \
             where message_id = ?1",
            [message_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
        )
        .expect("query durable message row");
    assert_eq!(row.0, 1, "message id must identify exactly one durable row");
    assert_eq!(row.1, "a");
    assert_eq!(row.2, "queued_coordinator_unavailable");
    assert_eq!(row.3.as_deref(), Some("coordinator_unavailable"));
    assert_eq!(row.4, None, "blocked row must not carry delivered timestamp");

    let events = std::fs::read_to_string(ws.events_jsonl_path())
        .expect("read durable event log")
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|event| event["message_id"].as_str() == Some(message_id))
        .collect::<Vec<_>>();
    assert!(
        events.iter().any(|event| {
            event["event"] == "send.message_queued"
                && event["blocker"] == "coordinator_unavailable"
        }),
        "queued event must preserve typed blocker: {events:?}"
    );
    assert!(
        events.iter().all(|event| event["event"] != "message.delivered"),
        "blocked message must not emit a delivered event: {events:?}"
    );
    assert_file_exists(&ws.path().join(".team/runtime/team.db"));

    let _ = run_ta(
        &ws,
        &[
            "shutdown",
            "--workspace",
            ws.path().to_str().unwrap(),
            "--keep-logs",
            "--json",
        ],
    );
}
