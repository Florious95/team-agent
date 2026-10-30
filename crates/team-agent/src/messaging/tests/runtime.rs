use super::*;
use crate::transport::test_support::OfflineTransport;
use crate::transport::{PaneId, Target};

fn pane_info(pane_id: &str, session: &str, window: &str) -> PaneInfo {
    PaneInfo {
        pane_id: PaneId::new(pane_id),
        session: SessionName::new(session),
        window_index: None,
        window_name: Some(WindowName::new(window)),
        pane_index: None,
        tty: None,
        current_command: None,
        current_path: None,
        active: true,
        pane_pid: None,
        leader_env: BTreeMap::new(),
    }
}

// ════════════════════════════════════════════════════════════════════════
// GROUP E — legacy diagnostics may only advance an existing durable row.
// ════════════════════════════════════════════════════════════════════════


// ════════════════════════════════════════════════════════════════════════
// GROUP F — session_drift_refusal: None-vs-refused fallthrough chain.
// session_drift.py:69-91.
// ════════════════════════════════════════════════════════════════════════

#[test]
fn session_drift_refusal_none_for_no_target_leader_or_broadcast() {
    let ws = tmp_ws("driftnone");
    let log = EventLog::new(&ws);
    let state = json(serde_json::json!({"agents": {}}));
    // target == leader_id → None (no refusal).
    assert!(
        session_drift_refusal(&state, "leader", "leader", "s", None, &log)
            .unwrap()
            .is_none()
    );
    // target == "*" (broadcast) → None.
    assert!(
        session_drift_refusal(&state, "*", "leader", "s", None, &log)
            .unwrap()
            .is_none()
    );
}

#[test]
fn session_drift_refusal_none_when_status_not_drift() {
    let ws = tmp_ws("driftok");
    let log = EventLog::new(&ws);
    let state = json(serde_json::json!({"agents": {"w1": {"status": "idle"}}}));
    assert!(
        session_drift_refusal(&state, "w1", "leader", "s", None, &log)
            .unwrap()
            .is_none()
    );
}

#[test]
fn session_drift_refusal_refuses_when_agent_in_drift() {
    // session_drift.py:84-91 → ok False, reason session_drift, action reset-agent.
    let ws = tmp_ws("driftrefuse");
    let log = EventLog::new(&ws);
    let state = json(serde_json::json!({
        "agents": {"w1": {"status": "session_drift",
            "session_drift": {"stored_session_id": "S", "actual_thread_id": "A"}}}
    }));
    let out = session_drift_refusal(&state, "w1", "leader", "leader", None, &log)
        .unwrap()
        .expect("drift agent must be refused");
    assert!(!out.ok);
    assert_eq!(out.status, DeliveryStatus::Refused);
    assert_eq!(out.reason, Some(DeliveryRefusal::SessionDrift));
}

// ════════════════════════════════════════════════════════════════════════
// GROUP G — classify_agent_activity: every branch incl. the uncertain
// fallthrough iron law. activity_detector.py:90-146 (golden probed).
// ════════════════════════════════════════════════════════════════════════

#[test]
fn classify_pane_in_mode_is_uncertain_high_confidence() {
    let state = json(serde_json::json!({}));
    let a = classify_agent_activity(&state, "", true, None, None);
    assert_eq!(a.status, ActivityStatus::Uncertain);
    assert_eq!(a.confidence, 0.9);
}

#[test]
fn classify_idle_prompt_is_idle() {
    // "❯ \n" matches the Claude idle prompt → idle 0.9.
    let state = json(serde_json::json!({}));
    let a = classify_agent_activity(&state, "❯ \n", false, None, None);
    assert_eq!(a.status, ActivityStatus::Idle);
    assert_eq!(a.confidence, 0.9);
}

#[test]
fn classify_working_indicator_is_working() {
    let state = json(serde_json::json!({}));
    let a = classify_agent_activity(&state, "Working (5s)", false, None, None);
    assert_eq!(a.status, ActivityStatus::Working);
    assert_eq!(a.confidence, 0.9);
}

#[test]
fn classify_stale_working_is_stuck() {
    // elapsed >= stuck_timeout (300) → stuck 0.85.
    let state = json(serde_json::json!({}));
    let a = classify_agent_activity(&state, "Working (400s)", false, None, None);
    assert_eq!(a.status, ActivityStatus::Stuck);
    assert_eq!(a.confidence, 0.85);
}

#[test]
fn classify_no_signal_is_uncertain_never_idle() {
    // THE IRON LAW: no decisive prompt/working signal → uncertain 0.5, NOT idle.
    let state = json(serde_json::json!({}));
    let a = classify_agent_activity(&state, "random prose nothing", false, None, None);
    assert_eq!(a.status, ActivityStatus::Uncertain);
    assert_eq!(a.confidence, 0.5);
    assert_ne!(a.status, ActivityStatus::Idle);
}

#[test]
fn classify_recent_provider_output_is_working_low_confidence() {
    // age <= 120 with provider/no command → working 0.7.
    let state = json(serde_json::json!({}));
    let now = chrono::Utc::now();
    let recent = (now - chrono::Duration::seconds(30)).to_rfc3339();
    let a = classify_agent_activity(&state, "prose", false, None, Some(&recent));
    assert_eq!(a.status, ActivityStatus::Working);
    assert_eq!(a.confidence, 0.7);
}

// STAGE-B REGRESSION RED (dispatch-to-just-launched-agent → deferred_busy never closes the round-trip).
// golden activity_detector.py (classify_agent_activity): the provider IDLE PROMPT is checked FIRST as a
// scrollback-position signal (C14, "provider idle prompt is the latest scrollback signal" → idle 0.9),
// BEFORE the `age<=120 → working 0.7` recent-output branch (:56). Rust (activity.rs:192) fires
// `recent_rfc3339(last_output_at,120) → Working` BEFORE `latest_prompt_signal` (:200), so a just-launched
// agent (startup banner = recent output, but pane shows the idle prompt awaiting input) mis-classifies
// WORKING → sync_agent_health writes agent_health=WORKING → recipient_is_busy → send.deferred_busy.
// Golden evidence (probe): classify(idle-prompt scrollback, recent last_output_at) = idle 0.9 regardless
// of active_task. FIX = reorder: latest_prompt_signal (idle/working scrollback-position) BEFORE the
// last_output_at age block.
#[test]
fn classify_idle_prompt_beats_recent_output_for_just_launched_agent() {
    let state = json(serde_json::json!({}));
    let recent = chrono::Utc::now().to_rfc3339();
    let a = classify_agent_activity(
        &state,
        "codex ready\n❯ \n",
        false,
        Some("codex"),
        Some(&recent),
    );
    assert_eq!(
        a.status,
        ActivityStatus::Idle,
        "just-launched agent showing the idle prompt must classify IDLE (golden idle-prompt-position is the \
         latest scrollback signal, checked before the age<=120 recent-output branch), not WORKING because \
         the startup banner is recent (activity.rs:192 recent-output fires before latest_prompt_signal:200) \
         — the Stage B dispatch deferred_busy regression. got {a:?}"
    );
    assert_eq!(
        a.confidence, 0.9,
        "golden idle-prompt confidence is 0.9; got {a:?}"
    );
}

#[test]
fn classify_copilot_idle_prompt_is_not_masked_by_current_command() {
    let state = json(serde_json::json!({}));
    let a = classify_agent_activity(
        &state,
        " / commands · ? help\n❯ \n",
        false,
        Some("copilot"),
        None,
    );
    assert_eq!(a.status, ActivityStatus::Idle);
    assert_eq!(a.confidence, 0.9);
}

// ════════════════════════════════════════════════════════════════════════
// GROUP H — attempt_trust_auto_answer: own-vs-foreign realpath + fail-safe
// pane-width + opt-in gate + reason byte-locks. leader_panes.py:383-470.
// ════════════════════════════════════════════════════════════════════════

#[test]
fn trust_auto_answer_pane_id_missing_reason() {
    // leader_panes.py:417-424 — pane_id None → pane_id_missing (after opt-in).
    let ws = tmp_ws("trustpane");
    let log = EventLog::new(&ws);
    let t = NoopTransport;
    let out = attempt_trust_auto_answer(
        &ws,
        &t,
        None,
        "some prompt",
        &PaneWidthQuery::Ok { pane_width: 120 },
        &log,
    )
    .unwrap();
    assert!(!out.ok);
    assert!(!out.answered);
    assert_eq!(out.reason, "pane_id_missing");
}

#[test]
fn trust_auto_answer_foreign_workspace_refused() {
    // leader_panes.py:430-444 — prompt names a FOREIGN dir → workspace_dir_mismatch,
    // action prompt_leader. (own-vs-foreign realpath gate.)
    let ws = tmp_ws("trustforeign");
    let log = EventLog::new(&ws);
    let t = NoopTransport;
    let pane = PaneId::new("%7");
    let foreign_tail = "Allow Codex to access /some/other/foreign/dir ?";
    let out = attempt_trust_auto_answer(
        &ws,
        &t,
        Some(&pane),
        foreign_tail,
        &PaneWidthQuery::Ok { pane_width: 120 },
        &log,
    )
    .unwrap();
    assert!(!out.answered);
    assert_eq!(out.reason, "workspace_dir_mismatch");
    assert_eq!(out.action.as_deref(), Some("prompt_leader"));
}

#[test]
fn trust_auto_answer_own_workspace_realpath_equal_answers() {
    // Exact canonical equality of the prompt path with the workspace → answered.
    let ws = tmp_ws("trustown");
    let canonical = std::fs::canonicalize(&ws).unwrap();
    let log = EventLog::new(&ws);
    let t = NoopTransport;
    let pane = PaneId::new("%7");
    let tail = format!("Allow Codex to write to {} ?", canonical.display());
    let out = attempt_trust_auto_answer(
        &canonical,
        &t,
        Some(&pane),
        &tail,
        &PaneWidthQuery::Ok { pane_width: 240 },
        &log,
    )
    .unwrap();
    assert!(
        out.answered,
        "own-workspace realpath-equal prompt must auto-answer"
    );
    assert_eq!(out.reason, "trust_auto_answered");
}

// ════════════════════════════════════════════════════════════════════════
// GROUP I — PaneWidthQuery fail-safe (bug-064/082): Failed NEVER carries a
// default width; tmux_pane_width returns Failed on any query failure.
// delivery.py:20-51.
// ════════════════════════════════════════════════════════════════════════

#[test]
fn pane_width_failed_forces_exact_match_never_default() {
    // GROUP-I fail-safe (bug-064/082, folded from the old structural placeholder):
    // calls the REAL attempt_trust_auto_answer. A FOREIGN path that is merely a
    // truncated PREFIX of this workspace would only match with a width signal that
    // proves right-edge truncation (leader_panes.py:_token_reaches_right_edge). With
    // PaneWidthQuery::Failed there is NO width signal and NO default width to leak,
    // so the matcher MUST fall back to exact canonical equality → the prefix does
    // NOT match → workspace_dir_mismatch / prompt_leader. Probed golden:
    // leader_panes.py:430-444 with pane_width=None (Failed) → workspace_dir_mismatch.
    let ws = tmp_ws("panewidthfailsafe");
    let canonical = std::fs::canonicalize(&ws).unwrap();
    let log = EventLog::new(&ws);
    let t = NoopTransport;
    let pane = PaneId::new("%7");
    // A right-edge-truncated prefix of the real workspace path (drop the last char):
    // would auto-answer IF a width signal proved truncation — but Failed forbids that.
    let canon_str = canonical.to_string_lossy();
    let truncated_prefix = &canon_str[..canon_str.len().saturating_sub(1)];
    let tail = format!("Allow Codex to write to {truncated_prefix}");
    let out = attempt_trust_auto_answer(
        &canonical,
        &t,
        Some(&pane),
        &tail,
        &PaneWidthQuery::Failed {
            error: "tmux_query_failed:CalledProcessError".to_string(),
        },
        &log,
    )
    .unwrap();
    // fail-safe: no width → exact-equality only → truncated prefix refused.
    assert!(
        !out.answered,
        "Failed pane-width must NOT enable prefix/truncation matching"
    );
    assert_eq!(out.reason, "workspace_dir_mismatch");
    assert_eq!(out.action.as_deref(), Some("prompt_leader"));
}

#[test]
fn tmux_pane_width_failure_yields_failed_not_default() {
    // delivery.py:37-50 — any failure path returns Failed (never a guessed width).
    let t = NoopTransport;
    let target = Target::Pane(PaneId::new("%nonexistent"));
    let q = tmux_pane_width(&t, &target);
    assert!(
        matches!(q, PaneWidthQuery::Failed { .. }),
        "query failure must be fail-safe Failed, never a default width"
    );
}

// ════════════════════════════════════════════════════════════════════════
// GROUP J — trust retry status machine: bounded attempt → exhausted terminal.
// delivery.py:221-319 (_handle_trust_retry_needed).
// ════════════════════════════════════════════════════════════════════════



// ════════════════════════════════════════════════════════════════════════
// GROUP K — send_message target resolution / fallback chain (send.py:36-372).
// RED via unimplemented!(); golden status/reason encoded in assertions.
// ════════════════════════════════════════════════════════════════════════

#[test]
fn send_message_target_not_in_team_is_refused() {
    // send.py:259-261 — non-leader, non-team target → refused. M1 (station::send) refuses
    // before storing anything.
    let ws = tmp_ws("sendrefuse");
    crate::state::persist::save_runtime_state(
        &ws,
        &serde_json::json!({"session_name": "team-x", "agents": {"w1": {"provider": "copilot"}}}),
    )
    .unwrap();
    let opts = SendOptions::default();
    let out = send_message(
        &ws,
        &MessageTarget::Single("ghost".to_string()),
        "hi",
        &opts,
    );
    assert!(
        matches!(&out, Err(MessagingError::Routing(reason)) if reason.contains("unknown recipient: ghost")),
        "{out:?}"
    );
    let inbox = crate::inbox_store::read(&ws, "ghost", 10, None).unwrap();
    assert!(inbox["messages"].as_array().unwrap().is_empty(), "a refused send stores nothing");
}

#[test]
fn send_message_broadcast_empty_team_skips_no_recipients() {
    // send.py:391-393 — "*" with no team recipients →
    //   {"ok": False, "status": "failed", "reason": "no_team_recipients", "to": "*"}.
    // Post-#230 N31/N32 funnel implementation (cr-approved): broadcast now expands the
    // recipient set via `broadcast_recipients(state, sender, team)` and routes each
    // recipient through the SAME primitives as a single send (leader → primitive,
    // peer → send_message). The assertions stay the same: with no agents seeded and
    // sender="leader" (default opts.sender), `broadcast_recipients` returns an empty
    // list — outcome is Failed/no-recipients with channel="*". The "*" channel label
    // is preserved through the new `fanout_send(..., channel_label="*")` parameter so
    // legacy consumers can still tell broadcast (`*`) apart from explicit fanout list.
    // M1 (station::send): a broadcast that expands to no recipient fails before storing.
    let ws = tmp_ws("sendbcast");
    crate::state::persist::save_runtime_state(
        &ws,
        &serde_json::json!({"session_name": "team-x", "agents": {}}),
    )
    .unwrap();
    let opts = SendOptions::default();
    let out = send_message(&ws, &MessageTarget::Broadcast, "hi", &opts);
    assert!(
        matches!(&out, Err(MessagingError::Routing(reason)) if reason == "send requires at least one recipient"),
        "{out:?}"
    );
}

#[test]
fn send_message_fanout_empty_recipients_fails() {
    // send.py:456-457 — fanout with no usable recipients → ok False,
    // no_fanout_recipients. (Dedup-then-deliver happy path needs team fixtures.)
    let ws = tmp_ws("sendfanout");
    let opts = SendOptions::default();
    let out = send_message(&ws, &MessageTarget::Fanout(vec![]), "hi", &opts);
    assert!(
        matches!(&out, Err(MessagingError::Routing(reason)) if reason == "send requires at least one recipient"),
        "{out:?}"
    );
}

// ════════════════════════════════════════════════════════════════════════
// GROUP L — apply_worker_sender_bypass: owner-gate first-door bypass event.
// owner_bypass.py:9-26.
// ════════════════════════════════════════════════════════════════════════

#[test]
fn worker_sender_bypass_false_for_leader_sender() {
    // owner_bypass.py — leader sender never bypasses (worker_sender_bypasses=None).
    let ws = tmp_ws("bypassleader");
    let log = EventLog::new(&ws);
    let state = json(serde_json::json!({"agents": {"w1": {}}}));
    let bypassed = apply_worker_sender_bypass(
        &state,
        Some("leader"),
        &MessageTarget::Single("w1".to_string()),
        None,
        &log,
    )
    .unwrap();
    assert!(!bypassed);
}

#[test]
#[serial_test::serial(env)]
fn worker_sender_bypass_true_for_known_worker_sender() {
    // owner_bypass.py:18-26 — worker in agents bypasses, writes
    // send.bypassed_owner_gate_worker_sender.
    // Isolate from ambient TEAM_AGENT_ID: the env identity gate only activates when
    // TEAM_AGENT_ID is SET (and != sender → deny, see p2_owner_bypass_denies_*). Unset
    // here so the agents-membership bypass is tested deterministically regardless of the
    // process env (workers run with TEAM_AGENT_ID set; the leader does not).
    let _g = ENV_LOCK_MSG.lock().unwrap_or_else(|p| p.into_inner());
    let _e = EnvGuardMsg::set("TEAM_AGENT_ID", None);
    let ws = tmp_ws("bypassworker");
    let log = EventLog::new(&ws);
    let state = json(serde_json::json!({"agents": {"w1": {}}}));
    let bypassed = apply_worker_sender_bypass(
        &state,
        Some("w1"),
        &MessageTarget::Single("w2".to_string()),
        None,
        &log,
    )
    .unwrap();
    assert!(bypassed);
}

// ════════════════════════════════════════════════════════════════════════
// GROUP M — report_result intake (results.py:191-227): validate envelope,
// queue leader notify (channel coordinator), return ok shape.
// ════════════════════════════════════════════════════════════════════════




#[test]
fn report_result_invalid_envelope_errors_validation() {
    // validate_result_envelope raises ValidationError → MessagingError::Validation.
    let ws = tmp_ws("reportbad");
    let envelope = json(serde_json::json!({"schema_version": "result_envelope_v1"}));
    let err = report_result(&ws, &envelope).unwrap_err();
    assert!(
        matches!(err, MessagingError::Validation(_)),
        "missing required fields must surface as Validation, got {err:?}"
    );
}


// ════════════════════════════════════════════════════════════════════════
// GROUP N — notify_result_watchers dedupe (exactly-once, Gap 32/38).
// result_delivery.py:38-132. superseded for duplicate watchers same result.
// ════════════════════════════════════════════════════════════════════════



// ════════════════════════════════════════════════════════════════════════
// GROUP O — requeue_after_claim_leader: notified_message_id must SURVIVE (Gap
// 32) — already-notified watchers are NOT requeued. result_delivery.py:428-506.
// ════════════════════════════════════════════════════════════════════════



// ════════════════════════════════════════════════════════════════════════
// GROUP Q — collect intake (results.py:45-167): valid result advances task,
// returns collected_results + delivered_messages + results counts shape.
// ════════════════════════════════════════════════════════════════════════

#[test]
fn collect_without_spec_surfaces_validation_error() {
    // results.py:46-48 — collect() reads spec_path then load_spec(); against a
    // workspace with NO team.spec.yaml, load_spec RAISES ValidationError
    // ("Cannot read <path>: ...", spec.py:18-20) BEFORE any collection. The
    // previous `.unwrap()` (expecting an Ok dict with present-only keys) was wrong:
    // the real Python collect on a bare workspace does not return a dict, it raises.
    // At the typed boundary that surfaces as MessagingError::Validation.
    //
    // The full collected_results-count golden (seed an uncollected result, assert it
    // collects and the task advances) is DEFERRED: it requires a valid on-disk
    // team.spec.yaml + runtime state, whose formats are owned by the spec/state lanes
    // (this file may not edit them). seed_result() exists for the retry path that
    // does NOT need a spec; the collect happy-path needs an integration fixture.
    let ws = tmp_ws("collect");
    let err = collect(&ws, None).unwrap_err();
    assert!(
        matches!(err, MessagingError::Validation(_)),
        "collect without a team spec must surface Validation, got {err:?}"
    );
}



// ════════════════════════════════════════════════════════════════════════
// GROUP R — run_comms_selftest: §84 / MUST-NOT-13 zero-provider-SDK gate.
// diagnose/comms.py:21-47. The whole point: assert zero provider client calls.
// ════════════════════════════════════════════════════════════════════════

#[test]
fn run_comms_selftest_zero_provider_sdk_passes_and_locks_scope() {
    let ws = tmp_ws("selftestok");
    let driver = ZeroSdkDriver {
        run_id: Some("fixedrunid01".to_string()),
        calls: ProviderSdkCalls::default(),
    };
    let report = run_comms_selftest(&ws, None, &driver).unwrap();
    // diagnose/comms.py:44 scope == "binding_consistency".
    assert_eq!(report.scope, "binding_consistency");
    assert_eq!(report.run_id, "fixedrunid01");
    // The mechanical gate: provider_sdk_calls check is a Pass with all-zero evidence.
    assert_eq!(report.provider_sdk_calls.status, CheckStatus::Pass);
    match &report.provider_sdk_calls.evidence {
        CheckEvidence::ProviderSdkCalls(calls) => assert!(calls.is_zero()),
        other => panic!("expected ProviderSdkCalls evidence, got {other:?}"),
    }
    assert!(report.ok);
}

#[test]
fn run_comms_selftest_nonzero_provider_sdk_fails_gate() {
    // Any non-zero SDK call count → provider_sdk_calls check FAILS, report not ok.
    let ws = tmp_ws("selftestbad");
    let driver = ZeroSdkDriver {
        run_id: Some("r2".to_string()),
        calls: ProviderSdkCalls {
            anthropic: 1,
            openai: 0,
            httpx: 0,
        },
    };
    let report = run_comms_selftest(&ws, None, &driver).unwrap();
    assert_eq!(report.provider_sdk_calls.status, CheckStatus::Fail);
    assert!(!report.ok);
}


// ════════════════════════════════════════════════════════════════════════
// GROUP S — evaluate_idle_behavior: claimed_status normalization
// (IDLE/WORKING/RUNNING → not_challenged). diagnose/comms.py:50-94.
// ════════════════════════════════════════════════════════════════════════

#[test]
fn evaluate_idle_behavior_recognized_status_is_not_challenged() {
    // diagnose/comms.py:86-94 — claimed_status in {IDLE,WORKING,RUNNING} (case-
    // insensitive) and no driver result → status not_challenged, ok True.
    let ws = tmp_ws("idleeval");
    let driver = ZeroSdkDriver {
        run_id: None,
        calls: ProviderSdkCalls::default(),
    };
    let out = evaluate_idle_behavior(&ws, "w1", "IDLE", None, &driver).unwrap();
    assert_eq!(out.status, CheckStatus::NotChallenged);
    assert!(out.ok);
    assert_eq!(out.agent_id, "w1");
}

// ════════════════════════════════════════════════════════════════════════
// GROUP T — deliver_pending_message claim atomicity + status machine.
// delivery.py:63-218. missing message / unknown recipient / already-claimed.
// ════════════════════════════════════════════════════════════════════════


// ════════════════════════════════════════════════════════════════════════
// GROUP U — fire_due_scheduled_events: exhaustive ScheduledKind dispatch +
// send dedupe. scheduler.py:41-121. Returns fired event-id list.
// ════════════════════════════════════════════════════════════════════════


struct UnverifiedInjectTransport {
    readback_visible: bool,
}
impl Transport for UnverifiedInjectTransport {
    fn kind(&self) -> BackendKind {
        BackendKind::Tmux
    }
    fn spawn_first(
        &self,
        _s: &SessionName,
        _w: &WindowName,
        _a: &[String],
        _c: &Path,
        _e: &BTreeMap<String, String>,
    ) -> Result<SpawnResult, TransportError> {
        unimplemented!("not reached in delivery")
    }
    fn spawn_into(
        &self,
        _s: &SessionName,
        _w: &WindowName,
        _a: &[String],
        _c: &Path,
        _e: &BTreeMap<String, String>,
    ) -> Result<SpawnResult, TransportError> {
        unimplemented!("not reached in delivery")
    }
    fn inject(
        &self,
        _t: &Target,
        _p: &InjectPayload,
        _s: Key,
        _b: bool,
    ) -> Result<InjectReport, TransportError> {
        Ok(InjectReport {
            stage_reached: crate::transport::InjectStage::Submit,
            inject_verification: if self.readback_visible {
                crate::transport::InjectVerification::CaptureContainsToken
            } else {
                crate::transport::InjectVerification::CaptureMissingToken
            },
            submit_verification:
                crate::transport::SubmitVerification::PastedContentPromptStillPresentAfterSubmit,
            turn_verification: crate::transport::TurnVerification::NotYetObserved,
            attempts: u32::from(SEND_RETRY_MAX_ATTEMPTS),
            submit_diagnostics: None,
        })
    }
    fn send_keys(&self, _t: &Target, _k: &[Key]) -> Result<(), TransportError> {
        Ok(())
    }
    fn capture(&self, _t: &Target, range: CaptureRange) -> Result<CapturedText, TransportError> {
        Ok(CapturedText {
            text: String::new(),
            range,
        })
    }
    fn query(&self, _t: &Target, _f: PaneField) -> Result<Option<String>, TransportError> {
        Ok(None)
    }
    fn liveness(&self, _p: &PaneId) -> Result<PaneLiveness, TransportError> {
        Ok(PaneLiveness::Unknown)
    }
    fn list_targets(&self) -> Result<Vec<PaneInfo>, TransportError> {
        Ok(Vec::new())
    }
    fn has_session(&self, _s: &SessionName) -> Result<bool, TransportError> {
        Ok(true)
    }
    fn list_windows(&self, _s: &SessionName) -> Result<Vec<WindowName>, TransportError> {
        Ok(Vec::new())
    }
    fn set_session_env(
        &self,
        _s: &SessionName,
        _k: &str,
        _v: &str,
    ) -> Result<SetEnvOutcome, TransportError> {
        Ok(SetEnvOutcome::Applied)
    }
    fn kill_session(&self, _s: &SessionName) -> Result<(), TransportError> {
        Ok(())
    }
    fn kill_window(&self, _t: &Target) -> Result<(), TransportError> {
        Ok(())
    }
    fn attach_session(&self, _s: &SessionName) -> Result<AttachOutcome, TransportError> {
        Ok(AttachOutcome::Attached)
    }
}

struct FailingInjectTransport {
    attempts: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

impl Transport for FailingInjectTransport {
    fn kind(&self) -> BackendKind {
        BackendKind::Tmux
    }
    fn spawn_first(
        &self,
        _s: &SessionName,
        _w: &WindowName,
        _a: &[String],
        _c: &Path,
        _e: &BTreeMap<String, String>,
    ) -> Result<SpawnResult, TransportError> {
        unimplemented!("not reached in delivery")
    }
    fn spawn_into(
        &self,
        _s: &SessionName,
        _w: &WindowName,
        _a: &[String],
        _c: &Path,
        _e: &BTreeMap<String, String>,
    ) -> Result<SpawnResult, TransportError> {
        unimplemented!("not reached in delivery")
    }
    fn inject(
        &self,
        t: &Target,
        _p: &InjectPayload,
        _s: Key,
        _b: bool,
    ) -> Result<InjectReport, TransportError> {
        if matches!(t, Target::Pane(pane) if pane.as_str() == "%1") {
            self.attempts
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        Err(TransportError::Inject {
            stage: crate::transport::InjectStage::PasteBuffer,
            source: std::io::Error::other("paste-buffer failed"),
        })
    }
    fn send_keys(&self, _t: &Target, _k: &[Key]) -> Result<(), TransportError> {
        Ok(())
    }
    fn capture(&self, _t: &Target, range: CaptureRange) -> Result<CapturedText, TransportError> {
        Ok(CapturedText {
            text: String::new(),
            range,
        })
    }
    fn query(&self, _t: &Target, _f: PaneField) -> Result<Option<String>, TransportError> {
        Ok(None)
    }
    fn liveness(&self, _p: &PaneId) -> Result<PaneLiveness, TransportError> {
        Ok(PaneLiveness::Unknown)
    }
    fn list_targets(&self) -> Result<Vec<PaneInfo>, TransportError> {
        Ok(Vec::new())
    }
    fn has_session(&self, _s: &SessionName) -> Result<bool, TransportError> {
        Ok(true)
    }
    fn list_windows(&self, _s: &SessionName) -> Result<Vec<WindowName>, TransportError> {
        Ok(Vec::new())
    }
    fn set_session_env(
        &self,
        _s: &SessionName,
        _k: &str,
        _v: &str,
    ) -> Result<SetEnvOutcome, TransportError> {
        Ok(SetEnvOutcome::Applied)
    }
    fn kill_session(&self, _s: &SessionName) -> Result<(), TransportError> {
        Ok(())
    }
    fn kill_window(&self, _t: &Target) -> Result<(), TransportError> {
        Ok(())
    }
    fn attach_session(&self, _s: &SessionName) -> Result<AttachOutcome, TransportError> {
        Ok(AttachOutcome::Attached)
    }
}

// 0.3.30: submit evidence supersedes stale readback. This transport returns a
// verified submit but a missing-token readback.
struct ReadbackMissingTransport;
impl Transport for ReadbackMissingTransport {
    fn kind(&self) -> BackendKind {
        BackendKind::Tmux
    }
    fn spawn_first(
        &self,
        _: &SessionName,
        _: &WindowName,
        _: &[String],
        _: &Path,
        _: &BTreeMap<String, String>,
    ) -> Result<SpawnResult, TransportError> {
        unimplemented!("not reached in delivery")
    }
    fn spawn_into(
        &self,
        _: &SessionName,
        _: &WindowName,
        _: &[String],
        _: &Path,
        _: &BTreeMap<String, String>,
    ) -> Result<SpawnResult, TransportError> {
        unimplemented!("not reached in delivery")
    }
    fn inject(
        &self,
        _: &Target,
        _: &InjectPayload,
        _: Key,
        _: bool,
    ) -> Result<InjectReport, TransportError> {
        Ok(InjectReport {
            stage_reached: crate::transport::InjectStage::Submit,
            // submit verified, but the token never appeared in the pane → readback missing.
            inject_verification: crate::transport::InjectVerification::CaptureMissingToken,
            submit_verification: crate::transport::SubmitVerification::KeySentAfterVisibleToken {
                key: Key::Enter,
            },
            turn_verification: crate::transport::TurnVerification::NotYetObserved,
            attempts: 1,
            submit_diagnostics: None,
        })
    }
    fn send_keys(&self, _: &Target, _: &[Key]) -> Result<(), TransportError> {
        Ok(())
    }
    fn capture(&self, _: &Target, range: CaptureRange) -> Result<CapturedText, TransportError> {
        Ok(CapturedText {
            text: String::new(),
            range,
        })
    }
    fn query(&self, _: &Target, _: PaneField) -> Result<Option<String>, TransportError> {
        Ok(None)
    }
    fn liveness(&self, _: &PaneId) -> Result<PaneLiveness, TransportError> {
        Ok(PaneLiveness::Unknown)
    }
    fn list_targets(&self) -> Result<Vec<PaneInfo>, TransportError> {
        Ok(Vec::new())
    }
    fn has_session(&self, _: &SessionName) -> Result<bool, TransportError> {
        Ok(true)
    }
    fn list_windows(&self, _: &SessionName) -> Result<Vec<WindowName>, TransportError> {
        Ok(Vec::new())
    }
    fn set_session_env(
        &self,
        _: &SessionName,
        _: &str,
        _: &str,
    ) -> Result<SetEnvOutcome, TransportError> {
        Ok(SetEnvOutcome::Applied)
    }
    fn kill_session(&self, _: &SessionName) -> Result<(), TransportError> {
        Ok(())
    }
    fn kill_window(&self, _: &Target) -> Result<(), TransportError> {
        Ok(())
    }
    fn attach_session(&self, _: &SessionName) -> Result<AttachOutcome, TransportError> {
        Ok(AttachOutcome::Attached)
    }
}











fn app_server_state(
    ws: &Path,
    endpoint: &str,
    thread_id: &str,
    session_id: &str,
) -> serde_json::Value {
    serde_json::json!({
        "active_team_key": "team-a",
        "teams": {
            "team-a": {
                "team_key": "team-a",
                "agents": {},
                "leader_receiver": {
                    "mode": "codex_app_server",
                    "transport_kind": "codex_app_server",
                    "status": "attached",
                    "provider": "codex",
                    "owner_epoch": 7,
                    "app_server": {
                        "socket": endpoint,
                        "thread_id": thread_id,
                        "session_id": session_id,
                        "cwd": ws.to_string_lossy(),
                        "cli_version": "codex-appserver-team-agent-test/0.139.0",
                        "bound_at": "2026-07-05T00:00:00Z"
                    }
                },
                "team_owner": {
                    "provider": "codex",
                    "transport_kind": "codex_app_server",
                    "owner_epoch": 7
                },
                "owner_epoch": 7
            }
        }
    })
}

// 0.5.x Windows portability Batch 5: FakeAppServer uses Unix domain
// sockets — Unix-only.




#[test]
fn u1_multi_team_send_does_not_backfill_top_level_leader_binding() {
    let ws = tmp_ws("u1backfill");
    crate::state::persist::save_runtime_state(
        &ws,
        &serde_json::json!({
            "session_name": "team-a",
            "active_team_key": "team-a",
            "leader_receiver": {"status": "unbound", "pane_id": "__team_agent_unbound__"},
            "team_owner": {"pane_id": "__team_agent_unbound__"},
            "teams": {
                "team-a": {
                    "session_name": "team-a",
                    "leader_receiver": {"status": "unbound", "pane_id": "__team_agent_unbound__"},
                    "agents": {}
                },
                "team-b": {
                    "session_name": "team-b",
                    "agents": {}
                }
            },
            "agents": {}
        }),
    )
    .unwrap();
    let opts = SendOptions {
        team: Some(TeamKey::new("team-b")),
        requires_ack: false,
        ..SendOptions::default()
    };

    let out = send_message(
        &ws,
        &MessageTarget::Single("leader".to_string()),
        "hello team-b",
        &opts,
    )
    .unwrap();

    assert!(out.ok, "{out:?}");
    let message_id = out.message_id.expect("stored leader message id");
    // M1 inboxes are team-scoped (red_m1_r10): the record belongs to team-b only.
    let team_b = crate::inbox_store::read(&ws, "leader", 10, Some("team-b")).unwrap();
    let records = team_b["messages"].as_array().unwrap();
    assert_eq!(records.len(), 1, "team-b leader send must land in team-b's inbox: {team_b}");
    assert_eq!(records[0]["message_id"], message_id.as_str());
    assert_eq!(records[0]["team"], "team-b");
    let team_a = crate::inbox_store::read(&ws, "leader", 10, Some("team-a")).unwrap();
    assert!(
        team_a["messages"].as_array().unwrap().is_empty(),
        "team-b leader send must not inherit team-a/top-level unbound leader binding: {team_a}"
    );
}

#[test]
fn casefile_leader_send_is_durable_without_entering_leader_funnel() {
    let ws = tmp_ws("casefile-send");
    crate::state::persist::save_runtime_state(
        &ws,
        &serde_json::json!({
            "session_name": "team-a",
            "active_team_key": "team-a",
            "agents": {}
        }),
    )
    .unwrap();
    let opts = SendOptions {
        team: Some(TeamKey::new("team-a")),
        requires_ack: false,
        presentation: crate::messaging::presentation::PresentationRequest {
            sink: crate::messaging::presentation::PresentationSink::Casefile,
            class: crate::messaging::presentation::PresentationClass::Progress,
            case_id: Some("case-1".to_string()),
        },
        ..SendOptions::default()
    };
    let out = send_message(
        &ws,
        &MessageTarget::Single("leader".to_string()),
        "internal progress",
        &opts,
    )
    .unwrap();
    assert_eq!(out.status, DeliveryStatus::StoredOnly);
    assert_eq!(out.message_status.0, "stored");
    // M1: the durable record is the inbox; a casefile send never attempts submission.
    let receipt: serde_json::Value =
        serde_json::from_str(out.verification.as_deref().unwrap()).unwrap();
    assert_eq!(receipt["live"], false);
    assert!(receipt.get("submission").is_none(), "{receipt}");
    let inbox = crate::inbox_store::read(&ws, "leader", 10, Some("team-a")).unwrap();
    let record = &inbox["messages"][0];
    assert_eq!(record["message_id"], out.message_id.as_deref().unwrap());
    assert_eq!(record["presentation"]["sink"], "casefile");
}

// ════════════════════════════════════════════════════════════════════════
// GROUP V — retry_result_deliveries: re-route notify_failed watchers with
// dedupe_reason rebind_retry. result_delivery.py:19-35.
// ════════════════════════════════════════════════════════════════════════


// ════════════════════════════════════════════════════════════════════════
// GROUP X — delivered_result_message content-level dedupe lookup +
// result_id_from_text dual (scheduler send dedupe path). result_delivery.py:394.
// ════════════════════════════════════════════════════════════════════════



// ═══════════════════════════════════════════════════════════════════════════
// collect #223 — task-scoped collect + send --task validation (RED).
// ═══════════════════════════════════════════════════════════════════════════

// (c) a result whose task_id ∈ state.tasks collects as scope:"task"; the task row advances to
// "done" (success → done, runtime.py:1066); results.collected ≥ 1. Proves the collect-READ works
// once state.tasks is seeded — so the #223 fix target is the upstream seeding, not collect.

// (c-C1) collect OUTPUT shape: collected_results entries are the 8-KEY SUMMARY (NO inlined
// envelope; carry summary+tests) and the full envelopes live in a SEPARATE top-level `collected`
// list (golden results.py:86/131). Rust inlines `envelope`+`owner_team_id` and emits no
// `collected` list → RED.

// ════════════════════════════════════════════════════════════════════════
// P0 REGRESSION (0700cff "send 0 bytes, nothing queued" / coordinator never delivers).
// golden gates the unknown-task RAISE on route_task_id (send.py:204 `if task_id and route_task_id`);
// delivery/fanout/internal sends pass route_task_id=False (internal_delivery.py:44, send.py:412/481)
// → the task is a label, NOT validated. 0700cff's UNCONDITIONAL task_exists gate broke every
// task-tagged delivery/internal send at CREATION time. The gate the OfflineTransport tests missed.
// ════════════════════════════════════════════════════════════════════════

// (a) [REGRESSION GATE] route_task_id=false + task_id NOT in state.tasks → send SUCCEEDS and the
// message is QUEUED (real create path; no transport). Must NOT raise "unknown task id".

// (c) [LOCK] route_task_id=true + task_id IN state.tasks → send SUCCEEDS (routing happy-path).

// ════════════════════════════════════════════════════════════════════════
// R8 byte-parity (leader attach requeue, advisor-ruled + e3eac28-reconciled):
// drive a watcher to delivery_exhausted via notify_result_watchers (attempts>=MAX) — proving the
// requeue input is REAL (non-空过) — then attach-requeue and assert the golden observable contract:
//   D2 status: delivery_exhausted -> notify_failed (golden result_watchers.py:95), NOT 'pending'.
//   D1 ✦ team-scoped + unnotified SELECTION (anti cross-team pollution / CP-1) — KEEP.
//   D3 result_watcher.requeued payload == golden attach form {watcher_id, trigger:"attach_leader", new_pane_id}.
// (D4 leader_receiver.requeued_exhausted_watchers + D6 string return are the attach-wrapper/CLI layer —
//  lease.rs:140 + cli/mod.rs:1088 — flagged for the porter; D5 event-layer is internal/optional.)
// ════════════════════════════════════════════════════════════════════════

// E15/E23 single-funnel guard: report_result presents a compact result through
// the shared leader receiver and does not carry a private direct-inject retry.

// ════════════════════════════════════════════════════════════════════════
// 0.5.4 gate054 — cross-team notify boundary.
//
// Triage: .team/artifacts/0.5.4-gate-crossteam-notify-triage.md
//
// Shape (regression):
//   - Gate team's `leader_receiver` records `pane_id=%1`, `tmux_socket=/tmp/gate`,
//     `status="rebind_required"` (disposable leader was killed post-restart).
//   - The main tmux server has a live `%1` on the workspace-canonical socket.
//   - `deliver_pending_message` MUST refuse with `channel=rebind_required` and
//     MUST NOT probe the main/default tmux server for the same pane id.
// ════════════════════════════════════════════════════════════════════════



// ════════════════════════════════════════════════════════════════════════
// 0.5.5 gate054 B-arm — deliver_to_leader_fallback_pane must not cross to
// another tmux server by pane id when a socket is recorded.
//
// Triage: real-machine acceptance B-fail — primary path (delivery.rs) refused
// with rebind_required, but report_result's fallback_pane bailout selected a
// same-numbered pane (%1) on the default tmux server (the main team's leader)
// and returned leader_notified:true / delivered_via=fallback_pane.
// ════════════════════════════════════════════════════════════════════════






fn delivery_blocked_channel_reasons(ws: &std::path::Path) -> Vec<String> {
    EventLog::new(ws)
        .tail(80)
        .unwrap()
        .into_iter()
        .filter(|event| event.get("event") == Some(&serde_json::json!("leader_receiver.delivery_blocked")))
        .filter_map(|event| {
            event
                .get("channel_reason")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        })
        .collect()
}

fn live_leader_pane(ws: &std::path::Path, pane_id: &str) -> PaneInfo {
    let mut pane = pane_info(pane_id, "team-current", "leader");
    pane.current_path = Some(ws.to_path_buf());
    pane
}


#[test]
fn owner_projection_non_object_does_not_match_current() {
    assert!(
        !crate::messaging::delivery::top_level_state_matches_owner_team(
            &serde_json::Value::Null,
            "current"
        ),
        "Null must not match current through team_state_key default"
    );
    assert!(
        !crate::messaging::delivery::top_level_state_matches_owner_team(
            &serde_json::json!([]),
            "current"
        ),
        "non-object must not match current"
    );
}



