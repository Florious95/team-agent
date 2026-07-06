//! E2E-INBOX-001 inbox is history, so it must show delivery lifecycle state.

use crate::framework::*;

#[test]
fn inbox_001_delivery_status_visible_for_blocked_inbound_message() {
    let team_id = "inbox001";
    let ws = TestWorkspace::new(team_id).with_fake_spec(&["a", "b"]);
    let ws_path = ws.path().to_str().unwrap();
    let qs = quick_start_fake(&ws, team_id);
    assert!(quick_start_launched(&qs), "quick-start: {}", qs.stdout);

    let stop = run_ta(&ws, &["stop-agent", "a", "--workspace", ws_path, "--json"]);
    assert!(stop.is_success(), "stop-agent: {}", stop.stdout);

    let send = run_ta(
        &ws,
        &[
            "send",
            "a",
            "history is not receipt",
            "--workspace",
            ws_path,
            "--sender",
            "leader",
            "--message-id",
            "msg-inbox001",
            "--watch-result",
            "--json",
        ],
    );
    let sent = send.json();
    assert_json_field_eq_str(&sent, "/message_status", "queued_pane_missing");

    let inbox_json = run_ta(&ws, &["inbox", "a", "--workspace", ws_path, "--json"]);
    assert!(inbox_json.is_success(), "inbox json: {}", inbox_json.stdout);
    let j = inbox_json.json();
    assert_json_field_eq_str(&j, "/messages/0/message_id", "msg-inbox001");
    assert_json_field_eq_str(&j, "/messages/0/status", "queued_pane_missing");
    assert_json_field_eq_str(&j, "/messages/0/error", "tmux_target_missing");
    assert_json_field_present(&j, "/messages/0/delivery_attempts");

    let inbox_human = run_ta(&ws, &["inbox", "a", "--workspace", ws_path]);
    assert!(
        inbox_human.is_success(),
        "inbox human exit {}; stdout={} stderr={}",
        inbox_human.exit_code,
        inbox_human.stdout,
        inbox_human.stderr
    );
    assert!(
        inbox_human.stdout.contains("status=queued_pane_missing")
            && inbox_human.stdout.contains("error=tmux_target_missing")
            && inbox_human.stdout.contains("attempts="),
        "human inbox must show lifecycle status, attempts, and error; got:\n{}",
        inbox_human.stdout
    );

    let _ = run_ta(
        &ws,
        &["shutdown", "--workspace", ws_path, "--keep-logs", "--json"],
    );
}
