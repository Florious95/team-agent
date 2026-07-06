use super::*;
use crate::transport::test_support::OfflineTransport;

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
// GROUP E — _fail_leader_delivery: bug-52 fallback-log semantics. ok=True but
// status=FallbackLog (NOT a real submit). leader.py:394-436.
// ════════════════════════════════════════════════════════════════════════

#[test]
fn fail_leader_delivery_returns_fallback_log_ok_true_not_submitted() {
    let ws = tmp_ws("faillead");
    let payload = json(serde_json::json!({
        "to": "leader", "content": "hi", "sender": "coordinator"
    }));
    let out = fail_leader_delivery(
        &ws,
        &payload,
        DeliveryRefusal::LeaderNotAttached,
        Some("No direct leader tmux pane is attached. Run team-agent attach-leader."),
    )
    .unwrap();
    // leader.py:423-431 — ok True, status fallback_log, channel fallback_inbox.
    assert!(out.ok);
    assert_eq!(out.status, DeliveryStatus::FallbackLog);
    assert_eq!(out.reason, Some(DeliveryRefusal::LeaderNotAttached));
    // The audit must be distinguishable from a real submit (Delivered).
    assert_ne!(out.status, DeliveryStatus::Delivered);
}

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

#[test]
fn handle_trust_retry_below_max_schedules_retry() {
    // attempt 1 (< 4) → next_attempt 2 scheduled, status retry_scheduled,
    // stage trust_auto_answer_dismissal_wait. NOT marked terminal-failed.
    let ws = tmp_ws("trustretry1");
    let store = store_for(&ws);
    let log = EventLog::new(&ws);
    let payload = TrustRetryPayload {
        message_id: "m1".to_string(),
        attempt: 1,
        max_attempts: TRUST_RETRY_MAX_ATTEMPTS,
        first_target: PaneId::new("%7"),
    };
    let out = handle_trust_retry_needed(&store, &payload, &log).unwrap();
    assert_eq!(out.status, DeliveryStatus::RetryScheduled);
    assert_eq!(out.stage, Some(DeliveryStage::TrustAutoAnswerDismissalWait));
    assert!(!out.ok);
}

#[test]
fn handle_trust_retry_at_max_is_exhausted_terminal() {
    // attempt == 4 (== MAX) → next_attempt 5 > MAX → terminal exhausted, marks
    // message failed, emits trust_auto_answer_exhausted. delivery.py:246-266.
    let ws = tmp_ws("trustretry4");
    let store = store_for(&ws);
    let log = EventLog::new(&ws);
    let payload = TrustRetryPayload {
        message_id: "m1".to_string(),
        attempt: TRUST_RETRY_MAX_ATTEMPTS,
        max_attempts: TRUST_RETRY_MAX_ATTEMPTS,
        first_target: PaneId::new("%7"),
    };
    let out = handle_trust_retry_needed(&store, &payload, &log).unwrap();
    // delivery.py:257-259 — terminal exhausted: ok False, status the dedicated
    // trust_auto_answer_exhausted (a bounded-loop termination guarantee, NOT a
    // refusal reason — `reason` stays None at the typed boundary).
    assert_eq!(out.status, DeliveryStatus::TrustAutoAnswerExhausted);
    assert!(!out.ok);
}

// ════════════════════════════════════════════════════════════════════════
// GROUP K — send_message target resolution / fallback chain (send.py:36-372).
// RED via unimplemented!(); golden status/reason encoded in assertions.
// ════════════════════════════════════════════════════════════════════════

#[test]
fn send_message_target_not_in_team_is_refused() {
    // send.py:259-261 — non-leader, non-team target → refused/target_not_in_team.
    let ws = tmp_ws("sendrefuse");
    let opts = SendOptions::default();
    let out = send_message(
        &ws,
        &MessageTarget::Single("ghost".to_string()),
        "hi",
        &opts,
    )
    .unwrap();
    assert_eq!(out.status, DeliveryStatus::Refused);
    assert_eq!(out.reason, Some(DeliveryRefusal::TargetNotInTeam));
}

#[test]
fn message_not_silently_stuck_accepted_when_coordinator_dead() {
    let ws = tmp_ws("sendcoorddead");
    crate::state::persist::save_runtime_state(
        &ws,
        &serde_json::json!({
            "session_name": "team-x",
            "agents": {
                "worker-1": {"status": "running", "agent_id": "worker-1", "window": "worker-1"},
            },
        }),
    )
    .unwrap();
    let coordinator_ws = crate::coordinator::WorkspacePath::new(ws.clone());
    std::fs::create_dir_all(crate::model::paths::runtime_dir(&ws)).unwrap();
    let _ = MessageStore::open(&ws).unwrap();
    let stale_pid = crate::coordinator::Pid::new(99_999_999);
    crate::coordinator::write_coordinator_metadata(
        &coordinator_ws,
        stale_pid,
        crate::coordinator::MetadataSource::Boot,
    )
    .unwrap();
    std::fs::write(
        crate::coordinator::coordinator_pid_path(&coordinator_ws),
        stale_pid.to_string(),
    )
    .unwrap();

    let out = send_message(
        &ws,
        &MessageTarget::Single("worker-1".to_string()),
        "hi",
        &SendOptions::default(),
    )
    .unwrap();

    assert!(!out.ok);
    assert_eq!(out.status, DeliveryStatus::Degraded);
    assert_eq!(out.message_status.0, "degraded");
    assert_eq!(out.reason, Some(DeliveryRefusal::CoordinatorUnavailable));
    assert!(
        out.verification
            .as_deref()
            .is_some_and(|warning| warning.contains("coordinator is not running")),
        "N38 warning must explain why the message was not queued; out={out:?}"
    );
    let store = MessageStore::open(&ws).unwrap();
    let conn = crate::db::schema::open_db(store.db_path()).unwrap();
    let accepted: i64 = conn
        .query_row(
            "select count(*) from messages where status = 'accepted'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        accepted, 0,
        "dead coordinator send must not strand an accepted row"
    );
    let events = EventLog::new(&ws).tail(20).unwrap();
    assert!(
        events.iter().any(|event| {
            event.get("event").and_then(serde_json::Value::as_str)
                == Some("send.coordinator_unavailable")
                && event
                    .get("message_queued")
                    .and_then(serde_json::Value::as_bool)
                    == Some(false)
        }),
        "send.coordinator_unavailable event must be durable; events={events:?}"
    );
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
    let ws = tmp_ws("sendbcast");
    let opts = SendOptions::default();
    let out = send_message(&ws, &MessageTarget::Broadcast, "hi", &opts).unwrap();
    assert!(!out.ok);
    assert_eq!(out.status, DeliveryStatus::Failed);
    assert_eq!(
        out.reason, None,
        "no_team_recipients is a failed-status terminal, not a typed refusal reason"
    );
    assert_eq!(
        out.channel.as_deref(),
        Some("*"),
        "broadcast outcome must carry the '*' channel (send.py to='*'); fanout_send(channel_label=\"*\") preserves this"
    );
}

#[test]
fn send_message_fanout_empty_recipients_fails() {
    // send.py:456-457 — fanout with no usable recipients → ok False,
    // no_fanout_recipients. (Dedup-then-deliver happy path needs team fixtures.)
    let ws = tmp_ws("sendfanout");
    let opts = SendOptions::default();
    let out = send_message(&ws, &MessageTarget::Fanout(vec![]), "hi", &opts).unwrap();
    assert!(!out.ok);
    assert_eq!(out.status, DeliveryStatus::Failed);
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
fn report_result_valid_envelope_returns_ok_with_result_id() {
    let ws = tmp_ws("report");
    let envelope = json(serde_json::json!({
        "schema_version": "result_envelope_v1",
        "task_id": "t1", "agent_id": "alice", "status": "success",
        "summary": "done", "changes": [], "tests": [], "risks": [],
        "artifacts": [], "next_actions": []
    }));
    let out = report_result(&ws, &envelope).unwrap();
    // results.py:216-227 — ok True with result_id/task_id/agent_id echoed.
    assert_eq!(out.get("ok").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(out.get("task_id").and_then(|v| v.as_str()), Some("t1"));
    assert_eq!(out.get("agent_id").and_then(|v| v.as_str()), Some("alice"));
    assert!(out.get("result_id").and_then(|v| v.as_str()).is_some());
}

#[test]
fn report_result_funnels_into_leader_delivery_primitive_not_queued_scheduled_event() {
    // #230 N31/N32 funnel (cr verdict §3 I-3 + MUST-8):
    //
    // [OLD assertion] report_result inserted a parallel `scheduled_events(kind='send',
    // target='leader', status='pending')` row + returned `notification_status="queued"`,
    // and the worker-facing tool body claimed success while leader had not yet seen the
    // result. That queued-only path was MUST-8 / I-3 violating — `notification_status=
    // queued` was returned as success but the leader pane never actually got the text.
    //
    // [NEW assertion] report_result now synchronously funnels through the shared leader-
    // delivery primitive (`send_to_leader_receiver`), creating a `messages` row that
    // `deliver_pending_messages` picks up on the same tick (NO `scheduled_events` row).
    // Without a bound leader pane (this fixture has no `leader_receiver.pane_id`), the
    // primitive returns I-4 `rebind_required` (Blocked / ok=false) — the row is persisted
    // as `failed` for audit and the tool body's `notification_status` is `rebind_required`,
    // NOT a misleading `queued` success. The contract grep that bans `queue_report_result_
    // notification` / `notification_status="queued[_only]"` literals in `results.rs` is the
    // direct mechanical counterpart of this assertion.
    let ws = tmp_ws("reportnotify");
    crate::state::persist::save_runtime_state(
        &ws,
        &serde_json::json!({
            "session_name": null,
            "leader": {"id": "leader"},
            "agents": {"worker": {"status": "running"}},
            "tasks": [{"id": "task_1", "status": "running", "assignee": "worker"}]
        }),
    )
    .unwrap();
    let store = store_for(&ws);
    let envelope = json(serde_json::json!({
        "schema_version": "result_envelope_v1",
        "task_id": "task_1",
        "agent_id": "worker",
        "status": "success",
        "summary": "done",
        "changes": [],
        "tests": [{"command": "cargo test", "status": "passed"}],
        "risks": [],
        "artifacts": [],
        "next_actions": []
    }));

    let out = report_result(&ws, &envelope).unwrap();
    let result_id = out
        .get("result_id")
        .and_then(|v| v.as_str())
        .expect("report_result returns generated result_id");
    assert!(
        result_id.starts_with("res_"),
        "MessageStore.add_result generates res_* ids; got {result_id}"
    );

    // No `scheduled_events` rows: the queued parallel path is gone.
    let conn = seed_conn(&store);
    let scheduled_count: i64 = conn
        .query_row("select count(*) from scheduled_events", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(
        scheduled_count, 0,
        "N31/N32 funnel: report_result must NOT insert a parallel scheduled_events 'send' row; the leader-delivery primitive is the single funnel"
    );

    // Tool body: no `queued`/`queued_only` notification_status. Without a bound leader
    // pane this fixture surfaces I-4 `rebind_required` (ok=false on the leader delivery,
    // but the result row + audit trail are durable for rebind replay).
    assert_eq!(
        out.get("notification_status").and_then(|v| v.as_str()),
        Some("rebind_required"),
        "I-4: unbound leader pane → rebind_required, never queued/queued_only success"
    );
    assert_eq!(
        out.get("leader_notified").and_then(|v| v.as_bool()),
        Some(false),
        "I-4: leader_notified=false when no leader pane is bound"
    );
    assert!(
        out.get("notification_event_id")
            .is_some_and(|v| v.is_null()),
        "no scheduled_events row → notification_event_id is null"
    );

    // Audit events: the funnel emits leader_receiver.delivery_blocked (I-4 rebind),
    // and the legacy mcp.report_result_notify_queued audit is gone.
    let events_path = ws.join(".team").join("logs").join("events.jsonl");
    let event_lines =
        std::fs::read_to_string(events_path).expect("report_result writes events.jsonl");
    assert!(
        event_lines.contains("\"leader_receiver.delivery_blocked\""),
        "I-4 rebind path must emit leader_receiver.delivery_blocked audit; got {event_lines}",
    );
    assert!(
        !event_lines.contains("mcp.report_result_notify_queued"),
        "legacy queued-notification audit must be gone; got {event_lines}",
    );
    assert!(
        event_lines.contains("\"mcp.report_result\""),
        "report_result still emits its own audit event; got {event_lines}",
    );
}

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

#[test]
fn notify_result_watchers_no_match_returns_empty() {
    // result_delivery.py:51-52 — no candidate watcher matches → empty list.
    let ws = tmp_ws("notifyempty");
    let log = EventLog::new(&ws);
    let result = json(serde_json::json!({
        "result_id": "r1", "task_id": "t1", "agent_id": "alice"
    }));
    let watchers = vec![json(serde_json::json!({
        "watcher_id": "w-x", "task_id": "OTHER", "agent_id": "alice"
    }))];
    let notices = notify_result_watchers(&ws, &result, &log, Some(&watchers), None).unwrap();
    assert!(notices.is_empty());
}

#[test]
fn notify_result_watchers_supersedes_duplicate_watchers() {
    // result_delivery.py:53-78 — two watchers same (task,agent,result): earliest is
    // primary, the other gets superseded (ok False, notice records superseded).
    let ws = tmp_ws("notifysup");
    let log = EventLog::new(&ws);
    let result = json(serde_json::json!({
        "result_id": "r1", "task_id": "t1", "agent_id": "alice"
    }));
    let watchers = vec![
        json(serde_json::json!({
            "watcher_id": "w-early", "task_id": "t1", "agent_id": "alice",
            "created_at": "2026-06-02T10:00:00+00:00"
        })),
        json(serde_json::json!({
            "watcher_id": "w-late", "task_id": "t1", "agent_id": "alice",
            "created_at": "2026-06-02T11:00:00+00:00"
        })),
    ];
    let notices = notify_result_watchers(&ws, &result, &log, Some(&watchers), None).unwrap();
    // The late watcher must appear as a superseded (not-ok) notice — exactly-once.
    let superseded = notices
        .iter()
        .find(|n| n.watcher_id == "w-late")
        .expect("late watcher must be reported");
    assert!(
        !superseded.ok,
        "duplicate watcher must be superseded, not re-delivered"
    );
}

// ════════════════════════════════════════════════════════════════════════
// GROUP O — requeue_after_claim_leader: notified_message_id must SURVIVE (Gap
// 32) — already-notified watchers are NOT requeued. result_delivery.py:428-506.
// ════════════════════════════════════════════════════════════════════════

#[test]
fn requeue_after_claim_leader_skips_already_notified() {
    // Gap 32 (result_delivery.py:467-471) — SEEDED dedupe gate: two same-team
    // watchers, one already notified (notified_message_id set), one un-notified.
    // requeue must return ONLY the un-notified watcher; the notified one is NOT
    // requeued and its notified_message_id SURVIVES (clearing it would cause a
    // second injection). Probed golden: requeued == [w_un] (result_id null,
    // prior_state "pending"); notified watcher keeps notified_message_id.
    let ws = tmp_ws("requeue");
    let store = store_for(&ws);
    let log = EventLog::new(&ws);
    let team = TeamKey::new("team-a");
    let pane = PaneId::new("%new-leader");

    let w_un = seed_watcher(
        &store,
        "w-unnotified",
        "team-a",
        "t1",
        "alice",
        "pending",
        None,
        None,
    );
    let w_notified = seed_watcher(
        &store,
        "w-notified",
        "team-a",
        "t2",
        "bob",
        "pending",
        None,
        Some("msg_already"),
    );

    let requeued = requeue_after_claim_leader(&ws, &store, &log, &team, &pane, None).unwrap();

    // ONLY the un-notified watcher requeues (the notified one is the dedupe gate).
    let ids: Vec<&str> = requeued.iter().map(|n| n.watcher_id.as_str()).collect();
    assert_eq!(
        ids,
        vec![w_un.as_str()],
        "exactly the un-notified watcher requeues"
    );
    assert!(
        !requeued.iter().any(|n| n.watcher_id == w_notified),
        "already-notified watcher must NOT be requeued (Gap 32)"
    );

    // Gap 32 survival: notified_message_id is preserved on the skipped watcher.
    let (_status, notified) = watcher_state(&store, &w_notified);
    assert_eq!(
        notified.as_deref(),
        Some("msg_already"),
        "notified_message_id MUST survive requeue — clearing it re-injects (Gap 32)"
    );
}

#[test]
fn requeue_delivery_exhausted_watchers_reopens_only_exhausted() {
    let ws = tmp_ws("requeueexhausted");
    let store = store_for(&ws);
    let log = EventLog::new(&ws);
    let team = TeamKey::new("team-a");
    let pane = PaneId::new("%leader");

    let rid = seed_result(&store, "res_exhausted", "t1", "alice", "success");
    let exhausted = seed_watcher(
        &store,
        "w-exhausted",
        "team-a",
        "t1",
        "alice",
        "delivery_exhausted",
        Some(&rid),
        None,
    );
    let notified = seed_watcher(
        &store,
        "w-exhausted-notified",
        "team-a",
        "t2",
        "bob",
        "delivery_exhausted",
        Some("res_skip"),
        Some("msg_done"),
    );
    let failed = seed_watcher(
        &store,
        "w-failed",
        "team-a",
        "t3",
        "carol",
        "notify_failed",
        Some("res_failed"),
        None,
    );

    let requeued = requeue_delivery_exhausted_watchers(&ws, &store, &log, &team, &pane).unwrap();

    assert_eq!(
        requeued.len(),
        1,
        "only delivery_exhausted unnotified watchers requeue"
    );
    let notice = &requeued[0];
    assert_eq!(notice.watcher_id, exhausted);
    assert_eq!(notice.result_id.as_deref(), Some(rid.as_str()));
    assert_eq!(notice.prior_state.as_deref(), Some("delivery_exhausted"));
    // R8 (golden result_watchers.py:95): attach requeue flips delivery_exhausted -> notify_failed (NOT pending).
    assert_eq!(notice.status.as_deref(), Some("notify_failed"));

    let (status, _notified_id) = watcher_state(&store, &exhausted);
    // R8 (golden): attach requeue leaves the watcher at notify_failed and DEFERS retry to the coordinator
    // tick — it does NOT immediately re-deliver (only the claim path retries). So the persisted status is
    // notify_failed, not 'notified'.
    assert_eq!(
        status, "notify_failed",
        "attach requeue flips to notify_failed and defers retry (golden)"
    );
    let (status, notified_id) = watcher_state(&store, &notified);
    assert_eq!(status, "delivery_exhausted");
    assert_eq!(notified_id.as_deref(), Some("msg_done"));
    let (status, _notified_id) = watcher_state(&store, &failed);
    assert_eq!(
        status, "notify_failed",
        "non-exhausted watcher is not selected"
    );

    // R8 (golden leader/__init__.py:46-50): result_watcher.requeued is the ATTACH form
    // {watcher_id, trigger:"attach_leader", new_pane_id} — NOT the claim-style {prior_state,claimed_pane_id,team_id}.
    let events = log.tail(0).unwrap();
    let ev = events
        .iter()
        .rev()
        .find(|event| {
            event.get("event").and_then(|v| v.as_str()) == Some("result_watcher.requeued")
        })
        .expect("result_watcher.requeued event");
    let keys: std::collections::BTreeSet<&str> = ev
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .filter(|k| *k != "ts" && *k != "event")
        .collect();
    let expected: std::collections::BTreeSet<&str> = ["watcher_id", "trigger", "new_pane_id"]
        .into_iter()
        .collect();
    assert_eq!(
        keys, expected,
        "result_watcher.requeued must be golden attach form {{watcher_id, trigger, new_pane_id}}"
    );
    assert_eq!(
        ev.get("watcher_id").and_then(|v| v.as_str()),
        Some("w-exhausted")
    );
    assert_eq!(
        ev.get("trigger").and_then(|v| v.as_str()),
        Some("attach_leader")
    );
    assert_eq!(
        ev.get("new_pane_id").and_then(|v| v.as_str()),
        Some("%leader")
    );
}

// ════════════════════════════════════════════════════════════════════════
// GROUP P — stuck_cancel owner-gate + invalid alert type refusal.
// scheduler.py:262-294.
// ════════════════════════════════════════════════════════════════════════

#[test]
fn stuck_cancel_none_alert_type_expands_to_all() {
    // alert_type None == Python "all" → sorted(_ALERT_TYPES) expansion.
    let ws = tmp_ws("stuckcancel");
    let out = stuck_cancel(&ws, "w1", None, "leader").unwrap();
    // The suppression result must enumerate all three alert types.
    let types = out.get("alert_types").and_then(|v| v.as_array()).map(|a| {
        a.iter()
            .filter_map(|x| x.as_str().map(str::to_string))
            .collect::<Vec<_>>()
    });
    assert_eq!(
        types,
        Some(vec![
            "cross_worker_deadlock".to_string(),
            "idle_fallback".to_string(),
            "stuck".to_string()
        ])
    );
}

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
    let err = collect(&ws, None, false).unwrap_err();
    assert!(
        matches!(err, MessagingError::Validation(_)),
        "collect without a team spec must surface Validation, got {err:?}"
    );
}

#[test]
fn collect_accepts_message_scoped_result_for_matching_recipient() {
    let ws = tmp_ws("collectmsgok");
    std::fs::write(ws.join("team.spec.yaml"), "version: 1\n").unwrap();
    let store = store_for(&ws);
    let message_id = store
        .create_message(None, "leader", "w1", "please reply", None, false, None)
        .unwrap();
    seed_result(&store, "res_msg_ok", &message_id, "w1", "success");

    let out = collect(&ws, None, false).unwrap();
    assert_eq!(out.get("ok").and_then(|v| v.as_bool()), Some(true));
    let collected = out
        .get("collected_results")
        .and_then(|v| v.as_array())
        .expect("collected_results");
    assert_eq!(collected.len(), 1);
    assert_eq!(
        collected[0].get("task_id").and_then(|v| v.as_str()),
        Some(message_id.as_str())
    );
    assert_eq!(
        collected[0].get("agent_id").and_then(|v| v.as_str()),
        Some("w1")
    );
    assert_eq!(
        collected[0].get("scope").and_then(|v| v.as_str()),
        Some("message")
    );
    // D3 (leader-adjudicated): golden collected_results entry is EXACTLY the 8-key summary for BOTH
    // scopes; golden's task_status feeds only the `collect.result` EVENT, never the entry. So a
    // message-scope entry carries NO task_status key (the prior `Some("message_scoped")` lock encoded a
    // port divergence — dropped per ruling).
    assert!(
        collected[0].get("task_status").is_none(),
        "collected_results entry must NOT carry task_status (golden 8-key summary; event-only); got {:?}",
        collected[0]
    );
    let keys: Vec<&str> = collected[0]
        .as_object()
        .expect("entry is an object")
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        keys,
        vec!["result_id", "task_id", "agent_id", "status", "summary", "tests", "created_at", "scope"],
        "message-scope collected_results entry must be EXACTLY the golden 8 keys in order; got {keys:?}"
    );
}

#[test]
fn collect_rejects_message_scoped_result_without_matching_recipient() {
    let ws = tmp_ws("collectmsgbad");
    std::fs::write(ws.join("team.spec.yaml"), "version: 1\n").unwrap();
    let store = store_for(&ws);
    let message_id = store
        .create_message(None, "leader", "w1", "please reply", None, false, None)
        .unwrap();
    seed_result(&store, "res_msg_bad", &message_id, "w2", "success");

    let out = collect(&ws, None, false).unwrap();
    assert_eq!(out.get("ok").and_then(|v| v.as_bool()), Some(false));
    assert!(
        out.get("collected_results")
            .and_then(|v| v.as_array())
            .is_some_and(Vec::is_empty),
        "recipient mismatch must not collect as message-scoped"
    );
    let invalid = out
        .get("invalid_results")
        .and_then(|v| v.as_array())
        .expect("invalid_results");
    assert_eq!(invalid.len(), 1);
    assert_eq!(
        invalid[0].get("task_id").and_then(|v| v.as_str()),
        Some(message_id.as_str())
    );
    assert_eq!(
        invalid[0].get("error").and_then(|v| v.as_str()),
        Some(format!("unknown task id: {message_id}").as_str())
    );
}

#[test]
fn allow_peer_talk_records_bidirectional_allowlist_and_event() {
    let ws = tmp_ws("allowpeer");
    let out = allow_peer_talk(&ws, "alice", "bob").unwrap();
    assert_eq!(out.get("ok").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(out.get("a").and_then(|v| v.as_str()), Some("alice"));
    assert_eq!(out.get("b").and_then(|v| v.as_str()), Some("bob"));
    assert_eq!(
        out.get("status").and_then(|v| v.as_str()),
        Some("compat_noop")
    );
    assert_eq!(
        out.get("reason").and_then(|v| v.as_str()),
        Some("team_scoped_peer_messages_enabled")
    );

    let store = store_for(&ws);
    let conn = seed_conn(&store);
    let rows = conn
        .prepare("select a, b from peer_allowlist order by a, b")
        .unwrap()
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(
        rows,
        vec![
            ("alice".to_string(), "bob".to_string()),
            ("bob".to_string(), "alice".to_string()),
        ]
    );

    let events = EventLog::new(&ws).tail(10).unwrap();
    let event = events
        .iter()
        .find(|event| {
            event.get("event").and_then(|v| v.as_str()) == Some("communication.peer_allowed")
        })
        .expect("communication.peer_allowed event");
    assert_eq!(event.get("a").and_then(|v| v.as_str()), Some("alice"));
    assert_eq!(event.get("b").and_then(|v| v.as_str()), Some("bob"));
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

#[test]
fn run_comms_selftest_contract_suite_is_executable_zero_token() {
    let ws = tmp_ws("selftestdefer");
    let driver = ZeroSdkDriver {
        run_id: Some("r3".to_string()),
        calls: ProviderSdkCalls::default(),
    };
    let report = run_comms_selftest(&ws, None, &driver).unwrap();
    assert_eq!(report.contract_suite.status, CheckStatus::Pass);
    match &report.contract_suite.evidence {
        CheckEvidence::ContractSuite { checks } => {
            assert!(
                checks.iter().all(|check| check.status == CheckStatus::Pass),
                "contract suite subchecks must all pass: {checks:?}"
            );
            let names: Vec<&str> = checks.iter().map(|check| check.name.as_str()).collect();
            assert!(
                names.contains(&"message_store_schema")
                    && names.contains(&"message_token_shape")
                    && names.contains(&"result_notification_render")
                    && names.contains(&"leader_projection_owner_team"),
                "contract suite must cover schema, token, result notification, and leader projection: {names:?}"
            );
        }
        other => panic!("expected ContractSuite evidence, got {other:?}"),
    }
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

#[test]
fn deliver_pending_message_missing_message_fails() {
    // delivery.py:73-75 — no such message row → ok False, status failed,
    // reason message_missing.
    let ws = tmp_ws("delivermissing");
    let store = store_for(&ws);
    let log = EventLog::new(&ws);
    let t = NoopTransport;
    let out =
        deliver_pending_message(&ws, &store, &t, "nope", &log, &serde_json::json!({})).unwrap();
    assert!(!out.ok);
    assert_eq!(out.status, DeliveryStatus::Failed);
}

// ════════════════════════════════════════════════════════════════════════
// GROUP U — fire_due_scheduled_events: exhaustive ScheduledKind dispatch +
// send dedupe. scheduler.py:41-121. Returns fired event-id list.
// ════════════════════════════════════════════════════════════════════════

#[test]
fn fire_due_scheduled_events_fires_each_scheduled_kind() {
    // SEEDED exhaustive-dispatch contract (scheduler.py:41-121): seed one due row
    // of EACH ScheduledKind (send / health_ping / trust_retry). The dispatch loop
    // must fire all three (one match arm per kind, no runtime fallback) and return
    // each fired event id. Probed golden: a due health_ping fires → marked 'done'
    // with {"ok":true,"status":"logged"} and its id appears in the fired list;
    // every due row's id is appended regardless of kind (scheduler.py:118).
    let ws = tmp_ws("scheduler");
    let store = store_for(&ws);
    let log = EventLog::new(&ws);
    let t = NoopTransport;

    let send_id = seed_scheduled_event(
        &store,
        ScheduledKind::Send,
        "%w1",
        &serde_json::json!({"content": "ping", "attempt": 1, "max_attempts": 1}),
    );
    let ping_id = seed_scheduled_event(
        &store,
        ScheduledKind::HealthPing,
        "%w1",
        &serde_json::json!({}),
    );
    let trust_id = seed_scheduled_event(
        &store,
        ScheduledKind::TrustRetry,
        "%w1",
        &serde_json::json!({"message_id": "m1", "attempt": 1, "max_attempts": 4}),
    );

    let fired = fire_due_scheduled_events(&ws, &store, &t, &log).unwrap();

    // Every seeded due kind must be dispatched and its id returned (exhaustive,
    // no kind silently dropped via a fallthrough).
    for id in [send_id, ping_id, trust_id] {
        assert!(
            fired.contains(&id),
            "scheduled event id {id} (each ScheduledKind) must fire; got {fired:?}"
        );
    }
    assert_eq!(
        fired.len(),
        3,
        "exactly the three seeded due events fire, no extras"
    );
}

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
    fn spawn_first(&self, _: &SessionName, _: &WindowName, _: &[String], _: &Path, _: &BTreeMap<String, String>) -> Result<SpawnResult, TransportError> {
        unimplemented!("not reached in delivery")
    }
    fn spawn_into(&self, _: &SessionName, _: &WindowName, _: &[String], _: &Path, _: &BTreeMap<String, String>) -> Result<SpawnResult, TransportError> {
        unimplemented!("not reached in delivery")
    }
    fn inject(&self, _: &Target, _: &InjectPayload, _: Key, _: bool) -> Result<InjectReport, TransportError> {
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
        Ok(CapturedText { text: String::new(), range })
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
    fn set_session_env(&self, _: &SessionName, _: &str, _: &str) -> Result<SetEnvOutcome, TransportError> {
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

#[test]
fn deliver_pending_submit_verified_overrides_missing_readback() {
    let ws = tmp_ws("readbackmiss");
    let store = store_for(&ws);
    let log = EventLog::new(&ws);
    let state = serde_json::json!({
        "session_name": "team-readbackmiss",
        "leader_receiver": {"pane_id": "%leader"},
        "agents": {"w1": {"provider": "fake", "pane_id": "%1"}}
    });
    crate::state::persist::save_runtime_state(&ws, &state).unwrap();
    let message_id = store
        .create_message(None, "leader", "w1", "ping", None, false, None)
        .unwrap();

    let out = deliver_pending_message(
        &ws,
        &store,
        &ReadbackMissingTransport,
        &message_id,
        &log,
        &state,
    )
    .unwrap();

    assert!(
        out.ok,
        "0.3.30: submit-verified delivery must not be vetoed by stale CaptureMissingToken readback"
    );
    assert_eq!(
        out.message_status.0, "delivered",
        "0.3.30: submit evidence is stronger than readback"
    );
}

#[test]
fn deliver_pending_submit_unverified_is_not_saved_by_readback() {
    let ws = tmp_ws("readbacksaves");
    let store = store_for(&ws);
    let log = EventLog::new(&ws);
    let state = serde_json::json!({
        "session_name": "team-readbacksaves",
        "leader_receiver": {"pane_id": "%leader"},
        "agents": {"w1": {"provider": "fake", "pane_id": "%1"}}
    });
    crate::state::persist::save_runtime_state(&ws, &state).unwrap();
    let message_id = store
        .create_message(None, "leader", "w1", "ping", None, false, None)
        .unwrap();

    let out = deliver_pending_message(
        &ws,
        &store,
        &UnverifiedInjectTransport {
            readback_visible: true,
        },
        &message_id,
        &log,
        &state,
    )
    .unwrap();

    assert!(
        !out.ok,
        "0.3.30: positive readback alone must not mark delivered when submit is unverified"
    );
    assert_ne!(out.message_status.0, "delivered");
}

#[test]
fn deliver_pending_exhausted_unverified_send_emits_failed_event() {
    let ws = tmp_ws("sendfailed");
    let store = store_for(&ws);
    let log = EventLog::new(&ws);
    let state = serde_json::json!({
        "session_name": "team-sendfailed",
        "leader_receiver": {"pane_id": "%leader"},
        "agents": {"w1": {"provider": "fake", "pane_id": "%1"}}
    });
    crate::state::persist::save_runtime_state(&ws, &state).unwrap();
    let message_id = store
        .create_message(None, "leader", "w1", "ping", None, false, None)
        .unwrap();

    let out = deliver_pending_message(
        &ws,
        &store,
        &UnverifiedInjectTransport {
            readback_visible: false,
        },
        &message_id,
        &log,
        &state,
    )
    .unwrap();

    assert!(!out.ok);
    assert_eq!(out.message_status.0, "failed");
    let events = log.tail(0).unwrap();
    assert!(
        events.iter().any(
            |event| event.get("event").and_then(serde_json::Value::as_str) == Some("send.failed")
        ),
        "exhausted unverified send must emit send.failed; got {events:?}"
    );
    assert!(
        events.iter().any(
            |event| event.get("event").and_then(serde_json::Value::as_str)
                == Some("send.failed_notification")
        ),
        "exhausted unverified send must queue a leader-visible notification; got {events:?}"
    );
}

#[test]
fn slice1_inject_failures_are_bounded_and_stop_retrying_same_message() {
    let ws = tmp_ws("injectstorm");
    let store = store_for(&ws);
    let log = EventLog::new(&ws);
    let state = serde_json::json!({
        "session_name": "team-injectstorm",
        "leader_receiver": {"pane_id": "%leader"},
        "agents": {"w1": {"provider": "fake", "pane_id": "%1"}}
    });
    crate::state::persist::save_runtime_state(&ws, &state).unwrap();
    let message_id = store
        .create_message(None, "leader", "w1", "ping", None, false, None)
        .unwrap();
    let attempts = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let transport = FailingInjectTransport {
        attempts: std::sync::Arc::clone(&attempts),
    };

    for _ in 0..(usize::from(SEND_RETRY_MAX_ATTEMPTS) + 2) {
        let _ = deliver_pending_messages(&ws, &state, &transport, &log).unwrap();
    }

    assert_eq!(
        attempts.load(std::sync::atomic::Ordering::Relaxed),
        usize::from(SEND_RETRY_MAX_ATTEMPTS),
        "same message/target must stop retrying after the bounded inject-failure threshold"
    );
    let conn = crate::db::schema::open_db(store.db_path()).unwrap();
    let (status, delivery_attempts, error): (String, i64, Option<String>) = conn
        .query_row(
            "select status, delivery_attempts, error from messages where message_id = ?1",
            [&message_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(status, "failed");
    assert_eq!(delivery_attempts, i64::from(SEND_RETRY_MAX_ATTEMPTS));
    assert_eq!(error.as_deref(), Some("send_inject_exhausted"));
    let events = log.tail(0).unwrap();
    let inject_failures = events
        .iter()
        .filter(|event| {
            event.get("event").and_then(serde_json::Value::as_str) == Some("send.inject_failed")
        })
        .count();
    assert_eq!(
        inject_failures,
        usize::from(SEND_RETRY_MAX_ATTEMPTS),
        "failed physical injects should be audited only until the bounded threshold; events={events:?}"
    );
    assert!(
        events.iter().any(|event| {
            event.get("event").and_then(serde_json::Value::as_str) == Some("send.failed")
                && event.get("reason").and_then(serde_json::Value::as_str)
                    == Some("send_inject_exhausted")
        }),
        "terminal inject exhaustion must emit send.failed; events={events:?}"
    );
}

#[test]
fn target_missing_session_window_blocks_without_generic_inject_exhaustion() {
    let ws = tmp_ws("targetmissing");
    let store = store_for(&ws);
    let log = EventLog::new(&ws);
    let state = serde_json::json!({
        "session_name": "team-targetmissing",
        "leader_receiver": {"pane_id": "%leader"},
        "agents": {"w1": {"provider": "fake", "window": "w1"}}
    });
    crate::state::persist::save_runtime_state(&ws, &state).unwrap();
    let message_id = store
        .create_message(None, "leader", "w1", "ping", None, false, None)
        .unwrap();
    let transport = OfflineTransport::new()
        .with_session_present(true)
        .with_targets(vec![pane_info("%2", "team-targetmissing", "other")]);

    let out = deliver_pending_message(&ws, &store, &transport, &message_id, &log, &state)
        .expect("target-missing classification should be a delivery outcome");

    assert!(!out.ok);
    assert_eq!(out.status, DeliveryStatus::Blocked);
    assert_eq!(out.message_status.0, "queued_pane_missing");
    assert_eq!(out.reason, Some(DeliveryRefusal::TmuxTargetMissing));
    assert!(
        transport.inject_targets().is_empty(),
        "target-missing must block before physical inject; targets={:?}",
        transport.inject_targets()
    );

    let conn = crate::db::schema::open_db(store.db_path()).unwrap();
    let (status, error): (String, Option<String>) = conn
        .query_row(
            "select status, error from messages where message_id = ?1",
            [&message_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(status, "queued_pane_missing");
    assert_eq!(error.as_deref(), Some("tmux_target_missing"));
    let events = log.tail(0).unwrap();
    assert!(
        events.iter().any(|event| {
            event.get("event").and_then(serde_json::Value::as_str) == Some("send.inject_blocked")
                && event.get("reason").and_then(serde_json::Value::as_str)
                    == Some("tmux_target_missing")
        }),
        "target-missing block must be explicit in events; events={events:?}"
    );
}

#[test]
fn u1_projection_refused_emits_delivery_event() {
    let ws = tmp_ws("u1projref");
    let store = store_for(&ws);
    let log = EventLog::new(&ws);
    let state = serde_json::json!({
        "session_name": "team-a",
        "active_team_key": "team-a",
        "teams": {
            "team-a": {
                "session_name": "team-a",
                "agents": {"w1": {"provider": "fake", "window": "w1"}}
            }
        },
        "agents": {"w1": {"provider": "fake", "window": "w1"}}
    });
    crate::state::persist::save_runtime_state(&ws, &state).unwrap();
    let message_id = store
        .create_message(
            None,
            "leader",
            "w1",
            "wrong team",
            None,
            false,
            Some("missing-team"),
        )
        .unwrap();

    let delivered = deliver_pending_messages(&ws, &state, &OfflineTransport::new(), &log)
        .expect("projection refusal must not abort the delivery batch");

    assert!(
        delivered.is_empty(),
        "the refused owner-team row must not be delivered"
    );
    let events = log.tail(0).unwrap();
    assert!(
        events.iter().any(|event| {
            event.get("event").and_then(serde_json::Value::as_str)
                == Some("delivery.projection_refused")
                && event.get("message_id").and_then(serde_json::Value::as_str)
                    == Some(message_id.as_str())
                && event
                    .get("owner_team_id")
                    .and_then(serde_json::Value::as_str)
                    == Some("missing-team")
        }),
        "OwnerTeamProjection::Refused must leave a delivery.projection_refused audit event; events={events:?}"
    );
}

#[test]
fn u1_deliver_item_error_does_not_halt_remaining_batch() {
    let ws = tmp_ws("u1itemblock");
    let store = store_for(&ws);
    let log = EventLog::new(&ws);
    let state = serde_json::json!({
        "session_name": "team-u1itemblock",
        "agents": {
            "w1": {"provider": "fake", "window": "w1"},
            "w2": {"provider": "fake", "window": "w2"}
        }
    });
    crate::state::persist::save_runtime_state(&ws, &state).unwrap();
    let blocked = store
        .create_message(None, "leader", "w1", "poison", None, false, None)
        .unwrap();
    let delivered_id = store
        .create_message(None, "peer", "w2", "still deliver", None, false, None)
        .unwrap();

    let state_path = crate::state::persist::runtime_state_path(&ws);
    std::fs::remove_file(&state_path).unwrap();
    std::fs::create_dir(&state_path).unwrap();
    let transport = OfflineTransport::new();

    let delivered = deliver_pending_messages(&ws, &state, &transport, &log)
        .expect("one item error must not halt the whole pending batch");

    assert_eq!(
        delivered,
        vec![delivered_id.clone()],
        "a poison item must be isolated while later pending rows still deliver"
    );
    let events = log.tail(0).unwrap();
    assert!(
        events.iter().any(|event| {
            event.get("event").and_then(serde_json::Value::as_str) == Some("delivery.item_blocked")
                && event.get("message_id").and_then(serde_json::Value::as_str)
                    == Some(blocked.as_str())
        }),
        "the isolated poison item must emit delivery.item_blocked; events={events:?}"
    );
}

#[test]
fn u1_resolve_inject_target_uses_live_pane_when_cached_pane_drifted() {
    let ws = tmp_ws("u1drift");
    let store = store_for(&ws);
    let log = EventLog::new(&ws);
    let state = serde_json::json!({
        "session_name": "team-u1drift",
        "agents": {
            "w1": {
                "provider": "fake",
                "pane_id": "%stale",
                "window": "w1"
            }
        }
    });
    crate::state::persist::save_runtime_state(&ws, &state).unwrap();
    let _ = store
        .create_message(None, "peer", "w1", "drift-safe", None, false, None)
        .unwrap();
    let transport =
        OfflineTransport::new().with_targets(vec![pane_info("%live", "team-u1drift", "w1")]);

    let delivered = deliver_pending_messages(&ws, &state, &transport, &log)
        .expect("live target re-resolution must not fail");

    assert_eq!(delivered.len(), 1);
    assert_eq!(
        transport.inject_targets(),
        vec![Target::Pane(PaneId::new("%live"))],
        "delivery must validate the cached pane against live targets and re-resolve by session/window when it drifted"
    );
}

#[test]
fn e51_delivery_allows_same_pane_id_on_different_tmux_sockets() {
    let ws = tmp_ws("e51diffsocket");
    let store = store_for(&ws);
    let log = EventLog::new(&ws);
    let state = serde_json::json!({
        "session_name": "team-e51",
        "tmux_endpoint": "/private/tmp/tmux-501/ta-worker",
        "leader_receiver": {
            "pane_id": "%0",
            "tmux_socket": "/private/tmp/tmux-501/default"
        },
        "agents": {
            "architect": {
                "provider": "fake",
                "pane_id": "%0",
                "window": "architect"
            }
        }
    });
    crate::state::persist::save_runtime_state(&ws, &state).unwrap();
    let _ = store
        .create_message(None, "peer", "architect", "socket-aware", None, false, None)
        .unwrap();
    let transport = OfflineTransport::new()
        .with_tmux_endpoint("/private/tmp/tmux-501/ta-worker")
        .with_targets(vec![pane_info("%0", "team-e51", "architect")]);

    let delivered = deliver_pending_messages(&ws, &state, &transport, &log)
        .expect("same pane id on different sockets must not poison delivery");

    assert_eq!(delivered.len(), 1);
    assert_eq!(
        transport.inject_targets(),
        vec![Target::Pane(PaneId::new("%0"))],
        "E51 must compare pane_id with tmux_socket; different sockets are not a leader conflict"
    );
}

#[test]
fn e51_delivery_keeps_same_socket_pane_conflict_guard() {
    let ws = tmp_ws("e51samesocket");
    let store = store_for(&ws);
    let log = EventLog::new(&ws);
    let socket = "/private/tmp/tmux-501/default";
    let state = serde_json::json!({
        "session_name": "team-e51",
        "tmux_endpoint": socket,
        "leader_receiver": {
            "pane_id": "%0",
            "tmux_socket": socket
        },
        "agents": {
            "architect": {
                "provider": "fake",
                "pane_id": "%0",
                "window": "architect"
            }
        }
    });
    crate::state::persist::save_runtime_state(&ws, &state).unwrap();
    let _ = store
        .create_message(None, "peer", "architect", "same-socket", None, false, None)
        .unwrap();
    let transport = OfflineTransport::new()
        .with_tmux_endpoint(socket)
        .with_targets(vec![pane_info("%0", "team-e51", "architect")]);

    let delivered = deliver_pending_messages(&ws, &state, &transport, &log)
        .expect("same-socket E51 guard should still resolve to a structured target");

    assert_eq!(delivered.len(), 1);
    assert_eq!(
        transport.inject_targets(),
        vec![Target::SessionWindow {
            session: SessionName::new("team-e51"),
            window: WindowName::new("architect_pane_conflicts_with_leader"),
        }],
        "same pane id on the same socket must keep the E51 loop guard"
    );
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
#[cfg(unix)]
#[test]
fn app_server_leader_delivery_marks_delivered_without_tmux_transport() {
    let ws = tmp_ws("appdeliver");
    let fake = crate::app_server_test_support::FakeAppServer::start(
        "deliver-ok",
        crate::app_server_test_support::FakeAppServerScript::happy(
            "thread-live",
            "session-live",
            ws.to_str().unwrap(),
        ),
    );
    let state = app_server_state(&ws, fake.endpoint(), "thread-live", "session-live");
    crate::state::persist::save_runtime_state(&ws, &state).unwrap();
    let store = store_for(&ws);
    let log = EventLog::new(&ws);
    let message_id = store
        .create_message(
            None,
            "worker_a",
            "leader",
            "app server hello",
            None,
            false,
            Some("team-a"),
        )
        .unwrap();

    let out = deliver_pending_message(&ws, &store, &NoopTransport, &message_id, &log, &state)
        .expect("app-server delivery should not require tmux transport");

    assert!(
        out.ok,
        "app-server turn/start acceptance is delivery proof: {out:?}"
    );
    assert_eq!(out.message_status.0, "delivered");
    assert_eq!(out.channel.as_deref(), Some("codex_app_server"));
    let turns = fake.received_turns();
    assert_eq!(turns.len(), 1);
    assert_eq!(
        turns[0]["params"]["clientUserMessageId"],
        serde_json::json!(message_id)
    );
    let events = log.tail(0).unwrap();
    assert!(
        events.iter().any(|event| {
            event.get("event").and_then(serde_json::Value::as_str)
                == Some("leader_receiver.app_server_submitted")
        }),
        "delivery must emit app-server submission event; events={events:?}"
    );
}

#[cfg(unix)]
#[test]
fn app_server_leader_delivery_stale_tuple_fails_closed_with_event() {
    let ws = tmp_ws("appstale");
    let fake = crate::app_server_test_support::FakeAppServer::start(
        "deliver-stale",
        crate::app_server_test_support::FakeAppServerScript::happy(
            "thread-live",
            "session-new",
            ws.to_str().unwrap(),
        ),
    );
    let state = app_server_state(&ws, fake.endpoint(), "thread-live", "session-old");
    crate::state::persist::save_runtime_state(&ws, &state).unwrap();
    let store = store_for(&ws);
    let log = EventLog::new(&ws);
    let message_id = store
        .create_message(
            None,
            "worker_a",
            "leader",
            "stale",
            None,
            false,
            Some("team-a"),
        )
        .unwrap();

    let out = deliver_pending_message(&ws, &store, &NoopTransport, &message_id, &log, &state)
        .expect("stale app-server binding should be a business failure, not panic");

    assert!(!out.ok);
    assert_eq!(out.channel.as_deref(), Some("rebind_required"));
    assert_eq!(out.message_status.0, "failed");
    let events = log.tail(0).unwrap();
    assert!(
        events.iter().any(|event| {
            event.get("event").and_then(serde_json::Value::as_str)
                == Some("leader_receiver.app_server_thread_stale")
                && event.get("message_id").and_then(serde_json::Value::as_str)
                    == Some(message_id.as_str())
        }),
        "stale app-server tuple must emit app_server_thread_stale; events={events:?}"
    );
}

#[test]
fn app_server_leader_delivery_fails_closed_on_mode_transport_conflict() {
    let ws = tmp_ws("appconflict");
    let mut state = app_server_state(
        &ws,
        "unix:///tmp/team-agent-should-not-connect.sock",
        "thread-live",
        "session-live",
    );
    state["teams"]["team-a"]["leader_receiver"]["mode"] = serde_json::json!("direct_tmux");
    crate::state::persist::save_runtime_state(&ws, &state).unwrap();
    let store = store_for(&ws);
    let log = EventLog::new(&ws);
    let message_id = store
        .create_message(
            None,
            "worker_a",
            "leader",
            "conflict",
            None,
            false,
            Some("team-a"),
        )
        .unwrap();

    let out = deliver_pending_message(&ws, &store, &NoopTransport, &message_id, &log, &state)
        .expect("transport conflict should be a business failure");

    assert!(!out.ok);
    assert_eq!(out.channel.as_deref(), Some("rebind_required"));
    assert_eq!(out.message_status.0, "failed");
    assert_eq!(
        out.verification.as_deref(),
        Some("run team-agent claim-leader, takeover, or attach-app-server-leader")
    );
    let events = log.tail(0).unwrap();
    assert!(
        events.iter().any(|event| {
            event.get("event").and_then(serde_json::Value::as_str)
                == Some("leader_receiver.delivery_blocked")
                && event.get("reason").and_then(serde_json::Value::as_str)
                    == Some("leader_receiver_transport_conflict")
        }),
        "transport conflict must emit explicit fail-closed event; events={events:?}"
    );
}

#[cfg(unix)]
#[test]
fn app_server_leader_busy_is_retryable_and_does_not_steer() {
    let ws = tmp_ws("appbusy");
    let mut script = crate::app_server_test_support::FakeAppServerScript::happy(
        "thread-live",
        "session-live",
        ws.to_str().unwrap(),
    );
    script.turn_error = Some("thread already has an active turn".to_string());
    let fake = crate::app_server_test_support::FakeAppServer::start("deliver-busy", script);
    let state = app_server_state(&ws, fake.endpoint(), "thread-live", "session-live");
    crate::state::persist::save_runtime_state(&ws, &state).unwrap();
    let store = store_for(&ws);
    let log = EventLog::new(&ws);
    let message_id = store
        .create_message(
            None,
            "worker_a",
            "leader",
            "busy",
            None,
            false,
            Some("team-a"),
        )
        .unwrap();

    let out = deliver_pending_message(&ws, &store, &NoopTransport, &message_id, &log, &state)
        .expect("leader busy should be retryable");

    assert!(!out.ok);
    assert_eq!(out.status, DeliveryStatus::RetryScheduled);
    assert_eq!(out.channel.as_deref(), Some("leader_busy"));
    let conn = crate::db::schema::open_db(store.db_path()).unwrap();
    let status: String = conn
        .query_row(
            "select status from messages where message_id = ?1",
            [&message_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(status, "target_resolved", "busy leader remains retryable");
    let turns = fake.received_turns();
    assert_eq!(
        turns.len(),
        1,
        "must only call turn/start, never turn/steer"
    );
    assert_eq!(turns[0]["method"], serde_json::json!("turn/start"));
}

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

    assert_eq!(
        out.status,
        DeliveryStatus::Queued,
        "team-b leader send must not inherit team-a/top-level unbound leader binding"
    );
    let message_id = out.message_id.expect("queued leader message id");
    let store = store_for(&ws);
    let conn = crate::db::schema::open_db(store.db_path()).unwrap();
    let owner_team_id: Option<String> = conn
        .query_row(
            "select owner_team_id from messages where message_id = ?1",
            [&message_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(owner_team_id.as_deref(), Some("team-b"));
}

// ════════════════════════════════════════════════════════════════════════
// GROUP V — retry_result_deliveries: re-route notify_failed watchers with
// dedupe_reason rebind_retry. result_delivery.py:19-35.
// ════════════════════════════════════════════════════════════════════════

#[test]
fn retry_result_deliveries_retries_notify_failed_watcher() {
    // SEEDED contract (result_delivery.py:18-34): retry_result_deliveries scans
    // retryable_result_watchers (status in pending/notify_failed), resolves each
    // watcher's result via result_by_id, and re-routes through notify_result_watchers
    // with dedupe_reason="rebind_retry". Seed a notify_failed watcher + its matching
    // result row → the watcher IS retried and a WatcherNotice for it is returned.
    // Probed golden: notices == [{watcher_id, result_id, ok, ...}] for the seeded
    // watcher (delivery ok depends on full team state; the retry-was-attempted
    // contract is the invariant — an empty store would NOT exercise it).
    let ws = tmp_ws("retrydeliv");
    let store = store_for(&ws);
    let log = EventLog::new(&ws);

    let rid = seed_result(&store, "res_r1", "t1", "alice", "success");
    let w = seed_watcher(
        &store,
        "w-failed",
        "team-a",
        "t1",
        "alice",
        "notify_failed",
        Some(&rid),
        None,
    );

    let notices = retry_result_deliveries(&ws, &log).unwrap();

    assert_eq!(
        notices.len(),
        1,
        "the single notify_failed watcher must be retried"
    );
    let notice = &notices[0];
    assert_eq!(
        notice.watcher_id, w,
        "the retried notice names the seeded watcher"
    );
    assert_eq!(
        notice.result_id.as_deref(),
        Some(rid.as_str()),
        "retry resolves and carries the watcher's result_id (rebind_retry path)"
    );
}

// ════════════════════════════════════════════════════════════════════════
// GROUP W — collect_results_and_notify_watchers orchestration shape.
// results.py:430-447.
// ════════════════════════════════════════════════════════════════════════

#[test]
fn collect_results_and_notify_watchers_returns_concrete_ok_shape() {
    // SEEDED contract (results.py:430-447): with NO uncollected results, collect() is
    // skipped (the `if store.results(uncollected_only=True)` guard is false), so the
    // result stays {ok:true, collected_results:[]}; a seeded notify_failed watcher
    // whose result_id has no matching results row is resolved to None by
    // retry_result_deliveries → skipped → notified stays []. Probed golden (against
    // exactly this fixture): {"ok": true, "collected": 0, "notified": []}.
    // (The previous test asserted only out["ok"].is_some(), trivially passed by
    // {"ok": false}.)
    let ws = tmp_ws("collectnotify");
    let store = store_for(&ws);
    let log = EventLog::new(&ws);

    seed_watcher(
        &store,
        "w-orphan",
        "team-a",
        "t1",
        "alice",
        "notify_failed",
        Some("res_missing"),
        None,
    );

    let out = collect_results_and_notify_watchers(&ws, &log).unwrap();
    assert_eq!(
        out.get("ok").and_then(|v| v.as_bool()),
        Some(true),
        "ok==true"
    );
    assert_eq!(
        out.get("collected").and_then(|v| v.as_i64()),
        Some(0),
        "no uncollected results → collected==0"
    );
    assert_eq!(
        out.get("notified")
            .and_then(|v| v.as_array())
            .map(|a| a.len()),
        Some(0),
        "orphan watcher (missing result) is skipped → notified empty"
    );
}

// ════════════════════════════════════════════════════════════════════════
// GROUP X — delivered_result_message content-level dedupe lookup +
// result_id_from_text dual (scheduler send dedupe path). result_delivery.py:394.
// ════════════════════════════════════════════════════════════════════════

#[test]
fn delivered_result_message_none_in_fresh_store() {
    let ws = tmp_ws("delivdedupe");
    let store = store_for(&ws);
    let found = delivered_result_message(&store, "r1", None, None).unwrap();
    assert!(found.is_none());
}

#[test]
fn delivered_result_message_empty_result_id_is_none() {
    // result_delivery.py:401-402 — empty result_id short-circuits to None.
    let ws = tmp_ws("delivdedupe2");
    let store = store_for(&ws);
    let found = delivered_result_message(&store, "", None, None).unwrap();
    assert!(found.is_none());
}

// ═══════════════════════════════════════════════════════════════════════════
// collect #223 — task-scoped collect + send --task validation (RED).
// ═══════════════════════════════════════════════════════════════════════════

// (c) a result whose task_id ∈ state.tasks collects as scope:"task"; the task row advances to
// "done" (success → done, runtime.py:1066); results.collected ≥ 1. Proves the collect-READ works
// once state.tasks is seeded — so the #223 fix target is the upstream seeding, not collect.
#[test]
fn collect_task_scoped_result_collects_and_marks_task_done() {
    let ws = tmp_ws("collecttask223");
    std::fs::write(ws.join("team.spec.yaml"), "version: 1\n").unwrap();
    crate::state::persist::save_runtime_state(
        &ws,
        &serde_json::json!({
            "session_name": "team-x",
            "agents": { "w1": { "provider": "codex" } },
            "tasks": [ { "id": "t2", "assignee": "w1", "title": "t2", "status": "pending" } ]
        }),
    )
    .unwrap();
    let store = store_for(&ws);
    seed_result(&store, "res_t2", "t2", "w1", "success");

    let out = collect(&ws, None, false).unwrap();
    assert_eq!(
        out.get("ok").and_then(|v| v.as_bool()),
        Some(true),
        "no invalid → ok:true"
    );
    let cr = out
        .get("collected_results")
        .and_then(|v| v.as_array())
        .expect("collected_results");
    assert_eq!(cr.len(), 1, "the seeded t2 result must collect");
    assert_eq!(
        cr[0].get("scope").and_then(|v| v.as_str()),
        Some("task"),
        "t2 ∈ state.tasks → scope:task"
    );
    assert_eq!(cr[0].get("task_id").and_then(|v| v.as_str()), Some("t2"));
    assert_eq!(cr[0].get("agent_id").and_then(|v| v.as_str()), Some("w1"));
    assert!(
        out.get("results")
            .and_then(|r| r.get("collected"))
            .and_then(|v| v.as_i64())
            .unwrap_or(0)
            >= 1,
        "results.collected must be ≥ 1"
    );
    let st = crate::state::persist::load_runtime_state(&ws).unwrap();
    let t2_status = st
        .get("tasks")
        .and_then(|v| v.as_array())
        .and_then(|ts| {
            ts.iter()
                .find(|t| t.get("id").and_then(|v| v.as_str()) == Some("t2"))
        })
        .and_then(|t| t.get("status"))
        .and_then(|v| v.as_str());
    assert_eq!(
        t2_status,
        Some("done"),
        "success result → task row status 'done' (runtime.py:1066)"
    );
}

// (c-C1) collect OUTPUT shape: collected_results entries are the 8-KEY SUMMARY (NO inlined
// envelope; carry summary+tests) and the full envelopes live in a SEPARATE top-level `collected`
// list (golden results.py:86/131). Rust inlines `envelope`+`owner_team_id` and emits no
// `collected` list → RED.
#[test]
fn collect_output_matches_golden_collected_shape() {
    let ws = tmp_ws("collectshape223");
    std::fs::write(ws.join("team.spec.yaml"), "version: 1\n").unwrap();
    crate::state::persist::save_runtime_state(
        &ws,
        &serde_json::json!({
            "session_name": "team-x",
            "agents": { "w1": { "provider": "codex" } },
            "tasks": [ { "id": "t2", "assignee": "w1", "title": "t2", "status": "pending" } ]
        }),
    )
    .unwrap();
    let store = store_for(&ws);
    seed_result(&store, "res_t2s", "t2", "w1", "success");

    let out = collect(&ws, None, false).unwrap();
    let cr = out
        .get("collected_results")
        .and_then(|v| v.as_array())
        .expect("collected_results");
    let e = &cr[0];
    // C1: collected_results entry is the 8-key SUMMARY — NO envelope inlined; carries summary+tests.
    assert!(e.get("envelope").is_none(),
        "collected_results entry must NOT inline `envelope` (golden 8-key summary); the full envelope belongs in `collected`. got {e:?}");
    assert!(
        e.get("summary").is_some() && e.get("tests").is_some(),
        "collected_results summary entry must carry `summary`+`tests` (golden results.py:131)"
    );
    // C1: the full envelopes live in a separate top-level `collected` list.
    let collected = out
        .get("collected")
        .and_then(|v| v.as_array())
        .expect("golden collect returns a top-level `collected` list of full envelopes");
    assert!(
        collected
            .first()
            .and_then(|env| env.get("schema_version"))
            .and_then(|v| v.as_str())
            == Some("result_envelope_v1"),
        "collected[0] must be the full result_envelope_v1 envelope; got {collected:?}"
    );

    // ── STRENGTHENED (option-B byte-parity, leader-adjudicated 0700cff review) ──
    // D3 — task-scope collected_results entry must be EXACTLY the golden 8 keys, in order, NO task_status.
    let keys: Vec<&str> = e
        .as_object()
        .expect("entry is an object")
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        keys,
        vec!["result_id", "task_id", "agent_id", "status", "summary", "tests", "created_at", "scope"],
        "collected_results entry must be EXACTLY the golden 8 keys in order (results.py:131; no task_status/envelope/owner_team_id); got {keys:?}"
    );
    // D1+D2 — collect RETURN top-level key order must match golden EXACTLY: delivered_messages BEFORE
    // invalid_results, AND a `coordinator` key (mirroring golden _ensure_coordinator_after_collect).
    let top: Vec<&str> = out
        .as_object()
        .expect("collect result is an object")
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        top,
        vec![
            "ok",
            "collected",
            "collected_results",
            "delivered_messages",
            "invalid_results",
            "results",
            "state_file",
            "coordinator"
        ],
        "collect return top-level key order must match golden return shape; got {top:?}"
    );
}

// (d) send --task <unknown id> must RAISE golden "unknown task id" (runtime.py:1032 _find_task),
// not silently create a message. Rust send_message attaches task_id without validating → Ok. RED.
// block_until_delivered=false isolates the task-validation from any delivery side-effect.
#[test]
fn send_with_unknown_task_id_raises_unknown_task() {
    let ws = tmp_ws("sendunknowntask223");
    crate::state::persist::save_runtime_state(
        &ws,
        &serde_json::json!({
            "session_name": "team-x",
            "agents": { "w1": { "provider": "codex" } },
            "tasks": []
        }),
    )
    .unwrap();
    let _ = store_for(&ws);
    let opts = SendOptions {
        task_id: Some(crate::model::ids::TaskId::new("t2-unknown")),
        block_until_delivered: false,
        ..SendOptions::default()
    };
    let out = send_message(&ws, &MessageTarget::Single("w1".to_string()), "go", &opts);
    match out {
        Err(e) => {
            // SURFACED error = the CLI `error` field = CliError::from(MessagingError).to_string()
            // (to_payload uses self.to_string(), types.rs:59). Must EQUAL golden's bare message —
            // NO "validation:" variant prefix (golden runtime.py:1032 surfaces str(exc)).
            let surfaced = crate::cli::CliError::from(e).to_string();
            assert_eq!(
                surfaced, "unknown task id: t2-unknown",
                "surfaced CLI error must EQUAL golden's message with NO variant prefix; got {surfaced:?}"
            );
        }
        Ok(o) => panic!(
            "send --task <unknown id> must RAISE 'unknown task id' (golden runtime.py:1032 _find_task), \
             not silently create a message; got Ok({o:?})"
        ),
    }
}

// ════════════════════════════════════════════════════════════════════════
// P0 REGRESSION (0700cff "send 0 bytes, nothing queued" / coordinator never delivers).
// golden gates the unknown-task RAISE on route_task_id (send.py:204 `if task_id and route_task_id`);
// delivery/fanout/internal sends pass route_task_id=False (internal_delivery.py:44, send.py:412/481)
// → the task is a label, NOT validated. 0700cff's UNCONDITIONAL task_exists gate broke every
// task-tagged delivery/internal send at CREATION time. The gate the OfflineTransport tests missed.
// ════════════════════════════════════════════════════════════════════════

// (a) [REGRESSION GATE] route_task_id=false + task_id NOT in state.tasks → send SUCCEEDS and the
// message is QUEUED (real create path; no transport). Must NOT raise "unknown task id".
#[test]
fn send_route_task_id_false_skips_task_validation_and_queues() {
    let ws = tmp_ws("sendroutefalse");
    crate::state::persist::save_runtime_state(
        &ws,
        &serde_json::json!({ "session_name": "team-x", "agents": { "w1": { "provider": "codex" } }, "tasks": [] }),
    ).unwrap();
    let _ = store_for(&ws);
    let opts = SendOptions {
        task_id: Some(crate::model::ids::TaskId::new("t-not-seeded")),
        route_task_id: false,
        block_until_delivered: false,
        ..SendOptions::default()
    };
    let out = send_message(&ws, &MessageTarget::Single("w1".to_string()), "deliver me", &opts)
        .expect("route_task_id=false must NOT validate the task — golden delivery/internal path queues regardless of state.tasks");
    assert!(
        out.message_id.is_some(),
        "the message must be CREATED (message_id present) on the route_task_id=false path; got {out:?}"
    );
    // real queue verification (not an Ok shell): the message landed in w1's inbox.
    let inbox = store_for(&ws).inbox("w1", 10, None).expect("inbox");
    assert!(
        !inbox.is_empty(),
        "the task-tagged message must be QUEUED for w1 on the delivery/internal path; inbox empty (0 bytes queued = the P0)"
    );
}

// (c) [LOCK] route_task_id=true + task_id IN state.tasks → send SUCCEEDS (routing happy-path).
#[test]
fn send_route_task_id_true_known_task_succeeds() {
    let ws = tmp_ws("sendrouteknown");
    crate::state::persist::save_runtime_state(
        &ws,
        &serde_json::json!({
            "session_name": "team-x",
            "agents": { "w1": { "provider": "codex" } },
            "tasks": [ { "id": "t-known", "assignee": "w1", "title": "t", "status": "pending" } ]
        }),
    )
    .unwrap();
    let _ = store_for(&ws);
    let opts = SendOptions {
        task_id: Some(crate::model::ids::TaskId::new("t-known")),
        route_task_id: true,
        block_until_delivered: false,
        ..SendOptions::default()
    };
    let out = send_message(&ws, &MessageTarget::Single("w1".to_string()), "go", &opts)
        .expect("route_task_id=true with a KNOWN task must succeed");
    assert!(
        out.message_id.is_some(),
        "known-task routing send must create the message; got {out:?}"
    );
}

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
#[test]
fn r8_attach_requeue_exhausted_to_notify_failed_golden_attach_event() {
    let ws = tmp_ws("r8requeue");
    let store = store_for(&ws);
    let log = EventLog::new(&ws);
    let team = TeamKey::new("team-a");
    let pane = PaneId::new("%leader-new");

    // --- Sub-A: DRIVE w-r8 (team-a) to delivery_exhausted via notify_result_watchers (attempts>=MAX) ---
    let rid = seed_result(&store, "res_r8", "t1", "alice", "success");
    seed_watcher(
        &store,
        "w-r8",
        "team-a",
        "t1",
        "alice",
        "pending",
        Some(&rid),
        None,
    );
    // attempts are EVENT-counted (result_watcher.notify_failed/retry_notified) — seed MAX prior failures.
    for n in 0..u64::from(RESULT_DELIVERY_MAX_ATTEMPTS) {
        log.write(
            "result_watcher.notify_failed",
            json(serde_json::json!({"watcher_id": "w-r8", "result_id": rid.as_str(), "status": "notify_failed", "error": "x", "n": n})),
        ).unwrap();
    }
    let result_env =
        json(serde_json::json!({"result_id": rid.as_str(), "task_id": "t1", "agent_id": "alice"}));
    let watcher_view = json(serde_json::json!({
        "watcher_id": "w-r8", "task_id": "t1", "agent_id": "alice",
        "created_at": "2026-01-01T00:00:00Z", "owner_team_id": "team-a",
        "leader_id": "leader", "result_id": rid.as_str()
    }));
    notify_result_watchers(&ws, &result_env, &log, Some(&[watcher_view]), None).unwrap();
    let (driven, _) = watcher_state(&store, "w-r8");
    assert_eq!(driven, "delivery_exhausted",
        "PRECONDITION: notify_result_watchers at attempts>=MAX must persist delivery_exhausted (watchers.rs:161-168) — \
         proves the attach-requeue input is real, not 空过");

    // selection-lock fixtures: cross-team exhausted + notified exhausted (Gap-32) + pending.
    let team_b = seed_watcher(
        &store,
        "w-teamb",
        "team-b",
        "t2",
        "bob",
        "delivery_exhausted",
        Some("res_b"),
        None,
    );
    let notif = seed_watcher(
        &store,
        "w-notified",
        "team-a",
        "t3",
        "carol",
        "delivery_exhausted",
        Some("res_c"),
        Some("msg_done"),
    );
    seed_watcher(
        &store,
        "w-pending",
        "team-a",
        "t4",
        "dave",
        "pending",
        Some("res_d"),
        None,
    );

    // --- Sub-B: attach requeue (golden contract) ---
    let requeued = requeue_delivery_exhausted_watchers(&ws, &store, &log, &team, &pane).unwrap();

    // D2: team-a exhausted -> notify_failed (NOT pending).
    let (st_a, _) = watcher_state(&store, "w-r8");
    assert_eq!(st_a, "notify_failed",
        "D2: attach requeue must flip delivery_exhausted -> 'notify_failed' (golden result_watchers.py:95), not 'pending'");
    // D1 ✦ team-scoped: cross-team exhausted watcher must NOT requeue onto team-a's pane.
    let (st_b, _) = watcher_state(&store, &team_b);
    assert_eq!(st_b, "delivery_exhausted",
        "D1 ✦: team-scoped selection — a team-b exhausted watcher must NOT be requeued by a team-a attach (anti cross-team pollution / CP-1)");
    // Gap-32: a notified watcher is never requeued; its notified_message_id survives.
    let (st_n, nid) = watcher_state(&store, &notif);
    assert_eq!(
        st_n, "delivery_exhausted",
        "Gap-32: notified watcher not requeued"
    );
    assert_eq!(
        nid.as_deref(),
        Some("msg_done"),
        "Gap-32: notified_message_id preserved"
    );
    // only the team-a unnotified exhausted watcher requeues.
    let ids: Vec<&str> = requeued.iter().map(|n| n.watcher_id.as_str()).collect();
    assert_eq!(
        ids,
        vec!["w-r8"],
        "only team-a unnotified delivery_exhausted watcher requeues"
    );

    // D3: result_watcher.requeued payload == golden ATTACH form {watcher_id, trigger, new_pane_id}.
    let events = log.tail(0).unwrap();
    let ev = events
        .iter()
        .rev()
        .find(|e| e.get("event").and_then(|v| v.as_str()) == Some("result_watcher.requeued"))
        .expect("result_watcher.requeued event");
    let keys: std::collections::BTreeSet<&str> = ev
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .filter(|k| *k != "ts" && *k != "event")
        .collect();
    let expected: std::collections::BTreeSet<&str> = ["watcher_id", "trigger", "new_pane_id"]
        .into_iter()
        .collect();
    assert_eq!(keys, expected,
        "D3: result_watcher.requeued must be golden ATTACH form {{watcher_id, trigger, new_pane_id}} (leader/__init__.py:46-50), not claim-style; got {keys:?}");
    assert_eq!(
        ev.get("trigger").and_then(|v| v.as_str()),
        Some("attach_leader")
    );
    assert_eq!(
        ev.get("new_pane_id").and_then(|v| v.as_str()),
        Some("%leader-new")
    );
}

// E15/E23 双投守卫:report_result 的 pane fallback 必须被 `if !outcome.ok` 守为
// deliver 失败时的真兜底,绝不在 deliver 成功后无条件再投一次(=用户看到两条同内容回复)。
#[test]
fn e15_direct_inject_is_gated_by_deliver_failure_not_unconditional() {
    let src = include_str!("../results.rs");
    // E23:旧 private direct inject 已合并到 shared deliver_to_leader_fallback_pane primitive。
    let gate = "if !outcome.ok {";
    let fallback_call = "deliver_to_leader_fallback_pane(";
    let gate_pos = src.find(gate);
    assert!(
        gate_pos.is_some(),
        "E15/E23: pane fallback must be gated by `if !outcome.ok` (deliver-fail fallback)"
    );
    let fallback_after_gate = src[gate_pos.unwrap()..].find(fallback_call);
    assert!(
        fallback_after_gate.is_some(),
        "E15/E23: report_result fallback must call the shared fallback pane primitive after the failure gate"
    );
    assert!(
        !src.contains("inject_leader_notification_direct"),
        "E23: private direct inject must stay deleted; all callers use deliver_to_leader_fallback_pane"
    );
}
