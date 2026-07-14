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
    assert!(quick_start_launched(&qs), "quick-start: {}", qs.stdout);

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
            "--sender",
            "leader",
            "--message-id",
            "msg-send002",
            "--no-wait",
            "--json",
        ],
    );
    let j = out.json();
    let status = j.pointer("/status").and_then(|v| v.as_str()).unwrap_or("");
    let ok = j.pointer("/ok").and_then(|v| v.as_bool()).unwrap_or(false);

    // The forbidden combination.
    let false_positive = ok && status == "delivered";
    assert!(
        !false_positive,
        "send to stopped worker must not report ok:true,status:'delivered' (E55 regression); got {j}"
    );

    let allowed = [
        "queued",
        "accepted",
        "submitted_unverified",
        "queued_pane_missing",
        "tmux_target_missing",
        "failed",
    ];
    assert!(
        allowed.contains(&status),
        "send-to-stopped status {status:?} should be one of {allowed:?}; json={j}"
    );

    let _ = run_ta(
        &ws,
        &["shutdown", "--workspace", ws_path, "--keep-logs", "--json"],
    );
}
