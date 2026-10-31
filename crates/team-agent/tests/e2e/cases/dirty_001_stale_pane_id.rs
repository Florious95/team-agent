//! E2E-DIRTY-001 Stale pane_id does not become a false delivered send.

use crate::framework::*;
use serde_json::json;

#[test]
fn dirty_001_stale_pane_id_is_not_reported_delivered() {
    let team_id = "dirty001";
    let ws = TestWorkspace::new(team_id).with_fake_spec(&["a"]);
    let qs = quick_start_fake(&ws, team_id);
    assert!(quick_start_workers_available(&qs), "quick-start: {}", qs.stdout);

    ws.mutate_agent_everywhere("a", |agent| {
        agent.insert("pane_id".to_string(), json!("%99999"));
    });

    let out = run_ta(
        &ws,
        &[
            "send",
            "a",
            "stale pane",
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
