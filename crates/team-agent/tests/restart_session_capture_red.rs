//! Restart must preserve provider session context before destructive teardown.
//!
//! #240 root cause: `restart`/`shutdown` decide or kill from runtime state before
//! force-refreshing missing provider `session_id`s. A real provider session can exist under the
//! provider home while `state.agents[*].session_id` is still null; destructive lifecycle must capture
//! it first and resume from that durable id.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use serde_json::{json, Value};
use serial_test::serial;
use team_agent::cli::{cmd_wait_ready, CmdOutput, WaitReadyArgs};
use team_agent::lifecycle::{
    classify_restart_plan, restart_with_transport, RestartReport, ResumeDecision, StartMode,
};
use team_agent::model::enums::Provider;
use team_agent::provider::get_adapter;
use team_agent::state::persist::{load_runtime_state, save_runtime_state};
use team_agent::transport::{
    AttachOutcome, BackendKind, CaptureRange, CapturedText, InjectPayload, InjectReport,
    InjectStage, InjectVerification, Key, PaneField, PaneId, PaneInfo, SessionName,
    SetEnvOutcome, SpawnResult, SubmitVerification, Target, Transport, TransportError,
    TurnVerification, WindowName,
};

#[test]
#[ignore = "real-machine: provider session filesystem + restart/shutdown lifecycle convergence"]
#[serial(env)]
fn provider_home_codex_rollout_session_meta_is_captured_for_restart_resume() {
    let fixture = RestartFixture::new("codex-home-capture", "codex");
    let _home = HomeGuard::with_home(fixture.root.join("home"));
    let codex_session = "codex-sess-240";
    seed_codex_home_rollout(&fixture.home(), &fixture.spawn_cwd, codex_session);
    seed_running_state_without_session(&fixture.team, "codex", &fixture.spawn_cwd);

    let adapter = get_adapter(Provider::Codex);
    let captured = adapter
        .capture_session_id("worker_a", &fixture.spawn_cwd, 0)
        .expect("Codex provider-home capture should not error")
        .expect("Codex rollout under HOME/.codex/sessions must be discoverable before restart");

    assert_eq!(
        captured.session_id.as_ref().map(|id| id.as_str()),
        Some(codex_session),
        "Codex capture must extract session_meta.payload.id from provider-home rollout JSONL"
    );
    assert!(
        captured
            .rollout_path
            .as_ref()
            .is_some_and(|path| path.as_path().starts_with(fixture.home().join(".codex/sessions"))),
        "Codex capture must scan provider home, not only spawn_cwd; captured={captured:?}"
    );
}

#[test]
#[ignore = "real-machine: provider session filesystem + restart/shutdown lifecycle convergence"]
#[serial(env)]
fn provider_home_codex_rollout_matches_workspace_spawn_cwd_to_team_subdir_across_symlink() {
    let fixture = RestartFixture::new_symlinked_workspace("codex-workspace-team-cwd", "codex");
    let _home = HomeGuard::with_home(fixture.root.join("home"));
    let codex_session = "codex-sess-workspace-team-subdir";
    seed_codex_home_rollout_with_cwd(&fixture.home(), &fixture.team, codex_session);
    seed_running_state_without_session(&fixture.team, "codex", &fixture.spawn_cwd);

    let adapter = get_adapter(Provider::Codex);
    let captured = adapter
        .capture_session_id("worker_a", &fixture.spawn_cwd, 0)
        .expect("Codex provider-home capture should not error")
        .expect(
            "Codex rollout cwd at the team subdir must match a worker spawn_cwd at the parent workspace, even through symlink-equivalent paths",
        );

    assert_eq!(
        captured.session_id.as_ref().map(|id| id.as_str()),
        Some(codex_session),
        "Codex capture must accept workspace↔team-subdir cwd granularity when the team dir belongs to this worker workspace"
    );
}

#[test]
#[ignore = "real-machine: provider session filesystem + restart/shutdown lifecycle convergence"]
#[serial(env)]
fn codex_session_capture_assigns_complete_unique_rollouts_to_each_live_same_team_agent() {
    let fixture = RestartFixture::new("codex-session-complete-unique-per-agent", "codex");
    let _home = HomeGuard::with_home(fixture.root.join("home"));
    seed_codex_home_rollout_with_cwd(&fixture.home(), &fixture.spawn_cwd, "codex-sess-alpha");
    seed_codex_home_rollout_with_cwd(&fixture.home(), &fixture.spawn_cwd, "codex-sess-beta");
    seed_two_running_codex_agents_without_sessions(&fixture.team, &fixture.spawn_cwd);
    let transport = RecordingTransport::new().with_session_present(true);

    let report = restart_with_transport(&fixture.team, false, None, &transport)
        .expect("restart should refresh missing provider sessions for both live agents");

    let state = load_runtime_state(&fixture.team).unwrap();
    assert_all_live_codex_agents_have_complete_unique_sessions(&state, ["worker_a", "worker_b"]);
    assert_restart_resumed_all_agents(&report, ["worker_a", "worker_b"]);
}

#[test]
#[ignore = "real-machine: provider session filesystem + restart/shutdown lifecycle convergence"]
#[serial(env)]
fn session_attribution_prefers_positive_pid_env_over_path_order() {
    let fixture = RestartFixture::new("codex-session-positive-source-order", "codex");
    let _home = HomeGuard::with_home(fixture.root.join("home"));
    seed_codex_home_rollout_with_agent_hint(
        &fixture.home(),
        &fixture.spawn_cwd,
        "000-path-first-worker-b.jsonl",
        "codex-sess-worker-b",
        "worker_b",
        41002,
    );
    seed_codex_home_rollout_with_agent_hint(
        &fixture.home(),
        &fixture.spawn_cwd,
        "999-path-last-worker-a.jsonl",
        "codex-sess-worker-a",
        "worker_a",
        41001,
    );
    seed_two_running_codex_agents_without_sessions(&fixture.team, &fixture.spawn_cwd);
    let transport = RecordingTransport::new().with_session_present(true);

    restart_with_transport(&fixture.team, false, None, &transport)
        .expect("restart should run the real session refresh path");

    let state = load_runtime_state(&fixture.team).unwrap();
    assert_eq!(
        agent_session(&state, "worker_a"),
        Some("codex-sess-worker-a"),
        "MUST-11: positive pane_pid/process-env attribution must beat rollout path ordering for worker_a; state={state}"
    );
    assert_eq!(
        agent_session(&state, "worker_b"),
        Some("codex-sess-worker-b"),
        "MUST-11: positive pane_pid/process-env attribution must beat rollout path ordering for worker_b; state={state}"
    );
    assert_all_live_codex_agents_have_complete_unique_sessions(&state, ["worker_a", "worker_b"]);
}

#[test]
#[ignore = "real-machine: provider session filesystem + restart/shutdown lifecycle convergence"]
#[serial(env)]
fn session_attribution_leaves_ambiguous_when_no_unique_match() {
    let fixture = RestartFixture::new("codex-session-ambiguous-honest", "codex");
    let _home = HomeGuard::with_home(fixture.root.join("home"));
    seed_codex_home_rollout_with_cwd(&fixture.home(), &fixture.spawn_cwd, "codex-sess-only-one");
    seed_two_running_codex_agents_without_sessions(&fixture.team, &fixture.spawn_cwd);
    let transport = RecordingTransport::new().with_session_present(true);

    restart_with_transport(&fixture.team, false, None, &transport)
        .expect("restart should run the real session refresh path");

    let state = load_runtime_state(&fixture.team).unwrap();
    let nonempty = ["worker_a", "worker_b"]
        .into_iter()
        .filter(|agent_id| agent_session(&state, agent_id).is_some())
        .count();
    assert_eq!(
        nonempty, 1,
        "with only one matching rollout, exactly one live agent may be captured; state={state}"
    );
    let ambiguous_agent = ["worker_a", "worker_b"]
        .into_iter()
        .find(|agent_id| agent_session(&state, agent_id).is_none())
        .expect("one agent should remain uncaptured");
    let agent = state.get("agents").and_then(|agents| agents.get(ambiguous_agent)).unwrap();
    assert_eq!(
        agent.get("attribution_ambiguous").and_then(serde_json::Value::as_bool),
        Some(true),
        "ambiguous session attribution must be explicit and null, not silently duplicated or path-order guessed; agent={agent} state={state}"
    );
}

#[test]
#[ignore = "real-machine: provider session filesystem + restart/shutdown lifecycle convergence"]
#[serial(env)]
fn restart_waits_for_delayed_rollout_before_writing_resume_decisions() {
    let fixture = RestartFixture::new("codex-delayed-rollout-converges", "codex");
    let _home = HomeGuard::with_home(fixture.root.join("home"));
    seed_codex_home_rollout_with_agent_hint(
        &fixture.home(),
        &fixture.spawn_cwd,
        "rollout-worker-a-visible-first.jsonl",
        "codex-sess-delayed-a",
        "worker_a",
        41001,
    );
    seed_two_interacted_running_codex_agents_without_sessions(&fixture.team, &fixture.spawn_cwd);
    let delayed_home = fixture.home();
    let delayed_cwd = fixture.spawn_cwd.clone();
    let delayed = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(750));
        seed_codex_home_rollout_with_agent_hint(
            &delayed_home,
            &delayed_cwd,
            "rollout-worker-b-arrives-after-first-poll.jsonl",
            "codex-sess-delayed-b",
            "worker_b",
            41002,
        );
    });
    let transport = RecordingTransport::new().with_session_present(true);

    let report = restart_with_transport(&fixture.team, false, None, &transport);
    delayed.join().expect("delayed rollout writer should finish");
    let report = report.expect("restart should wait for delayed rollout convergence instead of refusing early");
    let state = load_runtime_state(&fixture.team).unwrap();
    let events = read_events(&fixture.team);

    assert_no_agent_attribution_ambiguous(&state, ["worker_a", "worker_b"]);
    assert_events_have_converging_progress(&events, ["worker_b"]);
    assert_all_live_codex_agents_have_complete_unique_sessions(&state, ["worker_a", "worker_b"]);
    assert_restart_resumed_all_agents(&report, ["worker_a", "worker_b"]);
    assert_all_resume_decisions_are_session_backed(&events, ["worker_a", "worker_b"]);
}

#[test]
#[ignore = "real-machine: provider session filesystem + restart/shutdown lifecycle convergence"]
#[serial(env)]
fn restart_refuses_resume_not_ready_without_teardown_when_delayed_rollout_never_arrives() {
    let fixture = RestartFixture::new("codex-delayed-rollout-never-arrives", "codex");
    let _home = HomeGuard::with_home(fixture.root.join("home"));
    seed_codex_home_rollout_with_agent_hint(
        &fixture.home(),
        &fixture.spawn_cwd,
        "rollout-worker-a-only.jsonl",
        "codex-sess-only-a",
        "worker_a",
        41001,
    );
    seed_two_interacted_running_codex_agents_without_sessions(&fixture.team, &fixture.spawn_cwd);
    let state_before = load_runtime_state(&fixture.team).unwrap();
    let transport = RecordingTransport::new().with_session_present(true);

    let report = restart_with_transport(&fixture.team, false, None, &transport)
        .expect("restart should return a structured refusal, not throw, when session convergence misses its deadline");

    match report {
        RestartReport::RefusedResumeAtomicity { error, .. } => {
            assert!(
                error.contains("resume_not_ready") || error.contains("session convergence"),
                "missing delayed rollout should be reported as resume_not_ready/session convergence, not ordinary resume atomicity; error={error}"
            );
        }
        other => panic!(
            "without --allow-fresh, missing delayed rollout must refuse before teardown, not restart/fresh-start; got {other:?}"
        ),
    }
    assert_eq!(transport.kill_count(), 0, "resume_not_ready refusal must not teardown the existing session");
    assert_eq!(transport.spawn_count(), 0, "resume_not_ready refusal must not spawn fresh workers");
    let state_after = load_runtime_state(&fixture.team).unwrap();
    assert_eq!(
        state_after, state_before,
        "resume_not_ready must be atomic: deadline refusal leaves session_id/rollout_path/attribution fields unchanged; before={state_before} after={state_after}"
    );
    let events = read_events(&fixture.team);
    assert!(
        !events.contains("\"event\":\"restart.resume_decision\"")
            && !events.contains("\"event\": \"restart.resume_decision\""),
        "restart.resume_decision must not be written before all running provider sessions converge; events={events}"
    );
    assert!(
        !events.contains("degraded") && !events.contains(r#""decision":"fresh_start""#),
        "without --allow-fresh, restart must hard-refuse resume_not_ready; it must not choose silent/degraded fresh. events={events}"
    );
}

#[test]
#[ignore = "real-machine: provider session filesystem + restart/shutdown lifecycle convergence"]
#[serial(env)]
fn restart_with_allow_fresh_audit_marks_forced_fresh_true() {
    let fixture = RestartFixture::new("codex-delayed-rollout-allow-fresh", "codex");
    let _home = HomeGuard::with_home(fixture.root.join("home"));
    seed_codex_home_rollout_with_agent_hint(
        &fixture.home(),
        &fixture.spawn_cwd,
        "rollout-worker-a-only.jsonl",
        "codex-sess-only-a",
        "worker_a",
        41001,
    );
    seed_two_interacted_running_codex_agents_without_sessions(&fixture.team, &fixture.spawn_cwd);
    let transport = RecordingTransport::new().with_session_present(true);

    let report = restart_with_transport(&fixture.team, true, None, &transport)
        .expect("allow_fresh may proceed only after auditing delayed rollout convergence failure");
    let RestartReport::Restarted { agents, .. } = report else {
        panic!("--allow-fresh should permit explicit forced fresh path after deadline");
    };
    assert!(
        agents
            .iter()
            .any(|agent| agent.agent_id.as_str() == "worker_b" && agent.decision == ResumeDecision::FreshStart),
        "fixture expects worker_b to be the only delayed rollout miss and explicit forced-fresh candidate; agents={agents:?}"
    );
    let events = read_events(&fixture.team);
    assert!(
        events
            .lines()
            .any(|line| line.contains("restart.resume_decision")
                && line.contains(r#""worker_id":"worker_b""#)
                && line.contains(r#""decision":"fresh_start""#)
                && line.contains(r#""allow_fresh":true"#)
                && line.contains(r#""forced_fresh":true"#)
                && line.contains("resume_not_ready")),
        "--allow-fresh after session convergence miss must audit worker_b as forced_fresh=true with resume_not_ready context, not silent fresh_start; events={events}"
    );
}

#[test]
#[ignore = "real-machine: provider session filesystem + restart/shutdown lifecycle convergence"]
#[serial(env)]
fn restart_waits_for_running_resumable_session_incomplete_agent_even_when_first_send_at_null() {
    let fixture = RestartFixture::new("codex-first-send-null-delayed-rollout", "codex");
    let _home = HomeGuard::with_home(fixture.root.join("home"));
    seed_running_codex_agent_without_session_and_first_send_at(&fixture.team, &fixture.spawn_cwd);
    let delayed_home = fixture.home();
    let delayed_cwd = fixture.spawn_cwd.clone();
    let delayed = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(1_500));
        seed_codex_home_rollout_with_agent_hint(
            &delayed_home,
            &delayed_cwd,
            "rollout-worker-a-first-send-null-arrives-after-first-poll.jsonl",
            "codex-sess-first-send-null-delayed",
            "worker_a",
            41001,
        );
    });
    let transport = RecordingTransport::new().with_session_present(true);

    let report = restart_with_transport(&fixture.team, false, None, &transport);
    delayed.join().expect("delayed rollout writer should finish");
    let report = report.expect("restart should wait for first_send_at=null running resumable worker");
    let events = read_events(&fixture.team);

    assert_convergence_required_missing_contains(&events, "worker_a");
    assert_restarted_with_resume(&report, "codex-sess-first-send-null-delayed");
    assert_events_contain_resume_decision(&fixture.team, "resume", "codex-sess-first-send-null-delayed");
    assert_eq!(
        transport.kill_count(),
        1,
        "restart may teardown only after session convergence captures the delayed rollout; events={events}"
    );
    assert_eq!(
        transport.spawn_count(),
        1,
        "restart must respawn worker_a as resume, not fresh_start; events={events}"
    );
}

#[test]
#[ignore = "real-machine: provider session filesystem + restart/shutdown lifecycle convergence"]
#[serial(env)]
fn restart_refuses_first_send_at_null_running_resumable_session_incomplete_when_rollout_never_arrives() {
    let fixture = RestartFixture::new("codex-first-send-null-never-arrives", "codex");
    let _home = HomeGuard::with_home(fixture.root.join("home"));
    let _deadline = EnvGuard::set("TEAM_AGENT_RESTART_SESSION_CONVERGENCE_DEADLINE_MS", "0");
    seed_running_codex_agent_without_session_and_first_send_at(&fixture.team, &fixture.spawn_cwd);
    let state_before = load_runtime_state(&fixture.team).unwrap();
    let transport = RecordingTransport::new().with_session_present(true);

    let report = restart_with_transport(&fixture.team, false, None, &transport)
        .expect("restart should return structured resume_not_ready refusal");

    match report {
        RestartReport::RefusedResumeAtomicity {
            unresumable: missing,
            allow_fresh,
            error,
        } => {
            assert!(!allow_fresh, "fixture covers hard refusal without --allow-fresh");
            assert!(
                missing.iter().any(|worker| worker.agent_id.as_str() == "worker_a"),
                "running resumable session-incomplete worker_a must be required missing even when first_send_at=null; missing={missing:?}"
            );
            assert!(
                error.contains("resume_not_ready") || error.contains("session_capture_incomplete"),
                "deadline miss must be named resume_not_ready/session_capture_incomplete; error={error}"
            );
        }
        other => panic!(
            "first_send_at=null must not let allow_fresh=false choose silent fresh_start; expected RefusedResumeNotReady, got {other:?}; events={}",
            read_events(&fixture.team)
        ),
    }
    assert_eq!(transport.kill_count(), 0, "resume_not_ready must not teardown existing session");
    assert_eq!(transport.spawn_count(), 0, "resume_not_ready must not spawn fresh worker");
    assert_eq!(
        load_runtime_state(&fixture.team).unwrap(),
        state_before,
        "resume_not_ready hard refusal must leave state unchanged when first_send_at=null"
    );
    let events = read_events(&fixture.team);
    assert_convergence_required_missing_contains(&events, "worker_a");
    assert!(
        !events
            .lines()
            .any(|line| line.contains("restart.resume_decision")
                && line.contains(r#""worker_id":"worker_a""#)
                && line.contains(r#""decision":"fresh_start""#)
                && line.contains(r#""allow_fresh":false"#)),
        "event invariant: pending first_send_at=null worker may not get fresh_start with allow_fresh=false; events={events}"
    );
}

#[test]
#[ignore = "real-machine: provider session filesystem + restart/shutdown lifecycle convergence"]
#[serial(env)]
fn wait_ready_reports_session_capture_incomplete_for_running_resumable_worker_without_session() {
    let fixture = RestartFixture::new("wait-ready-session-incomplete", "codex");
    seed_running_state_without_session(&fixture.team, "codex", &fixture.spawn_cwd);
    let mut state = load_runtime_state(&fixture.team).unwrap();
    let mcp_config = fixture.team.join(".team/runtime/worker_a.mcp.json");
    std::fs::create_dir_all(mcp_config.parent().unwrap()).unwrap();
    std::fs::write(&mcp_config, "{}").unwrap();
    state["agents"]["worker_a"]["pane_id"] = json!("%1");
    state["agents"]["worker_a"]["mcp_config"] = json!(mcp_config.to_string_lossy().to_string());
    save_runtime_state(&fixture.team, &state).unwrap();

    let out = json_result(
        cmd_wait_ready(&WaitReadyArgs {
            workspace: fixture.team.clone(),
            timeout: 0.0,
            json: true,
        })
        .expect("wait-ready should return structured JSON"),
    );

    assert_eq!(
        out["ok"],
        json!(false),
        "wait-ready cannot report ready while a running Codex/Claude worker is missing captured session context; out={out}"
    );
    assert_eq!(
        out["reason"],
        json!("session_capture_incomplete"),
        "readiness reason must distinguish missing resumable session from generic workers_not_ready; out={out}"
    );
    assert!(
        out["readiness"]["session_capture_incomplete"].as_bool() == Some(true)
            && out["readiness"]["all_resumable_have_session"].as_bool() == Some(false),
        "readiness truth table must include all_resumable_have_session and session_capture_incomplete; out={out}"
    );
}

#[test]
fn status_quick_start_and_wait_ready_surfaces_include_session_capture_completeness() {
    let diagnose = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/cli/diagnose.rs"
    ))
    .unwrap();
    let status = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/cli/status_port.rs"
    ))
    .unwrap();
    let launch = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/lifecycle/launch.rs"
    ))
    .unwrap();
    let adapters = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/cli/adapters.rs"
    ))
    .unwrap();

    for (name, source) in [
        ("wait-ready", diagnose.as_str()),
        ("status --json", status.as_str()),
        ("quick-start readiness", launch.as_str()),
        ("quick-start --json", adapters.as_str()),
    ] {
        assert!(
            source.contains("session_capture_incomplete")
                && source.contains("all_resumable_have_session"),
            "C6-C8: {name} must expose readiness = all spawned + attached receiver + all resumable agents have sessions; source lacks session completeness guard"
        );
    }
}

#[test]
fn restart_session_convergence_deadline_is_configurable_and_bounded() {
    let restart = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/lifecycle/restart/rebuild.rs"
    ))
    .unwrap();
    let types = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/cli/types.rs"
    ))
    .unwrap();
    let emit = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/cli/emit.rs"
    ))
    .unwrap();

    assert!(
        restart.contains("session_converge_deadline")
            || restart.contains("SessionConvergenceDeadline"),
        "C14: restart must have a bounded session convergence deadline, not one opportunistic pass or an unbounded wait"
    );
    assert!(
        restart.contains("10") || restart.contains("15"),
        "C14: default convergence deadline should be a bounded 10-15s class value; restart={restart}"
    );
    assert!(
        types.contains("session_converge_deadline")
            && emit.contains("--session-converge-deadline")
            && emit.contains("deadline"),
        "C14: tests/scenarios need an explicit deadline knob; CLI types/parser must expose --session-converge-deadline for restart"
    );
}

#[test]
fn attribution_ambiguous_is_final_only_after_convergence_deadline() {
    let common = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/lifecycle/restart/common.rs"
    ))
    .unwrap();
    let tick = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/coordinator/tick.rs"
    ))
    .unwrap();

    assert!(
        !common.contains(r#""attribution_ambiguous""#)
            || common.contains("deadline_expired")
            || common.contains("final_ambiguous"),
        "C9-C11/C16: restart capture must not durably write attribution_ambiguous during an in-window poll; final ambiguous is allowed only after deadline with unallocatable candidates"
    );
    assert!(
        tick.contains("provider.session.attribution_ambiguous"),
        "C16 guard: opportunistic coordinator tick may still record final attribution ambiguity when it is not the restart convergence gate; tick capture must not be deleted"
    );
}

#[test]
fn coordinator_tick_session_capture_stays_opportunistic_not_restart_barrier() {
    let tick = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/coordinator/tick.rs"
    ))
    .unwrap();

    assert!(
        !tick.contains("session_converge_deadline")
            && !tick.contains("provider.session.converging")
            && !tick.contains("resume_not_ready"),
        "C15/C16: only destructive lifecycle restart uses the convergence barrier; coordinator tick capture remains opportunistic and must not block on restart resume readiness"
    );
}

#[test]
fn session_capture_enumerates_candidates_and_carries_per_agent_context_grep_guard() {
    let adapter = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/provider/adapter.rs"
    ))
    .unwrap();
    let tick = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/coordinator/tick.rs"
    ))
    .unwrap();
    let restart = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/lifecycle/restart/common.rs"
    ))
    .unwrap();
    let launch = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/lifecycle/launch.rs"
    ))
    .unwrap();

    assert!(
        adapter.contains("capture_session_candidates")
            || adapter.contains("matching_session_candidates")
            || adapter.contains("SessionAttributionContext"),
        "C9: provider capture must expose/enumerate all cwd-matching session candidates instead of returning only the first path-ordered candidate"
    );
    assert!(
        !tick.contains(".capture_session_id(") && !restart.contains(".capture_session_id("),
        "C9/C10: coordinator tick and restart refresh must allocate across all candidates with per-agent context, not call capture_session_id(agent,cwd) one agent at a time"
    );
    assert!(
        adapter.contains("pane_pid") && adapter.contains("spawned_at") && adapter.contains("TEAM_AGENT_ID"),
        "C10/C11: attribution context/decision must include pane_pid, per-agent spawned_at/process time, and positive MCP env TEAM_AGENT_ID before bounded fallback"
    );
    assert!(
        launch.contains("spawned_at") && !launch.contains("let spawned_at = spawn_timestamp();"),
        "C10: launch must persist a per-agent spawned_at/process time, not one shared timestamp for the whole launch"
    );
}

#[test]
#[ignore = "real-machine: provider session filesystem + restart/shutdown lifecycle convergence"]
#[serial(env)]
fn restart_force_capture_matches_workspace_spawn_cwd_to_team_subdir_across_symlink() {
    let fixture = RestartFixture::new_symlinked_workspace("restart-workspace-team-cwd", "codex");
    let _home = HomeGuard::with_home(fixture.root.join("home"));
    let codex_session = "codex-sess-resume-team-subdir";
    seed_codex_home_rollout_with_cwd(&fixture.home(), &fixture.team, codex_session);
    seed_running_state_without_session(&fixture.team, "codex", &fixture.spawn_cwd);
    let transport = RecordingTransport::new().with_session_present(true);

    let report = restart_with_transport(&fixture.team, false, None, &transport)
        .expect("restart should succeed after forced session capture from team-subdir rollout cwd");

    assert_restarted_with_resume(&report, codex_session);
    assert_state_session_id(&fixture.team, codex_session);
    assert_events_contain_resume_decision(&fixture.team, "resume", codex_session);
}

#[test]
#[ignore = "real-machine: provider session filesystem + restart/shutdown lifecycle convergence"]
#[serial(env)]
fn provider_home_codex_rollout_from_foreign_workspace_is_not_captured() {
    let fixture = RestartFixture::new_symlinked_workspace("codex-foreign-workspace", "codex");
    let _home = HomeGuard::with_home(fixture.root.join("home"));
    let foreign = fixture.root.join("foreign-workspace").join("teamdir");
    std::fs::create_dir_all(&foreign).unwrap();
    seed_codex_home_rollout_with_cwd(&fixture.home(), &foreign, "codex-sess-foreign");
    seed_running_state_without_session(&fixture.team, "codex", &fixture.spawn_cwd);

    let adapter = get_adapter(Provider::Codex);
    let captured = adapter
        .capture_session_id("worker_a", &fixture.spawn_cwd, 0)
        .expect("Codex provider-home capture should not error");

    assert!(
        captured.is_none(),
        "workspace↔team-subdir matching must stay scoped to this worker workspace; foreign rollout cwd must not be captured: {captured:?}"
    );
}

#[test]
#[ignore = "real-machine: provider session filesystem + restart/shutdown lifecycle convergence"]
#[serial(env)]
fn provider_home_claude_session_is_captured_for_restart_resume() {
    let fixture = RestartFixture::new("claude-home-capture", "claude");
    let _home = HomeGuard::with_home(fixture.root.join("home"));
    let claude_session = "claude-sess-240";
    seed_claude_home_session(&fixture.home(), &fixture.spawn_cwd, claude_session);
    seed_running_state_without_session(&fixture.team, "claude", &fixture.spawn_cwd);

    let adapter = get_adapter(Provider::Claude);
    let captured = adapter
        .capture_session_id("worker_a", &fixture.spawn_cwd, 0)
        .expect("Claude provider-home capture should not error")
        .expect("Claude session under HOME/.claude/sessions must be discoverable before restart");

    assert_eq!(
        captured.session_id.as_ref().map(|id| id.as_str()),
        Some(claude_session),
        "Claude capture must recover sessionId from provider-home session JSON"
    );
    assert!(
        captured
            .rollout_path
            .as_ref()
            .is_some_and(|path| path.as_path().starts_with(fixture.home().join(".claude/sessions"))),
        "Claude capture must scan provider home, not only spawn_cwd; captured={captured:?}"
    );
}

#[test]
#[ignore = "real-machine: provider session filesystem + restart/shutdown lifecycle convergence"]
#[serial(env)]
fn restart_force_captures_real_codex_session_meta_before_teardown() {
    let fixture = RestartFixture::new("restart-codex-force-capture", "codex");
    let _home = HomeGuard::with_home(fixture.root.join("home"));
    let codex_session = "codex-sess-before-kill";
    seed_codex_home_rollout(&fixture.home(), &fixture.spawn_cwd, codex_session);
    seed_running_state_without_session(&fixture.team, "codex", &fixture.spawn_cwd);
    let transport = RecordingTransport::new().with_session_present(true);

    let report = restart_with_transport(&fixture.team, false, None, &transport)
        .expect("restart should succeed after forced session capture");

    assert_restarted_with_resume(&report, codex_session);
    assert_spawn_contains_ordered(
        &transport.single_spawn_argv(),
        &["resume", codex_session],
        "Codex restart must spawn `codex ... resume <session_id>`",
    );
    assert_state_session_id(&fixture.team, codex_session);
    assert_events_contain_resume_decision(&fixture.team, "resume", codex_session);
    assert!(
        transport.kill_count() <= 1,
        "transport should not need multiple destructive teardowns; kill_count={}",
        transport.kill_count()
    );
}

#[test]
#[ignore = "real-machine: provider session filesystem + restart/shutdown lifecycle convergence"]
#[serial(env)]
fn restart_force_captures_claude_session_before_teardown() {
    let fixture = RestartFixture::new("restart-claude-force-capture", "claude");
    let _home = HomeGuard::with_home(fixture.root.join("home"));
    let claude_session = "claude-sess-before-kill";
    seed_claude_home_session(&fixture.home(), &fixture.spawn_cwd, claude_session);
    seed_running_state_without_session(&fixture.team, "claude", &fixture.spawn_cwd);
    let transport = RecordingTransport::new().with_session_present(true);

    let report = restart_with_transport(&fixture.team, false, None, &transport)
        .expect("restart should succeed after forced session capture");

    assert_restarted_with_resume(&report, claude_session);
    assert_spawn_contains_adjacent(
        &transport.single_spawn_argv(),
        &["--resume", claude_session],
        "Claude restart must spawn `claude ... --resume <session_id>`",
    );
    assert_state_session_id(&fixture.team, claude_session);
    assert_events_contain_resume_decision(&fixture.team, "resume", claude_session);
}

#[test]
#[ignore = "real-machine: provider session filesystem + restart/shutdown lifecycle convergence"]
#[serial(env)]
fn shutdown_force_captures_missing_provider_session_before_kill() {
    let fixture = RestartFixture::new("shutdown-force-capture", "codex");
    let _home = HomeGuard::with_home(fixture.root.join("home"));
    let codex_session = "codex-sess-before-shutdown";
    seed_codex_home_rollout(&fixture.home(), &fixture.spawn_cwd, codex_session);
    seed_running_state_without_session_no_coordinator(&fixture.team, "codex", &fixture.spawn_cwd);
    let transport = RecordingTransport::new().with_session_present(true);

    let result = team_agent::cli::lifecycle_port::shutdown_with_transport(
        &fixture.team,
        true,
        None,
        &transport,
    )
    .expect("shutdown should complete after pre-kill session capture");

    assert_eq!(result.get("ok").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(transport.kill_count(), 1, "shutdown fixture should kill the team session once");
    assert_state_session_id(&fixture.team, codex_session);
}

#[test]
#[ignore = "real-machine: provider session filesystem + restart/shutdown lifecycle convergence"]
fn persisted_session_id_is_the_resume_authority_for_restart_plan_and_spawn_argv() {
    let codex = RestartFixture::new("codex-resume-authority", "codex");
    seed_running_state_with_session(&codex.team, "codex", &codex.spawn_cwd, "codex-known-session");
    let codex_state = load_runtime_state(&codex.team).unwrap();
    let codex_plan = classify_restart_plan(&codex_state, false).unwrap();
    assert_eq!(codex_plan.decisions.len(), 1);
    assert_eq!(codex_plan.decisions[0].decision, ResumeDecision::Resume);
    assert_eq!(codex_plan.decisions[0].restart_mode, StartMode::Resumed);

    let codex_transport = RecordingTransport::new().with_session_present(true);
    let codex_report = restart_with_transport(&codex.team, false, None, &codex_transport)
        .expect("persisted Codex session_id must be resumable");
    assert_restarted_with_resume(&codex_report, "codex-known-session");
    assert_spawn_contains_ordered(
        &codex_transport.single_spawn_argv(),
        &["resume", "codex-known-session"],
        "Codex restart must use the persisted session id",
    );

    let claude = RestartFixture::new("claude-resume-authority", "claude");
    seed_running_state_with_session(&claude.team, "claude", &claude.spawn_cwd, "claude-known-session");
    let claude_transport = RecordingTransport::new().with_session_present(true);
    let claude_report = restart_with_transport(&claude.team, false, None, &claude_transport)
        .expect("persisted Claude session_id must be resumable");
    assert_restarted_with_resume(&claude_report, "claude-known-session");
    assert_spawn_contains_adjacent(
        &claude_transport.single_spawn_argv(),
        &["--resume", "claude-known-session"],
        "Claude restart must use the persisted session id",
    );
}

fn assert_restarted_with_resume(report: &RestartReport, expected_session: &str) {
    match report {
        RestartReport::Restarted { agents, .. } => {
            assert_eq!(agents.len(), 1, "fixture has exactly one worker; agents={agents:?}");
            assert_eq!(agents[0].decision, ResumeDecision::Resume);
            assert_eq!(agents[0].restart_mode, StartMode::Resumed);
            assert_eq!(
                agents[0].session_id.as_ref().map(|id| id.as_str()),
                Some(expected_session)
            );
        }
        other => panic!("restart must resume after pre-kill capture, got {other:?}"),
    }
}

fn assert_restart_resumed_all_agents<const N: usize>(report: &RestartReport, expected_agents: [&str; N]) {
    match report {
        RestartReport::Restarted { agents, .. } => {
            let resumed = agents
                .iter()
                .map(|agent| {
                    (
                        agent.agent_id.as_str(),
                        (
                            agent.decision,
                            agent.restart_mode,
                            agent.session_id.as_ref().map(|id| id.as_str()),
                        ),
                    )
                })
                .collect::<std::collections::BTreeMap<_, _>>();
            for agent_id in expected_agents {
                let Some((decision, mode, session_id)) = resumed.get(agent_id).copied() else {
                    panic!("restart must report a decision for live Codex agent {agent_id}; resumed={resumed:?}");
                };
                assert_eq!(
                    decision,
                    ResumeDecision::Resume,
                    "restart must not fresh-start live Codex agent {agent_id} after session capture; resumed={resumed:?}"
                );
                assert_eq!(
                    mode,
                    StartMode::Resumed,
                    "restart spawn mode must be resumed for live Codex agent {agent_id}; resumed={resumed:?}"
                );
                assert!(
                    session_id.is_some_and(|id| !id.is_empty()),
                    "restart resume decision for {agent_id} must carry the captured session_id; resumed={resumed:?}"
                );
            }
        }
        other => panic!("restart must resume every live Codex agent after capture, got {other:?}"),
    }
}

fn assert_all_live_codex_agents_have_complete_unique_sessions<const N: usize>(
    state: &serde_json::Value,
    expected_agents: [&str; N],
) {
    let mut pairs = std::collections::BTreeSet::new();
    for agent_id in expected_agents {
        let agent = state
            .get("agents")
            .and_then(|agents| agents.get(agent_id))
            .unwrap_or_else(|| panic!("state must contain live Codex agent {agent_id}; state={state}"));
        let session_id = agent
            .get("session_id")
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| {
                panic!("live Codex agent {agent_id} must have a nonempty captured session_id before restart teardown; state={state}")
            });
        let rollout_path = agent
            .get("rollout_path")
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| {
                panic!("live Codex agent {agent_id} must have a nonempty captured rollout_path before restart teardown; state={state}")
            });
        assert!(
            pairs.insert((session_id.to_string(), rollout_path.to_string())),
            "Codex session capture must allocate a unique nonempty (session_id, rollout_path) pair per live agent; duplicate for {agent_id}; state={state}"
        );
    }
}

fn assert_all_resume_decisions_are_session_backed<const N: usize>(events: &str, expected_agents: [&str; N]) {
    for agent_id in expected_agents {
        let matching = events
            .lines()
            .filter(|line| {
                line.contains("restart.resume_decision")
                    && line.contains(&format!(r#""worker_id":"{agent_id}""#))
            })
            .collect::<Vec<_>>();
        assert!(
            !matching.is_empty(),
            "restart must write a resume decision for {agent_id} only after session convergence; events={events}"
        );
        for line in matching {
            assert!(
                line.contains(r#""decision":"resume""#) && line.contains(r#""has_session_id":true"#),
                "restart.resume_decision for {agent_id} must be session-backed resume, never fresh/refuse before delayed rollout convergence; line={line}"
            );
        }
    }
    assert!(
        !events.contains(r#""decision":"fresh_start""#) && !events.contains(r#""decision":"refuse""#),
        "restart must not emit fresh/refuse resume decisions while delayed rollout convergence is still possible; events={events}"
    );
}

fn read_events(team: &Path) -> String {
    std::fs::read_to_string(team.join(".team/logs/events.jsonl")).unwrap_or_default()
}

fn json_result(result: team_agent::cli::CmdResult) -> Value {
    match result.output {
        CmdOutput::Json(value) => value,
        other => panic!("expected JSON command output, got {other:?}"),
    }
}

fn assert_no_agent_attribution_ambiguous<const N: usize>(state: &Value, expected_agents: [&str; N]) {
    for agent_id in expected_agents {
        let agent = state
            .get("agents")
            .and_then(|agents| agents.get(agent_id))
            .unwrap_or_else(|| panic!("state must contain agent {agent_id}; state={state}"));
        assert!(
            agent.get("attribution_ambiguous").and_then(Value::as_bool) != Some(true),
            "C9-C11: delayed rollout convergence must not write durable attribution_ambiguous during the transient window; agent={agent} state={state}"
        );
    }
}

fn assert_events_have_converging_progress<const N: usize>(events: &str, pending_agents: [&str; N]) {
    let progress = events
        .lines()
        .filter(|line| line.contains("provider.session.converging"))
        .collect::<Vec<_>>();
    assert!(
        !progress.is_empty(),
        "C12: every delayed capture poll must emit provider.session.converging progress instead of silently waiting; events={events}"
    );
    for line in progress {
        assert!(
            line.contains("elapsed_ms")
                && line.contains("deadline_ms")
                && line.contains("pending_agent_ids"),
            "C12/C14: converging progress event must expose elapsed_ms, deadline_ms, and pending_agent_ids; line={line}"
        );
    }
    for agent_id in pending_agents {
        assert!(
            events
                .lines()
                .any(|line| line.contains("provider.session.converging") && line.contains(agent_id)),
            "C12: pending agent {agent_id} must be named in convergence progress events; events={events}"
        );
    }
}

fn assert_convergence_required_missing_contains(events: &str, agent_id: &str) {
    let matching = events
        .lines()
        .filter(|line| line.contains("provider.session.converging"))
        .collect::<Vec<_>>();
    assert!(
        !matching.is_empty(),
        "restart must emit convergence progress before deciding resume/fresh/refuse; events={events}"
    );
    assert!(
        matching.iter().any(|line| {
            let expected_missing = format!(r#""missing":["{agent_id}"]"#);
            let expected_required = format!(r#""required_missing_agent_ids":["{agent_id}"]"#);
            line.contains(&expected_missing) || line.contains(&expected_required)
        }),
        "running resumable session-incomplete agent {agent_id} must be in required missing even when first_send_at=null; convergence_events={matching:?}"
    );
}

fn agent_session<'a>(state: &'a serde_json::Value, agent_id: &str) -> Option<&'a str> {
    state
        .get("agents")
        .and_then(|agents| agents.get(agent_id))
        .and_then(|agent| agent.get("session_id"))
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.is_empty())
}

fn assert_spawn_contains_adjacent(argv: &[String], expected: &[&str], message: &str) {
    assert!(
        argv.windows(expected.len())
            .any(|window| window.iter().map(String::as_str).eq(expected.iter().copied())),
        "{message}; argv={argv:?}"
    );
}

fn assert_spawn_contains_ordered(argv: &[String], expected: &[&str], message: &str) {
    let mut index = 0;
    for arg in argv {
        if expected.get(index).is_some_and(|needle| arg == needle) {
            index += 1;
        }
    }
    assert_eq!(
        index,
        expected.len(),
        "{message}; expected ordered tokens {expected:?}; argv={argv:?}"
    );
}

fn assert_state_session_id(team: &Path, expected_session: &str) {
    let state = load_runtime_state(team).unwrap();
    assert_eq!(
        state
            .get("agents")
            .and_then(|agents| agents.get("worker_a"))
            .and_then(|agent| agent.get("session_id"))
            .and_then(|value| value.as_str()),
        Some(expected_session),
        "destructive lifecycle must persist captured session_id as resume authority before teardown; state={state}"
    );
}

fn assert_events_contain_resume_decision(team: &Path, expected_decision: &str, expected_session: &str) {
    let events = std::fs::read_to_string(team.join(".team/logs/events.jsonl")).unwrap();
    let needle = format!(
        r#""event":"restart.resume_decision".*"decision":"{expected_decision}".*"has_session_id":true.*"session_id":"{expected_session}""#
    );
    assert!(
        events.lines().any(|line| {
            line.contains(r#""event":"restart.resume_decision""#)
                && line.contains(&format!(r#""decision":"{expected_decision}""#))
                && line.contains(r#""has_session_id":true"#)
                && line.contains(&format!(r#""session_id":"{expected_session}""#))
        }),
        "restart must audit a resume decision with captured session_id; missing pattern {needle}; events={events}"
    );
}

fn seed_codex_home_rollout(home: &Path, spawn_cwd: &Path, session_id: &str) {
    seed_codex_home_rollout_with_cwd(home, spawn_cwd, session_id);
}

fn seed_codex_home_rollout_with_cwd(home: &Path, rollout_cwd: &Path, session_id: &str) {
    let dir = home.join(".codex/sessions/2026/06/07");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join(format!("rollout-worker-a-{session_id}.jsonl")),
        format!(
            "{}\n{}\n",
            json!({
                "type": "session_meta",
                "payload": {
                    "id": session_id,
                    "cwd": rollout_cwd.to_string_lossy().to_string()
                }
            }),
            json!({"type":"event","payload":{"message":"later record"}})
        ),
    )
    .unwrap();
}

fn seed_codex_home_rollout_with_agent_hint(
    home: &Path,
    rollout_cwd: &Path,
    file_name: &str,
    session_id: &str,
    agent_id: &str,
    pane_pid: u32,
) {
    let dir = home.join(".codex/sessions/2026/06/07");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join(file_name),
        format!(
            "{}\n{}\n",
            json!({
                "type": "session_meta",
                "payload": {
                    "id": session_id,
                    "cwd": rollout_cwd.to_string_lossy().to_string()
                }
            }),
            json!({
                "type": "event",
                "payload": {
                    "pane_pid": pane_pid,
                    "mcp_servers": {
                        "team_orchestrator": {
                            "env": {
                                "TEAM_AGENT_ID": agent_id
                            }
                        }
                    }
                }
            })
        ),
    )
    .unwrap();
}

fn seed_claude_home_session(home: &Path, spawn_cwd: &Path, session_id: &str) {
    let dir = home.join(".claude/sessions");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("worker-a-session.json"),
        serde_json::to_string(&json!({
            "sessionId": session_id,
            "cwd": spawn_cwd.to_string_lossy().to_string()
        }))
        .unwrap(),
    )
    .unwrap();
}

fn seed_running_state_without_session(team: &Path, provider: &str, spawn_cwd: &Path) {
    seed_running_state(team, provider, spawn_cwd, None);
    seed_healthy_coordinator(team);
}

fn seed_running_state_without_session_no_coordinator(team: &Path, provider: &str, spawn_cwd: &Path) {
    seed_running_state(team, provider, spawn_cwd, None);
}

fn seed_running_state_with_session(team: &Path, provider: &str, spawn_cwd: &Path, session_id: &str) {
    seed_running_state(team, provider, spawn_cwd, Some(session_id));
    seed_healthy_coordinator(team);
}

fn seed_two_running_codex_agents_without_sessions(team: &Path, spawn_cwd: &Path) {
    save_runtime_state(
        team,
        &json!({
            "active_team_key": "ctxteam",
            "team_dir": team.to_string_lossy().to_string(),
            "spec_path": team.join("team.spec.yaml").to_string_lossy().to_string(),
            "session_name": "team-ctxteam",
            "agents": {
                "worker_a": {
                    "status": "running",
                    "provider": "codex",
                    "role": "Worker",
                    "tools": ["mcp_team"],
                    "window": "worker_a",
                    "owner_team_id": "ctxteam",
                    "session_id": null,
                    "rollout_path": null,
                    "spawn_cwd": spawn_cwd.to_string_lossy().to_string(),
                    "spawned_at": "2026-06-07T00:00:00+00:00",
                    "pane_pid": 41001
                },
                "worker_b": {
                    "status": "running",
                    "provider": "codex",
                    "role": "Worker",
                    "tools": ["mcp_team"],
                    "window": "worker_b",
                    "owner_team_id": "ctxteam",
                    "session_id": null,
                    "rollout_path": null,
                    "spawn_cwd": spawn_cwd.to_string_lossy().to_string(),
                    "spawned_at": "2026-06-07T00:00:07+00:00",
                    "pane_pid": 41002
                }
            }
        }),
    )
    .unwrap();
    seed_healthy_coordinator(team);
}

fn seed_two_interacted_running_codex_agents_without_sessions(team: &Path, spawn_cwd: &Path) {
    seed_two_running_codex_agents_without_sessions(team, spawn_cwd);
    let mut state = load_runtime_state(team).unwrap();
    for (agent_id, first_send_at) in [
        ("worker_a", "2026-06-07T00:02:00+00:00"),
        ("worker_b", "2026-06-07T00:02:07+00:00"),
    ] {
        if let Some(agent) = state
            .get_mut("agents")
            .and_then(serde_json::Value::as_object_mut)
            .and_then(|agents| agents.get_mut(agent_id))
            .and_then(serde_json::Value::as_object_mut)
        {
            agent.insert("first_send_at".to_string(), serde_json::json!(first_send_at));
        }
    }
    save_runtime_state(team, &state).unwrap();
}

fn seed_running_codex_agent_without_session_and_first_send_at(team: &Path, spawn_cwd: &Path) {
    seed_running_state(team, "codex", spawn_cwd, None);
    let mut state = load_runtime_state(team).unwrap();
    if let Some(agent) = state
        .get_mut("agents")
        .and_then(serde_json::Value::as_object_mut)
        .and_then(|agents| agents.get_mut("worker_a"))
        .and_then(serde_json::Value::as_object_mut)
    {
        agent.insert("first_send_at".to_string(), serde_json::Value::Null);
        agent.insert("rollout_path".to_string(), serde_json::Value::Null);
        agent.insert("pane_id".to_string(), serde_json::json!("%1"));
        agent.insert("pane_pid".to_string(), serde_json::json!(41001));
        agent.insert("mcp_ready".to_string(), serde_json::json!(true));
        agent.insert(
            "startup_prompts".to_string(),
            serde_json::json!("complete"),
        );
    }
    save_runtime_state(team, &state).unwrap();
    seed_true_machine_interaction_evidence(team);
}

fn seed_true_machine_interaction_evidence(team: &Path) {
    let path = team.join(".team/logs/events.jsonl");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(
        path,
        [
            serde_json::json!({
                "event": "send.delivered",
                "message_id": "msg_true_machine_leader_to_worker",
                "from": "leader",
                "to": "worker_a",
                "owner_team_id": "ctxteam",
                "status": "delivered"
            })
            .to_string(),
            serde_json::json!({
                "event": "turn_open.armed_after_delivery",
                "agent_id": "worker_a",
                "owner_team_id": "ctxteam",
                "pane_id": "%1"
            })
            .to_string(),
            serde_json::json!({
                "event": "mcp.report_result",
                "agent_id": "worker_a",
                "owner_team_id": "ctxteam",
                "status": "delivered"
            })
            .to_string(),
        ]
        .join("\n")
            + "\n",
    )
    .unwrap();
}

fn seed_running_state(team: &Path, provider: &str, spawn_cwd: &Path, session_id: Option<&str>) {
    save_runtime_state(
        team,
        &json!({
            "active_team_key": "ctxteam",
            "team_dir": team.to_string_lossy().to_string(),
            "spec_path": team.join("team.spec.yaml").to_string_lossy().to_string(),
            "session_name": "team-ctxteam",
            "agents": {
                "worker_a": {
                    "status": "running",
                    "provider": provider,
                    "role": "Worker",
                    "tools": ["mcp_team"],
                    "window": "worker_a",
                    "owner_team_id": "ctxteam",
                    "session_id": session_id,
                    "rollout_path": null,
                    "spawn_cwd": spawn_cwd.to_string_lossy().to_string(),
                    "spawned_at": "2026-06-07T00:00:00+00:00",
                    "first_send_at": "2026-06-07T00:01:00+00:00"
                }
            }
        }),
    )
    .unwrap();
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

struct RestartFixture {
    root: PathBuf,
    team: PathBuf,
    spawn_cwd: PathBuf,
}

impl RestartFixture {
    fn new(label: &str, provider: &str) -> Self {
        let root = tmp_dir(label);
        let team = root.join("teamdir");
        std::fs::create_dir_all(team.join("agents")).unwrap();
        std::fs::write(
            team.join("TEAM.md"),
            "---\nname: ctxteam\nobjective: Restart session context contract.\nprovider: codex\n---\n\nTeam.\n",
        )
        .unwrap();
        std::fs::write(
            team.join("agents/worker_a.md"),
            format!(
                "---\nname: worker_a\nrole: Worker\nprovider: {provider}\nmodel: {}\nauth_mode: subscription\ntools:\n  - mcp_team\n---\n\nWorker.\n",
                if provider == "claude" {
                    "claude-sonnet-4-6"
                } else {
                    "gpt-5.5"
                }
            ),
        )
        .unwrap();
        let spec = team_agent::compiler::compile_team(&team).unwrap();
        std::fs::write(
            team.join("team.spec.yaml"),
            team_agent::model::yaml::dumps(&spec),
        )
        .unwrap();
        let spawn_cwd = root.join("spawn-cwd");
        std::fs::create_dir_all(&spawn_cwd).unwrap();
        Self {
            root,
            team,
            spawn_cwd,
        }
    }

    fn new_symlinked_workspace(label: &str, provider: &str) -> Self {
        let root = tmp_dir(label);
        let workspace = root.join("real-workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let team = workspace.join("teamdir");
        std::fs::create_dir_all(team.join("agents")).unwrap();
        std::fs::write(
            team.join("TEAM.md"),
            "---\nname: ctxteam\nobjective: Restart session context contract.\nprovider: codex\n---\n\nTeam.\n",
        )
        .unwrap();
        std::fs::write(
            team.join("agents/worker_a.md"),
            format!(
                "---\nname: worker_a\nrole: Worker\nprovider: {provider}\nmodel: {}\nauth_mode: subscription\ntools:\n  - mcp_team\n---\n\nWorker.\n",
                if provider == "claude" {
                    "claude-sonnet-4-6"
                } else {
                    "gpt-5.5"
                }
            ),
        )
        .unwrap();
        let spec = team_agent::compiler::compile_team(&team).unwrap();
        std::fs::write(
            team.join("team.spec.yaml"),
            team_agent::model::yaml::dumps(&spec),
        )
        .unwrap();
        let spawn_cwd = root.join("workspace-symlink");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&workspace, &spawn_cwd).unwrap();
        #[cfg(not(unix))]
        {
            std::fs::create_dir_all(&spawn_cwd).unwrap();
        }
        Self {
            root,
            team,
            spawn_cwd,
        }
    }

    fn home(&self) -> PathBuf {
        self.root.join("home")
    }
}

fn tmp_dir(tag: &str) -> PathBuf {
    static N: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "ta-rs-restart-session-capture-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::canonicalize(dir).unwrap()
}

struct HomeGuard {
    previous_home: Option<String>,
}

impl HomeGuard {
    fn with_home(home: PathBuf) -> Self {
        std::fs::create_dir_all(&home).unwrap();
        let previous_home = std::env::var("HOME").ok();
        unsafe {
            std::env::set_var("HOME", home);
        }
        Self { previous_home }
    }
}

impl Drop for HomeGuard {
    fn drop(&mut self) {
        unsafe {
            if let Some(value) = self.previous_home.take() {
                std::env::set_var("HOME", value);
            } else {
                std::env::remove_var("HOME");
            }
        }
    }
}

struct EnvGuard {
    key: &'static str,
    previous: Option<String>,
}

impl EnvGuard {
    fn set(key: &'static str, value: &str) -> Self {
        let previous = std::env::var(key).ok();
        unsafe {
            std::env::set_var(key, value);
        }
        Self { key, previous }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        unsafe {
            if let Some(value) = self.previous.take() {
                std::env::set_var(self.key, value);
            } else {
                std::env::remove_var(self.key);
            }
        }
    }
}

#[derive(Debug, Clone)]
struct RecordedSpawn {
    argv: Vec<String>,
}

#[derive(Debug, Default)]
struct RecordingTransport {
    spawns: Mutex<Vec<RecordedSpawn>>,
    kills: Mutex<u32>,
    session_present: bool,
}

impl RecordingTransport {
    fn new() -> Self {
        Self::default()
    }

    fn with_session_present(mut self, present: bool) -> Self {
        self.session_present = present;
        self
    }

    fn single_spawn_argv(&self) -> Vec<String> {
        let spawns = self.spawns.lock().unwrap();
        assert_eq!(
            spawns.len(),
            1,
            "fixture should record exactly one worker spawn; spawns={spawns:?}"
        );
        spawns[0].argv.clone()
    }

    fn kill_count(&self) -> u32 {
        *self.kills.lock().unwrap()
    }

    fn spawn_count(&self) -> usize {
        self.spawns.lock().unwrap().len()
    }

    fn spawn_result(
        &self,
        session: &SessionName,
        window: &WindowName,
        argv: &[String],
    ) -> SpawnResult {
        let mut spawns = self.spawns.lock().unwrap();
        spawns.push(RecordedSpawn {
            argv: argv.to_vec(),
        });
        SpawnResult {
            pane_id: PaneId::new(format!("%{}", spawns.len())),
            session: session.clone(),
            window: window.clone(),
            child_pid: None,
        }
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
        _cwd: &Path,
        _env: &BTreeMap<String, String>,
    ) -> Result<SpawnResult, TransportError> {
        Ok(self.spawn_result(session, window, argv))
    }

    fn spawn_into(
        &self,
        session: &SessionName,
        window: &WindowName,
        argv: &[String],
        _cwd: &Path,
        _env: &BTreeMap<String, String>,
    ) -> Result<SpawnResult, TransportError> {
        Ok(self.spawn_result(session, window, argv))
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
        field: PaneField,
    ) -> Result<Option<String>, TransportError> {
        match field {
            PaneField::PaneWidth => Ok(Some("120".to_string())),
            _ => Ok(None),
        }
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

    fn has_session(&self, _session: &SessionName) -> Result<bool, TransportError> {
        Ok(self.session_present)
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
        *self.kills.lock().unwrap() += 1;
        Ok(())
    }

    fn kill_window(&self, _target: &Target) -> Result<(), TransportError> {
        Ok(())
    }

    fn attach_session(&self, _session: &SessionName) -> Result<AttachOutcome, TransportError> {
        Ok(AttachOutcome::Attached)
    }
}
