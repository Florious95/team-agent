//! E2E-DIRTY-005 Explicit --team selection resists sibling-team state pollution.

use crate::framework::*;
use serde_json::json;

#[test]
fn dirty_005_cross_team_binding_pollution_keeps_explicit_team_scope() {
    let team_id = "dirty005a";
    let ws = TestWorkspace::new("dirty005").with_fake_spec(&["a"]);
    let qs = quick_start_fake(&ws, team_id);
    assert!(quick_start_launched(&qs), "quick-start: {}", qs.stdout);

    ws.mutate_state(|state| {
        let active = state
            .get("active_team_key")
            .and_then(|v| v.as_str())
            .unwrap()
            .to_string();
        let sibling = state
            .get("teams")
            .and_then(|v| v.as_object())
            .and_then(|teams| teams.get(&active))
            .cloned()
            .expect("active team");
        let teams = state
            .get_mut("teams")
            .and_then(|v| v.as_object_mut())
            .unwrap();
        teams.insert("dirty005b".to_string(), sibling);
        if let Some(team) = teams.get_mut("dirty005b").and_then(|v| v.as_object_mut()) {
            team.insert("session_name".to_string(), json!("team-dirty005b"));
            team.insert(
                "leader_receiver".to_string(),
                json!({"mode": "direct_tmux", "status": "attached", "pane_id": "%polluted"}),
            );
        }
    });

    let out = run_ta(
        &ws,
        &[
            "status",
            "--workspace",
            ws.path().to_str().unwrap(),
            "--team",
            team_id,
            "--json",
        ],
    );
    assert!(out.is_success(), "status --team stderr={}", out.stderr);
    let j = out.json();
    assert_json_field_eq_str(&j, "/session_name", &worker_session_name(team_id));
    let dump = serde_json::to_string(&j).unwrap();
    assert!(
        !dump.contains("team-dirty005b") && !dump.contains("%polluted"),
        "explicit team status should not leak sibling binding: {dump}"
    );

    let _ = run_ta(
        &ws,
        &[
            "shutdown",
            "--workspace",
            ws.path().to_str().unwrap(),
            "--team",
            team_id,
            "--keep-logs",
            "--json",
        ],
    );
}
