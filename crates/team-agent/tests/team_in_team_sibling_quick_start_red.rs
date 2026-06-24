//! Team-in-team sibling quick-start contracts.
//!
//! #241: two team directories under the same parent workspace must be independent teams. An
//! existing runtime for `teamA` must not make `quick-start teamB` return `ExistingRuntime` before
//! reaching the existing CR-040/042 per-team state/session merge path.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use rusqlite::params;
use serial_test::{file_serial, serial};
use serde_json::Value;
use team_agent::event_log::EventLog;
use team_agent::lifecycle::{quick_start_with_transport, QuickStartReport};
use team_agent::message_store::MessageStore;
use team_agent::messaging::deliver_pending_messages;
use team_agent::state::persist::load_runtime_state;
use team_agent::tmux_backend::TmuxBackend;
use team_agent::transport::{
    AttachOutcome, BackendKind, CaptureRange, CapturedText, InjectPayload, InjectReport,
    InjectStage, InjectVerification, Key, PaneField, PaneId, PaneInfo, SessionName,
    SetEnvOutcome, SpawnResult, SubmitVerification, Target, Transport, TransportError,
    TurnVerification, WindowName,
};

#[test]
#[ignore = "real-machine: quick-start/session lifecycle gate"]
#[serial(env)]
fn quick_start_sibling_teamdir_in_same_workspace_starts_new_team_not_existing_runtime() {
    let _env = EnvGuard::unset([
        "TMUX",
        "TMUX_PANE",
        "TEAM_AGENT_ID",
        "TEAM_AGENT_TEAM_ID",
        "TEAM_AGENT_LEADER_PANE_ID",
        "TEAM_AGENT_LEADER_SESSION_UUID",
        "TEAM_AGENT_LEADER_SESSION_UUID_OVERRIDE",
        "TEAM_AGENT_LEADER_PROVIDER",
    ]);
    let root = tmp_dir("sibling-teamdirs");
    seed_healthy_coordinator(&root);
    let team_a = team_dir(&root, "teamA", "worker_a");
    let team_b = team_dir(&root, "teamB", "worker_b");
    let transport = SessionRecordingTransport::default();

    let first = quick_start_with_transport(
        &team_a,
        Some("teamA"),
        true,
        Some("teamA"),
        &transport,
    )
    .expect("fixture: first teamA quick-start should succeed");
    assert_ready_team("teamA first quick-start", &first, "team-teamA");

    let second = quick_start_with_transport(
        &team_b,
        Some("teamB"),
        true,
        Some("teamB"),
        &transport,
    )
    .expect("teamB quick-start must not be refused by teamA's runtime");

    assert_ready_team(
        "sibling teamB quick-start",
        &second,
        "team-teamB",
    );
    let state = load_runtime_state(&root).expect("state.json should exist in shared parent workspace");
    assert_team_present(&state, "teamA", "team-teamA");
    assert_team_present(&state, "teamB", "team-teamB");
    assert_eq!(
        state.get("active_team_key").and_then(Value::as_str),
        Some("teamB"),
        "the newly launched sibling team should become the active top-level projection; state={state}"
    );
    let sessions = transport.spawned_sessions();
    assert!(
        sessions.iter().any(|session| session == "team-teamA")
            && sessions.iter().any(|session| session == "team-teamB"),
        "sibling teamdirs must spawn independent tmux sessions derived from requested team identity; sessions={sessions:?}"
    );
}

#[test]
#[ignore = "real-machine: quick-start/session lifecycle gate"]
#[serial(env)]
fn quick_start_sibling_teamdir_without_team_arg_infers_compiled_spec_name() {
    let _env = EnvGuard::unset([
        "TMUX",
        "TMUX_PANE",
        "TEAM_AGENT_ID",
        "TEAM_AGENT_TEAM_ID",
        "TEAM_AGENT_LEADER_PANE_ID",
        "TEAM_AGENT_LEADER_SESSION_UUID",
        "TEAM_AGENT_LEADER_SESSION_UUID_OVERRIDE",
        "TEAM_AGENT_LEADER_PROVIDER",
    ]);
    let root = tmp_dir("sibling-no-team-infers-spec-name");
    seed_healthy_coordinator(&root);
    let team_a = team_dir(&root, "teamA", "worker_a");
    let team_b = team_dir(&root, "teamB", "worker_b");
    let transport = SessionRecordingTransport::default();

    let first = quick_start_with_transport(
        &team_a,
        Some("teamA"),
        true,
        Some("teamA"),
        &transport,
    )
    .expect("fixture: first teamA quick-start should succeed");
    assert_ready_team("teamA first quick-start", &first, "team-teamA");

    let second = quick_start_with_transport(
        &team_b,
        None,
        true,
        None,
        &transport,
    )
    .expect("quick-start teamB without --team should still use TEAM.md name=teamB");

    assert_ready_team(
        "sibling teamB quick-start without --team",
        &second,
        "team-teamB",
    );
    let state = load_runtime_state(&root).expect("state.json should exist in shared parent workspace");
    assert_team_present(&state, "teamA", "team-teamA");
    assert_team_present(&state, "teamB", "team-teamB");
    assert_eq!(
        state.get("active_team_key").and_then(Value::as_str),
        Some("teamB"),
        "quick-start <teamBdir> without --team must infer compiled spec name=teamB and activate that sibling team; state={state}"
    );
    let sessions = transport.spawned_sessions();
    assert!(
        sessions.iter().any(|session| session == "team-teamA")
            && sessions.iter().any(|session| session == "team-teamB"),
        "missing --team must not collapse sibling teamB into teamA's existing runtime; sessions={sessions:?}"
    );
}

#[test]
#[ignore = "real-machine: quick-start/session lifecycle gate"]
#[serial(env)]
fn quick_start_sibling_teamdir_without_team_arg_same_spec_name_still_returns_existing_runtime() {
    let _env = EnvGuard::unset([
        "TMUX",
        "TMUX_PANE",
        "TEAM_AGENT_ID",
        "TEAM_AGENT_TEAM_ID",
        "TEAM_AGENT_LEADER_PANE_ID",
        "TEAM_AGENT_LEADER_SESSION_UUID",
        "TEAM_AGENT_LEADER_SESSION_UUID_OVERRIDE",
        "TEAM_AGENT_LEADER_PROVIDER",
    ]);
    let root = tmp_dir("sibling-no-team-same-spec-name");
    seed_healthy_coordinator(&root);
    let team_a = team_dir(&root, "teamA", "worker_a");
    let team_b_same_name = team_dir_with_name(&root, "teamB", "teamA", "worker_b");
    let transport = SessionRecordingTransport::default();

    let first = quick_start_with_transport(
        &team_a,
        Some("teamA"),
        true,
        Some("teamA"),
        &transport,
    )
    .expect("fixture: first teamA quick-start should succeed");
    assert_ready_team("teamA first quick-start", &first, "team-teamA");

    let duplicate = quick_start_with_transport(
        &team_b_same_name,
        None,
        true,
        None,
        &transport,
    )
    .expect("same spec.name should return a typed ExistingRuntime report");

    match duplicate {
        QuickStartReport::ExistingRuntime {
            session_name,
            state_path,
            ..
        } => {
            assert_eq!(session_name.as_ref().map(|s| s.as_str()), Some("team-teamA"));
            assert!(
                state_path.as_ref().is_some_and(|path| path.starts_with(&root)),
                "same-name ExistingRuntime should point at the shared workspace state path; state_path={state_path:?}"
            );
        }
        other => panic!(
            "when sibling teamdir compiles to existing spec.name=teamA, quick-start without --team should remain ExistingRuntime; got {other:?}"
        ),
    }
    assert_eq!(
        transport.spawned_sessions(),
        vec!["team-teamA".to_string()],
        "same compiled spec.name must not spawn a second sibling session without --fresh"
    );
}

#[test]
#[ignore = "real-machine: quick-start/session lifecycle gate"]
#[serial(env)]
fn quick_start_same_existing_team_still_returns_existing_runtime() {
    let _env = EnvGuard::unset([
        "TMUX",
        "TMUX_PANE",
        "TEAM_AGENT_ID",
        "TEAM_AGENT_TEAM_ID",
        "TEAM_AGENT_LEADER_PANE_ID",
        "TEAM_AGENT_LEADER_SESSION_UUID",
        "TEAM_AGENT_LEADER_SESSION_UUID_OVERRIDE",
        "TEAM_AGENT_LEADER_PROVIDER",
    ]);
    let root = tmp_dir("same-team-existing");
    seed_healthy_coordinator(&root);
    let team_a = team_dir(&root, "teamA", "worker_a");
    let transport = SessionRecordingTransport::default();

    let first = quick_start_with_transport(
        &team_a,
        Some("teamA"),
        true,
        Some("teamA"),
        &transport,
    )
    .expect("fixture: first teamA quick-start should succeed");
    assert_ready_team("teamA first quick-start", &first, "team-teamA");

    let duplicate = quick_start_with_transport(
        &team_a,
        Some("teamA"),
        true,
        Some("teamA"),
        &transport,
    )
    .expect("same-team duplicate should return a typed report, not an error");

    match duplicate {
        QuickStartReport::ExistingRuntime {
            team,
            session_name,
            state_path,
            ..
        } => {
            assert_eq!(team.as_deref(), Some("teamA"));
            assert_eq!(session_name.as_ref().map(|s| s.as_str()), Some("team-teamA"));
            assert!(
                state_path.as_ref().is_some_and(|path| path.starts_with(&root)),
                "ExistingRuntime should point at the shared workspace state path; state_path={state_path:?}"
            );
        }
        other => panic!(
            "same requested team with an existing runtime should remain ExistingRuntime; got {other:?}"
        ),
    }
    assert_eq!(
        transport.spawned_sessions(),
        vec!["team-teamA".to_string()],
        "same-team duplicate must not spawn another session without --fresh"
    );
}

#[test]
#[ignore = "real-machine: quick-start/session lifecycle gate"]
#[serial(env)]
fn quick_start_sibling_after_shutdown_is_allowed_not_nested() {
    let _env = EnvGuard::unset([
        "TMUX",
        "TMUX_PANE",
        "TEAM_AGENT_ID",
        "TEAM_AGENT_TEAM_ID",
        "TEAM_AGENT_LEADER_PANE_ID",
        "TEAM_AGENT_LEADER_SESSION_UUID",
        "TEAM_AGENT_LEADER_SESSION_UUID_OVERRIDE",
        "TEAM_AGENT_LEADER_PROVIDER",
    ]);
    let root = tmp_dir("sibling-after-shutdown");
    seed_healthy_coordinator(&root);
    let team_a = team_dir(&root, "teamA", "worker_a");
    let team_b = team_dir(&root, "child-team", "worker_b");
    let transport = SessionRecordingTransport::default();

    let first = quick_start_with_transport(
        &team_a,
        Some("teamA"),
        true,
        Some("teamA"),
        &transport,
    )
    .expect("fixture: first teamA quick-start should succeed");
    assert_ready_team("teamA first quick-start", &first, "team-teamA");

    let shutdown = team_agent::cli::lifecycle_port::shutdown_with_transport(
        &root,
        true,
        Some("teamA"),
        &transport,
    )
    .expect("fixture: shutdown --team teamA should mark the old team stopped");
    assert_eq!(
        shutdown.get("ok").and_then(Value::as_bool),
        Some(true),
        "fixture: shutdown must complete before sibling quick-start; shutdown={shutdown}"
    );

    let second = quick_start_with_transport(
        &team_b,
        None,
        false,
        None,
        &transport,
    )
    .expect(
        "after a same-workspace team has been shut down, bare quick-start <child-teamdir> must be treated as a sibling team, not an ambiguous nested quick-start",
    );

    assert_ready_team(
        "bare sibling child-like quick-start after shutdown",
        &second,
        "team-child-team",
    );
    let state = load_runtime_state(&root).expect("state after sibling quick-start");
    assert_team_present(&state, "teamA", "team-teamA");
    assert_team_present(&state, "child-team", "team-child-team");
    assert_eq!(
        state.get("active_team_key").and_then(Value::as_str),
        Some("child-team"),
        "bare quick-start after shutdown should activate sibling child-like team; state={state}"
    );
    assert_eq!(
        state["teams"]["child-team"].get("parent_team_key").and_then(Value::as_str),
        None,
        "sibling child-like team after shutdown must not be persisted as a nested child; state={state}"
    );
    assert!(
        transport
            .spawned_sessions()
            .iter()
            .any(|session| session == "team-child-team"),
        "bare sibling quick-start after shutdown must spawn its own team-child-team session; sessions={:?}",
        transport.spawned_sessions()
    );
}

#[test]
#[ignore = "real-machine: needs real tmux/coordinator/binary"]
#[serial(env)]
#[file_serial(tmux)]
fn dirty_sibling_quick_start_from_same_leader_pane_binds_receiver_and_delivers_to_leader() {
    let case = RealSiblingCase::new("dirty-sibling-real");
    let leader = case.spawn_leader_pane();
    let leader_env = [
        ("TEAM_AGENT_LEADER_PANE_ID", leader.as_str()),
        ("TMUX_PANE", leader.as_str()),
        ("TEAM_AGENT_LEADER_PROVIDER", "codex"),
    ];
    let current = fake_team_dir(&case.root, "current", "current_worker");
    let sibling = fake_team_dir(&case.root, "sibling", "sibling_worker");

    let current_out = case.run_team_agent_json(
        &[
            "quick-start",
            current.to_str().unwrap(),
            "--workspace",
            case.root.to_str().unwrap(),
            "--team-id",
            "current",
            "--yes",
            "--json",
        ],
        &leader_env,
    );
    assert_eq!(
        current_out.get("ok").and_then(Value::as_bool),
        Some(true),
        "fixture sanity: current quick-start from leader pane must succeed; out={current_out}"
    );

    let before_sibling = load_runtime_state(&case.root).expect("state before sibling quick-start");
    let current_receiver_before = before_sibling
        .pointer("/teams/current/leader_receiver")
        .cloned()
        .expect("current receiver exists before sibling quick-start");

    let sibling_out = case.run_team_agent_json(
        &[
            "quick-start",
            sibling.to_str().unwrap(),
            "--workspace",
            case.root.to_str().unwrap(),
            "--team-id",
            "sibling",
            "--yes",
            "--json",
        ],
        &leader_env,
    );
    let state = load_runtime_state(&case.root).expect("state after sibling quick-start");
    let receiver = state
        .pointer("/teams/sibling/leader_receiver")
        .unwrap_or_else(|| panic!("state.teams.sibling.leader_receiver must exist; state={state}"));
    let mut failures = Vec::new();
    if receiver.get("status").and_then(Value::as_str) != Some("attached") {
        failures.push(format!(
            "teams.sibling.leader_receiver.status must be attached when quick-start is run from a positive leader pane; receiver={receiver}"
        ));
    }
    if receiver.get("pane_id").and_then(Value::as_str) != Some(leader.as_str()) {
        failures.push(format!(
            "the same physical pane may be leader receiver for current and sibling team scopes; expected sibling pane_id={}, receiver={receiver}",
            leader.as_str()
        ));
    }
    let current_receiver_after = state
        .pointer("/teams/current/leader_receiver")
        .cloned()
        .expect("current receiver exists after sibling quick-start");
    if current_receiver_after != current_receiver_before {
        failures.push(format!(
            "sibling quick-start must not rewrite current team's leader_receiver; before={current_receiver_before} after={current_receiver_after}"
        ));
    }
    if receiver.get("status").and_then(Value::as_str) == Some("unbound")
        && sibling_out.get("ok").and_then(Value::as_bool) == Some(true)
    {
        failures.push(format!(
            "quick-start --json must not return ok=true after saving the launched team with leader_receiver.status=unbound; it must return degraded/ok=false with a claim-leader next action. out={sibling_out}"
        ));
    }

    let token = format!("DIRTY_SIBLING_TO_LEADER_{}", std::process::id());
    let send_raw = case.run_team_agent_status(
        &[
            "send",
            "leader",
            &token,
            "--workspace",
            case.root.to_str().unwrap(),
            "--team",
            "sibling",
            "--sender",
            "sibling_worker",
            "--no-wait",
            "--json",
        ],
        &[],
    );
    let send_out: Value = serde_json::from_slice(&send_raw.stdout).unwrap_or_else(|err| {
        panic!(
            "send stdout must be JSON even on delivery refusal: {err}; status={:?} stdout={} stderr={}",
            send_raw.status.code(),
            String::from_utf8_lossy(&send_raw.stdout),
            String::from_utf8_lossy(&send_raw.stderr)
        )
    });
    let _ = case.run_team_agent_status(
        &[
            "coordinator",
            "--workspace",
            case.root.to_str().unwrap(),
            "--once",
        ],
        &[],
    );
    let row = message_row_for_token(&case.root, &token);
    match row.as_ref() {
        Some((status, delivered_at)) if status == "delivered" && delivered_at.is_some() => {}
        other => failures.push(format!(
            "sibling_worker -> leader must produce a delivered DB row with delivered_at after quick-start binds the leader receiver; send_out={send_out} row={other:?}"
        )),
    }
    let leader_text = case.capture_pane(&leader);
    if !leader_text.contains(&token) {
        failures.push(format!(
            "leader pane {} must visibly receive sibling worker token {token}; capture={leader_text:?}",
            leader.as_str()
        ));
    }

    assert!(
        failures.is_empty(),
        "dirty sibling quick-start from the same leader pane must bind per-team receiver and make worker->leader delivery real, without delivery-time auto-claim:\n{}\nstate={state}\nsibling_out={sibling_out}",
        failures.join("\n")
    );
}

#[test]
#[ignore = "real-machine: quick-start/session lifecycle gate"]
#[serial(env)]
fn quick_start_returns_degraded_when_no_positive_caller_pane() {
    let _env = EnvGuard::unset([
        "TMUX",
        "TMUX_PANE",
        "TEAM_AGENT_ID",
        "TEAM_AGENT_TEAM_ID",
        "TEAM_AGENT_LEADER_PANE_ID",
        "TEAM_AGENT_LEADER_SESSION_UUID",
        "TEAM_AGENT_LEADER_SESSION_UUID_OVERRIDE",
        "TEAM_AGENT_LEADER_PROVIDER",
    ]);
    let root = tmp_dir("no-positive-caller-degraded");
    let team = fake_team_dir(&root, "unbound", "worker_a");
    let out = Command::new(env!("CARGO_BIN_EXE_team-agent"))
        .args([
            "quick-start",
            team.to_str().unwrap(),
            "--workspace",
            root.to_str().unwrap(),
            "--team-id",
            "unbound",
            "--yes",
            "--json",
        ])
        .current_dir(&root)
        .output()
        .expect("run team-agent quick-start");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let value: Value = serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|err| panic!("quick-start stdout must be JSON: {err}; stdout={stdout} stderr={}", String::from_utf8_lossy(&out.stderr)));
    let state = load_runtime_state(&root).expect("state after unbound quick-start");
    let receiver = state
        .pointer("/teams/unbound/leader_receiver")
        .or_else(|| state.pointer("/leader_receiver"))
        .unwrap_or_else(|| panic!("leader_receiver must exist after quick-start; state={state}"));

    assert_eq!(
        receiver.get("status").and_then(Value::as_str),
        Some("unbound"),
        "fixture sanity: no positive caller pane should persist an unbound receiver; receiver={receiver} state={state}"
    );
    assert_ne!(
        value.get("ok").and_then(Value::as_bool),
        Some(true),
        "C1-C3: quick-start --json must not return ok=true when launched leader_receiver is unbound; value={value} state={state}"
    );
    assert!(
        value
            .get("state")
            .and_then(Value::as_str)
            .is_some_and(|state| state == "leader_receiver_unbound")
            || value
                .get("worker_readiness")
                .and_then(|v| v.get("state"))
                .and_then(Value::as_str)
                .is_some_and(|state| state == "leader_receiver_unbound"),
        "C2: unbound quick-start must return structured degraded state=leader_receiver_unbound; value={value}"
    );
    assert!(
        value.to_string().contains("claim-leader"),
        "C2: unbound quick-start must include a claim-leader recovery next action; value={value}"
    );
}

#[test]
#[ignore = "real-machine: quick-start/session lifecycle gate"]
#[serial(env)]
fn delivery_does_not_autobind_unbound_receiver() {
    let root = tmp_dir("delivery-no-autobind");
    seed_unbound_delivery_state(&root);
    let before = load_runtime_state(&root).expect("state before delivery");
    let store = MessageStore::open(&root).unwrap();
    let message_id = store
        .create_message(
            Some("task-1"),
            "sibling_worker",
            "leader",
            "delivery must not claim while sending to leader",
            None,
            false,
            Some("sibling"),
        )
        .unwrap();
    let event_log = EventLog::new(&root);
    let transport = SessionRecordingTransport::default();

    let delivered = deliver_pending_messages(&root, &before, &transport, &event_log)
        .expect("delivery should return a rebind-required outcome, not panic");

    let after = load_runtime_state(&root).expect("state after delivery");
    assert!(
        delivered.is_empty(),
        "unbound leader receiver must not be delivered by auto-binding during delivery; delivered={delivered:?}"
    );
    assert_eq!(
        before.pointer("/teams/sibling/leader_receiver"),
        after.pointer("/teams/sibling/leader_receiver"),
        "C8/C14: delivery must not mutate leader_receiver/team_owner/owner_epoch while handling rebind_required; message_id={message_id} before={before} after={after}"
    );
    let events = std::fs::read_to_string(root.join(".team/logs/events.jsonl")).unwrap_or_default();
    assert!(
        events.contains("leader_not_attached") || events.contains("rebind_required"),
        "unbound delivery must emit leader_not_attached/rebind_required; events={events}"
    );
}

#[test]
fn delivery_grep_guard_no_owner_mutation() {
    let delivery = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/messaging/delivery.rs"
    ))
    .unwrap();
    let leader_receiver = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/messaging/leader_receiver.rs"
    ))
    .unwrap_or_default();
    let send_to_leader_body = leader_receiver
        .split("pub fn send_to_leader_receiver")
        .nth(1)
        .and_then(|tail| tail.split("/// `claim_leader_receiver`").next())
        .unwrap_or("");
    let send = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/messaging/send.rs")).unwrap();
    let scheduler = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/messaging/scheduler.rs"
    ))
    .unwrap_or_default();
    let tick = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/coordinator/tick.rs")).unwrap();
    let global_delivery_guard = format!("{delivery}\n{send_to_leader_body}\n{send}\n{scheduler}\n{tick}");
    for forbidden in [
        "seed_launched_owner",
        "seed_unbound_launched_owner",
        "claim_lease",
        "claim_leader_receiver(",
    ] {
        assert!(
            !global_delivery_guard.contains(forbidden),
            "C7/C14: delivery/send/scheduler/coordinator delivery paths must stay reclaim-neutral and must not write owner/receiver fields; forbidden={forbidden}"
        );
    }
}

#[test]
#[ignore = "real-machine: quick-start/session lifecycle gate"]
#[serial(env)]
fn worker_pane_seeded_as_leader_still_dropped() {
    let _env = EnvGuard::set([
        ("TMUX_PANE", "%1-first"),
        ("TEAM_AGENT_LEADER_PANE_ID", ""),
        ("TEAM_AGENT_ID", ""),
        ("TEAM_AGENT_TEAM_ID", ""),
        ("TEAM_AGENT_LEADER_SESSION_UUID", ""),
        ("TEAM_AGENT_LEADER_SESSION_UUID_OVERRIDE", ""),
        ("TEAM_AGENT_LEADER_PROVIDER", ""),
        ("TEAM_AGENT_OWNER_TEAM_ID", ""),
    ]);
    let root = tmp_dir("worker-pane-seeded-dropped");
    let team = fake_team_dir(&root, "workerseed", "worker_a");
    let transport = SessionRecordingTransport::default();

    team_agent::lifecycle::quick_start_with_transport_in_workspace(
        &root,
        &team,
        Some("workerseed"),
        true,
        Some("workerseed"),
        &transport,
    )
    .expect("quick-start should complete with recording transport");

    let state = load_runtime_state(&root).expect("state after worker-pane quick-start");
    let receiver = state
        .pointer("/teams/workerseed/leader_receiver")
        .or_else(|| state.pointer("/leader_receiver"))
        .unwrap_or_else(|| panic!("leader_receiver must exist; state={state}"));
    assert_eq!(
        receiver.get("status").and_then(Value::as_str),
        Some("unbound"),
        "C6: bare worker pane seeding must still be dropped; only a positive caller leader pane can bind. receiver={receiver} state={state}"
    );
}

fn assert_ready_team(label: &str, report: &QuickStartReport, expected_session: &str) {
    match report {
        QuickStartReport::Ready { session_name, .. } => assert_eq!(
            session_name.as_str(),
            expected_session,
            "{label}: session_name should derive from requested team identity"
        ),
        other => panic!("{label}: expected Ready, got {other:?}"),
    }
}

fn assert_team_present(state: &Value, team_key: &str, expected_session: &str) {
    let Some(team) = state.get("teams").and_then(|teams| teams.get(team_key)) else {
        panic!("state.teams.{team_key} must exist after sibling quick-start; state={state}");
    };
    assert_eq!(
        team.get("session_name").and_then(Value::as_str),
        Some(expected_session),
        "state.teams.{team_key}.session_name must be isolated per requested team; team={team}"
    );
    assert!(
        team.get("agents").and_then(Value::as_object).is_some_and(|agents| !agents.is_empty()),
        "state.teams.{team_key}.agents must be retained; team={team}"
    );
}

struct RealSiblingCase {
    root: PathBuf,
    backend: TmuxBackend,
    leader_session: SessionName,
}

impl RealSiblingCase {
    fn new(tag: &str) -> Self {
        let root = tmp_dir(tag);
        let backend = TmuxBackend::for_workspace(&root);
        let leader_session = SessionName::new(format!(
            "team-red2-leader-{}-{}",
            std::process::id(),
            root.file_name().and_then(|name| name.to_str()).unwrap_or("case")
        ));
        Self {
            root,
            backend,
            leader_session,
        }
    }

    fn spawn_leader_pane(&self) -> PaneId {
        let result = self
            .backend
            .spawn_first(
                &self.leader_session,
                &WindowName::new("leader"),
                &[
                    "sh".to_string(),
                    "-lc".to_string(),
                    "stty -echo 2>/dev/null; exec cat".to_string(),
                ],
                &self.root,
                &BTreeMap::new(),
            )
            .expect("spawn real tmux leader pane");
        result.pane_id
    }

    fn run_team_agent_json(&self, args: &[&str], extra_env: &[(&str, &str)]) -> Value {
        let output = self.run_team_agent_status(args, extra_env);
        assert!(
            output.status.success(),
            "team-agent {:?} should exit 0; status={:?} stdout={} stderr={}",
            args,
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice::<Value>(&output.stdout).unwrap_or_else(|err| {
            panic!(
                "team-agent {:?} stdout must be JSON: {err}; stdout={} stderr={}",
                args,
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            )
        })
    }

    fn run_team_agent_status(&self, args: &[&str], extra_env: &[(&str, &str)]) -> std::process::Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_team-agent"));
        command.args(args);
        command.current_dir(&self.root);
        for key in [
            "TEAM_AGENT_ID",
            "TEAM_AGENT_TEAM_ID",
            "TEAM_AGENT_OWNER_TEAM_ID",
            "TEAM_AGENT_LEADER_PANE_ID",
            "TEAM_AGENT_LEADER_SESSION_UUID",
            "TEAM_AGENT_LEADER_SESSION_UUID_OVERRIDE",
            "TEAM_AGENT_LEADER_PROVIDER",
            "TMUX",
            "TMUX_PANE",
        ] {
            command.env_remove(key);
        }
        for (key, value) in extra_env {
            command.env(key, value);
        }
        command.output().expect("run team-agent binary")
    }

    fn capture_pane(&self, pane: &PaneId) -> String {
        self.backend
            .capture(&Target::Pane(pane.clone()), CaptureRange::Full)
            .expect("capture leader pane")
            .text
    }
}

impl Drop for RealSiblingCase {
    fn drop(&mut self) {
        let _ = self.backend.kill_server();
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn message_row_for_token(workspace: &Path, token: &str) -> Option<(String, Option<String>)> {
    let db = workspace.join(".team/runtime/team.db");
    let conn = team_agent::db::schema::open_db(&db).ok()?;
    conn.query_row(
        "select status, delivered_at
         from messages
         where owner_team_id='sibling'
           and sender='sibling_worker'
           and recipient='leader'
           and content like ?1
         order by created_at desc
         limit 1",
        params![format!("%{token}%")],
        |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)),
    )
    .ok()
}

fn fake_team_dir(root: &Path, name: &str, agent_id: &str) -> PathBuf {
    let team = root.join(name);
    std::fs::create_dir_all(team.join("agents")).unwrap();
    std::fs::write(
        team.join("TEAM.md"),
        format!(
            "---\nname: {name}\nobjective: Real sibling quick-start receiver binding contract.\nprovider: fake\n---\n\n{name} team.\n"
        ),
    )
    .unwrap();
    std::fs::write(
        team.join("agents").join(format!("{agent_id}.md")),
        format!(
            "---\nname: {agent_id}\nrole: Worker\nprovider: fake\ntools:\n  - mcp_team\n---\n\nWorker.\n"
        ),
    )
    .unwrap();
    team
}

fn seed_unbound_delivery_state(root: &Path) {
    team_agent::state::persist::save_runtime_state(
        root,
        &serde_json::json!({
            "active_team_key": "sibling",
            "session_name": "team-sibling",
            "agents": {
                "sibling_worker": {
                    "status": "running",
                    "provider": "fake",
                    "window": "sibling_worker",
                    "owner_team_id": "sibling"
                }
            },
            "teams": {
                "sibling": {
                    "session_name": "team-sibling",
                    "leader_receiver": {
                        "mode": "direct_tmux",
                        "status": "unbound",
                        "provider": "codex",
                        "owner_epoch": 1
                    },
                    "team_owner": {
                        "provider": "codex",
                        "owner_epoch": 1
                    },
                    "agents": {
                        "sibling_worker": {
                            "status": "running",
                            "provider": "fake",
                            "window": "sibling_worker",
                            "owner_team_id": "sibling"
                        }
                    }
                }
            }
        }),
    )
    .unwrap();
}

fn team_dir(root: &Path, name: &str, agent_id: &str) -> PathBuf {
    team_dir_with_name(root, name, name, agent_id)
}

fn team_dir_with_name(root: &Path, dir_name: &str, spec_name: &str, agent_id: &str) -> PathBuf {
    let team = root.join(dir_name);
    std::fs::create_dir_all(team.join("agents")).unwrap();
    std::fs::write(
        team.join("TEAM.md"),
        format!(
            "---\nname: {spec_name}\nobjective: Sibling team quick-start contract.\nprovider: codex\n---\n\n{spec_name} team.\n"
        ),
    )
    .unwrap();
    std::fs::write(team.join("agents").join(format!("{agent_id}.md")), role_doc(agent_id)).unwrap();
    team
}

fn role_doc(name: &str) -> String {
    format!(
        "---\nname: {name}\nrole: Worker\nprovider: codex\nmodel: gpt-5.5\nauth_mode: subscription\ntools:\n  - mcp_team\n---\n\nWorker.\n"
    )
}

fn seed_healthy_coordinator(workspace: &Path) {
    let workspace = team_agent::coordinator::WorkspacePath::new(workspace.to_path_buf());
    std::fs::create_dir_all(team_agent::model::paths::runtime_dir(workspace.as_path())).unwrap();
    let _ = team_agent::message_store::MessageStore::open(workspace.as_path()).unwrap();
    let pid = team_agent::coordinator::Pid::new(std::process::id());
    team_agent::coordinator::write_coordinator_metadata(
        &workspace,
        pid,
        team_agent::coordinator::MetadataSource::Boot,
    )
    .unwrap();
    std::fs::write(
        team_agent::coordinator::coordinator_pid_path(&workspace),
        pid.to_string(),
    )
    .unwrap();
}

fn tmp_dir(tag: &str) -> PathBuf {
    static N: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "ta-rs-team-in-team-sibling-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::canonicalize(dir).unwrap()
}

struct EnvGuard {
    previous: Vec<(&'static str, Option<String>)>,
}

impl EnvGuard {
    fn unset(keys: [&'static str; 8]) -> Self {
        let previous = keys
            .iter()
            .map(|key| (*key, std::env::var(key).ok()))
            .collect::<Vec<_>>();
        for key in keys {
            unsafe {
                std::env::remove_var(key);
            }
        }
        Self { previous }
    }

    fn set(values: [(&'static str, &'static str); 8]) -> Self {
        let previous = values
            .iter()
            .map(|(key, _)| (*key, std::env::var(key).ok()))
            .collect::<Vec<_>>();
        for (key, value) in values {
            unsafe {
                if value.is_empty() {
                    std::env::remove_var(key);
                } else {
                    std::env::set_var(key, value);
                }
            }
        }
        Self { previous }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (key, value) in self.previous.drain(..).rev() {
            unsafe {
                if let Some(value) = value {
                    std::env::set_var(key, value);
                } else {
                    std::env::remove_var(key);
                }
            }
        }
    }
}

#[derive(Debug, Default)]
struct SessionRecordingTransport {
    sessions: Mutex<HashSet<String>>,
    spawned: Mutex<Vec<String>>,
}

impl SessionRecordingTransport {
    fn spawned_sessions(&self) -> Vec<String> {
        self.spawned.lock().unwrap().clone()
    }

    fn spawn_result(
        &self,
        session: &SessionName,
        window: &WindowName,
        kind: &'static str,
    ) -> SpawnResult {
        self.sessions
            .lock()
            .unwrap()
            .insert(session.as_str().to_string());
        let mut spawned = self.spawned.lock().unwrap();
        spawned.push(session.as_str().to_string());
        SpawnResult {
            pane_id: PaneId::new(format!("%{}-{kind}", spawned.len())),
            session: session.clone(),
            window: window.clone(),
            child_pid: None,
        }
    }
}

impl Transport for SessionRecordingTransport {
    fn kind(&self) -> BackendKind {
        BackendKind::Tmux
    }

    fn spawn_first(
        &self,
        session: &SessionName,
        window: &WindowName,
        _argv: &[String],
        _cwd: &Path,
        _env: &BTreeMap<String, String>,
    ) -> Result<SpawnResult, TransportError> {
        Ok(self.spawn_result(session, window, "first"))
    }

    fn spawn_into(
        &self,
        session: &SessionName,
        window: &WindowName,
        _argv: &[String],
        _cwd: &Path,
        _env: &BTreeMap<String, String>,
    ) -> Result<SpawnResult, TransportError> {
        Ok(self.spawn_result(session, window, "into"))
    }

    fn inject(
        &self,
        _target: &Target,
        _payload: &InjectPayload,
        _submit: Key,
        _bracketed: bool,
    ) -> Result<InjectReport, TransportError> {
        Ok(InjectReport {
            stage_reached: InjectStage::Submit,
            inject_verification: InjectVerification::CaptureContainsToken,
            submit_verification: SubmitVerification::EnterSentWithoutPlaceholderCheck,
            turn_verification: TurnVerification::NotYetObserved,
            attempts: 1,
            submit_diagnostics: None,
        })
    }

    fn send_keys(&self, _target: &Target, _keys: &[Key]) -> Result<(), TransportError> {
        Ok(())
    }

    fn capture(
        &self,
        _target: &Target,
        range: CaptureRange,
    ) -> Result<CapturedText, TransportError> {
        Ok(CapturedText {
            text: String::new(),
            range,
        })
    }

    fn query(
        &self,
        _target: &Target,
        _field: PaneField,
    ) -> Result<Option<String>, TransportError> {
        Ok(None)
    }

    fn liveness(
        &self,
        _pane: &PaneId,
    ) -> Result<team_agent::transport::PaneLiveness, TransportError> {
        Ok(team_agent::transport::PaneLiveness::Live)
    }

    fn list_targets(&self) -> Result<Vec<PaneInfo>, TransportError> {
        Ok(Vec::new())
    }

    fn has_session(&self, session: &SessionName) -> Result<bool, TransportError> {
        Ok(self.sessions.lock().unwrap().contains(session.as_str()))
    }

    fn list_windows(&self, _session: &SessionName) -> Result<Vec<WindowName>, TransportError> {
        Ok(Vec::new())
    }

    fn set_session_env(
        &self,
        _session: &SessionName,
        _key: &str,
        _value: &str,
    ) -> Result<SetEnvOutcome, TransportError> {
        Ok(SetEnvOutcome::Applied)
    }

    fn kill_session(&self, _session: &SessionName) -> Result<(), TransportError> {
        self.sessions.lock().unwrap().remove(_session.as_str());
        Ok(())
    }

    fn kill_window(&self, _target: &Target) -> Result<(), TransportError> {
        Ok(())
    }

    fn attach_session(&self, _session: &SessionName) -> Result<AttachOutcome, TransportError> {
        Ok(AttachOutcome::Attached)
    }
}
