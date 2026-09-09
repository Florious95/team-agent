//! E2E-DIRTY-005 Explicit --team selection resists sibling-team state pollution.

use crate::framework::*;
#[path = "../../support/brief_probe.rs"]
mod brief_probe;
use serde_json::{json, Value};

#[test]
fn dirty_005_cross_team_binding_pollution_keeps_explicit_team_scope() {
    let team_id = "dirty005a";
    let ws = TestWorkspace::new("dirty005").with_fake_spec(&["a"]);
    let qs = quick_start_fake(&ws, team_id);
    assert!(quick_start_workers_available(&qs), "quick-start: {}", qs.stdout);

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

    let state = ws.read_state();
    let team = state
        .pointer(&format!("/teams/{team_id}"))
        .expect("selected team state");
    let endpoint = team
        .get("tmux_endpoint")
        .or_else(|| team.get("tmux_socket"))
        .and_then(Value::as_str)
        .expect("selected team tmux endpoint");
    let session = team
        .get("session_name")
        .and_then(Value::as_str)
        .expect("selected team session");
    let agent = team
        .pointer("/agents/a")
        .expect("selected team worker");
    let pane = agent
        .get("pane_id")
        .and_then(Value::as_str)
        .expect("selected team worker pane");
    let window = agent
        .get("window")
        .or_else(|| agent.get("window_name"))
        .and_then(Value::as_str)
        .unwrap_or("a");
    let probe = brief_probe::install_trusted_nodeprobe(
        &ws.path().join("probe"),
        endpoint,
        session,
        window,
        pane,
        "fake",
    );
    let probe_path = brief_probe::path_with_probe(&probe, std::env::var_os("PATH").as_deref());
    let out = run_ta_env(
        &ws,
        &[
            "status",
            "--workspace",
            ws.path().to_str().unwrap(),
            "--team",
            team_id,
            "--json",
        ],
        &[("PATH", probe_path.as_str())],
    );
    assert!(out.is_success(), "status --team stderr={}", out.stderr);
    let j = out.json();
    let nodes = j
        .get("nodes")
        .and_then(serde_json::Value::as_array)
        .expect("status brief nodes");
    assert_eq!(nodes.len(), 1);
    assert_eq!(nodes[0].get("name").and_then(|v| v.as_str()), Some("a"));
    assert_eq!(nodes[0].get("provider").and_then(Value::as_str), Some("fake"));
    assert_eq!(nodes[0].get("runtime_status").and_then(Value::as_str), Some("running"));
    assert_eq!(nodes[0].get("activity").and_then(Value::as_str), Some("idle"));
    assert_eq!(nodes[0].get("health").and_then(Value::as_str), Some("normal"));
    assert_eq!(nodes[0].get("session_name").and_then(Value::as_str), Some(session));
    assert!(
        nodes[0]
            .get("tmux_command")
            .and_then(Value::as_str)
            .is_some_and(|command| command.contains(endpoint) && command.contains(&format!("{session}:{window}.{pane}"))),
        "trusted selected-team probe must produce exact endpoint/session/pane command: {nodes:?}"
    );
    let mut keys = nodes[0]
        .as_object()
        .expect("status brief node")
        .keys()
        .cloned()
        .collect::<Vec<_>>();
    keys.sort();
    assert_eq!(
        keys,
        vec!["activity", "health", "name", "provider", "runtime_status", "session_name", "tmux_command"]
    );
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
