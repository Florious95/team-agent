//! E2E-SEND-002 Send to a stopped worker queues honestly.
//!
//! After `stop-agent a`, a subsequent `send a` must:
//! - either refuse (ok:false with stale-pane reason), OR
//! - queue with a non-terminal status (`queued`, `accepted`, `submitted_unverified`).
//!
//! The forbidden combination is `ok:true, status:"delivered"` — that would
//! be the E55 delivery-false-positive bug (target pane is gone but JSON
//! claims delivery). The team.db row, if any, must not be terminal
//! `delivered`.

use crate::framework::*;

#[test]
fn send_002_send_to_stopped_worker_does_not_falsely_deliver() {
    let team_id = "send002";
    let ws = TestWorkspace::new(team_id).with_fake_spec(&["a"]);
    let ws_path = ws.path().to_str().unwrap();
    let qs = quick_start_fake(&ws, team_id);
    assert!(quick_start_workers_available(&qs), "quick-start: {}", qs.stdout);

    let stop = run_ta(&ws, &["stop-agent", "a", "--workspace", ws_path, "--json"]);
    assert!(stop.is_success(), "stop-agent: {}", stop.stdout);
    assert_json_field_eq_bool(&stop.json(), "/ok", true);

    let out = run_ta(
        &ws,
        &[
            "send",
            "a",
            "queued to stopped",
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
