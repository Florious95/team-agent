//! E2E-SEND-008 canonical send to a missing worker window must not advertise
//! a result watcher before physical delivery.

use crate::framework::*;
use std::time::Duration;

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
            "--json",
        ],
    );
    let j = out.json();
    assert!(j.pointer("/message_id").and_then(|v| v.as_str()).is_some());
    assert_json_field_eq_bool(&j, "/ok", true);
    assert_json_field_eq_str(&j, "/status", "queued");
    assert_json_field_eq_str(&j, "/message_status", "accepted");
    assert_json_field_eq_str(&j, "/delivery_status", "pending");
    assert_json_field_eq_bool(&j, "/delivered", false);
    assert!(
        j.pointer("/watch").is_none(),
        "watch-result must not register a result watcher before delivery; got {j}"
    );
    wait_for_or_panic(
        "missing worker becomes a blocked inbox row",
        || {
            let inbox = run_ta(&ws, &["inbox", "a", "--workspace", ws_path, "--json"]);
            inbox
                .json()
                .pointer("/messages/0/status")
                .and_then(|value| value.as_str())
                == Some("queued_pane_missing")
        },
        Duration::from_secs(6),
    );

    let _ = run_ta(
        &ws,
        &["shutdown", "--workspace", ws_path, "--keep-logs", "--json"],
    );
}
