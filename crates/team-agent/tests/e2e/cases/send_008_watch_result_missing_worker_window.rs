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
    assert!(quick_start_workers_available(&qs), "quick-start: {}", qs.stdout);

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
    let value = out.json();
    assert!(!out.is_success(), "missing physical endpoint must refuse: {value}");
    assert_eq!(value["ok"], false);
    assert!(value.get("message_id").is_none(), "refusal must not queue work: {value}");
    assert!(value.get("delivery_pending").is_none());
    assert!(value["error"].is_string());

    let _ = run_ta(
        &ws,
        &["shutdown", "--workspace", ws_path, "--keep-logs", "--json"],
    );
}
