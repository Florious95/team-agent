//! E2E-SEND-001 Send To Fake Worker Delivers Token And Stores DB Row.
//!
//! Architecture: T4 §4 delivery FSM, T6 §1 L6 message invariants, T1 §6 team.db.
//!
//! Black-box invariants:
//! - `ok == true` in JSON
//! - `message_id` round-trips to the requested id
//! - `target == "a"`, `sender == "leader"`
//! - `status` is a forward-progress label (queued / accepted / submitted /
//!   delivered) — NOT `failed`.
//! - team.db file exists after a send (storage layer touched).

use crate::framework::*;

#[test]
fn send_001_delivers_to_fake_worker() {
    let team_id = "send001";
    let ws = TestWorkspace::new(team_id).with_fake_spec(&["a"]);
    let qs = quick_start_fake(&ws, team_id);
    assert!(
        quick_start_launched(&qs),
        "quick-start did not launch: {}",
        qs.stdout
    );

    let mid = "msg-e2e-send-001";
    let out = run_ta(
        &ws,
        &[
            "send",
            "a",
            "hello from e2e",
            "--workspace",
            ws.path().to_str().unwrap(),
            "--sender",
            "leader",
            "--message-id",
            mid,
            "--no-wait",
            "--json",
        ],
    );

    assert!(
        out.is_success(),
        "send exit {}; stdout={} stderr={}",
        out.exit_code,
        out.stdout,
        out.stderr
    );
    let j = out.json();

    assert_json_field_eq_bool(&j, "/ok", true);
    assert_json_field_eq_str(&j, "/message_id", mid);
    assert_json_field_eq_str(&j, "/target", "a");
    assert_json_field_eq_str(&j, "/sender", "leader");

    let status = j.pointer("/status").and_then(|v| v.as_str()).unwrap_or("");
    let allowed = [
        "queued",
        "accepted",
        "submitted",
        "submitted_unverified",
        "delivered",
    ];
    assert!(
        allowed.contains(&status),
        "status {status:?} should be in {allowed:?}; full json: {j}"
    );

    // team.db should exist after touching messaging
    let db = ws.path().join(".team/runtime/team.db");
    assert_file_exists(&db);

    // cleanup
    let _ = run_ta(
        &ws,
        &[
            "shutdown",
            "--workspace",
            ws.path().to_str().unwrap(),
            "--keep-logs",
            "--json",
        ],
    );
}
