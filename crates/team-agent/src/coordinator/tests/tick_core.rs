use super::*;

// ═════════════════════════════════════════════════════════════════════════
// GROUP I/J — tick orchestration (§10 no-panic + bug-084 degraded + tick ORDER
//   + §84 zero-injection) and health/start/stop outcomes — RED via unimplemented!().
//   These are this lane's #1 invariants. `coord_for_test` constructs a real
//   `Coordinator` over a temp workspace with an injected MockTransport + MockRegistry
//   (+ optional save-failure hook + ORDER recorder), so the contracts below ASSERT
//   concrete golden against the unimplemented production `tick`/`health`/`start`/`stop`.
// ═════════════════════════════════════════════════════════════════════════

#[test]
fn tick_never_panics_returns_ok_tickreport_on_clean_state() {
    // §10: daemon-path tick(..) -> Result<TickReport, TickError>; a clean, no-obligation
    // workspace with a live tmux session must yield Ok(TickReport) — never panic, never Err.
    let (coord, _calls) = coord_for_test(/*session_present=*/ true, None, None);
    let report = coord.tick();
    let report = report.expect("tick must not Err on clean state");
    assert!(report.ok, "clean tick is ok=true");
    assert!(!report.stop, "clean tick does not stop the main loop");
    assert_eq!(report.reason, None, "clean tick has no degraded/stop reason");
    assert_eq!(report.persisted, Some(true), "clean tick persisted state");
}

#[test]
fn tick_save_failure_returns_degraded_not_panic_not_err() {
    // bug-084 (lifecycle.py:345-363): tick-end save_runtime_state failure => degraded
    // TickReport{ok:false, reason:PersistenceDegraded, persisted:Some(false), stop:false}
    // returned as Ok — NOT a panic, NOT an Err (the main loop must NOT catch+backoff this).
    let (coord, _calls) = coord_for_test(true, Some(failing_save_hook()), None);
    let report = coord.tick();
    let report = report.expect("save failure is a DEGRADED Ok, NOT an Err (main loop must not backoff)");
    assert!(!report.ok, "bug-084: degraded => ok=false");
    assert_eq!(
        report.reason,
        Some(TickStopReason::PersistenceDegraded),
        "bug-084: reason=persistence_degraded"
    );
    assert_eq!(report.persisted, Some(false), "bug-084: persisted=Some(false)");
    assert!(!report.stop, "bug-084: stop=false (degrade, do NOT exit main loop)");
}

#[test]
fn tick_tmux_session_missing_returns_stop_true() {
    // lifecycle.py:277-279 — a TRUTHY session_name whose tmux session is gone =>
    // {ok:false, stop:true, reason:tmux_session_missing} (triggers main-loop break). The
    // gate is the SECOND step after load_runtime_state, BEFORE any capture/refresh/prompt
    // side-effects. (A null/empty session_name skips the gate entirely — see
    // p2_tick_skips_tmux_gate_when_session_name_absent.)
    let (coord, _calls) =
        coord_for_test_with_session(/*session_present=*/ false, "team-missing");
    let report = coord.tick().expect("session-missing is a typed report, not an Err");
    assert!(!report.ok, "session missing => ok=false");
    assert!(report.stop, "session missing => stop=true (break main loop)");
    assert_eq!(
        report.reason,
        Some(TickStopReason::TmuxSessionMissing),
        "reason=tmux_session_missing"
    );
}

#[test]
fn tick_side_effect_order_is_the_fixed_sequence() {
    // lifecycle.py:273-372 — the tick chained side-effect ORDER is fixed:
    //   load_state -> tmux_session_gate -> capture_missing -> refresh_statuses ->
    //   startup_prompts -> runtime_prompts -> sync_health -> deliver_pending ->
    //   fire_scheduled -> detect_stuck -> record_unknown_idle -> evaluate_takeover ->
    //   detect_deadlocks -> detect_compaction -> detect_drift -> detect_api_errors ->
    //   ATOMIC_save (bug-084 wrap) -> collect_results -> prune_dedupe_log.
    // The porter must push each step name into the injected recorder at its call site.
    let recorder: OrderRecorder =
        std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let (coord, _calls) = coord_for_test(true, None, Some(std::sync::Arc::clone(&recorder)));
    let _ = coord.tick().expect("tick ok");
    let order = recorder.lock().unwrap().clone();
    let expected = vec![
        "load_state",
        "tmux_session_gate",
        "capture_missing",
        "refresh_statuses",
        "startup_prompts",
        "runtime_prompts",
        "sync_health",
        "deliver_pending",
        "fire_scheduled",
        "detect_stuck",
        "record_unknown_idle",
        "evaluate_takeover",
        "detect_deadlocks",
        "detect_compaction",
        "detect_drift",
        "detect_api_errors",
        "atomic_save",
        "collect_results",
        "prune_dedupe_log",
    ];
    assert_eq!(order, expected, "tick side-effect ORDER must match the fixed sequence");
    // ATOMIC save is the LAST mutation before read-only collect/prune (bug-084 wrap point).
    let save_idx = order.iter().position(|s| *s == "atomic_save").unwrap();
    let collect_idx = order.iter().position(|s| *s == "collect_results").unwrap();
    assert!(save_idx < collect_idx, "save precedes collect (bug-084 wrap is the last mutation)");
}

#[test]
fn tick_zero_provider_sdk_across_full_tick() {
    // §84 / MUST-NOT-13: a full no-obligation tick injects ZERO exploratory prompts and
    // touches NO provider client. MockTransport::inject is unimplemented!() (would panic if
    // reached); a clean tick must therefore NEVER call inject. (The MockRegistry adapter
    // count is asserted via the GROUP E abnormal path; here the no-inject Transport guards
    // the tick-level §84 obligation.)
    let (coord, calls) = coord_for_test(true, None, None);
    let _ = coord.tick().expect("clean tick ok");
    let names = calls.lock().unwrap().clone();
    assert!(
        !names.contains(&"inject"),
        "§84: no exploratory prompt injected across a no-obligation tick"
    );
}

#[test]
fn health_ok_is_conjunction_of_running_metadata_ok_and_schema_ok() {
    // lifecycle.py:38 — ok = running ∧ metadata_ok ∧ schema_ok. A fresh temp workspace has
    // NO coordinator.pid => status Missing, ok=false (not running). health() must return a
    // typed HealthReport, never panic.
    let (coord, _calls) = coord_for_test(true, None, None);
    let h = coord.health().expect("health is a typed report");
    assert!(!h.ok, "no pid file => not running => ok=false");
    assert_eq!(h.status, CoordinatorHealthStatus::Missing, "no pid => status=missing");
    assert!(!h.metadata_ok, "no metadata => metadata_ok=false");
}

#[test]
fn stop_of_missing_coordinator_returns_missing_outcome() {
    // lifecycle.py:230-232 — no coordinator.pid => StopOutcome::Missing, ok=true (nothing to do).
    let (coord, _calls) = coord_for_test(true, None, None);
    let r = coord.stop().expect("stop is a typed report");
    assert_eq!(r.status, StopOutcome::Missing, "no pid => missing");
    assert_eq!(r.pid, None);
}

// ═════════════════════════════════════════════════════════════════════════
// GROUP K — pid_is_running (metadata.py:16) — zombie detection — RED
// ═════════════════════════════════════════════════════════════════════════

#[test]
fn pid_is_running_false_for_impossible_pid() {
    // metadata.py:19-21 — os.kill(pid, 0) raises → False. pid 0/巨大 pid 不存在。
    let alive = pid_is_running(Pid(2_000_000_000)).expect("probe returns Result");
    assert!(!alive, "non-existent pid → not running");
}

#[test]
fn pid_is_running_true_for_self() {
    // current process is alive & not zombie → true.
    let me = std::process::id();
    let alive = pid_is_running(Pid(me)).expect("probe self");
    assert!(alive, "self pid is running");
}

// ═════════════════════════════════════════════════════════════════════════
// GROUP L — resolve_tick_interval fallback (__main__.py:104) — RED
// ═════════════════════════════════════════════════════════════════════════

#[test]
fn resolve_tick_interval_defaults_to_five() {
    // __main__.py:110-115 — missing/erroring spec → DEFAULT_TICK_INTERVAL_SEC (5.0).
    let w = ws();
    let interval = resolve_tick_interval(&w).expect("returns Result");
    assert_eq!(interval, DEFAULT_TICK_INTERVAL_SEC);
}

#[test]
fn read_coordinator_metadata_missing_file_is_none() {
    // metadata.py:30-34 — OSError/JSONDecodeError/非 dict → None.
    let w = WorkspacePath::new("/tmp/team-agent-NONEXISTENT-meta-read-xyz");
    assert_eq!(read_coordinator_metadata(&w), None);
}

// ═══════════════ P2 FIX-LOOP RED (复绿即对抗 cross-model findings) ═══════════════
// Golden re-probed vs team-agent-public @ 439bef8 (lifecycle/metadata/watch/abnormal_track).

// P0 — a fresh state has session_name:null. Python truthiness skips the tmux gate entirely
// (only probes when session_name is a non-empty string); the daemon proceeds. Current
// defaults to "team-agent" and, with the session absent, stops the daemon (stop:true).
#[test]
fn p2_tick_skips_tmux_gate_when_session_name_absent() {
    let (coord, _calls) = coord_for_test(false, None, None);
    let report = coord.tick().unwrap();
    assert!(!report.stop, "missing/null session_name must skip the gate, not stop the daemon");
    assert_ne!(report.reason, Some(TickStopReason::TmuxSessionMissing));
}

// P1 — pid_is_running must use os.kill(pid,0) first: a pid owned by another user (pid 1 /
// launchd, root) is EPERM → not signalable → False. Current only `ps -p` (rc=0) → True.
#[test]
fn p2_pid_is_running_false_for_cross_user_pid() {
    // The cross-user semantics only hold for a non-root caller (root CAN signal pid 1).
    if unsafe { libc::geteuid() } != 0 {
        assert!(
            !pid_is_running(Pid(1)).expect("probe pid 1"),
            "pid 1 (root-owned) must read as not-running for a non-root caller (kill EPERM)"
        );
    }
}

// P1 — render_event_line(result_received) truncates the summary to 80 chars
// (watch.py:115-116 `_clean(summary)[:80]`).
#[test]
fn p2_render_result_received_truncates_summary_to_80_chars() {
    let long = "x".repeat(200);
    let e = serde_json::json!({"event":"result_received","agent_id":"w1","summary": long});
    let line = render_event_line(&e).expect("renders");
    assert!(line.contains(&"x".repeat(80)), "first 80 summary chars are kept");
    assert!(!line.contains(&"x".repeat(81)), "summary must be truncated to 80 chars");
}

// P1 — idle_takeover.unknown_persistent must carry the auth_mode field (null when absent)
// (lifecycle.py:401-408). NOTE: the variant currently lacks the field; the porter adds
// `auth_mode` between provider and consecutive_ticks and updates this construction.
#[test]
fn p2_unknown_persistent_event_serializes_auth_mode_key() {
    let evt = CoordinatorEvent::IdleTakeoverUnknownPersistent {
        node_id: "w7".into(),
        provider: None,
        auth_mode: None,
        consecutive_ticks: 72,
        rollout_path: None,
    };
    let json = serde_json::to_value(&evt).unwrap();
    assert!(
        json.get("auth_mode").is_some(),
        "idle_takeover.unknown_persistent must serialize an auth_mode key (null when absent)"
    );
}

// P1 — process_abnormal_records matches a LOWERCASED signature too (abnormal_track.py:49,198):
// raw has no needle but signature 'TimeoutError' matches blacklist 'timeout'. Current is
// case-sensitive on `raw` only and ignores the signature → NotifyDefault.
#[test]
fn p2_abnormal_matches_lowercased_signature_too() {
    let reg = MockRegistry::new(&[], &["timeout"]);
    let records = vec![serde_json::json!({"raw":"nothing here","signature":"TimeoutError","kind":"error"})];
    let out = process_abnormal_records(
        &records,
        &reg,
        Provider::Codex,
        &AbnormalNotificationState::default(),
    )
    .unwrap();
    assert_eq!(out.notifications.len(), 1);
    assert_eq!(
        out.notifications[0].decision,
        AbnormalDecision::NotifyBlacklist,
        "blacklist 'timeout' must match the lowercased signature 'TimeoutError'"
    );
}
