//! E2E-STAT-003 Dirty state is surfaced through diagnose/status JSON.

use crate::framework::*;
use serde_json::json;

#[test]
fn stat_003_status_dirty_state_reporting() {
    let team_id = "stat003";
    let ws = TestWorkspace::new(team_id).with_fake_spec(&["a"]);
    let qs = quick_start_fake(&ws, team_id);
    assert!(quick_start_workers_available(&qs), "quick-start: {}", qs.stdout);
    // Remove the independent healthy owner before corrupting both receiver
    // mirrors; otherwise the negative fixture still has a valid live Leader.
    seed_unattached_leader(&ws);
    let mut state = ws.read_state();
    let dirty = json!({"mode": "direct_tmux", "status": "attached", "pane_id": ""});
    state["leader_receiver"] = dirty.clone();
    state["teams"][team_id]["leader_receiver"] = dirty;
    ws.seed_state(state);

    let out = run_ta(
        &ws,
        &[
            "doctor",
            "--workspace",
            ws.path().to_str().unwrap(),
            "--json",
        ],
    );
    let j = out.json();
    assert_json_field_eq_bool(&j, "/ok", false);
    // 262f6cdb: doctor v1 reports failures as links, not an issues list; the
    // dirty leader receiver fails the return link (L6), naming the leader.
    let links = j["links"].as_array().cloned().unwrap_or_default();
    assert!(
        links.iter().any(|link| link["status"] != "PASS"),
        "dirty state should produce diagnose issues: {j}"
    );
    let leader = doctor_link(&j, "L6");
    assert!(
        leader["status"] == "FAIL" && leader["message"].as_str().unwrap_or_default().contains("leader"),
        "diagnose should name dirty-state reason; got {j}"
    );

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
