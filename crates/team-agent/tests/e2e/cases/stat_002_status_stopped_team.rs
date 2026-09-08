//! E2E-STAT-002 Status reports a stopped team as stopped/missing, not live.

use crate::framework::*;

#[test]
fn stat_002_status_stopped_team() {
    let team_id = "stat002";
    let ws = TestWorkspace::new(team_id).with_fake_spec(&["a"]);
    let qs = quick_start_fake(&ws, team_id);
    assert!(quick_start_launched(&qs), "quick-start: {}", qs.stdout);

    let shut = run_ta(
        &ws,
        &[
            "shutdown",
            "--workspace",
            ws.path().to_str().unwrap(),
            "--keep-logs",
            "--json",
        ],
    );
    assert!(shut.is_success(), "shutdown stderr={}", shut.stderr);

    let out = run_ta(
        &ws,
        &[
            "status",
            "--workspace",
            ws.path().to_str().unwrap(),
            "--json",
        ],
    );
    assert!(out.is_success(), "status stderr={}", out.stderr);
    let j = out.json();
    assert_stopped_brief(&j);

    let detail = run_ta(
        &ws,
        &[
            "status",
            "--workspace",
            ws.path().to_str().unwrap(),
            "--json",
            "--detail",
        ],
    );
    assert!(
        detail.is_success(),
        "status --detail stderr={}",
        detail.stderr
    );
    let d = detail.json();
    assert_stopped_brief(&d);
    assert_eq!(j.get("nodes"), d.get("nodes"), "detail must preserve brief projection");
}

fn assert_stopped_brief(value: &serde_json::Value) {
    let node = value
        .get("nodes")
        .and_then(serde_json::Value::as_array)
        .and_then(|nodes| nodes.iter().find(|node| node.get("name").and_then(|v| v.as_str()) == Some("a")))
        .expect("stopped status must include node a");
    assert_eq!(node.get("runtime_status").and_then(|v| v.as_str()), Some("stopped"));
    let mut keys = node.as_object().expect("status node object").keys().cloned().collect::<Vec<_>>();
    keys.sort();
    assert_eq!(keys, vec!["activity", "health", "name", "provider", "runtime_status", "session_name", "tmux_command"]);
}
