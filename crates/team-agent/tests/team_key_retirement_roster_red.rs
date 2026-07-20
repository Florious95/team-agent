//! team-key slice2 · layer 1 — pure retirement/roster table (RED).
//!
//! From locate §6.2 (agent lifecycle authority) and §7 observation points 6/8,
//! NOT from the root-cause reasoning: a `retired` tombstone under
//! `teams.<canonical>.agent_lifecycle.<agent_id>` must exclude that agent from
//! the restart desired roster, so a static/dynamic role source can no longer
//! silently resurrect a retired seat. Baseline 1ba6313 has no tombstone
//! semantics, so a retired agent with a dynamic_role_file is still rostered.

use serde_json::json;
use std::path::Path;
use team_agent::lifecycle::restart::restart_candidates;
use team_agent::state::persist::save_runtime_state;

/// A team `alpha` with two agents: `keep` (active) and `gone` (has a
/// dynamic_role_file AND a retired tombstone under agent_lifecycle).
fn seed(ws: &Path) {
    let role_dir = ws.join(".team/runtime/teams/alpha/role-masters");
    std::fs::create_dir_all(&role_dir).unwrap();
    let gone_role = role_dir.join("gone.md");
    std::fs::write(&gone_role, "---\nname: gone\nprovider: fake\n---\ngone\n").unwrap();

    let state = json!({
        "active_team_key": "alpha",
        "session_name": "team-alpha",
        "agents": {
            "keep": { "status": "running", "provider": "fake" },
            "gone": { "status": "running", "provider": "fake",
                      "dynamic_role_file": gone_role.to_string_lossy() }
        },
        "teams": {
            "alpha": {
                "status": "alive",
                "session_name": "team-alpha",
                "agents": {
                    "keep": { "status": "running", "provider": "fake" },
                    "gone": { "status": "running", "provider": "fake",
                              "dynamic_role_file": gone_role.to_string_lossy() }
                },
                "agent_lifecycle": {
                    "gone": { "state": "retired", "changed_at": "2026-07-20T00:00:00Z",
                              "reason": "one-shot complete" }
                }
            }
        }
    });
    std::fs::create_dir_all(ws.join(".team/runtime")).unwrap();
    save_runtime_state(ws, &state).unwrap();
    std::fs::write(ws.join("team.spec.yaml"), "name: alpha\n").unwrap();
}

#[test]
fn retired_tombstone_excludes_agent_from_restart_roster() {
    let dir = std::env::temp_dir().join(format!("tk2-roster-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    seed(&dir);

    let candidates = restart_candidates(&dir).expect("restart_candidates must resolve");
    let _ = std::fs::remove_dir_all(&dir);

    let alpha = candidates
        .iter()
        .find(|c| c.team_name == "alpha")
        .expect("alpha team must be a restart candidate");
    let rostered: Vec<&str> = alpha.agents.iter().map(|a| a.as_str()).collect();

    assert!(
        rostered.contains(&"keep"),
        "the active agent must stay in the restart roster; roster={rostered:?}"
    );
    assert!(
        !rostered.contains(&"gone"),
        "a retired-tombstoned agent must NOT be in the restart desired roster \
         (its dynamic_role_file must not resurrect it); roster={rostered:?}"
    );
}

/// L1 补牙 (r15): a retired agent that is ALSO referenced as default_assignee,
/// in startup_order, and as a task assignee must STILL be excluded from the
/// restart roster (the pub-reachable surface). The full 5-surface spec scrub is
/// verified by owner-b's src unit test on the pure filter fn and by the L3
/// real-machine rebuilt-spec assertion; this pins the roster surface under a
/// MULTI-reference retired agent so a lingering reference cannot resurrect it.
fn seed_multiref(ws: &std::path::Path) {
    let role_dir = ws.join(".team/runtime/teams/alpha/role-masters");
    std::fs::create_dir_all(&role_dir).unwrap();
    let gone_role = role_dir.join("boss.md");
    std::fs::write(&gone_role, "---\nname: boss\nprovider: fake\n---\nboss\n").unwrap();
    let state = json!({
        "active_team_key": "alpha",
        "session_name": "team-alpha",
        "agents": {
            "keep": { "status": "running", "provider": "fake" },
            "boss": { "status": "running", "provider": "fake",
                      "dynamic_role_file": gone_role.to_string_lossy() }
        },
        "teams": {
            "alpha": {
                "status": "alive",
                "session_name": "team-alpha",
                "agents": {
                    "keep": { "status": "running", "provider": "fake" },
                    "boss": { "status": "running", "provider": "fake",
                              "dynamic_role_file": gone_role.to_string_lossy() }
                },
                "routing": { "default_assignee": "boss",
                             "rules": [{ "assign_to": "boss", "match": "*" }] },
                "runtime": { "startup_order": ["boss", "keep"] },
                "tasks": [{ "id": "t1", "assignee": "boss" }],
                "agent_lifecycle": {
                    "boss": { "state": "retired", "changed_at": "2026-07-20T00:00:00Z",
                              "reason": "one-shot lead complete" }
                }
            }
        }
    });
    std::fs::create_dir_all(ws.join(".team/runtime")).unwrap();
    save_runtime_state(ws, &state).unwrap();
    std::fs::write(ws.join("team.spec.yaml"), "name: alpha\n").unwrap();
}

#[test]
fn retired_default_assignee_with_many_references_is_still_excluded_from_roster() {
    let dir = std::env::temp_dir().join(format!("tk2-roster-mref-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    seed_multiref(&dir);
    let candidates = restart_candidates(&dir).expect("restart_candidates must resolve");
    let _ = std::fs::remove_dir_all(&dir);
    let alpha = candidates
        .iter()
        .find(|c| c.team_name == "alpha")
        .expect("alpha team must be a restart candidate");
    let rostered: Vec<&str> = alpha.agents.iter().map(|a| a.as_str()).collect();
    assert!(
        rostered.contains(&"keep"),
        "active agent must stay; roster={rostered:?}"
    );
    assert!(
        !rostered.contains(&"boss"),
        "a retired agent that is default_assignee / in startup_order / a task assignee \
         must STILL be excluded from the restart roster; roster={rostered:?}"
    );
}
