//! E2E-RFS 0.5.11 RED contracts: restart must not report success on split
//! tmux endpoint state.
//!
//! References:
//! - `.team/artifacts/restart-false-success-locate.md` §9 RED contracts.
//! - leader addendum: never-captured no-session seats must fresh-start instead
//!   of `refused_no_session_id`; current 0.5.9 behavior is probed below.
//!
//! User-visible contract: `restart ok:true` means workers, leader receiver,
//! coordinator, and diagnose agree on one canonical tmux endpoint.

use crate::framework::*;
use serde_json::{json, Value};
use std::fs;
use std::path::PathBuf;
use std::process::Command;

#[test]
fn rfs_restart_refuses_tmux_endpoint_socket_split_brain_before_ok() {
    let team_id = "rfs001";
    let ws = TestWorkspace::new(team_id).with_fake_spec(&["a"]);
    let ws_path = ws.path().to_str().unwrap();
    let qs = quick_start_fake(&ws, team_id);
    assert!(quick_start_launched(&qs), "quick-start: {}", qs.stdout);
    let old_socket = state_socket(&ws);
    let new_socket = unique_socket("rfs001-new");
    let _guard = TmuxSocketGuard::new(vec![old_socket.clone(), new_socket.clone()]);
    create_dummy_session(&new_socket, "rfs001-leader-side", ws.path().to_path_buf());
    write_split_brain_state(&ws, &old_socket, &new_socket);

    let out = run_ta(&ws, &["restart", ws_path, "--json"]);
    let body = out.json();
    assert_eq!(
        body.pointer("/ok").and_then(Value::as_bool),
        Some(false),
        "RFS RED: restart must refuse dirty tmux_endpoint/tmux_socket split before lifecycle spawn; stdout={body} stderr={}",
        out.stderr
    );
    assert_eq!(
        body.pointer("/status").and_then(Value::as_str),
        Some("refused_dirty_topology"),
        "RFS RED: split endpoint state must be classified as refused_dirty_topology, not a generic or successful restart; json={body}"
    );
    assert!(
        body.to_string().contains("tmux_endpoint_socket_conflict"),
        "RFS RED: refusal details must name tmux_endpoint_socket_conflict and both socket paths; json={body}"
    );
    assert!(
        !restart_completed_ok_after_split(&ws),
        "RFS RED: restart.completed rc=ok must not be written for split-brain state"
    );
}

#[test]
fn rfs_diagnose_reports_endpoint_socket_conflict_and_ignored_worker_session() {
    let team_id = "rfs002";
    let ws = TestWorkspace::new(team_id).with_fake_spec(&["a"]);
    let ws_path = ws.path().to_str().unwrap();
    let qs = quick_start_fake(&ws, team_id);
    assert!(quick_start_launched(&qs), "quick-start: {}", qs.stdout);
    let old_socket = state_socket(&ws);
    let new_socket = unique_socket("rfs002-new");
    let _guard = TmuxSocketGuard::new(vec![old_socket.clone(), new_socket.clone()]);
    create_dummy_session(&new_socket, "rfs002-leader-side", ws.path().to_path_buf());
    write_split_brain_state(&ws, &old_socket, &new_socket);

    let out = run_ta(&ws, &["diagnose", "--workspace", ws_path, "--json"]);
    let body = out.json();
    assert_eq!(
        body.pointer("/ok").and_then(Value::as_bool),
        Some(false),
        "RFS RED: diagnose must mark endpoint/socket split-brain dirty; json={body}"
    );
    let text = body.to_string();
    assert!(
        text.contains("tmux_endpoint_socket_conflict"),
        "RFS RED: diagnose issue list must include tmux_endpoint_socket_conflict; json={body}"
    );
    assert!(
        text.contains("orphan_team_session_on_ignored_socket"),
        "RFS RED: diagnose must expose the worker session stranded on the ignored endpoint; json={body}"
    );
}

#[test]
fn rfs_topology_invariant_blocks_same_pane_id_only_when_socket_matches() {
    let team_id = "rfs003";
    let ws = TestWorkspace::new(team_id).with_fake_spec(&["a"]);
    let ws_path = ws.path().to_str().unwrap();
    let qs = quick_start_fake(&ws, team_id);
    assert!(quick_start_launched(&qs), "quick-start: {}", qs.stdout);
    let old_socket = state_socket(&ws);
    let new_socket = unique_socket("rfs003-new");
    let _guard = TmuxSocketGuard::new(vec![old_socket.clone(), new_socket.clone()]);
    create_dummy_session(&new_socket, "rfs003-leader-side", ws.path().to_path_buf());
    write_split_brain_state(&ws, &old_socket, &new_socket);

    let out = run_ta(&ws, &["restart", ws_path, "--json"]);
    let body = out.json();
    assert!(
        body.to_string().contains("tmux_endpoint_socket_conflict"),
        "RFS RED: topology gate must reject the endpoint conflict itself. Bare pane-id collision text is insufficient and misleading when %0 exists on both sockets; json={body}"
    );
    assert!(
        !body.to_string().contains("LeaderPaneIdCollidesWithAgent"),
        "RFS RED: same bare pane id on different sockets must not be reported as LeaderPaneIdCollidesWithAgent; compare transport-typed bindings instead; json={body}"
    );
}

#[test]
fn rfs_never_captured_no_session_worker_auto_freshes_without_allow_fresh() {
    let team_id = "rfs004";
    let ws = TestWorkspace::new(team_id).with_fake_spec(&["a"]);
    let ws_path = ws.path().to_str().unwrap();
    let qs = quick_start_fake(&ws, team_id);
    assert!(quick_start_launched(&qs), "quick-start: {}", qs.stdout);
    let _guard = TmuxSocketGuard::new(vec![state_socket(&ws)]);

    let _ = run_ta(
        &ws,
        &["shutdown", "--workspace", ws_path, "--keep-logs", "--json"],
    );
    ws.mutate_agent_everywhere("a", |agent| {
        agent.insert("session_id".to_string(), Value::Null);
        agent.insert("rollout_path".to_string(), Value::Null);
        agent.insert("captured_at".to_string(), Value::Null);
        agent.insert("first_send_at".to_string(), Value::Null);
        agent.remove("last_result_at");
        agent.remove("task_prompt_delivered");
    });

    let out = run_ta(&ws, &["restart", ws_path, "--json"]);
    let body = out.json();
    assert_eq!(
        body.pointer("/ok").and_then(Value::as_bool),
        Some(true),
        "RFS no-session probe: never-captured worker has no context to lose and must fresh-start without --allow-fresh, not refused_no_session_id; json={body} stderr={}",
        out.stderr
    );
    assert_eq!(
        body.pointer("/status").and_then(Value::as_str),
        Some("restarted"),
        "RFS no-session probe: expected status=restarted for never-captured no-session seat; json={body}"
    );
    assert!(
        !body.to_string().contains("refused_no_session_id"),
        "RFS no-session probe: refused_no_session_id is only valid when context exists to preserve; json={body}"
    );
}

struct TmuxSocketGuard {
    sockets: Vec<String>,
}

impl TmuxSocketGuard {
    fn new(sockets: Vec<String>) -> Self {
        Self { sockets }
    }
}

impl Drop for TmuxSocketGuard {
    fn drop(&mut self) {
        for socket in &self.sockets {
            if socket.contains("/ta-") || socket.contains("ta-0511-rfs") {
                let _ = Command::new("tmux")
                    .args(["-S", socket, "kill-server"])
                    .output();
            }
        }
    }
}

fn write_split_brain_state(ws: &TestWorkspace, old_socket: &str, new_socket: &str) {
    ws.mutate_state(|state| {
        state["tmux_endpoint"] = json!(old_socket);
        state["tmux_socket"] = json!(new_socket);
        state["leader_receiver"] = json!({
            "mode": "direct_tmux",
            "status": "attached",
            "pane_id": "%0",
            "tmux_socket": new_socket,
            "session_name": "rfs-leader",
            "window_name": "leader"
        });
        if let Some(active) = state
            .get("active_team_key")
            .and_then(Value::as_str)
            .map(str::to_string)
        {
            if let Some(team) = state
                .get_mut("teams")
                .and_then(Value::as_object_mut)
                .and_then(|teams| teams.get_mut(&active))
                .and_then(Value::as_object_mut)
            {
                team.insert("tmux_endpoint".to_string(), json!(old_socket));
                team.insert("tmux_socket".to_string(), json!(new_socket));
                team.insert(
                    "leader_receiver".to_string(),
                    json!({
                        "mode": "direct_tmux",
                        "status": "attached",
                        "pane_id": "%0",
                        "tmux_socket": new_socket,
                        "session_name": "rfs-leader",
                        "window_name": "leader"
                    }),
                );
            }
        }
    });
}

fn create_dummy_session(socket: &str, session: &str, cwd: PathBuf) {
    if let Some(parent) = std::path::Path::new(socket).parent() {
        fs::create_dir_all(parent).expect("create tmux socket parent");
    }
    let out = Command::new("tmux")
        .args([
            "-S",
            socket,
            "new-session",
            "-d",
            "-s",
            session,
            "-n",
            "leader",
            "-c",
            cwd.to_str().expect("cwd utf8"),
            "sleep 600",
        ])
        .output()
        .unwrap_or_else(|e| panic!("tmux new-session {session}: {e}"));
    assert!(
        out.status.success(),
        "tmux new-session {session} failed; stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

fn unique_socket(tag: &str) -> String {
    format!(
        "/Volumes/nvme/tmp/ta-0511-rfs-{}-{}",
        tag,
        std::process::id()
    )
}

fn state_socket(ws: &TestWorkspace) -> String {
    ws.read_state()
        .get("tmux_socket")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn restart_completed_ok_after_split(ws: &TestWorkspace) -> bool {
    std::fs::read_to_string(ws.events_jsonl_path())
        .unwrap_or_default()
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .any(|event| {
            event.get("event").and_then(Value::as_str) == Some("restart.completed")
                && event.get("rc").and_then(Value::as_str) == Some("ok")
        })
}
