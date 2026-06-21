//! E2E-DIRTY-004 Stale session_id with missing backing refuses restart.

use crate::framework::*;
use serde_json::json;

#[test]
fn dirty_004_stale_session_id_missing_backing_refuses_restart() {
    let team_id = "dirty004";
    let ws = TestWorkspace::new(team_id).with_fake_spec(&["a"]);
    let qs = quick_start_fake(&ws, team_id);
    assert!(quick_start_launched(&qs), "quick-start: {}", qs.stdout);

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
    ws.mutate_agent_everywhere("a", |agent| {
        agent.insert("provider".to_string(), json!("codex"));
        agent.insert("session_id".to_string(), json!("sess-dirty-004"));
        agent.insert("rollout_path".to_string(), json!("/missing/dirty004.jsonl"));
        agent.insert("first_send_at".to_string(), json!("2026-01-01T00:00:00Z"));
    });

    let out = run_ta(&ws, &["restart", ws.path().to_str().unwrap(), "--json"]);
    let j = out.json();
    assert_json_field_eq_bool(&j, "/ok", false);
    let dump = serde_json::to_string(&j).unwrap();
    assert!(
        dump.contains("backing") || dump.contains("unresumable") || dump.contains("refused"),
        "missing backing refusal should be visible; got {dump}"
    );
}
