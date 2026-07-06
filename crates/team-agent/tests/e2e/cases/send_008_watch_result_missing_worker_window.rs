//! E2E-SEND-008 `send --watch-result` to a missing worker window must not
//! advertise a result watcher before physical delivery.

use crate::framework::*;

#[test]
fn send_008_watch_result_missing_worker_window_blocks_before_watch() {
    let team_id = "send008";
    let ws = TestWorkspace::new(team_id).with_fake_spec(&["a", "b"]);
    let ws_path = ws.path().to_str().unwrap();
    let qs = quick_start_fake(&ws, team_id);
    assert!(quick_start_launched(&qs), "quick-start: {}", qs.stdout);

    let stop = run_ta(&ws, &["stop-agent", "a", "--workspace", ws_path, "--json"]);
    assert!(stop.is_success(), "stop-agent: {}", stop.stdout);

    let out = run_ta(
        &ws,
        &[
            "send",
            "a",
            "wake missing window",
            "--workspace",
            ws_path,
            "--sender",
            "leader",
            "--message-id",
            "msg-send008",
            "--watch-result",
            "--json",
        ],
    );
    let j = out.json();
    assert_json_field_eq_str(&j, "/message_id", "msg-send008");
    assert_json_field_eq_bool(&j, "/ok", false);
    assert_json_field_eq_str(&j, "/status", "blocked");
    assert_json_field_eq_str(&j, "/message_status", "queued_pane_missing");
    assert_json_field_eq_str(&j, "/delivery_status", "blocked");
    assert_json_field_eq_bool(&j, "/delivered", false);
    assert_json_field_eq_str(&j, "/reason", "tmux_target_missing");
    assert_json_field_eq_str(&j, "/channel", "delivery_blocked");
    assert!(
        j.pointer("/watch").is_none(),
        "watch-result must not register a result watcher before delivery; got {j}"
    );

    let _ = run_ta(
        &ws,
        &["shutdown", "--workspace", ws_path, "--keep-logs", "--json"],
    );
}
