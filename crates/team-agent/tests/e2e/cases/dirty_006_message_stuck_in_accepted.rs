//! E2E-DIRTY-006 Message stuck in accepted remains visible as queued/accepted.

use crate::framework::*;

#[test]
fn dirty_006_message_stuck_in_accepted_is_not_false_delivered() {
    let team_id = "dirty006";
    let ws = TestWorkspace::new(team_id).with_fake_spec(&["a"]);
    let qs = quick_start_fake(&ws, team_id);
    assert!(quick_start_launched(&qs), "quick-start: {}", qs.stdout);

    let stopped = run_ta(
        &ws,
        &[
            "stop-agent",
            "a",
            "--workspace",
            ws.path().to_str().unwrap(),
            "--json",
        ],
    );
    assert!(stopped.is_success(), "stop-agent stderr={}", stopped.stderr);

    let out = run_ta(
        &ws,
        &[
            "send",
            "a",
            "queued for stopped worker",
            "--workspace",
            ws.path().to_str().unwrap(),
            "--json",
        ],
    );
    assert!(out.is_success(), "send stderr={}", out.stderr);
    let j = out.json();
    assert!(j.pointer("/message_id").and_then(|v| v.as_str()).is_some());
    assert_json_field_eq_str(&j, "/message_status", "accepted");
    assert_ne!(
        j.pointer("/status").and_then(|v| v.as_str()),
        Some("delivered"),
        "accepted message must not be reported delivered: {j}"
    );
    assert_file_exists(&ws.path().join(".team/runtime/team.db"));

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
