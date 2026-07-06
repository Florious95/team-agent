//! E2E-SEND-003 Send JSON shape contract.
//!
//! Locks the public JSON shape that callers (skill / scribe / external
//! orchestrators) depend on. Required keys on a normal send:
//!   ok, agent_id, target, sender, content_length_bytes, message_id,
//!   status, message_status, delivery_status, delivered.
//! `reminder` must be present and non-empty so worker pane scraping is
//! discouraged at the API boundary.
//!
//! Pre-release 0.4.0 user directive: the send response MUST NOT echo the
//! message body. `content` is replaced by `content_length_bytes` (size
//! sanity) — external consumers who need the body read it via `inbox`.

use crate::framework::*;

#[test]
fn send_003_send_json_shape_locks_public_keys() {
    let team_id = "send003";
    let ws = TestWorkspace::new(team_id).with_fake_spec(&["a"]);
    let ws_path = ws.path().to_str().unwrap();
    let qs = quick_start_fake(&ws, team_id);
    assert!(quick_start_launched(&qs), "quick-start: {}", qs.stdout);

    let mid = "msg-send003-shape";
    let body = "shape contract";
    let out = run_ta(
        &ws,
        &[
            "send",
            "a",
            body,
            "--workspace",
            ws_path,
            "--sender",
            "leader",
            "--message-id",
            mid,
            "--no-wait",
            "--json",
        ],
    );
    assert!(out.is_success(), "send: {}", out.stdout);
    let j = out.json();

    let required = [
        "/ok",
        "/agent_id",
        "/target",
        "/sender",
        "/content_length_bytes",
        "/message_id",
        "/status",
        "/message_status",
        "/delivery_status",
        "/delivered",
    ];
    for p in &required {
        assert_json_field_present(&j, p);
    }
    assert_json_field_eq_str(&j, "/message_id", mid);
    assert_json_field_eq_str(&j, "/target", "a");
    assert_json_field_eq_str(&j, "/sender", "leader");
    assert_json_field_eq_bool(&j, "/ok", true);
    assert_json_field_eq_str(&j, "/delivery_status", "pending");
    assert_json_field_eq_bool(&j, "/delivered", false);

    // Pre-release 0.4.0: `content` must NOT appear in the send response.
    // Operators / scripts that need the body read it via `inbox`.
    assert!(
        j.pointer("/content").is_none(),
        "send response must NOT carry the message body; got {j}"
    );
    let len = j
        .pointer("/content_length_bytes")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    assert_eq!(
        len,
        body.len() as u64,
        "content_length_bytes must equal the byte length of the body sent; got {j}"
    );

    let reminder = j
        .pointer("/reminder")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    assert!(
        !reminder.is_empty(),
        "send JSON should include a non-empty 'reminder'; got {j}"
    );

    let _ = run_ta(
        &ws,
        &["shutdown", "--workspace", ws_path, "--keep-logs", "--json"],
    );
}
