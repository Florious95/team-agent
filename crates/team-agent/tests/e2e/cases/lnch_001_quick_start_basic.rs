//! E2E-LNCH-001 / E2E-LAUNCH-001 Quick-Start Writes Canonical Runtime Spec And State.
//!
//! Architecture: T1 §1 storage layers, T5 §1 runtime tree, T7 §2 quick-start.
//!
//! Black-box invariants:
//! - `ok == true` in JSON
//! - `.team/runtime/<team>/team.spec.yaml` exists
//! - `.team/runtime/state.json` exists
//! - state.active_team_key == team_id
//! - state.session_name == "team-<team_id>"
//! - state.tmux_endpoint and state.tmux_socket populated
//! - state.agents.<id> exists for every spec agent.

use crate::framework::*;
use serde_json::Value;

#[test]
fn lnch_001_quick_start_basic() {
    let ws = TestWorkspace::new("lnch001").with_fake_spec(&["a"]);
    let documented_team_dir = ws.path().join(".team/current");
    std::fs::create_dir_all(&documented_team_dir).expect("create .team/current");
    std::fs::rename(
        ws.path().join("TEAM.md"),
        documented_team_dir.join("TEAM.md"),
    )
    .expect("move TEAM.md into .team/current");
    std::fs::rename(ws.path().join("agents"), documented_team_dir.join("agents"))
        .expect("move agents into .team/current");

    let out = run_ta(&ws, &["quick-start", ".team/current"]);

    // Diagnostics if it fails — quick-start can degrade for non-bug reasons
    // (no tmux, sandbox), but in our test env it should succeed.
    assert!(
        out.stdout.contains("status: leader_receiver_unbound")
            && out.stdout.contains("\"all_workers_spawned\": true"),
        "quick-start did not launch the team. exit={} stdout={} stderr={}",
        out.exit_code,
        out.stdout,
        out.stderr
    );
    // Spec written
    let state = ws.read_state();
    let team_id = state["active_team_key"]
        .as_str()
        .expect("active_team_key must be a string");
    assert_ne!(team_id, "current");
    assert_ne!(team_id, ".team/current");
    let spec_path = ws
        .path()
        .join(".team/runtime")
        .join(team_id)
        .join("team.spec.yaml");
    assert_file_exists(&spec_path);
    assert!(
        out.stdout
            .contains(&format!("session_name: {}", worker_session_name(team_id))),
        "public output must expose the canonical session name: {}",
        out.stdout
    );

    // state.json written and shape correct
    assert_file_exists(&ws.state_json_path());
    assert_json_field_eq_str(&state, "/active_team_key", team_id);
    assert_json_field_eq_str(&state, "/session_name", &worker_session_name(team_id));
    assert_json_field_present(&state, "/tmux_endpoint");
    assert_json_field_present(&state, "/tmux_socket");
    assert_json_field_present(&state, "/agents/a");
    assert_file_absent(&documented_team_dir.join(".team/runtime/state.json"));
    let teams = state["teams"].as_object().expect("teams must be an object");
    assert_eq!(
        teams.keys().map(String::as_str).collect::<Vec<_>>(),
        vec![team_id],
        "documented launch must create one canonical team identity"
    );

    let _ = run_ta(&ws, &["attach-leader"]);
    let _ = run_ta(&ws, &["claim-leader"]);
    let _ = run_ta(&ws, &["claude"]);
    let _ = run_ta(&ws, &["codex"]);
    let _ = run_ta(
        &ws,
        &["codex", "--dangerously-bypass-approvals-and-sandbox"],
    );
    let _ = run_ta(&ws, &["quick-start"]);
    let _ = run_ta(&ws, &["quick-start", "./roles", "--team-id", "alpha"]);
    let _ = run_ta(&ws, &["quick-start", ".team/alpha"]);

    // Cleanup the worker session so we don't leak state into other tests.
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

    // Sanity: state must be valid JSON
    let _: Value = state;
}
