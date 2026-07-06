//! E2E-SEND-009 start-agent repair replays the same repair-safe blocked
//! worker-bound message id.

use crate::framework::*;
use std::time::Duration;

#[test]
fn send_009_worker_repair_requeues_blocked_message() {
    let team_id = "send009";
    let ws = TestWorkspace::new(team_id).with_fake_spec(&["a", "b"]);
    let ws_path = ws.path().to_str().unwrap();
    let qs = quick_start_fake(&ws, team_id);
    assert!(quick_start_launched(&qs), "quick-start: {}", qs.stdout);

    let stop = run_ta(&ws, &["stop-agent", "a", "--workspace", ws_path, "--json"]);
    assert!(stop.is_success(), "stop-agent: {}", stop.stdout);

    let mid = "msg-send009";
    let out = run_ta(
        &ws,
        &[
            "send",
            "a",
            "repair should replay this same row",
            "--workspace",
            ws_path,
            "--sender",
            "leader",
            "--message-id",
            mid,
            "--watch-result",
            "--json",
        ],
    );
    let j = out.json();
    assert_json_field_eq_str(&j, "/message_id", mid);
    assert_json_field_eq_str(&j, "/message_status", "queued_pane_missing");
    let before = message_row(&ws, mid).expect("blocked message row exists");
    assert_eq!(before.status, "queued_pane_missing");

    let start = run_ta(
        &ws,
        &[
            "start-agent",
            "a",
            "--workspace",
            ws_path,
            "--allow-fresh",
            "--no-display",
            "--json",
        ],
    );
    assert!(
        start.is_success(),
        "start-agent exit {}; stdout={} stderr={}",
        start.exit_code,
        start.stdout,
        start.stderr
    );

    wait_for_or_panic(
        "same blocked message id delivered after worker repair",
        || message_status(&ws, mid).as_deref() == Some("delivered"),
        Duration::from_secs(8),
    );
    std::thread::sleep(Duration::from_millis(700));
    let after = message_row(&ws, mid).expect("delivered message row exists");
    assert_eq!(after.status, "delivered");
    assert_eq!(
        after.delivery_attempts,
        before.delivery_attempts + 1,
        "repair replay must add exactly one delivery attempt"
    );
    assert_eq!(event_count(&ws, "message.delivered", mid), 1);
    assert_eq!(event_count(&ws, "turn_open.armed_after_delivery", mid), 1);

    let _ = run_ta(
        &ws,
        &["shutdown", "--workspace", ws_path, "--keep-logs", "--json"],
    );
}

struct MessageRow {
    status: String,
    delivery_attempts: i64,
}

fn message_status(ws: &TestWorkspace, message_id: &str) -> Option<String> {
    message_row(ws, message_id).map(|row| row.status)
}

fn message_row(ws: &TestWorkspace, message_id: &str) -> Option<MessageRow> {
    let db = ws.path().join(".team/runtime/team.db");
    let conn = rusqlite::Connection::open(db).ok()?;
    conn.query_row(
        "select status, delivery_attempts from messages where message_id = ?1",
        [message_id],
        |row| {
            Ok(MessageRow {
                status: row.get(0)?,
                delivery_attempts: row.get(1)?,
            })
        },
    )
    .ok()
}

fn event_count(ws: &TestWorkspace, event: &str, message_id: &str) -> usize {
    std::fs::read_to_string(ws.events_jsonl_path())
        .unwrap_or_default()
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|entry| {
            entry.get("event").and_then(serde_json::Value::as_str) == Some(event)
                && entry.get("message_id").and_then(serde_json::Value::as_str) == Some(message_id)
        })
        .count()
}
