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
    assert_file_contains(&ws.events_jsonl_path(), "turn_open.armed_after_delivery");
    assert_file_contains(&ws.events_jsonl_path(), mid);

    let _ = run_ta(
        &ws,
        &["shutdown", "--workspace", ws_path, "--keep-logs", "--json"],
    );
}

fn message_status(ws: &TestWorkspace, message_id: &str) -> Option<String> {
    let db = ws.path().join(".team/runtime/team.db");
    let conn = rusqlite::Connection::open(db).ok()?;
    conn.query_row(
        "select status from messages where message_id = ?1",
        [message_id],
        |row| row.get::<_, String>(0),
    )
    .ok()
}
