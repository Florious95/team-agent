//! E2E-AGENT-001 Start-agent repairs a missing/stopped worker window.
//!
//! Black-box invariant:
//! - After `stop-agent a`, `start-agent a --allow-fresh --no-display` returns
//!   ok and rebinds agent `a` to a live pane/window in state.

use crate::framework::*;
use std::process::Command;
use std::time::Duration;

#[test]
fn agent_001_start_agent_repairs_missing_window() {
    let team_id = "agent001";
    let ws = TestWorkspace::new(team_id).with_fake_spec(&["a"]);
    let qs = quick_start_fake(&ws, team_id);
    assert!(quick_start_launched(&qs), "quick-start: {}", qs.stdout);

    let session = worker_session_name(team_id);
    let stopped = run_ta(
        &ws,
        &[
            "stop-agent",
            "a",
            "--workspace",
            ws.path().to_str().unwrap(),
            "--json",
        ],
    );
    assert!(stopped.is_success(), "stop-agent stderr={}", stopped.stderr);
    assert_json_field_eq_bool(&stopped.json(), "/ok", true);

    let out = run_ta(
        &ws,
        &[
            "start-agent",
            "a",
            "--workspace",
            ws.path().to_str().unwrap(),
            "--allow-fresh",
            "--no-display",
            "--json",
        ],
    );
    assert!(
        out.is_success(),
        "start-agent exit {}; stdout={} stderr={}",
        out.exit_code,
        out.stdout,
        out.stderr
    );
    let j = out.json();
    assert_json_field_eq_bool(&j, "/ok", true);
    assert_json_field_eq_str(&j, "/agent_id", "a");

    wait_for_or_panic(
        "worker session present after start-agent",
        || tmux_session_exists_for_workspace(&ws, &session),
        Duration::from_secs(5),
    );
    let state = ws.read_state();
    let agent = state_agent(&state, "a");
    assert_eq!(
        agent.get("status").and_then(|v| v.as_str()),
        Some("running")
    );
    assert!(
        agent.get("pane_id").and_then(|v| v.as_str()).is_some(),
        "start-agent should write a pane_id; agent={agent}"
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

#[test]
fn agent_001_force_never_leaves_duplicate_same_role_panes() {
    let team_id = "agent001force";
    let ws = TestWorkspace::new(team_id).with_fake_spec(&["a"]);
    let qs = quick_start_fake(&ws, team_id);
    assert!(
        quick_start_launched(&qs),
        "quick-start: stdout={} stderr={}",
        qs.stdout,
        qs.stderr
    );

    let session = worker_session_name(team_id);
    let before_state = ws.read_state();
    let before_agent = state_agent(&before_state, "a");
    let before = same_role_panes(&ws, &session, "a");
    assert_eq!(before.len(), 1, "precondition: panes={before:?}");
    assert_eq!(before_agent["pane_id"], before[0].0);
    assert_eq!(before_agent["pane_pid"], before[0].1);

    let out = run_ta(
        &ws,
        &[
            "start-agent",
            "a",
            "--workspace",
            ws.path().to_str().unwrap(),
            "--force",
            "--allow-fresh",
            "--no-display",
            "--json",
        ],
    );
    let after = same_role_panes(&ws, &session, "a");
    let after_state = ws.read_state();
    let after_agent = state_agent(&after_state, "a");

    assert_eq!(
        after.len(),
        1,
        "start-agent --force must never leave duplicate same-role panes; exit={} stdout={} stderr={} before={before:?} after={after:?}",
        out.exit_code,
        out.stdout,
        out.stderr,
    );
    assert_eq!(after_agent["pane_id"], after[0].0);
    assert_eq!(after_agent["pane_pid"], after[0].1);
    if out.is_success() {
        assert_ne!(
            after[0].0, before[0].0,
            "successful force must replace old pane"
        );
    } else {
        assert_eq!(
            after, before,
            "refusal must be a zero-mutation pre-spawn refusal"
        );
        assert_eq!(after_agent["pane_id"], before_agent["pane_id"]);
        assert_eq!(after_agent["pane_pid"], before_agent["pane_pid"]);
        let error = format!("{}\n{}", out.stdout, out.stderr).to_ascii_lowercase();
        assert!(
            error.contains("cohort") || error.contains("same-role") || error.contains("duplicate"),
            "safe refusal must name the cohort proof; error={error}"
        );
    }

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

fn same_role_panes(ws: &TestWorkspace, session: &str, window: &str) -> Vec<(String, u64)> {
    let state = ws.read_state();
    let socket = state["tmux_socket"].as_str().expect("state.tmux_socket");
    let output = Command::new("tmux")
        .args([
            "-S",
            socket,
            "list-panes",
            "-a",
            "-F",
            "#{session_name}|#{window_name}|#{pane_id}|#{pane_pid}",
        ])
        .output()
        .expect("tmux list-panes");
    assert!(
        output.status.success(),
        "tmux list-panes: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| {
            let mut fields = line.split('|');
            let observed_session = fields.next()?;
            let observed_window = fields.next()?;
            let pane_id = fields.next()?;
            let pane_pid = fields.next()?.parse().ok()?;
            (observed_session == session && observed_window == window)
                .then(|| (pane_id.to_string(), pane_pid))
        })
        .collect()
}
