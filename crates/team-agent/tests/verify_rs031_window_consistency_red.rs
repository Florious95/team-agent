//! RS 0.3.1 dogfood BUG-2/BUG-4 window consistency contracts.
//!
//! Real-machine evidence:
//! - state/status reported `clauder` running, but tmux only listed the `codexer` window.
//! - worker->leader delivery queued, then the coordinator pasted to `team-verify031:leader`,
//!   but that window did not exist; state had a bound `leader_receiver.pane_id`.
//!
//! These are command-substitute contracts: no real provider or subscription is used, but the
//! observable invariant is the same as the Mac mini run.

#![allow(clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::json;
use serial_test::{file_serial, serial};
use team_agent::event_log::EventLog;
use team_agent::lifecycle::{launch_with_transport, quick_start_with_transport, QuickStartReport};
use team_agent::message_store::MessageStore;
use team_agent::messaging::{
    deliver_pending_message, deliver_pending_messages, send_message, DeliveryStatus, MessageTarget,
    SendOptions,
};
use team_agent::state::persist::{load_runtime_state, save_runtime_state};
use team_agent::transport::{
    AttachOutcome, BackendKind, CaptureRange, CapturedText, InjectPayload, InjectReport,
    InjectStage, InjectVerification, Key, PaneField, PaneId, PaneInfo, PaneLiveness, SessionName,
    SetEnvOutcome, SpawnResult, SubmitVerification, Target, Transport, TransportError,
    TurnVerification, WindowName,
};

const TEAM_MD: &str =
    "---\nname: verify031\nobjective: Verify mixed provider windows.\nprovider: codex\n---\n\nTeam.\n";

const CODEX_ROLE: &str = "---\nname: codexer\nrole: Codex Worker\nprovider: codex\nmodel: gpt-5.5\nauth_mode: subscription\ntools:\n  - mcp_team\n---\n\nCodex worker.\n";

const CLAUDE_ROLE: &str = "---\nname: clauder\nrole: Claude Worker\nprovider: claude\nmodel: claude-sonnet-4-6\nauth_mode: subscription\ntools:\n  - mcp_team\n---\n\nClaude worker.\n";

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

#[test]
fn worker_mcp_owner_team_scope_must_match_runtime_team_key_not_spec_name() {
    let team = mixed_provider_team_dir("mcp-owner-scope");
    let transport =
        RecordingTransport::new().with_windows(vec![WindowName::new("clauder"), WindowName::new("codexer")]);
    let launch = ready_launch(quick_start_with_transport(&team, None, true, None, &transport)
        .expect("quick-start should reach worker spawn path"));
    assert_eq!(launch.started.len(), 2, "fixture precondition: both workers spawn");

    let workspace = team.parent().expect("teamdir has parent workspace");
    let state = load_runtime_state(workspace).expect("runtime state written by launch");
    let runtime_team_key = team_agent::state::projection::team_state_key(&state);
    assert_eq!(
        runtime_team_key, "teamdir",
        "fixture precondition: runtime state key is derived from the team directory"
    );

    let bad_scopes = transport
        .spawn_envs()
        .into_iter()
        .filter_map(|env| env.get("TEAM_AGENT_OWNER_TEAM_ID").cloned())
        .filter(|scope| scope.as_str() != runtime_team_key)
        .collect::<Vec<_>>();
    let bad_mcp_config_scopes = transport
        .spawn_argvs()
        .into_iter()
        .filter(|argv| {
            argv.iter().any(|arg| {
                arg.contains("TEAM_AGENT_OWNER_TEAM_ID")
                    && !arg.contains(&format!("TEAM_AGENT_OWNER_TEAM_ID=\"{runtime_team_key}\""))
                    && !arg.contains(&format!("TEAM_AGENT_OWNER_TEAM_ID={runtime_team_key}"))
            })
        })
        .collect::<Vec<_>>();
    assert!(
        bad_scopes.is_empty() && bad_mcp_config_scopes.is_empty(),
        "BUG-4 contract: worker MCP TEAM_AGENT_OWNER_TEAM_ID must match the runtime team key used by state. \
         Mac mini saw coordinator notifications under `teamdir` but worker MCP scope_resolved under the spec name, \
         so send_message(to=leader) looked in the wrong team and reported leader_not_attached. \
         expected_scope={runtime_team_key:?} bad_scopes={bad_scopes:?} bad_mcp_config_scopes={bad_mcp_config_scopes:?} state={state}"
    );
}

#[test]
#[serial(env)]
fn quick_start_preserves_external_leader_receiver_when_worker_pane_id_collides_across_tmux_sockets() {
    let _env = EnvGuard::set(&[
        ("TMUX", Some("/tmp/default-tmux-socket,123,0")),
        ("TMUX_PANE", Some("%0")),
        ("TEAM_AGENT_LEADER_PANE_ID", None),
        ("TEAM_AGENT_LEADER_SESSION_UUID", None),
        ("TEAM_AGENT_LEADER_SESSION_UUID_OVERRIDE", None),
        ("TEAM_AGENT_LEADER_PROVIDER", None),
        ("TEAM_AGENT_ID", None),
        ("TEAM_AGENT_TEAM_ID", None),
    ]);
    let team = codex_only_team_dir("external-pane-collision");
    let transport = RecordingTransport::new().with_windows(vec![WindowName::new("codexer")]);
    let launch = ready_launch(quick_start_with_transport(&team, None, true, None, &transport)
        .expect("quick-start should reach worker spawn path"));
    let worker_pane = launch
        .started
        .iter()
        .find(|agent| agent.agent_id.as_str() == "codexer")
        .map(|agent| agent.target.as_str())
        .expect("fixture precondition: codexer spawned");
    assert_eq!(
        worker_pane, "%0",
        "fixture precondition: first worker in the product -L socket receives bare pane id %0"
    );

    let workspace = team.parent().expect("teamdir has parent workspace");
    let state = load_runtime_state(workspace).expect("runtime state written by launch");
    let top_receiver_pane = state
        .get("leader_receiver")
        .and_then(|receiver| receiver.get("pane_id"))
        .and_then(serde_json::Value::as_str);
    let receiver_pane = state
        .pointer("/teams/teamdir/leader_receiver/pane_id")
        .and_then(serde_json::Value::as_str);
    let top_owner_pane = state
        .get("team_owner")
        .and_then(|owner| owner.get("pane_id"))
        .and_then(serde_json::Value::as_str);
    let owner_pane = state
        .pointer("/teams/teamdir/team_owner/pane_id")
        .and_then(serde_json::Value::as_str);

    assert_eq!(
        receiver_pane,
        Some("%0"),
        "BUG-4 socket contract: quick-start may run from a real leader pane on the user's/default tmux socket \
         whose bare id is `%0`, while the first worker in the product per-team -L socket is also `%0`. \
         Different sockets are not a worker/leader collision: the launched leader_receiver must remain bound \
         to the caller pane instead of being cleared to unbound/leader_not_attached. \
         receiver_pane={receiver_pane:?} top_receiver_pane={top_receiver_pane:?} owner_pane={owner_pane:?} \
         top_owner_pane={top_owner_pane:?} \
         worker_pane={worker_pane:?} state={state}"
    );
    assert_eq!(
        owner_pane,
        Some("%0"),
        "BUG-4 socket contract: team_owner must not be replaced with __team_agent_unbound__ for a different-socket bare id collision; state={state}"
    );
    assert!(
        top_receiver_pane.is_none() && top_owner_pane.is_none(),
        "Stage 3d state saves strip legacy top-level owner copies; state={state}"
    );
}

#[test]
fn running_worker_state_must_only_be_written_for_windows_that_exist_after_launch() {
    let team = mixed_provider_team_dir("state-window");
    let spec = team_agent::compiler::compile_team(&team).expect("compile mixed provider team");
    let spec_path = team.join("team.spec.yaml");
    std::fs::write(&spec_path, team_agent::model::yaml::dumps(&spec)).expect("write spec");

    // Simulates the Mac mini final capture: the codex window exists, the claude window does not.
    // launch_with_transport currently never verifies the post-spawn window set before persisting
    // `status=running`, so this fixture pins the state/physical invariant directly.
    let transport = RecordingTransport::new().with_windows(vec![WindowName::new("codexer")]);
    let launch = launch_with_transport(&spec_path, false, true, true, &transport)
        .expect("launch should reach the real spawn/state path");
    assert_eq!(launch.started.len(), 2, "fixture precondition: both spawn calls returned");

    let workspace = team.parent().expect("teamdir has parent workspace");
    let state = load_runtime_state(workspace).expect("runtime state written by launch");
    let live_windows = transport
        .list_windows(&launch.session_name)
        .expect("recording transport list_windows");

    let mut violations = Vec::new();
    for (agent_id, agent) in state
        .get("agents")
        .and_then(serde_json::Value::as_object)
        .expect("state agents object")
    {
        let status = agent.get("status").and_then(serde_json::Value::as_str);
        if status != Some("running") {
            continue;
        }
        let window = agent
            .get("window")
            .and_then(serde_json::Value::as_str)
            .unwrap_or(agent_id);
        if !live_windows.iter().any(|live| live.as_str() == window) {
            violations.push(format!("{agent_id} status=running window={window} missing from tmux list"));
        }
    }

    assert!(
        violations.is_empty(),
        "BUG-2 contract: state/status must not mark an agent running unless its tmux window exists. \
         Mac mini saw state clauder=running while product_windows only had codexer. violations={violations:?} state={state}"
    );
}

#[test]
fn leader_bound_delivery_must_target_bound_leader_pane_not_missing_leader_window() {
    let workspace = temp_ws("leader-window");
    let state = json!({
        "session_name": "team-verify031",
        "leader_receiver": {
            "mode": "direct_tmux",
            "status": "attached",
            "provider": "codex",
            "pane_id": "%0",
            "owner_epoch": 1
        },
        "team_owner": {
            "pane_id": "%0",
            "provider": "codex",
            "owner_epoch": 1
        },
        "owner_epoch": 1,
        "agents": {
            "codexer": {
                "status": "running",
                "provider": "codex",
                "window": "codexer"
            }
        }
    });
    save_runtime_state(&workspace, &state).expect("seed runtime state");

    let store = MessageStore::open(&workspace).expect("open message store");
    let message_id = store
        .create_message(None, "codexer", "leader", "BUG4 worker to leader canary", None, false, Some("teamdir"))
        .expect("create accepted leader-bound message");
    let event_log = EventLog::new(&workspace);
    let transport = RecordingTransport::new().with_windows(vec![WindowName::new("codexer")]);

    let outcome = deliver_pending_message(&workspace, &store, &transport, &message_id, &event_log, &state)
        .expect("delivery should use the recorded transport");
    assert!(outcome.ok, "fixture delivery should complete through OfflineTransport");

    let targets = transport.inject_targets();
    assert_eq!(targets.len(), 1, "expected exactly one physical injection target");
    assert_eq!(
        targets[0],
        Target::Pane(team_agent::transport::PaneId::new("%0")),
        "BUG-4 contract: leader-bound delivery must paste to state.leader_receiver.pane_id when bound. \
         It must not synthesize the nonexistent session window `team-verify031:leader`; Mac mini \
         failed with `can't find window: leader`. actual_targets={targets:?}"
    );
}

#[test]
#[ignore = "real-machine: needs real tmux/coordinator/binary"]
#[file_serial(tmux)]
fn worker_to_leader_delivery_succeeds_when_attached_leader_pane_lives_on_default_socket() {
    let workspace = temp_ws("leader-default-socket-e2e");
    let leader_pane = PaneId::new("%default-leader");
    let product_worker_pane = PaneId::new("%product-worker");
    let product_session = SessionName::new("team-bug4-product");
    let transport = RecordingTransport::new()
        .with_liveness(vec![
            (leader_pane.as_str().to_string(), PaneLiveness::Dead),
            (product_worker_pane.as_str().to_string(), PaneLiveness::Live),
        ])
        .with_targets(vec![
            pane_info(leader_pane.as_str(), "default-leader-socket", "leader"),
            pane_info(product_worker_pane.as_str(), product_session.as_str(), "codexer"),
        ]);

    save_runtime_state(
        &workspace,
        &json!({
            "session_name": product_session.as_str(),
            "active_team_key": "teamdir",
            "leader": {"id": "leader", "provider": "codex"},
            "leader_receiver": {
                "mode": "direct_tmux",
                "status": "attached",
                "provider": "codex",
                "pane_id": leader_pane.as_str(),
                "owner_epoch": 1,
                "discovery": "quick_start"
            },
            "team_owner": {
                "pane_id": leader_pane.as_str(),
                "provider": "codex",
                "owner_epoch": 1,
                "claimed_via": "quick-start"
            },
            "owner_epoch": 1,
            "agents": {
                "codexer": {
                    "status": "running",
                    "provider": "codex",
                    "window": "codexer",
                    "pane_id": product_worker_pane.as_str(),
                    "startup_prompts": "handled"
                }
            },
            "teams": {
                "teamdir": {
                    "session_name": product_session.as_str(),
                    "leader_receiver": {
                        "mode": "direct_tmux",
                        "status": "attached",
                        "provider": "codex",
                        "pane_id": leader_pane.as_str(),
                        "owner_epoch": 1,
                        "discovery": "quick_start"
                    },
                    "team_owner": {
                        "pane_id": leader_pane.as_str(),
                        "provider": "codex",
                        "owner_epoch": 1,
                        "claimed_via": "quick-start"
                    },
                    "agents": {
                        "codexer": {
                            "status": "running",
                            "provider": "codex",
                            "window": "codexer",
                            "pane_id": product_worker_pane.as_str(),
                            "startup_prompts": "handled"
                        }
                    }
                }
            }
        }),
    )
    .expect("seed runtime state");

    let store = MessageStore::open(&workspace).expect("open message store");
    let message_id = store
        .create_message(
            None,
            "codexer",
            "leader",
            "BUG4 worker-to-leader default socket canary",
            None,
            false,
            Some("teamdir"),
        )
        .expect("seed worker-origin leader-bound message");
    let state = load_runtime_state(&workspace).expect("load seeded runtime state");
    let event_log = EventLog::new(&workspace);

    let delivered = deliver_pending_messages(&workspace, &state, &transport, &event_log)
        .expect("worker-to-leader delivery should run through the product coordinator transport");
    let status = message_status(&workspace, &message_id).expect("message status exists");
    let events = std::fs::read_to_string(workspace.join(".team/logs/events.jsonl"))
        .expect("events.jsonl exists");
    let inject_targets = transport.inject_targets();

    assert!(
        delivered.iter().any(|id| id == &message_id)
            && status == "delivered"
            && inject_targets.contains(&Target::Pane(leader_pane.clone())),
        "BUG-4 E2E contract: leader_receiver is attached and combined discovery shows the leader pane live \
         on the caller/default socket. A product-socket liveness probe returning Dead for that pane must not \
         be interpreted as leader_not_attached; the worker->leader message must be physically injected to the \
         bound leader pane. delivered={delivered:?} status={status:?} inject_targets={inject_targets:?} events={events}"
    );
    assert!(
        !events.contains("\"leader_receiver.delivery_blocked\"")
            && !events.contains("\"reason\":\"leader_not_attached\""),
        "BUG-4 E2E contract: attached+live default-socket leader must not emit leader_not_attached; events={events}"
    );
}

fn assert_bound_reachable_leader_send_message_delivers(binding: &str, discovery: &str, claimed_via: &str) {
    let workspace = temp_ws(&format!("fresh-leader-single-authority-{binding}"));
    let leader_pane = PaneId::new("%claimed-leader");
    let worker_pane = PaneId::new("%worker");
    let state = json!({
        "session_name": "team-bug4-fresh",
        "active_team_key": "teamdir",
        "leader": {"id": "leader", "provider": "codex"},
        "leader_receiver": {
            "mode": "direct_tmux",
            "status": "attached",
            "provider": "codex",
            "pane_id": leader_pane.as_str(),
            "owner_epoch": 7,
            "discovery": discovery
        },
        "team_owner": {
            "pane_id": leader_pane.as_str(),
            "provider": "codex",
            "owner_epoch": 7,
            "claimed_via": claimed_via
        },
        "owner_epoch": 7,
        "agents": {
            "codexer": {
                "status": "running",
                "provider": "codex",
                "window": "codexer",
                "pane_id": worker_pane.as_str(),
                "startup_prompts": "handled"
            }
        }
    });
    save_runtime_state(&workspace, &state).expect("seed runtime state");
    let transport = RecordingTransport::new()
        .with_targets(vec![
            pane_info(leader_pane.as_str(), "external-leader-socket", "leader"),
            pane_info(worker_pane.as_str(), "team-bug4-fresh", "codexer"),
        ])
        .with_liveness(vec![
            (leader_pane.as_str().to_string(), PaneLiveness::Live),
            (worker_pane.as_str().to_string(), PaneLiveness::Live),
        ]);
    let event_log = EventLog::new(&workspace);

    let mut opts = SendOptions::default();
    opts.sender = "codexer".to_string();
    opts.requires_ack = false;
    opts.route_task_id = false;
    opts.team = Some(team_agent::model::ids::TeamKey::new("teamdir"));
    let queued = send_message(
        &workspace,
        &MessageTarget::Single("leader".to_string()),
        &format!("BUG4 fresh post-{binding} worker-to-leader canary"),
        &opts,
    )
    .expect("fresh leader-bound row should be created");

    assert_eq!(
        queued.status,
        DeliveryStatus::Queued,
        "BUG-4 true-entry contract: fresh messaging::send_message(to=leader) after {binding} must \
         return a non-null message_id and enqueue the row. It must not be synchronously rejected \
         by send.rs before the unified delivery authority runs. out={queued:?}"
    );
    let message_id = queued.message_id.as_deref().expect("queued row carries message id");

    let delivered = deliver_pending_messages(&workspace, &state, &transport, &event_log)
        .expect("deliver_pending_messages should own the physical delivery decision");
    let status = message_status(&workspace, message_id).expect("message status exists");
    let targets = transport.inject_targets();
    let events = std::fs::read_to_string(workspace.join(".team/logs/events.jsonl"))
        .expect("events.jsonl exists");

    assert!(
        delivered.iter().any(|id| id == message_id)
            && status == "delivered"
            && targets.contains(&Target::Pane(leader_pane.clone())),
        "BUG-4 single-authority contract: the fresh row created by send_message must be \
         physically injected by deliver_pending_messages to the claimed leader pane. \
         delivered={delivered:?} status={status:?} targets={targets:?} events={events}"
    );
    assert!(
        !events.contains("\"reason\":\"leader_not_attached\""),
        "BUG-4 single-authority contract: reachable post-{binding} fresh delivery must not emit \
         leader_not_attached before the physical delivery authority runs. events={events}"
    );
}

#[test]
fn bound_reachable_leader_from_claim_or_attach_is_delivered_by_single_authority() {
    assert_bound_reachable_leader_send_message_delivers("claim", "claim_leader", "claim-leader");
    assert_bound_reachable_leader_send_message_delivers("attach", "attach_leader", "attach-leader");
}

#[test]
fn unbound_or_unreachable_worker_to_leader_remains_rebind_required_without_autodelivery() {
    for (label, state, transport) in [
        (
            "unbound-no-leader-receiver",
            json!({
                "session_name": "team-bug4-unbound",
                "active_team_key": "teamdir",
                "leader": {"id": "leader", "provider": "codex"},
                "agents": {"codexer": {"status": "running", "provider": "codex", "window": "codexer"}}
            }),
            RecordingTransport::new(),
        ),
        (
            "attached-but-unreachable",
            json!({
                "session_name": "team-bug4-unreachable",
                "active_team_key": "teamdir",
                "leader": {"id": "leader", "provider": "codex"},
                "leader_receiver": {
                    "mode": "direct_tmux",
                    "status": "attached",
                    "provider": "codex",
                    "pane_id": "%missing-leader",
                    "owner_epoch": 1
                },
                "team_owner": {"pane_id": "%missing-leader", "provider": "codex", "owner_epoch": 1},
                "agents": {"codexer": {"status": "running", "provider": "codex", "window": "codexer"}}
            }),
            RecordingTransport::new().with_liveness(vec![("%missing-leader".to_string(), PaneLiveness::Dead)]),
        ),
    ] {
        let workspace = temp_ws(label);
        save_runtime_state(&workspace, &state).expect("seed runtime state");
        let event_log = EventLog::new(&workspace);
        let mut opts = SendOptions::default();
        opts.sender = "codexer".to_string();
        opts.requires_ack = false;
        opts.route_task_id = false;
        opts.team = Some(team_agent::model::ids::TeamKey::new("teamdir"));
        let outcome = send_message(
            &workspace,
            &MessageTarget::Single("leader".to_string()),
            "BUG4 unreachable leader canary",
            &opts,
        )
        .expect("send_message should enqueue a leader-bound row for the delivery authority");
        let message_id = outcome.message_id.as_deref().unwrap_or_else(|| {
            panic!(
                "{label}: send_message(to=leader) must return a non-null message_id before \
                 deliver_pending_messages decides rebind_required; out={outcome:?}"
            )
        });

        assert_eq!(
            outcome.status,
            DeliveryStatus::Queued,
            "{label}: send_message(to=leader) must not synchronously refuse before enqueue. \
             It should return a non-null message_id and let deliver_pending_messages be the \
             single authority for leader_not_attached/rebind_required. out={outcome:?}"
        );

        let delivered = deliver_pending_messages(&workspace, &state, &transport, &event_log)
            .expect("single authority should reject unreachable leader rows");
        assert!(
            delivered.iter().all(|id| id != message_id),
            "{label}: unbound/unreachable leader must not be delivered; delivered={delivered:?}"
        );

        let status = message_status(&workspace, message_id).expect("message status exists");
        let events = std::fs::read_to_string(workspace.join(".team/logs/events.jsonl"))
            .expect("events.jsonl exists");
        assert_eq!(
            status, "failed",
            "{label}: A(a) guard: unreachable leader rows must finish blocked/failed until explicit claim/rebind; \
             out={outcome:?} events={events}"
        );
        assert!(
            events.contains("leader_not_attached") && events.contains("rebind_required"),
            "{label}: A(a) guard must emit leader_not_attached + rebind_required audit; events={events}"
        );
    }
}

#[test]
fn send_message_entrypoint_has_no_pre_enqueue_leader_liveness_gate() {
    let src = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/messaging/send.rs"
    ))
    .expect("read send.rs");
    let body = src
        .split("pub fn send_message")
        .nth(1)
        .and_then(|tail| tail.split("fn task_exists").next())
        .expect("send_message body");

    for forbidden in [
        "leader_pane_bound_but_not_live",
        "rebind_required_outcome(None)",
        "\"leader_receiver.delivery_blocked\"",
        "\"leader_not_attached\"",
    ] {
        assert!(
            !body.contains(forbidden),
            "BUG-4 true-entry contract: messaging::send_message must not have a pre-enqueue \
             leader liveness gate ({forbidden}); it must create a message_id and let \
             deliver_pending_messages decide endpoint/liveness/injection"
        );
    }
}

#[test]
fn send_to_leader_receiver_entrypoint_has_no_private_attached_precheck() {
    let src = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/messaging/leader_receiver.rs"
    ))
    .expect("read leader_receiver.rs");
    let body = src
        .split("pub fn send_to_leader_receiver")
        .nth(1)
        .and_then(|tail| tail.split("pub fn claim_leader_receiver").next())
        .expect("send_to_leader_receiver body");

    for forbidden in [
        "leader_pane_is_live",
        "pane_attached",
        "leader_receiver.delivery_blocked",
        "leader_not_attached",
    ] {
        assert!(
            !body.contains(forbidden),
            "BUG-4 single-authority contract: send_to_leader_receiver must not carry a private \
             leader-attached precheck ({forbidden}); endpoint/liveness/blocking belongs only to \
             deliver_pending_messages/deliver_pending_message"
        );
    }
}

fn mixed_provider_team_dir(tag: &str) -> PathBuf {
    let team = temp_ws(tag).join("teamdir");
    std::fs::create_dir_all(team.join("agents")).expect("create agents dir");
    std::fs::write(team.join("TEAM.md"), TEAM_MD).expect("write TEAM.md");
    std::fs::write(team.join("agents").join("clauder.md"), CLAUDE_ROLE).expect("write clauder role");
    std::fs::write(team.join("agents").join("codexer.md"), CODEX_ROLE).expect("write codexer role");
    team
}

fn message_status(workspace: &std::path::Path, message_id: &str) -> Option<String> {
    let store = MessageStore::open(workspace).ok()?;
    let conn = team_agent::db::schema::open_db(store.db_path()).ok()?;
    conn.query_row(
        "select status from messages where message_id = ?1",
        [message_id],
        |row| row.get(0),
    )
    .ok()
}

fn pane_info(pane_id: &str, session: &str, window: &str) -> PaneInfo {
    PaneInfo {
        pane_id: PaneId::new(pane_id),
        session: SessionName::new(session),
        window_index: Some(0),
        window_name: Some(WindowName::new(window)),
        pane_index: Some(0),
        tty: None,
        current_command: Some("bash".to_string()),
        current_path: None,
        active: true,
        pane_pid: None,
        leader_env: BTreeMap::new(),
    }
}

fn temp_ws(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "ta-verify-rs031-window-red-{tag}-{}-{}",
        std::process::id(),
        TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create temp workspace");
    std::fs::canonicalize(dir).expect("canonical temp workspace")
}

#[derive(Clone, Default)]
struct RecordingTransport {
    inner: Arc<Mutex<RecordingState>>,
}

#[derive(Default)]
struct RecordingState {
    windows: Vec<WindowName>,
    spawns: usize,
    inject_targets: Vec<Target>,
    spawn_envs: Vec<BTreeMap<String, String>>,
    spawn_argvs: Vec<Vec<String>>,
    liveness: BTreeMap<String, PaneLiveness>,
    targets: Vec<PaneInfo>,
}

impl RecordingTransport {
    fn new() -> Self {
        Self::default()
    }

    fn with_windows(self, windows: Vec<WindowName>) -> Self {
        self.with_state(|state| state.windows = windows);
        self
    }

    fn with_liveness(self, entries: Vec<(String, PaneLiveness)>) -> Self {
        self.with_state(|state| {
            state.liveness = entries.into_iter().collect();
        });
        self
    }

    fn with_targets(self, targets: Vec<PaneInfo>) -> Self {
        self.with_state(|state| state.targets = targets);
        self
    }

    fn inject_targets(&self) -> Vec<Target> {
        self.with_state(|state| state.inject_targets.clone())
    }

    fn spawn_envs(&self) -> Vec<BTreeMap<String, String>> {
        self.with_state(|state| state.spawn_envs.clone())
    }

    fn spawn_argvs(&self) -> Vec<Vec<String>> {
        self.with_state(|state| state.spawn_argvs.clone())
    }

    fn with_state<T>(&self, f: impl FnOnce(&mut RecordingState) -> T) -> T {
        let mut guard = self.inner.lock().expect("recording transport lock");
        f(&mut guard)
    }
}

impl Transport for RecordingTransport {
    fn kind(&self) -> BackendKind {
        BackendKind::Tmux
    }

    fn spawn_first(
        &self,
        session: &SessionName,
        window: &WindowName,
        argv: &[String],
        _cwd: &std::path::Path,
        env: &BTreeMap<String, String>,
    ) -> Result<SpawnResult, TransportError> {
        self.with_state(|state| {
            state.spawn_envs.push(env.clone());
            state.spawn_argvs.push(argv.to_vec());
        });
        Ok(self.spawn_result(session, window))
    }

    fn spawn_into(
        &self,
        session: &SessionName,
        window: &WindowName,
        argv: &[String],
        _cwd: &std::path::Path,
        env: &BTreeMap<String, String>,
    ) -> Result<SpawnResult, TransportError> {
        self.with_state(|state| {
            state.spawn_envs.push(env.clone());
            state.spawn_argvs.push(argv.to_vec());
        });
        Ok(self.spawn_result(session, window))
    }

    fn inject(
        &self,
        target: &Target,
        _payload: &InjectPayload,
        _submit: Key,
        _bracketed: bool,
    ) -> Result<InjectReport, TransportError> {
        self.with_state(|state| state.inject_targets.push(target.clone()));
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

    fn capture(&self, _target: &Target, range: CaptureRange) -> Result<CapturedText, TransportError> {
        Ok(CapturedText { text: "OpenAI Codex".to_string(), range })
    }

    fn query(&self, _target: &Target, _field: PaneField) -> Result<Option<String>, TransportError> {
        Ok(None)
    }

    fn liveness(&self, pane: &PaneId) -> Result<PaneLiveness, TransportError> {
        Ok(self
            .with_state(|state| state.liveness.get(pane.as_str()).copied())
            .unwrap_or(PaneLiveness::Unknown))
    }

    fn list_targets(&self) -> Result<Vec<PaneInfo>, TransportError> {
        Ok(self.with_state(|state| state.targets.clone()))
    }

    fn has_session(&self, _session: &SessionName) -> Result<bool, TransportError> {
        Ok(false)
    }

    fn list_windows(&self, _session: &SessionName) -> Result<Vec<WindowName>, TransportError> {
        Ok(self.with_state(|state| state.windows.clone()))
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
        Ok(())
    }

    fn kill_window(&self, _target: &Target) -> Result<(), TransportError> {
        Ok(())
    }

    fn attach_session(&self, _session: &SessionName) -> Result<AttachOutcome, TransportError> {
        Ok(AttachOutcome::Attached)
    }
}

fn codex_only_team_dir(tag: &str) -> PathBuf {
    let team = temp_ws(tag).join("teamdir");
    std::fs::create_dir_all(team.join("agents")).expect("create agents dir");
    std::fs::write(team.join("TEAM.md"), TEAM_MD).expect("write TEAM.md");
    std::fs::write(team.join("agents").join("codexer.md"), CODEX_ROLE).expect("write codex role");
    team
}

fn ready_launch(report: QuickStartReport) -> team_agent::lifecycle::LaunchReport {
    match report {
        QuickStartReport::Ready { launch, .. } => *launch,
        other => panic!("fixture expected quick-start ready report, got {other:?}"),
    }
}

struct EnvGuard {
    previous: Vec<(&'static str, Option<String>)>,
}

impl EnvGuard {
    fn set(values: &[(&'static str, Option<&str>)]) -> Self {
        let mut previous = Vec::new();
        for (key, value) in values {
            previous.push((*key, std::env::var(key).ok()));
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
        Self { previous }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (key, value) in self.previous.drain(..).rev() {
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
    }
}

impl RecordingTransport {
    fn spawn_result(&self, session: &SessionName, window: &WindowName) -> SpawnResult {
        let pane_index = self.with_state(|state| {
            let idx = state.spawns;
            state.spawns = state.spawns.saturating_add(1);
            idx
        });
        SpawnResult {
            pane_id: PaneId::new(format!("%{pane_index}")),
            session: session.clone(),
            window: window.clone(),
            child_pid: None,
        }
    }
}
