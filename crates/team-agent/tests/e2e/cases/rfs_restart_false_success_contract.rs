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
use crate::support::source_walker::source_tree;
use crate::support::topology_issue_ids::{
    LEADER_PANE_ID_COLLIDES_WITH_AGENT, LEADER_RECEIVER_SOCKET_MISMATCH,
    ORPHAN_TEAM_SESSION_ON_IGNORED_SOCKET, RECENT_COORDINATOR_SESSION_MISSING,
    TEAM_SESSION_MISSING_ON_CANONICAL_SOCKET, TMUX_ENDPOINT_SOCKET_CONFLICT,
};
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const STATUS_REFUSED_DIRTY_TOPOLOGY: &str = "refused_dirty_topology";
const STATUS_RESTARTED: &str = "restarted";
const EVENT_RESTART_REFUSED_DIRTY_TOPOLOGY: &str = "restart.refused_dirty_topology";
const EVENT_PROVIDER_WORKER_SPAWN_ARGV: &str = "provider.worker.spawn_argv";
const REASON_REFUSED_NO_SESSION_ID: &str = "refused_no_session_id";
const STATUS_RESUME_NOT_READY: &str = "resume_not_ready";

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
        Some(STATUS_REFUSED_DIRTY_TOPOLOGY),
        "RFS RED: split endpoint state must be classified as refused_dirty_topology, not a generic or successful restart; json={body}"
    );
    assert_eq!(
        body.pointer("/issues/0/id").and_then(Value::as_str),
        Some(TMUX_ENDPOINT_SOCKET_CONFLICT),
        "R1 RED: restart refusal /issues/0/id must name tmux_endpoint_socket_conflict; json={body}"
    );
    assert_eq!(
        body.pointer("/issues/1/id").and_then(Value::as_str),
        Some(LEADER_RECEIVER_SOCKET_MISMATCH),
        "R1 RED: restart refusal /issues/1/id must name leader_receiver_socket_mismatch; json={body}"
    );
    assert_eq!(
        body.pointer("/issues/2/id").and_then(Value::as_str),
        Some(ORPHAN_TEAM_SESSION_ON_IGNORED_SOCKET),
        "R1 RED: restart refusal /issues/2/id must name orphan_team_session_on_ignored_socket; json={body}"
    );
    assert!(
        !restart_completed_ok_after_split(&ws),
        "RFS RED: restart.completed rc=ok must not be written for split-brain state"
    );
}

#[test]
fn rfs_refused_dirty_topology_event_precedes_any_spawn_argv_event() {
    let team_id = "rfs005";
    let ws = TestWorkspace::new(team_id).with_fake_spec(&["a"]);
    let ws_path = ws.path().to_str().unwrap();
    let qs = quick_start_fake(&ws, team_id);
    assert!(quick_start_launched(&qs), "quick-start: {}", qs.stdout);
    let old_socket = state_socket(&ws);
    let new_socket = unique_socket("rfs005-new");
    let _guard = TmuxSocketGuard::new(vec![old_socket.clone(), new_socket.clone()]);
    create_dummy_session(&new_socket, "rfs005-leader-side", ws.path().to_path_buf());
    write_split_brain_state(&ws, &old_socket, &new_socket);

    let _ = run_ta(&ws, &["restart", ws_path, "--json"]);
    let events = read_events(&ws);
    let refused = event_index(&events, EVENT_RESTART_REFUSED_DIRTY_TOPOLOGY).unwrap_or_else(|| {
        panic!("R2 RED: missing {EVENT_RESTART_REFUSED_DIRTY_TOPOLOGY}; events={events:?}")
    });
    if let Some(spawn) = event_index(&events, EVENT_PROVIDER_WORKER_SPAWN_ARGV) {
        assert!(
            refused < spawn,
            "R2 RED: {EVENT_PROVIDER_WORKER_SPAWN_ARGV} must not appear before dirty-topology refusal; refused_index={refused} spawn_index={spawn} events={events:?}"
        );
    }
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
    assert_eq!(
        body.pointer("/issues/0/id").and_then(Value::as_str),
        Some(TMUX_ENDPOINT_SOCKET_CONFLICT),
        "R3 RED: diagnose /issues/0/id must include tmux_endpoint_socket_conflict; json={body}"
    );
    assert_eq!(
        body.pointer("/issues/1/id").and_then(Value::as_str),
        Some(LEADER_RECEIVER_SOCKET_MISMATCH),
        "R3 RED: diagnose /issues/1/id must include leader_receiver_socket_mismatch; json={body}"
    );
    assert_eq!(
        body.pointer("/issues/2/id").and_then(Value::as_str),
        Some(ORPHAN_TEAM_SESSION_ON_IGNORED_SOCKET),
        "R3 RED: diagnose /issues/2/id must include orphan_team_session_on_ignored_socket; json={body}"
    );
    assert_eq!(
        body.pointer("/issues/3/id").and_then(Value::as_str),
        Some(TEAM_SESSION_MISSING_ON_CANONICAL_SOCKET),
        "R3 RED: diagnose /issues/3/id must include team_session_missing_on_canonical_socket; json={body}"
    );
    assert_eq!(
        body.pointer("/issues/4/id").and_then(Value::as_str),
        Some(RECENT_COORDINATOR_SESSION_MISSING),
        "R3 RED: diagnose /issues/4/id must include recent_coordinator_session_missing; json={body}"
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
    assert_eq!(
        body.pointer("/issues/0/id").and_then(Value::as_str),
        Some(TMUX_ENDPOINT_SOCKET_CONFLICT),
        "RFS RED: topology gate must reject the endpoint conflict itself. Bare pane-id collision text is insufficient and misleading when %0 exists on both sockets; json={body}"
    );
    assert_ne!(
        body.pointer("/issues/0/id").and_then(Value::as_str),
        Some(LEADER_PANE_ID_COLLIDES_WITH_AGENT),
        "RFS RED: same bare pane id on different sockets must not be reported as LeaderPaneIdCollidesWithAgent; compare transport-typed bindings instead; json={body}"
    );
}

#[test]
fn rfs_same_bare_pane_id_on_different_sockets_is_not_a_4_tuple_collision() {
    let team_id = "rfs006";
    let ws = TestWorkspace::new(team_id).with_fake_spec(&["a"]);
    let ws_path = ws.path().to_str().unwrap();
    let qs = quick_start_fake(&ws, team_id);
    assert!(quick_start_launched(&qs), "quick-start: {}", qs.stdout);
    let worker_socket = state_socket(&ws);
    let leader_socket = unique_socket("rfs006-leader");
    let _guard = TmuxSocketGuard::new(vec![worker_socket.clone(), leader_socket.clone()]);
    create_dummy_session(
        &leader_socket,
        "rfs006-leader-side",
        ws.path().to_path_buf(),
    );
    ws.mutate_state(|state| {
        state["tmux_endpoint"] = json!(worker_socket.clone());
        state["tmux_socket"] = json!(worker_socket.clone());
        state["leader_receiver"] = json!({
            "mode": "direct_tmux",
            "status": "attached",
            "pane_id": "%0",
            "tmux_socket": leader_socket,
            "session_name": "rfs006-leader-side",
            "window_name": "leader"
        });
    });

    let out = run_ta(&ws, &["diagnose", "--workspace", ws_path, "--json"]);
    let body = out.json();
    assert_ne!(
        body.pointer("/issues/0/id").and_then(Value::as_str),
        Some(LEADER_PANE_ID_COLLIDES_WITH_AGENT),
        "R5 guard: same bare %0 on different sockets is not a 4-tuple collision; diagnose must compare endpoint+session+window+pane_id, json={body}"
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
    seed_never_captured_no_session(&ws, "a");
    assert_never_captured_four_subconditions(&ws, "a");

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
        Some(STATUS_RESTARTED),
        "RFS no-session probe: expected status=restarted for never-captured no-session seat; json={body}"
    );
    assert_ne!(
        body.pointer("/reason").and_then(Value::as_str),
        Some(REASON_REFUSED_NO_SESSION_ID),
        "RFS no-session probe: refused_no_session_id is only valid when context exists to preserve; json={body}"
    );
}

#[test]
fn rfs_no_session_with_any_context_marker_still_refuses_without_allow_fresh() {
    for (tag, marker, value) in [
        (
            "first-send",
            "first_send_at",
            json!("2026-07-08T00:00:00+00:00"),
        ),
        (
            "last-result",
            "last_result_at",
            json!("2026-07-08T00:00:00+00:00"),
        ),
        ("task-delivered", "task_prompt_delivered", json!(true)),
    ] {
        let team_id = format!("rfs007{tag}");
        let ws = TestWorkspace::new(&team_id).with_fake_spec(&["a"]);
        let ws_path = ws.path().to_str().unwrap();
        let qs = quick_start_fake(&ws, &team_id);
        assert!(quick_start_launched(&qs), "quick-start: {}", qs.stdout);
        let _guard = TmuxSocketGuard::new(vec![state_socket(&ws)]);
        let _ = run_ta(
            &ws,
            &["shutdown", "--workspace", ws_path, "--keep-logs", "--json"],
        );
        seed_never_captured_no_session(&ws, "a");
        ws.mutate_agent_everywhere("a", |agent| {
            agent.insert(marker.to_string(), value.clone());
        });

        let out = run_ta(&ws, &["restart", ws_path, "--json"]);
        let body = out.json();
        assert_eq!(
            body.pointer("/ok").and_then(Value::as_bool),
            Some(false),
            "R7/R8 guard: no-session worker with context marker {marker} must refuse without --allow-fresh, not silently fresh; json={body}"
        );
        assert!(
            body.pointer("/reason").and_then(Value::as_str) == Some(REASON_REFUSED_NO_SESSION_ID)
                || body.pointer("/status").and_then(Value::as_str)
                    == Some(REASON_REFUSED_NO_SESSION_ID)
                || body.pointer("/status").and_then(Value::as_str) == Some(STATUS_RESUME_NOT_READY)
                || body.pointer("/issues/0/id").and_then(Value::as_str)
                    == Some(REASON_REFUSED_NO_SESSION_ID),
            "R7/R8 guard: refusal must name missing session context for marker {marker}; json={body}"
        );
    }
}

#[test]
fn rfs_source_guard_forbids_cross_socket_auto_rebind_and_keeps_resume_signature() {
    let restart_surface = source_tree(&["src/lifecycle/restart", "src/messaging"]);
    let forbidden_rebind = [
        "auto_rebind_cross_socket",
        "force_rebind_tmux_socket",
        "rewrite_leader_receiver_tmux_socket_on_restart",
    ]
    .into_iter()
    .filter(|needle| restart_surface.contains(needle))
    .collect::<Vec<_>>();
    assert!(
        forbidden_rebind.is_empty(),
        "R10 guard: restart must not grow an auto-rebind-across-socket escape hatch; forbidden markers={forbidden_rebind:?}"
    );

    let resume = source_tree(&["src/provider/session/resume.rs"]);
    let restart_common = source_tree(&["src/lifecycle/restart/common.rs"]);
    assert!(
        resume.contains("pub fn check(")
            && resume.contains("provider_can_resume: bool")
            && resume.contains("backing: Option<&ProviderBackingCheck>"),
        "R10 guard: provider/session/resume.rs signature must stay explicit about provider_can_resume and backing validation"
    );
    assert!(
        !restart_common.contains(".capture_session_id("),
        "R10 guard: restart refresh must not call capture_session_id(agent,cwd) one agent at a time"
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
    if let Some(parent) = Path::new(socket).parent() {
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

fn seed_never_captured_no_session(ws: &TestWorkspace, agent_id: &str) {
    ws.mutate_agent_everywhere(agent_id, |agent| {
        agent.insert("session_id".to_string(), Value::Null);
        agent.insert("rollout_path".to_string(), Value::Null);
        agent.insert("captured_at".to_string(), Value::Null);
        agent.insert("first_send_at".to_string(), Value::Null);
        agent.remove("last_result_at");
        agent.remove("task_prompt_delivered");
    });
}

fn assert_never_captured_four_subconditions(ws: &TestWorkspace, agent_id: &str) {
    let state = ws.read_state();
    let agent = state_agent(&state, agent_id);
    assert!(
        agent.get("session_id").is_none_or(Value::is_null),
        "R7 RED: never-captured auto-fresh readiness requires session_id=null; agent={agent}"
    );
    assert!(
        agent.get("rollout_path").is_none_or(Value::is_null),
        "R7 RED: never-captured auto-fresh readiness requires rollout_path=null; agent={agent}"
    );
    assert!(
        agent.get("first_send_at").is_none_or(Value::is_null),
        "R7 RED: never-captured auto-fresh readiness requires first_send_at=null; agent={agent}"
    );
    assert!(
        agent.get("last_result_at").is_none()
            && agent.get("task_prompt_delivered").is_none(),
        "R7 RED: never-captured auto-fresh readiness requires no result/task-delivered context markers; agent={agent}"
    );
}

fn read_events(ws: &TestWorkspace) -> Vec<Value> {
    std::fs::read_to_string(ws.events_jsonl_path())
        .unwrap_or_default()
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .collect()
}

fn event_index(events: &[Value], event_name: &str) -> Option<usize> {
    events
        .iter()
        .position(|event| event.get("event").and_then(Value::as_str) == Some(event_name))
}
