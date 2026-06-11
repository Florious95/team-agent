use super::*;

// ════════════════════════════════════════════════════════════════════════
// GROUP A — serde byte-locks (audit/event wire values; changing one byte
// breaks downstream recognizers/event consumers). delivery.py / send.py /
// leader.py / scheduler.py / diagnose/comms.py.
// ════════════════════════════════════════════════════════════════════════

#[test]
fn delivery_status_serde_snake_case_byte_locked() {
    let cases = [
        (DeliveryStatus::Delivered, "\"delivered\""),
        (DeliveryStatus::Failed, "\"failed\""),
        (DeliveryStatus::Queued, "\"queued\""),
        (DeliveryStatus::Blocked, "\"blocked\""),
        (DeliveryStatus::Refused, "\"refused\""),
        (DeliveryStatus::Degraded, "\"degraded\""),
        (DeliveryStatus::RetryScheduled, "\"retry_scheduled\""),
        (
            DeliveryStatus::TrustAutoAnswerExhausted,
            "\"trust_auto_answer_exhausted\"",
        ),
        (DeliveryStatus::AlreadyDelivered, "\"already_delivered\""),
        (DeliveryStatus::FallbackLog, "\"fallback_log\""),
        (
            DeliveryStatus::BroadcastDelivered,
            "\"broadcast_delivered\"",
        ),
        (DeliveryStatus::BroadcastPartial, "\"broadcast_partial\""),
        (DeliveryStatus::FanoutDelivered, "\"fanout_delivered\""),
        (DeliveryStatus::FanoutPartial, "\"fanout_partial\""),
    ];
    for (variant, wire) in cases {
        assert_eq!(serde_json::to_string(&variant).unwrap(), wire);
    }
}

#[test]
fn delivery_refusal_serde_snake_case_byte_locked() {
    let cases = [
        (DeliveryRefusal::TargetNotInTeam, "\"target_not_in_team\""),
        (
            DeliveryRefusal::HumanConfirmationRequired,
            "\"human_confirmation_required\"",
        ),
        (
            DeliveryRefusal::MissingPermissions,
            "\"missing_permissions\"",
        ),
        (DeliveryRefusal::RecipientBusy, "\"recipient_busy\""),
        (DeliveryRefusal::UnknownRecipient, "\"unknown_recipient\""),
        (
            DeliveryRefusal::TmuxTargetMissing,
            "\"tmux_target_missing\"",
        ),
        (
            DeliveryRefusal::MessageAlreadyClaimed,
            "\"message_already_claimed\"",
        ),
        (
            DeliveryRefusal::LeaderNotAttached,
            "\"leader_not_attached\"",
        ),
        (
            DeliveryRefusal::CoordinatorUnavailable,
            "\"coordinator_unavailable\"",
        ),
        (
            DeliveryRefusal::TeamOwnerMismatch,
            "\"team_owner_mismatch\"",
        ),
        (DeliveryRefusal::Ambiguous, "\"ambiguous\""),
        (
            DeliveryRefusal::RecipientPaneInNonInputMode,
            "\"recipient_pane_in_non_input_mode\"",
        ),
        (DeliveryRefusal::SessionDrift, "\"session_drift\""),
    ];
    for (variant, wire) in cases {
        assert_eq!(serde_json::to_string(&variant).unwrap(), wire);
    }
}

#[test]
fn delivery_stage_serde_snake_case_byte_locked() {
    // delivery.py:309 injection.stage values.
    assert_eq!(
        serde_json::to_string(&DeliveryStage::TrustAutoAnswerDismissalWait).unwrap(),
        "\"trust_auto_answer_dismissal_wait\""
    );
    assert_eq!(
        serde_json::to_string(&DeliveryStage::Inject).unwrap(),
        "\"inject\""
    );
    assert_eq!(
        serde_json::to_string(&DeliveryStage::Submit).unwrap(),
        "\"submit\""
    );
    assert_eq!(
        serde_json::to_string(&DeliveryStage::VisibleCheck).unwrap(),
        "\"visible_check\""
    );
}

#[test]
fn scheduled_kind_serde_byte_locked() {
    // scheduler.py:46,84,87 dispatch keys — exhaustive, no runtime fallback.
    assert_eq!(
        serde_json::to_string(&ScheduledKind::Send).unwrap(),
        "\"send\""
    );
    assert_eq!(
        serde_json::to_string(&ScheduledKind::HealthPing).unwrap(),
        "\"health_ping\""
    );
    assert_eq!(
        serde_json::to_string(&ScheduledKind::TrustRetry).unwrap(),
        "\"trust_retry\""
    );
}

#[test]
fn activity_status_serde_byte_locked() {
    // activity_detector.py:107 status values consumed by ping/take-over.
    assert_eq!(
        serde_json::to_string(&ActivityStatus::Working).unwrap(),
        "\"working\""
    );
    assert_eq!(
        serde_json::to_string(&ActivityStatus::Idle).unwrap(),
        "\"idle\""
    );
    assert_eq!(
        serde_json::to_string(&ActivityStatus::Stuck).unwrap(),
        "\"stuck\""
    );
    assert_eq!(
        serde_json::to_string(&ActivityStatus::Uncertain).unwrap(),
        "\"uncertain\""
    );
}

#[test]
fn alert_type_serde_byte_locked() {
    // scheduler.py:38 _ALERT_TYPES set members.
    assert_eq!(
        serde_json::to_string(&AlertType::Stuck).unwrap(),
        "\"stuck\""
    );
    assert_eq!(
        serde_json::to_string(&AlertType::IdleFallback).unwrap(),
        "\"idle_fallback\""
    );
    assert_eq!(
        serde_json::to_string(&AlertType::CrossWorkerDeadlock).unwrap(),
        "\"cross_worker_deadlock\""
    );
}

#[test]
fn check_status_serde_byte_locked() {
    assert_eq!(
        serde_json::to_string(&CheckStatus::Pass).unwrap(),
        "\"pass\""
    );
    assert_eq!(
        serde_json::to_string(&CheckStatus::Fail).unwrap(),
        "\"fail\""
    );
    assert_eq!(
        serde_json::to_string(&CheckStatus::Deferred).unwrap(),
        "\"deferred\""
    );
    assert_eq!(
        serde_json::to_string(&CheckStatus::NotImplemented).unwrap(),
        "\"not_implemented\""
    );
    assert_eq!(
        serde_json::to_string(&CheckStatus::NotChallenged).unwrap(),
        "\"not_challenged\""
    );
}

#[test]
fn check_kind_serde_byte_locked() {
    // diagnose/comms.py:121,149 verifies values.
    assert_eq!(
        serde_json::to_string(&CheckKind::ReceiverBinding).unwrap(),
        "\"receiver_binding\""
    );
    assert_eq!(
        serde_json::to_string(&CheckKind::ContractSuite).unwrap(),
        "\"contract_suite\""
    );
    assert_eq!(
        serde_json::to_string(&CheckKind::NoProviderSdkCalls).unwrap(),
        "\"no_provider_sdk_calls\""
    );
}

#[test]
fn receiver_mode_serde_byte_locked() {
    // leader.py:103,166 — only direct_tmux is a legal receiver mode.
    assert_eq!(
        serde_json::to_string(&ReceiverMode::DirectTmux).unwrap(),
        "\"direct_tmux\""
    );
}

// ════════════════════════════════════════════════════════════════════════
// GROUP B — bounded-retry constants + backoff (delivery.py:60-61,
// scheduler.py:134 / results.py:251, result_delivery.py:15). Locks the
// termination-guarantee so no infinite-loop regression.
// ════════════════════════════════════════════════════════════════════════

#[test]
fn trust_retry_max_attempts_is_four() {
    assert_eq!(TRUST_RETRY_MAX_ATTEMPTS, 4);
}

#[test]
fn trust_retry_backoff_table_byte_locked() {
    // delivery.py:60 _TRUST_RETRY_BACKOFF_SECONDS = {2:5, 3:15, 4:30}.
    assert_eq!(TRUST_RETRY_BACKOFF_SECONDS, &[(2, 5), (3, 15), (4, 30)]);
}

#[test]
fn send_retry_max_attempts_is_three() {
    // scheduler.py:134 / results.py:251 max_attempts = 3.
    assert_eq!(SEND_RETRY_MAX_ATTEMPTS, 3);
}

#[test]
fn result_delivery_max_attempts_is_five() {
    // result_delivery.py:15 _RESULT_DELIVERY_MAX_ATTEMPTS = 5.
    assert_eq!(RESULT_DELIVERY_MAX_ATTEMPTS, 5);
}

// ════════════════════════════════════════════════════════════════════════
// GROUP C — pure-value helper impls already in the skeleton: the IRON LAW.
// ProviderSdkCalls::is_zero is implemented (GREEN check); the gate fns are
// unimplemented!() so they are RED until ported.
// ════════════════════════════════════════════════════════════════════════

#[test]
fn provider_sdk_calls_is_zero_only_when_all_three_zero() {
    // diagnose/comms.py:148 `any(calls.values())` negated.
    assert!(ProviderSdkCalls::default().is_zero());
    assert!(ProviderSdkCalls {
        anthropic: 0,
        openai: 0,
        httpx: 0
    }
    .is_zero());
    assert!(!ProviderSdkCalls {
        anthropic: 1,
        openai: 0,
        httpx: 0
    }
    .is_zero());
    assert!(!ProviderSdkCalls {
        anthropic: 0,
        openai: 0,
        httpx: 9
    }
    .is_zero());
}

#[test]
fn alert_type_all_is_sorted_full_set() {
    // scheduler.py:269 sorted(_ALERT_TYPES) =
    //   ['cross_worker_deadlock','idle_fallback','stuck'].
    assert_eq!(
        AlertType::all(),
        [
            AlertType::CrossWorkerDeadlock,
            AlertType::IdleFallback,
            AlertType::Stuck
        ]
    );
}

#[test]
fn activity_status_idle_takeover_gate_uncertain_never_idle() {
    // THE IRON LAW (bug-071/077/085): only Idle passes; Uncertain/Working/Stuck
    // are explicitly blocked — Uncertain must NOT fall through to idle.
    assert!(ActivityStatus::Idle.allows_idle_takeover());
    assert!(!ActivityStatus::Uncertain.allows_idle_takeover());
    assert!(!ActivityStatus::Working.allows_idle_takeover());
    assert!(!ActivityStatus::Stuck.allows_idle_takeover());
}

// ════════════════════════════════════════════════════════════════════════
// GROUP D — result_delivery.py pure fns: format/parse dual + KEY INSERTION
// ORDER + None-vs-empty + watcher matching. Byte-level golden (probed).
// ════════════════════════════════════════════════════════════════════════

#[test]
fn format_result_watcher_notification_full_shape_byte_locked() {
    // result_delivery.py:521 — with tests + result_id. INSERTION ORDER matters:
    // result_id inserted at idx 1 first, THEN tests inserted at idx 1 → Tests line
    // ends up ABOVE the Result id line. Tests capped at 3.
    let result = json(serde_json::json!({
        "task_id": "t1", "agent_id": "alice", "status": "success", "summary": "done",
        "result_id": "res-99",
        "tests": [
            {"command": "pytest", "status": "passed"},
            {"command": "lint", "status": "failed"},
            {"command": "a", "status": "x"},
            {"command": "extra", "status": "skip"}
        ]
    }));
    assert_eq!(
        format_result_watcher_notification(&result),
        "Task t1 reported success from alice: done\n\
         Tests: pytest=passed; lint=failed; a=x\n\
         Result id: res-99\n\
         Team Agent has collected this result and updated team_state.md. No manual polling is needed."
    );
}

#[test]
fn format_result_watcher_notification_minimal_uses_defaults() {
    // Empty result → unknown-task / unknown / unknown agent / completed defaults,
    // NO Tests line, NO Result id line.
    assert_eq!(
        format_result_watcher_notification(&json(serde_json::json!({}))),
        "Task unknown task reported unknown from unknown agent: completed\n\
         Team Agent has collected this result and updated team_state.md. No manual polling is needed."
    );
}

#[test]
fn format_result_watcher_notification_result_id_only_no_tests() {
    let result = json(serde_json::json!({
        "task_id": "t1", "agent_id": "bob", "status": "blocked", "summary": "s",
        "result_id": "R1"
    }));
    assert_eq!(
        format_result_watcher_notification(&result),
        "Task t1 reported blocked from bob: s\n\
         Result id: R1\n\
         Team Agent has collected this result and updated team_state.md. No manual polling is needed."
    );
}

#[test]
fn result_id_from_text_roundtrips_with_formatter() {
    // result_delivery.py:415 — dual of the formatter (content-level dedupe key).
    let result = json(serde_json::json!({
        "task_id": "t1", "agent_id": "alice", "status": "success",
        "summary": "done", "result_id": "res-99"
    }));
    let rendered = format_result_watcher_notification(&result);
    assert_eq!(result_id_from_text(&rendered), Some("res-99".to_string()));
}

#[test]
fn result_id_from_text_none_when_absent() {
    assert_eq!(result_id_from_text("no id here\nplain"), None);
}

#[test]
fn result_id_from_text_empty_value_is_none_not_empty_string() {
    // "Result id: " with empty payload → None (the `or None` after strip).
    assert_eq!(result_id_from_text("Result id: \nx"), None);
}

#[test]
fn result_id_from_text_strips_trailing_whitespace() {
    assert_eq!(
        result_id_from_text("Result id: abc  \n"),
        Some("abc".to_string())
    );
}
