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
    let value = out.json();
    assert!(!out.is_success(), "missing physical endpoint must refuse: {value}");
    assert_eq!(value["ok"], false);
    assert!(value.get("message_id").is_none(), "refusal must not queue work: {value}");
    assert!(value.get("delivery_pending").is_none());
    assert!(value["error"].is_string());

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
