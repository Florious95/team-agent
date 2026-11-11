//! E2E-LNCH-002 Quick-start refuses duplicate team-id in the same workspace.
//!
//! Invariants:
//! - First quick-start launches successfully.
//! - A second identical start of the live team is the idempotent
//!   `already_running` no-op (34d41fc7 / 1d2d4d5a): exit 0, no steps, the seat
//!   skipped as already running, and no duplicate state, session or pane.

use crate::framework::*;

#[test]
fn lnch_002_duplicate_quick_start_is_refused() {
    let team_id = "lnch002";
    let ws = TestWorkspace::new(team_id).with_fake_spec(&["a"]);
    let ws_path = ws.path().to_str().unwrap();

    let first = quick_start_fake(&ws, team_id);
    assert!(
        quick_start_workers_available(&first),
        "1st quick-start: {}",
        first.stdout
    );

    // 34d41fc7 / 1d2d4d5a: an identical second start of the live team is the
    // idempotent `already_running` no-op, not a refusal, and duplicates nothing.
    let state_before = std::fs::read(ws.state_json_path()).expect("state.json after 1st start");
    let topology_before = live_topology(&ws);
    let second = quick_start_fake(&ws, team_id);
    let j = second.json();
    assert_eq!(second.exit_code, 0, "2nd identical start must be a no-op; got {j}");
    assert_eq!(j["ok"], true, "2nd identical start must be a no-op; got {j}");
    assert_eq!(j["status"], "already_running", "{j}");
    assert_eq!(j["steps"], serde_json::json!([]), "{j}");
    assert!(
        j["skipped"].as_array().is_some_and(|skipped| skipped
            .iter()
            .any(|entry| entry["agent"] == "a" && entry["reason"] == "already_running")),
        "{j}"
    );
    assert_eq!(
        std::fs::read(ws.state_json_path()).expect("state.json after 2nd start"),
        state_before,
        "2nd identical start rewrote state.json"
    );
    assert_eq!(
        live_topology(&ws),
        topology_before,
        "2nd identical start changed the tmux sessions or live panes"
    );

    let dump = serde_json::to_string(&j).unwrap().to_lowercase();
    let signals = ["existing", "already", "restart", "--allow-fresh"];
    assert!(
        signals.iter().any(|s| dump.contains(s)),
        "duplicate refusal should steer user to restart --allow-fresh; got json={j}"
    );

    let _ = run_ta(
        &ws,
        &["shutdown", "--workspace", ws_path, "--keep-logs", "--json"],
    );
}

/// Sorted session names and the live pane count on the team's tmux socket.
fn live_topology(ws: &TestWorkspace) -> (Vec<String>, usize) {
    let state = ws.read_state();
    let socket = state["tmux_socket"].as_str().expect("state records the team tmux socket");
    let tmux = |args: &[&str]| -> Vec<String> {
        let out = std::process::Command::new("tmux")
            .args(["-S", socket])
            .args(args)
            .stdin(std::process::Stdio::null())
            .output()
            .expect("run tmux");
        assert!(out.status.success(), "tmux {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).lines().map(str::to_string).collect()
    };
    let mut sessions = tmux(&["list-sessions", "-F", "#{session_name}"]);
    sessions.sort();
    let live = tmux(&["list-panes", "-a", "-F", "#{pane_dead}"])
        .iter()
        .filter(|dead| dead.as_str() == "0")
        .count();
    (sessions, live)
}
