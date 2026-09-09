//! E2E-STAT-001 Status JSON exposes the expected machine-readable shape.

use crate::framework::*;

fn assert_brief_shape(value: &serde_json::Value, names: &[&str]) {
    let mut root_keys = value
        .as_object()
        .expect("status object")
        .keys()
        .cloned()
        .collect::<Vec<_>>();
    root_keys.sort();
    assert_eq!(root_keys, vec!["nodes"], "status brief must not carry diagnostics: {value}");
    let nodes = value
        .get("nodes")
        .and_then(serde_json::Value::as_array)
        .expect("status must return nodes array");
    assert_eq!(nodes.len(), names.len(), "status node count: {value}");
    let mut actual_names = nodes
        .iter()
        .filter_map(|node| node.get("name").and_then(|v| v.as_str()))
        .collect::<Vec<_>>();
    let mut expected_names = names.to_vec();
    actual_names.sort_unstable();
    expected_names.sort_unstable();
    assert_eq!(actual_names, expected_names, "status node names: {value}");
    for expected_name in names {
        let node = nodes
            .iter()
            .find(|node| node.get("name").and_then(|v| v.as_str()) == Some(*expected_name))
            .expect("expected status node");
        let mut keys = node
            .as_object()
            .expect("status node object")
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        keys.sort();
        assert_eq!(
            keys,
            vec![
                "activity",
                "health",
                "name",
                "provider",
                "runtime_status",
                "session_name",
                "tmux_command",
            ],
            "status node must be the exact seven-field projection: {node}"
        );
        assert_eq!(node.get("name").and_then(|v| v.as_str()), Some(*expected_name));
    }
}

#[test]
fn stat_001_status_json_shape() {
    let team_id = "stat001";
    let ws = TestWorkspace::new(team_id).with_fake_spec(&["a", "b"]);
    let qs = quick_start_fake(&ws, team_id);
    assert!(quick_start_launched(&qs), "quick-start: {}", qs.stdout);

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
    assert_brief_shape(&j, &["a", "b"]);
    assert_eq!(j.pointer("/nodes/0/name").and_then(|v| v.as_str()), Some("a"));
    assert_eq!(j.pointer("/nodes/1/name").and_then(|v| v.as_str()), Some("b"));

    // --detail is compatibility-only for the brief projection; it must not
    // restore diagnostics or change the seven-field information.
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
    assert_brief_shape(&d, &["a", "b"]);
    assert_eq!(j.get("nodes"), d.get("nodes"), "detail must preserve brief projection");

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
