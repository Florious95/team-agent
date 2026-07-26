//! E2E-SHUT-001 Clean Fake Shutdown Kills Worker Session.
//!
//! Architecture: T7 §2 shutdown, T6 §1 L4 topology, T3 §2 worker session.
//!
//! Black-box invariants:
//! - quick-start launches a fake team and creates tmux session `team-<id>`
//! - shutdown returns `ok:true`, `session_killed:true`, `killed_sessions`
//!   contains the worker session
//! - after shutdown, the worker tmux session is absent on the runtime socket.

use crate::framework::*;
use std::time::Duration;

#[test]
fn shut_001_clean_shutdown_kills_worker_session() {
    let team_id = "shut001";
    let ws = TestWorkspace::new(team_id).with_fake_spec(&["a"]);

    let qs = quick_start_fake(&ws, team_id);
    assert!(
        quick_start_launched(&qs),
        "quick-start did not launch: {} / {}",
        qs.stdout,
        qs.stderr
    );

    let session = worker_session_name(team_id);

    // tmux session should appear (give it a moment for the coordinator/launcher)
    wait_for_or_panic(
        &format!("tmux session {session} present after quick-start"),
        || tmux_session_exists_for_workspace(&ws, &session),
        Duration::from_secs(5),
    );

    let out = run_ta(
        &ws,
        &[
            "shutdown",
            "--workspace",
            ws.path().to_str().unwrap(),
            "--keep-logs",
            "--json",
        ],
    );
    assert!(
        out.is_success(),
        "shutdown exit {}; stdout={} stderr={}",
        out.exit_code,
        out.stdout,
        out.stderr
    );

    let j = out.json();
    assert_json_field_eq_bool(&j, "/ok", true);
    assert_json_field_eq_str(&j, "/status", "ok");
    assert_json_field_eq_bool(&j, "/session_killed", true);

    let killed = j
        .pointer("/killed_sessions")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    assert!(
        killed.iter().any(|v| v.as_str() == Some(session.as_str())),
        "killed_sessions should include {session:?}; got {killed:?}"
    );

    assert!(
        !tmux_session_exists_for_workspace(&ws, &session),
        "worker session {session} should be absent after shutdown"
    );
}
