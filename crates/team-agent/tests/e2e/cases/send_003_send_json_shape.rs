//! E2E-SEND-003 Send JSON shape contract.
//!
//! Locks the public JSON shape that callers (skill / scribe / external
//! orchestrators) depend on. Required keys on a normal send:
//!   ok, agent_id, target, sender, content, message_id, status, message_status.
//! `reminder` must be present and non-empty so worker pane scraping is
//! discouraged at the API boundary.

use crate::framework::*;

#[test]
fn send_003_send_json_shape_locks_public_keys() {
    let team_id = "send003";
    let ws = TestWorkspace::new(team_id).with_fake_spec(&["a"]);
    let ws_path = ws.path().to_str().unwrap();
    let qs = quick_start_fake(&ws, team_id);
    assert!(quick_start_launched(&qs), "quick-start: {}", qs.stdout);

    let mid = "msg-send003-shape";
    let out = run_ta(
        &ws,
        &[
            "send", "a", "shape contract",
            "--workspace", ws_path,
            "--sender", "leader",
            "--message-id", mid,
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
        "/content",
        "/message_id",
        "/status",
        "/message_status",
    ];
    for p in &required {
        assert_json_field_present(&j, p);
    }
    assert_json_field_eq_str(&j, "/message_id", mid);
    assert_json_field_eq_str(&j, "/target", "a");
    assert_json_field_eq_str(&j, "/sender", "leader");
    assert_json_field_eq_str(&j, "/content", "shape contract");
    assert_json_field_eq_bool(&j, "/ok", true);

    let reminder = j.pointer("/reminder").and_then(|v| v.as_str()).unwrap_or("");
    assert!(
        !reminder.is_empty(),
        "send JSON should include a non-empty 'reminder'; got {j}"
    );

    let _ = run_ta(
        &ws,
        &["shutdown", "--workspace", ws_path, "--keep-logs", "--json"],
    );
}
