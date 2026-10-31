//! E2E-DIRTY-006 Message stuck in accepted remains visible as queued/accepted.

use crate::framework::*;

#[test]
fn dirty_006_message_stuck_in_accepted_is_not_false_delivered() {
    let team_id = "dirty006";
    let ws = TestWorkspace::new(team_id).with_fake_spec(&["a"]);
    let qs = quick_start_fake(&ws, team_id);
    assert!(quick_start_workers_available(&qs), "quick-start: {}", qs.stdout);

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
    // R-M1b (M1 store-first): no live endpoint -> stored with an explained delivery_error, never delivered.
    assert_stored_not_delivered(&ws, &out);

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
