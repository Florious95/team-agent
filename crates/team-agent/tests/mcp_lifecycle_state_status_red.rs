#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[path = "support/mcp_sim_harness.rs"]
#[allow(dead_code)]
mod mcp_sim_harness;

use std::path::{Path, PathBuf};

use mcp_sim_harness::McpSimHarness;
use serde_json::{json, Value};
use team_agent::message_store::MessageStore;
use team_agent::state::persist::{load_runtime_state, save_runtime_state};

#[test]
fn mcp_get_team_status_returns_scoped_rich_status_without_sibling_leak() {
    let harness = McpSimHarness::new();
    let mut worker = harness.spawn_mcp_client("worker_a", "teamA");

    let call = worker.call_tool("get_team_status", json!({}));

    assert!(
        !call.is_error,
        "MCP get_team_status should return a status object for the worker owner team; body={} raw={}",
        call.body,
        call.raw
    );
    // 0.4.x compact slim: MCP get_team_status follows the same 7-field slim
    // shape as `status --json` (architect-decided MCP/CLI parity). Rich
    // diagnostics (messages, results, coordinator, readiness) moved to
    // CLI `--detail`; MCP does not have a detail mode yet.
    // The MCP wrapper additionally injects `teams` for sibling-leak safety.
    for key in ["agents", "ready", "not_ready", "session_name", "teams"] {
        assert!(
            call.body.get(key).is_some(),
            "0.4.x: get_team_status must include slim field `{key}`; body={}",
            call.body
        );
    }
    for forbidden in ["messages", "results", "coordinator", "readiness", "queued_messages", "latest_results"] {
        assert!(
            call.body.get(forbidden).is_none(),
            "0.4.x: get_team_status slim payload must NOT include diagnostic `{forbidden}` (CLI --detail only); body={}",
            call.body
        );
    }
    let body_text = call.body.to_string();
    assert!(
        body_text.contains("worker_a") && !body_text.contains("worker_x"),
        "get_team_status from TEAM_AGENT_OWNER_TEAM_ID=teamA must show teamA agents and must not leak sibling teamB worker_x; body={}",
        call.body
    );
}

#[test]
fn mcp_update_state_writes_selected_team_notes_and_team_state_file_only() {
    let harness = McpSimHarness::new();
    seed_two_team_spec_state(harness.workspace_path());
    let mut worker = harness.spawn_mcp_client("worker_b", "teamB");

    let call = worker.call_tool("update_state", json!({"note": "new teamB note"}));

    assert!(
        !call.is_error,
        "MCP update_state should return a structured ok envelope for the selected team; body={} raw={}",
        call.body,
        call.raw
    );
    assert_eq!(call.body["ok"], json!(true), "update_state should succeed after writing selected team state; body={}", call.body);
    assert_eq!(
        object_keys(&call.body),
        vec!["ok", "state_file"],
        "update_state must return raw {{ok,state_file}} in order, without compacting state_file away; body={}",
        call.body
    );

    let state_file = call.body["state_file"]
        .as_str()
        .expect("update_state must return state_file");
    assert!(
        state_file.contains("/teamB/") && Path::new(state_file).exists(),
        "update_state must write the selected teamB team_state.md, not a workspace-level placeholder file; body={}",
        call.body
    );
    let state = load_runtime_state(harness.workspace_path()).unwrap();
    assert_eq!(
        state.pointer("/teams/teamB/notes"),
        Some(&json!(["old teamB note", "new teamB note"])),
        "update_state must append to selected teams.teamB.notes; state={state}"
    );
    assert_eq!(
        state.pointer("/teams/teamA/notes"),
        Some(&json!(["old teamA note"])),
        "update_state from owner teamB must not mutate sibling teamA notes; state={state}"
    );
    let text = std::fs::read_to_string(state_file).unwrap();
    assert!(
        text.contains("old teamB note") && text.contains("new teamB note"),
        "team_state.md returned by update_state must contain old and new selected-team notes; state_file={state_file} text={text:?}"
    );
    let team_a_text = std::fs::read_to_string(harness.workspace_path().join("teamA").join("team_state.md"))
        .unwrap_or_default();
    assert!(
        !team_a_text.contains("new teamB note"),
        "update_state for teamB must not write the new note into teamA team_state.md; team_a_text={team_a_text:?}"
    );
}

fn seed_two_team_spec_state(root: &Path) {
    let team_a = write_team_dir(root, "teamA", "worker_a");
    let team_b = write_team_dir(root, "teamB", "worker_b");
    let _ = MessageStore::open(root).unwrap();
    let team_a_state = team_state("teamA", &team_a, "worker_a", "old teamA note");
    let team_b_state = team_state("teamB", &team_b, "worker_b", "old teamB note");
    save_runtime_state(
        root,
        &json!({
            "active_team_key": "teamA",
            "session_name": "team-teamA",
            "team_dir": team_a.to_string_lossy().to_string(),
            "spec_path": team_a.join("team.spec.yaml").to_string_lossy().to_string(),
            "agents": team_a_state["agents"].clone(),
            "notes": ["old teamA note"],
            "teams": {
                "teamA": team_a_state,
                "teamB": team_b_state
            }
        }),
    )
    .unwrap();
}

fn team_state(team: &str, team_dir: &Path, agent: &str, note: &str) -> Value {
    json!({
        "status": "alive",
        "session_name": format!("team-{team}"),
        "team_dir": team_dir.to_string_lossy().to_string(),
        "spec_path": team_dir.join("team.spec.yaml").to_string_lossy().to_string(),
        "leader_receiver": {
            "mode": "direct_tmux",
            "status": "attached",
            "pane_id": "%1",
            "provider": "fake",
            "owner_epoch": 1
        },
        "agents": {
            agent: {
                "agent_id": agent,
                "owner_team_id": team,
                "provider": "fake",
                "status": "running",
                "mcp_ready": true
            }
        },
        "tasks": [
            {"id": format!("task-{team}"), "assignee": agent, "status": "pending"}
        ],
        "notes": [note]
    })
}

fn write_team_dir(root: &Path, name: &str, agent: &str) -> PathBuf {
    let team = root.join(name);
    std::fs::create_dir_all(team.join("agents")).unwrap();
    std::fs::write(
        team.join("TEAM.md"),
        format!(
            "---\nname: {name}\nobjective: MCP lifecycle state/status contract.\nprovider: fake\n---\n\nTeam.\n"
        ),
    )
    .unwrap();
    std::fs::write(
        team.join("agents").join(format!("{agent}.md")),
        format!(
            "---\nname: {agent}\nrole: Worker\nprovider: fake\nmodel: fake\nauth_mode: subscription\ntools:\n  - mcp_team\n---\n\nWorker.\n"
        ),
    )
    .unwrap();
    let spec = team_agent::compiler::compile_team(&team).unwrap();
    std::fs::write(team.join("team.spec.yaml"), team_agent::model::yaml::dumps(&spec)).unwrap();
    team
}

fn object_keys(value: &Value) -> Vec<&str> {
    value
        .as_object()
        .unwrap_or_else(|| panic!("expected JSON object, got {value}"))
        .keys()
        .map(String::as_str)
        .collect()
}
