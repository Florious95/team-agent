//! E2E-AGENT-004 Add-agent updates runtime state and starts the new worker.

use crate::framework::*;

#[test]
fn agent_004_add_agent_runtime() {
    let team_id = "agent004";
    let ws = TestWorkspace::new(team_id).with_fake_spec(&["a"]);
    let qs = quick_start_fake(&ws, team_id);
    assert!(quick_start_launched(&qs), "quick-start: {}", qs.stdout);

    let roles = ws.path().join("roles");
    std::fs::create_dir_all(&roles).expect("create roles dir");
    let role = roles.join("b.md");
    std::fs::write(
        &role,
        "---\nname: b\nrole: Added fake worker\nprovider: fake\nmodel: fake\nauth_mode: subscription\ntools:\n  - mcp_team\n---\n\nAdded fake worker.\n",
    )
    .expect("write b role");

    let out = run_ta(
        &ws,
        &[
            "add-agent",
            "b",
            "--role-file",
            role.to_str().unwrap(),
            "--workspace",
            ws.path().to_str().unwrap(),
            "--no-display",
            "--json",
        ],
    );
    assert!(
        out.is_success(),
        "add-agent exit {}; stdout={} stderr={}",
        out.exit_code,
        out.stdout,
        out.stderr
    );
    let j = out.json();
    assert_json_field_eq_bool(&j, "/ok", true);
    assert_json_field_eq_str(&j, "/agent_id", "b");
    assert_file_exists(&role);

    let state = ws.read_state();
    let b = state_agent(&state, "b");
    assert_eq!(b.get("status").and_then(|v| v.as_str()), Some("running"));
    assert_eq!(
        b.get("spawn_cwd").and_then(|v| v.as_str()),
        Some(ws.path().to_str().unwrap())
    );

    let session = worker_session_name(team_id);
    assert!(
        tmux_window_exists_for_workspace(&ws, &session, "b"),
        "add-agent should create worker window b"
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
