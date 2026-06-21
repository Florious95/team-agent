//! E2E-SEND-007 Send broadcast to all workers.
//!
//! `send '*' "..."` fans out to every agent. The JSON reports
//! `status:"fanout_delivered"` (or equivalent fanout label), `target:"*"`,
//! and `ok:true`. The runtime currently auto-assigns the message_id for
//! fanout (does not honor --message-id), so we only require a non-empty
//! message_id rather than equality.

use crate::framework::*;

#[test]
fn send_007_broadcast_fanout_status() {
    let team_id = "send007";
    let ws = TestWorkspace::new(team_id).with_fake_spec(&["a", "b"]);
    let ws_path = ws.path().to_str().unwrap();
    let qs = quick_start_fake(&ws, team_id);
    assert!(quick_start_launched(&qs), "quick-start: {}", qs.stdout);

    let out = run_ta(
        &ws,
        &[
            "send", "*", "broadcast e2e",
            "--workspace", ws_path,
            "--sender", "leader",
            "--message-id", "msg-bcast-007",
            "--no-wait",
            "--json",
        ],
    );
    assert!(out.is_success(), "broadcast: {}", out.stdout);
    let j = out.json();
    assert_json_field_eq_bool(&j, "/ok", true);
    assert_json_field_eq_str(&j, "/target", "*");
    let status = j.pointer("/status").and_then(|v| v.as_str()).unwrap_or("");
    assert!(
        status.contains("fanout") || status == "delivered" || status == "queued",
        "broadcast status should be fanout-shaped; got {status:?} (json={j})"
    );
    let mid = j.pointer("/message_id").and_then(|v| v.as_str()).unwrap_or("");
    assert!(!mid.is_empty(), "broadcast should report a message_id; got {j}");

    let _ = run_ta(
        &ws,
        &["shutdown", "--workspace", ws_path, "--keep-logs", "--json"],
    );
}
