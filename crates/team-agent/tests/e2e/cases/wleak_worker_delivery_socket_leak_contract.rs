//! E2E-WLEAK 0.5.11 RED contracts: worker delivery must validate cached pane
//! ownership before injection.
//!
//! References:
//! - `.team/artifacts/worker-delivery-socket-leak-locate.md` §8 contracts 1-6.
//! - CR red-line spirit: pane id is physical metadata, not worker identity.
//!
//! User-visible contract: sending to worker `a`, or to a named leader binding,
//! can never land in another worker's pane just because a state pane id is stale.

use crate::framework::*;
use crate::support::source_walker::source_tree;
use crate::support::topology_issue_ids::WORKER_PANE_BINDING_STALE;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

const STATUS_QUEUED_PANE_MISSING: &str = "queued_pane_missing";
const RESOLVED_FROM_SESSION_WINDOW_LOOKUP: &str = "session_window_lookup";
const TARGET_KIND_PANE: &str = "pane";

#[test]
fn wleak_cached_pane_owned_by_other_window_never_receives_worker_message() {
    let team_id = "wleak001";
    let ws = TestWorkspace::new(team_id).with_fake_spec(&["a", "b"]);
    let ws_path = ws.path().to_str().unwrap();
    let qs = quick_start_fake(&ws, team_id);
    assert!(quick_start_launched(&qs), "quick-start: {}", qs.stdout);
    let _guard = TmuxServerGuard::for_workspace(&ws);

    let session = worker_session_name(team_id);
    let pane_a = pane_for_window(&ws, &session, "a");
    let pane_b = pane_for_window(&ws, &session, "b");
    write_agent_pane_tuple(&ws, "a", &pane_b);

    let token = "WLEAK_WRONG_WINDOW_TOKEN_001";
    let out = run_ta(&ws, &["send", "a", token, "--workspace", ws_path, "--json"]);
    assert!(
        out.is_success(),
        "send should complete before target ownership assertions; stdout={} stderr={}",
        out.stdout,
        out.stderr
    );
    let out_json = out.json();
    let mid = out_json
        .pointer("/message_id")
        .and_then(Value::as_str)
        .expect("generated message id");
    wait_for_or_panic(
        "message reaches terminal status",
        || {
            matches!(
                message_status(&ws, mid).as_deref(),
                Some("delivered" | "queued_pane_missing")
            )
        },
        Duration::from_secs(6),
    );

    let a_capture = capture_pane(&ws, pane_a.pane_id.as_str());
    let b_capture = capture_pane(&ws, pane_b.pane_id.as_str());
    assert!(
        a_capture.contains(token),
        "WLEAK RED: stale cached pane must be ignored when the intended session/window is live; expected token in worker a pane {} but capture was:\n{}",
        pane_a.pane_id,
        a_capture
    );
    assert!(
        !b_capture.contains(token),
        "WLEAK RED: delivery selected cached pane {} owned by window b instead of resolving {}:a; wrong pane capture:\n{}",
        pane_b.pane_id,
        session,
        b_capture
    );
    let event = delivered_event(&ws, mid).expect("message.delivered event");
    assert_eq!(
        event.get("target_window").and_then(Value::as_str),
        Some("a"),
        "WLEAK RED: even when delivery reaches the intended worker, the delivered event must prove the physical target window instead of hiding stale-cache provenance; event={event}"
    );
    assert_eq!(
        event.get("resolved_from").and_then(Value::as_str),
        Some("session_window_lookup"),
        "WLEAK RED: stale cached pane should be bypassed and the event should record resolved_from=session_window_lookup; event={event}"
    );
    assert_target_tuple(&event, &state_socket(&ws), &session, "a", &pane_a, "W1/W2");
}

#[test]
fn wleak_wrong_cached_pane_and_missing_intended_window_blocks_not_delivers() {
    let team_id = "wleak002";
    let ws = TestWorkspace::new(team_id).with_fake_spec(&["a", "b"]);
    let ws_path = ws.path().to_str().unwrap();
    let qs = quick_start_fake(&ws, team_id);
    assert!(quick_start_launched(&qs), "quick-start: {}", qs.stdout);
    let _guard = TmuxServerGuard::for_workspace(&ws);

    let session = worker_session_name(team_id);
    let pane_b = pane_for_window(&ws, &session, "b");
    kill_window(&ws, &session, "a");
    write_agent_pane_tuple(&ws, "a", &pane_b);

    let token = "WLEAK_MISSING_TARGET_TOKEN_002";
    let out = run_ta(&ws, &["send", "a", token, "--workspace", ws_path, "--json"]);
    let body = out.json();
    assert_eq!(
        body.pointer("/ok").and_then(Value::as_bool),
        Some(false),
        "WLEAK RED: missing intended worker window with wrong live cached pane must fail closed, not report delivered; json={body}"
    );
    assert_eq!(
        body.pointer("/message_status").and_then(Value::as_str),
        Some(STATUS_QUEUED_PANE_MISSING),
        "WLEAK RED: wrong cached pane must route to the existing missing-target blocker; json={body}"
    );
    assert!(
        !capture_pane(&ws, pane_b.pane_id.as_str()).contains(token),
        "WLEAK RED: when a's intended window is absent, delivery must not inject into b via stale cached pane"
    );
}

#[test]
fn wleak_stale_worker_block_persists_row_inbox_and_replays_after_start_agent() {
    let team_id = "wleak009";
    let ws = TestWorkspace::new(team_id).with_fake_spec(&["a"]);
    let ws_path = ws.path().to_str().unwrap();
    let qs = quick_start_fake(&ws, team_id);
    assert!(quick_start_launched(&qs), "quick-start: {}", qs.stdout);
    let _guard = TmuxServerGuard::for_workspace(&ws);

    let socket = state_socket(&ws);
    let intended_session = worker_session_name(team_id);
    kill_window(&ws, &intended_session, "a");
    let foreign_ws = TestWorkspace::new("wleak009-foreign").with_fake_spec(&["worker"]);
    let foreign = create_foreign_worker_session(
        &socket,
        "foreign-team-wleak009",
        "foreign-worker",
        foreign_ws.path().to_path_buf(),
    );
    write_agent_pane_tuple(&ws, "a", &foreign);

    let token = "WLEAK_STALE_REPLAY_TOKEN_009";
    let out = run_ta(&ws, &["send", "a", token, "--workspace", ws_path, "--json"]);
    let body = out.json();
    let mid = body
        .pointer("/message_id")
        .and_then(Value::as_str)
        .expect("B replay RED: stale target blocker must return the accepted message id");
    assert_eq!(
        body.pointer("/message_status").and_then(Value::as_str),
        Some(STATUS_QUEUED_PANE_MISSING),
        "B replay RED: stale target must use the repairable queued_pane_missing status; json={body}"
    );
    assert_eq!(
        capture_pane(&ws, &foreign.pane_id).matches(token).count(),
        0,
        "B replay RED: stale target blocker must not inject into the reused foreign pane"
    );

    let before = message_row(&ws, mid).expect("B replay RED: blocked message row exists");
    assert_eq!(before.status, STATUS_QUEUED_PANE_MISSING);
    assert_eq!(before.error.as_deref(), Some("tmux_target_missing"));

    let inbox_json = run_ta(&ws, &["inbox", "a", "--workspace", ws_path, "--json"]);
    assert!(
        inbox_json.is_success(),
        "B replay RED: blocked row must be visible in inbox; stdout={} stderr={}",
        inbox_json.stdout,
        inbox_json.stderr
    );
    let inbox = inbox_json.json();
    assert_eq!(
        inbox
            .pointer("/messages/0/message_id")
            .and_then(Value::as_str),
        Some(mid),
        "B replay RED: inbox must show the blocked message row; json={inbox}"
    );
    assert_eq!(
        inbox.pointer("/messages/0/status").and_then(Value::as_str),
        Some(STATUS_QUEUED_PANE_MISSING),
        "B replay RED: inbox must preserve the repairable status; json={inbox}"
    );

    let start = run_ta(
        &ws,
        &[
            "start-agent",
            "a",
            "--workspace",
            ws_path,
            "--allow-fresh",
            "--no-display",
            "--json",
        ],
    );
    assert!(
        start.is_success(),
        "start-agent exit {}; stdout={} stderr={}",
        start.exit_code,
        start.stdout,
        start.stderr
    );

    wait_for_or_panic(
        "same stale-blocked message id delivered after worker repair",
        || message_status(&ws, mid).as_deref() == Some("delivered"),
        Duration::from_secs(8),
    );
    std::thread::sleep(Duration::from_millis(700));
    let after = message_row(&ws, mid).expect("delivered message row exists");
    assert_eq!(after.status, "delivered");
    assert_eq!(
        after.delivery_attempts,
        before.delivery_attempts + 1,
        "B replay RED: start-agent repair must replay the same row exactly once"
    );
    assert_eq!(delivered_event_count(&ws, mid), 1);
    assert_eq!(event_count(&ws, "turn_open.armed_after_delivery", mid), 1);
    assert_eq!(
        capture_pane(&ws, &foreign.pane_id).matches(token).count(),
        0,
        "B replay RED: repaired replay must still never inject into the reused foreign pane"
    );
}

#[test]
fn wleak_message_delivered_event_records_physical_target_metadata() {
    let team_id = "wleak003";
    let ws = TestWorkspace::new(team_id).with_fake_spec(&["a"]);
    let ws_path = ws.path().to_str().unwrap();
    let qs = quick_start_fake(&ws, team_id);
    assert!(quick_start_launched(&qs), "quick-start: {}", qs.stdout);
    let _guard = TmuxServerGuard::for_workspace(&ws);
    let session = worker_session_name(team_id);
    let pane_a = pane_for_window(&ws, &session, "a");

    let out = run_ta(
        &ws,
        &[
            "send",
            "a",
            "WLEAK_EVENT_METADATA_TOKEN_003",
            "--workspace",
            ws_path,
            "--json",
        ],
    );
    assert!(
        out.is_success(),
        "send stdout={} stderr={}",
        out.stdout,
        out.stderr
    );
    let out_json = out.json();
    let mid = out_json
        .pointer("/message_id")
        .and_then(Value::as_str)
        .expect("generated message id");
    wait_for_or_panic(
        "message.delivered event",
        || delivered_event(&ws, mid).is_some(),
        Duration::from_secs(6),
    );
    let event = delivered_event(&ws, mid).expect("message.delivered event");
    let missing = [
        "target_kind",
        "tmux_endpoint",
        "target_session",
        "target_window",
        "target_pane_id",
        "target_pane_pid",
        "resolved_from",
    ]
    .into_iter()
    .filter(|key| event.get(*key).is_none())
    .collect::<Vec<_>>();
    assert!(
        missing.is_empty(),
        "WLEAK RED: message.delivered must include physical target provenance; missing={missing:?}; event={event}"
    );
    assert_target_tuple(&event, &state_socket(&ws), &session, "a", &pane_a, "W1");
}

#[test]
fn wleak_unvalidated_cached_pane_never_marks_delivered() {
    let team_id = "wleak006";
    let ws = TestWorkspace::new(team_id).with_fake_spec(&["a", "b"]);
    let ws_path = ws.path().to_str().unwrap();
    let qs = quick_start_fake(&ws, team_id);
    assert!(quick_start_launched(&qs), "quick-start: {}", qs.stdout);
    let _guard = TmuxServerGuard::for_workspace(&ws);

    let session = worker_session_name(team_id);
    let pane_b = pane_for_window(&ws, &session, "b");
    kill_window(&ws, &session, "a");
    write_agent_pane_tuple(&ws, "a", &pane_b);

    let out = run_ta(
        &ws,
        &[
            "send",
            "a",
            "WLEAK_UNVALIDATED_TOKEN_006",
            "--workspace",
            ws_path,
            "--json",
        ],
    );
    let body = out.json();
    assert_ne!(
        body.pointer("/reason").and_then(Value::as_str),
        Some("coordinator_unavailable"),
        "W3 RED: unvalidated-pane provenance contract must reach delivery, not pass because coordinator gate rejected the send; json={body}"
    );
    assert_eq!(
        body.pointer("/delivered").and_then(Value::as_bool),
        Some(false),
        "W3 RED: unvalidated cached pane must not surface delivered=true; json={body}"
    );
    let mid = body
        .pointer("/message_id")
        .and_then(Value::as_str)
        .expect("generated message id");
    let delivered_count = delivered_event_count(&ws, mid);
    assert_eq!(
        delivered_count, 0,
        "W3 RED: unvalidated cached pane must not emit message.delivered target provenance; count={delivered_count}"
    );
    assert_ne!(
        message_status(&ws, mid).as_deref(),
        Some("delivered"),
        "W3 RED: unvalidated cached pane must not mark the DB row delivered"
    );
}

#[test]
fn wleak_cross_session_multi_pane_window_missing_fails_closed() {
    let team_id = "wleak007";
    let ws = TestWorkspace::new(team_id).with_fake_spec(&["a"]);
    let ws_path = ws.path().to_str().unwrap();
    let qs = quick_start_fake(&ws, team_id);
    assert!(quick_start_launched(&qs), "quick-start: {}", qs.stdout);
    let _guard = TmuxServerGuard::for_workspace(&ws);

    let socket = state_socket(&ws);
    let intended_session = worker_session_name(team_id);
    kill_window(&ws, &intended_session, "a");
    let foreign = create_foreign_multi_pane_session(
        &socket,
        "team-wleak007-foreign",
        "a",
        ws.path().to_path_buf(),
    );
    write_agent_pane_tuple(&ws, "a", &foreign);

    let token = "WLEAK_CROSS_SESSION_TOKEN_007";
    let out = run_ta(&ws, &["send", "a", token, "--workspace", ws_path, "--json"]);
    let body = out.json();
    assert_eq!(
        body.pointer("/message_status").and_then(Value::as_str),
        Some(STATUS_QUEUED_PANE_MISSING),
        "W4/W5 RED: cached pane in another session/team, even with multiple panes and matching window name, must fail closed as window missing; json={body}"
    );
    assert!(
        !capture_pane(&ws, &foreign.pane_id).contains(token),
        "W4/W5 RED: cross-session/cross-team cached pane must not receive window-missing worker traffic"
    );
}

#[test]
fn wleak_to_name_leader_reused_pane_id_must_not_inject_foreign_worker() {
    let team_id = "wleak008";
    let target_ws = TestWorkspace::new(team_id).with_fake_spec(&["a"]);
    let target_ws_path = target_ws.path().to_str().unwrap();
    let qs = quick_start_fake(&target_ws, team_id);
    assert!(quick_start_launched(&qs), "quick-start: {}", qs.stdout);
    let _guard = TmuxServerGuard::for_workspace(&target_ws);

    let foreign_ws = TestWorkspace::new("wleak008-foreign").with_fake_spec(&["worker"]);
    let socket = state_socket(&target_ws);
    let foreign_session = "team-wleak008-foreign";
    let foreign_window = "foreign-worker";
    let foreign = create_foreign_worker_session(
        &socket,
        foreign_session,
        foreign_window,
        foreign_ws.path().to_path_buf(),
    );
    write_leader_receiver_tuple(
        &target_ws,
        team_id,
        json!({
            "mode": "direct_tmux",
            "transport_kind": "direct_tmux",
            "status": "attached",
            "pane_id": foreign.pane_id,
            "pane_pid": 1,
            "session_name": "team-wleak008-dead-leader",
            "window_name": "dead-leader",
            "tmux_socket": socket,
            "pane_current_path": target_ws_path,
        }),
    );

    let token = "WLEAK_TONAME_REUSED_LEADER_TOKEN_008";
    let to_name = format!("{target_ws_path}::{team_id}/leader");
    let foreign_ws_path = foreign_ws.path().to_str().unwrap();
    let out = run_ta_env(
        &foreign_ws,
        &[
            "send",
            "--to-name",
            &to_name,
            token,
            "--workspace",
            foreign_ws_path,
            "--json",
        ],
        &[("TEAM_AGENT_ID", "third-party")],
    );
    let body = out.json();
    let foreign_capture = capture_pane(&target_ws, &foreign.pane_id);
    let leaked_count = foreign_capture.matches(token).count();
    let acceptable = named_leader_stale_or_mailbox_outcome(&target_ws, &body);
    assert!(
        acceptable && leaked_count == 0,
        "WLEAK fifth-case RED: --to-name {to_name} must stale-refuse or E6-queue when \
         state leader_receiver pane_id is live only as another workspace's worker \
         ({foreign_session}:{foreign_window}); it must never report delivered or inject into \
         the reused pane. acceptable={acceptable} reused_pane_token_count={leaked_count} \
         json={body} reused_pane_capture=\n{foreign_capture}"
    );
}

#[test]
fn wleak_start_agent_noop_refreshes_stale_cached_pane_tuple() {
    let team_id = "wleak004";
    let ws = TestWorkspace::new(team_id).with_fake_spec(&["a", "b"]);
    let ws_path = ws.path().to_str().unwrap();
    let qs = quick_start_fake(&ws, team_id);
    assert!(quick_start_launched(&qs), "quick-start: {}", qs.stdout);
    let _guard = TmuxServerGuard::for_workspace(&ws);

    let session = worker_session_name(team_id);
    let pane_a = pane_for_window(&ws, &session, "a");
    let pane_b = pane_for_window(&ws, &session, "b");
    write_agent_pane_tuple(&ws, "a", &pane_b);

    let start = run_ta(
        &ws,
        &[
            "start-agent",
            "a",
            "--workspace",
            ws_path,
            "--no-display",
            "--json",
        ],
    );
    assert!(
        start.is_success(),
        "start-agent should succeed before tuple refresh assertion; stdout={} stderr={}",
        start.stdout,
        start.stderr
    );
    let state = ws.read_state();
    let agent = state_agent(&state, "a");
    assert_eq!(
        agent.get("pane_id").and_then(Value::as_str),
        Some(pane_a.pane_id.as_str()),
        "WLEAK RED: start-agent noop must refresh stale pane_id from live {session}:a, not keep b's cached pane; agent={agent}"
    );
    assert_eq!(
        agent.get("pane_pid").and_then(Value::as_i64),
        Some(pane_a.pane_pid),
        "WLEAK RED: start-agent noop must refresh pane_pid together with pane_id; agent={agent}"
    );
    assert!(
        events_contain(&ws, "agent_pane_binding_refreshed"),
        "WLEAK RED: refreshing a stale cached worker pane must emit agent_pane_binding_refreshed"
    );
}

#[test]
fn wleak_diagnose_exposes_stale_worker_pane_binding() {
    let team_id = "wleak005";
    let ws = TestWorkspace::new(team_id).with_fake_spec(&["a", "b"]);
    let ws_path = ws.path().to_str().unwrap();
    let qs = quick_start_fake(&ws, team_id);
    assert!(quick_start_launched(&qs), "quick-start: {}", qs.stdout);
    let _guard = TmuxServerGuard::for_workspace(&ws);

    let session = worker_session_name(team_id);
    let pane_b = pane_for_window(&ws, &session, "b");
    write_agent_pane_tuple(&ws, "a", &pane_b);

    let out = run_ta(&ws, &["diagnose", "--workspace", ws_path, "--json"]);
    let body = out.json();
    assert_eq!(
        body.pointer("/ok").and_then(Value::as_bool),
        Some(false),
        "WLEAK RED: diagnose must fail dirty when worker cached pane belongs to another window; json={body}"
    );
    assert!(
        body.pointer("/issues/0/id").and_then(Value::as_str) == Some(WORKER_PANE_BINDING_STALE),
        "WLEAK RED: diagnose issue /issues/0/id must be worker_pane_binding_stale; json={body}"
    );
    assert!(
        body.pointer("/issues/0/expected_session")
            .and_then(Value::as_str)
            .is_some(),
        "WLEAK RED: diagnose stale worker pane issue must expose expected_session via JSON pointer; json={body}"
    );
    assert!(
        body.pointer("/issues/0/observed_window")
            .and_then(Value::as_str)
            .is_some(),
        "WLEAK RED: diagnose stale worker pane issue must expose observed_window via JSON pointer; json={body}"
    );
}

#[test]
fn wleak_source_guard_does_not_cross_socket_enumerate_for_worker_delivery() {
    let messaging = source_tree(&["src/messaging"]);
    let forbidden = [
        "tmux_socket_roots",
        "probes_real_tmux_socket_roots",
        "list-sessions",
        "tmux_socket_name_transport",
        "for_tmux_endpoint",
    ]
    .into_iter()
    .filter(|needle| messaging.contains(needle))
    .collect::<Vec<_>>();
    assert!(
        forbidden.is_empty(),
        "W6 guard: worker delivery path must not enumerate/search across tmux sockets to recover a pane; forbidden markers={forbidden:?}"
    );
}

#[test]
fn wleak_source_guard_keeps_send_status_semantics_honest() {
    let send = source_tree(&["src/cli/send.rs"]);
    let delivery_surface = source_tree(&["src/messaging", "src/cli/send.rs"]);
    for required in [
        "\"delivery_status\"",
        "\"delivered\"",
        "delivery_proven",
        "DeliveryStatus::Delivered",
    ] {
        assert!(
            send.contains(required),
            "W7 guard: send JSON must keep honest delivery_status/delivered semantics instead of weakening blocked stale-target sends to accepted/queued prose; missing {required}"
        );
    }
    let stale_to_queued = format!("{WORKER_PANE_BINDING_STALE}\") => \"queued\"");
    assert!(
        !delivery_surface.contains(&stale_to_queued),
        "W7 guard: stale worker pane binding must not be weakened to queued/accepted success copy"
    );
}

#[derive(Debug, Clone)]
struct PaneSnapshot {
    pane_id: String,
    pane_pid: i64,
}

struct TmuxServerGuard {
    socket: String,
}

impl TmuxServerGuard {
    fn for_workspace(ws: &TestWorkspace) -> Self {
        let socket = state_socket(ws);
        assert!(
            socket.contains("/ta-"),
            "test must only guard a private team-agent tmux socket, got {socket:?}"
        );
        Self { socket }
    }
}

impl Drop for TmuxServerGuard {
    fn drop(&mut self) {
        let _ = Command::new("tmux")
            .args(["-S", &self.socket, "kill-server"])
            .output();
    }
}

fn write_agent_pane_tuple(ws: &TestWorkspace, agent_id: &str, pane: &PaneSnapshot) {
    ws.mutate_agent_everywhere(agent_id, |agent| {
        agent.insert("pane_id".to_string(), json!(pane.pane_id));
        agent.insert("pane_pid".to_string(), json!(pane.pane_pid));
    });
}

fn write_leader_receiver_tuple(ws: &TestWorkspace, team_key: &str, receiver: Value) {
    ws.mutate_state(|state| {
        if let Some(root) = state.as_object_mut() {
            root.insert("leader_receiver".to_string(), receiver.clone());
        }
        if let Some(team) = state
            .get_mut("teams")
            .and_then(Value::as_object_mut)
            .and_then(|teams| teams.get_mut(team_key))
            .and_then(Value::as_object_mut)
        {
            team.insert("leader_receiver".to_string(), receiver);
        }
    });
}

fn pane_for_window(ws: &TestWorkspace, session: &str, window: &str) -> PaneSnapshot {
    let socket = state_socket(ws);
    let out = Command::new("tmux")
        .args([
            "-S",
            &socket,
            "list-panes",
            "-t",
            &format!("{session}:{window}"),
            "-F",
            "#{pane_id}|#{pane_pid}",
        ])
        .output()
        .unwrap_or_else(|e| panic!("tmux list-panes {session}:{window}: {e}"));
    assert!(
        out.status.success(),
        "tmux list-panes failed for {session}:{window}; stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let raw = String::from_utf8_lossy(&out.stdout);
    let line = raw
        .lines()
        .next()
        .unwrap_or_else(|| panic!("no pane for {session}:{window}; output={raw:?}"));
    let (pane_id, pane_pid) = line
        .split_once('|')
        .unwrap_or_else(|| panic!("bad pane line {line:?}"));
    PaneSnapshot {
        pane_id: pane_id.to_string(),
        pane_pid: pane_pid
            .parse()
            .unwrap_or_else(|e| panic!("parse pane_pid {pane_pid:?}: {e}")),
    }
}

fn kill_window(ws: &TestWorkspace, session: &str, window: &str) {
    let socket = state_socket(ws);
    let _ = Command::new("tmux")
        .args([
            "-S",
            &socket,
            "kill-window",
            "-t",
            &format!("{session}:{window}"),
        ])
        .output();
}

fn capture_pane(ws: &TestWorkspace, pane_id: &str) -> String {
    let socket = state_socket(ws);
    let out = Command::new("tmux")
        .args(["-S", &socket, "capture-pane", "-t", pane_id, "-p"])
        .output()
        .unwrap_or_else(|e| panic!("tmux capture-pane {pane_id}: {e}"));
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn create_foreign_worker_session(
    socket: &str,
    session: &str,
    window: &str,
    cwd: PathBuf,
) -> PaneSnapshot {
    let out = Command::new("tmux")
        .args([
            "-S",
            socket,
            "new-session",
            "-d",
            "-s",
            session,
            "-n",
            window,
            "-c",
            cwd.to_str().expect("cwd utf8"),
            "cat",
        ])
        .output()
        .unwrap_or_else(|e| panic!("tmux new-session {session}: {e}"));
    assert!(
        out.status.success(),
        "tmux new-session failed; stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    pane_for_explicit_socket(socket, session, window)
}

fn create_foreign_multi_pane_session(
    socket: &str,
    session: &str,
    window: &str,
    cwd: PathBuf,
) -> PaneSnapshot {
    let out = Command::new("tmux")
        .args([
            "-S",
            socket,
            "new-session",
            "-d",
            "-s",
            session,
            "-n",
            window,
            "-c",
            cwd.to_str().expect("cwd utf8"),
            "sleep 600",
        ])
        .output()
        .unwrap_or_else(|e| panic!("tmux new-session {session}: {e}"));
    assert!(
        out.status.success(),
        "tmux new-session failed; stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let split = Command::new("tmux")
        .args([
            "-S",
            socket,
            "split-window",
            "-t",
            &format!("{session}:{window}"),
            "sleep 600",
        ])
        .output()
        .unwrap_or_else(|e| panic!("tmux split-window {session}:{window}: {e}"));
    assert!(
        split.status.success(),
        "tmux split-window failed; stdout={} stderr={}",
        String::from_utf8_lossy(&split.stdout),
        String::from_utf8_lossy(&split.stderr)
    );

    pane_for_explicit_socket(socket, session, window)
}

fn pane_for_explicit_socket(socket: &str, session: &str, window: &str) -> PaneSnapshot {
    let out = Command::new("tmux")
        .args([
            "-S",
            socket,
            "list-panes",
            "-t",
            &format!("{session}:{window}"),
            "-F",
            "#{pane_id}|#{pane_pid}",
        ])
        .output()
        .unwrap_or_else(|e| panic!("tmux list-panes {session}:{window}: {e}"));
    assert!(
        out.status.success(),
        "tmux list-panes failed for {session}:{window}; stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let raw = String::from_utf8_lossy(&out.stdout);
    let line = raw
        .lines()
        .next()
        .unwrap_or_else(|| panic!("no pane for {session}:{window}; output={raw:?}"));
    parse_pane_snapshot_line(line)
}

fn assert_target_tuple(
    event: &Value,
    endpoint: &str,
    session: &str,
    window: &str,
    pane: &PaneSnapshot,
    label: &str,
) {
    assert_eq!(
        event.get("target_kind").and_then(Value::as_str),
        Some(TARGET_KIND_PANE),
        "{label} RED: delivered event target_kind must be pane; event={event}"
    );
    assert_eq!(
        event.get("tmux_endpoint").and_then(Value::as_str),
        Some(endpoint),
        "{label} RED: delivered event tmux_endpoint must be the canonical worker endpoint; event={event}"
    );
    assert_eq!(
        event.get("target_session").and_then(Value::as_str),
        Some(session),
        "{label} RED: delivered event target_session mismatch; event={event}"
    );
    assert_eq!(
        event.get("target_window").and_then(Value::as_str),
        Some(window),
        "{label} RED: delivered event target_window mismatch; event={event}"
    );
    assert_eq!(
        event.get("target_pane_id").and_then(Value::as_str),
        Some(pane.pane_id.as_str()),
        "{label} RED: delivered event target_pane_id mismatch; event={event}"
    );
    assert_eq!(
        event.get("target_pane_pid").and_then(Value::as_i64),
        Some(pane.pane_pid),
        "{label} RED: pane_pid is a hard ownership field and must match the physical target; event={event}"
    );
    assert_eq!(
        event.get("resolved_from").and_then(Value::as_str),
        Some(RESOLVED_FROM_SESSION_WINDOW_LOOKUP),
        "{label} RED: stale cache bypass must record resolved_from=session_window_lookup; event={event}"
    );
}

fn state_socket(ws: &TestWorkspace) -> String {
    ws.read_state()
        .get("tmux_socket")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn parse_pane_snapshot_line(line: &str) -> PaneSnapshot {
    let (pane_id, pane_pid) = line
        .split_once('|')
        .unwrap_or_else(|| panic!("bad pane line {line:?}"));
    PaneSnapshot {
        pane_id: pane_id.to_string(),
        pane_pid: pane_pid
            .parse()
            .unwrap_or_else(|e| panic!("parse pane_pid {pane_pid:?}: {e}")),
    }
}

struct MessageRow {
    status: String,
    error: Option<String>,
    delivery_attempts: i64,
}

fn message_status(ws: &TestWorkspace, message_id: &str) -> Option<String> {
    message_row(ws, message_id).map(|row| row.status)
}

fn message_row(ws: &TestWorkspace, message_id: &str) -> Option<MessageRow> {
    let db = ws.path().join(".team/runtime/team.db");
    let conn = rusqlite::Connection::open(db).ok()?;
    conn.query_row(
        "select status, error, delivery_attempts from messages where message_id = ?1",
        [message_id],
        |row| {
            Ok(MessageRow {
                status: row.get(0)?,
                error: row.get(1)?,
                delivery_attempts: row.get(2)?,
            })
        },
    )
    .ok()
}

fn delivered_event(ws: &TestWorkspace, message_id: &str) -> Option<Value> {
    read_events(ws).into_iter().find(|entry| {
        entry.get("event").and_then(Value::as_str) == Some("message.delivered")
            && entry.get("message_id").and_then(Value::as_str) == Some(message_id)
    })
}

fn delivered_event_count(ws: &TestWorkspace, message_id: &str) -> usize {
    event_count(ws, "message.delivered", message_id)
}

fn event_count(ws: &TestWorkspace, event: &str, message_id: &str) -> usize {
    read_events(ws)
        .into_iter()
        .filter(|entry| {
            entry.get("event").and_then(Value::as_str) == Some(event)
                && entry.get("message_id").and_then(Value::as_str) == Some(message_id)
        })
        .count()
}

fn named_leader_stale_or_mailbox_outcome(ws: &TestWorkspace, body: &Value) -> bool {
    if body.pointer("/delivered").and_then(Value::as_bool) == Some(true)
        || body.pointer("/delivery_status").and_then(Value::as_str) == Some("delivered")
        || body.pointer("/status").and_then(Value::as_str) == Some("delivered")
    {
        return false;
    }
    if body.pointer("/message_status").and_then(Value::as_str) == Some("queued_until_leader_attach")
        || body.pointer("/status").and_then(Value::as_str) == Some("queued_until_leader_attach")
    {
        let Some(message_id) = body.pointer("/message_id").and_then(Value::as_str) else {
            return false;
        };
        return message_status(ws, message_id).as_deref() == Some("queued_until_leader_attach");
    }
    if body.pointer("/ok").and_then(Value::as_bool) != Some(false)
        || body.pointer("/status").and_then(Value::as_str) != Some("refused")
    {
        return false;
    }
    let reason = body
        .pointer("/reason")
        .and_then(Value::as_str)
        .unwrap_or("");
    let log = body.pointer("/log").and_then(Value::as_str).unwrap_or("");
    reason.contains("stale")
        || reason == "name_not_live"
        || reason.contains("not_live")
        || log.contains("stale")
}

fn events_contain(ws: &TestWorkspace, event: &str) -> bool {
    read_events(ws)
        .iter()
        .any(|entry| entry.get("event").and_then(Value::as_str) == Some(event))
}

fn read_events(ws: &TestWorkspace) -> Vec<Value> {
    std::fs::read_to_string(ws.events_jsonl_path())
        .unwrap_or_default()
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .collect()
}
