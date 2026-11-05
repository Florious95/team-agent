use super::*;

// ════════════════════════════════════════════════════════════════════════
// GROUP A — serde byte-locks (audit/event wire values; changing one byte
// breaks downstream recognizers/event consumers). delivery.py / send.py /
// leader.py / scheduler.py / diagnose/comms.py.
// ════════════════════════════════════════════════════════════════════════








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


// ════════════════════════════════════════════════════════════════════════
// GROUP B — bounded-retry constants + backoff (delivery.py:60-61,
// scheduler.py:134 / results.py:251, result_delivery.py:15). Locks the
// termination-guarantee so no infinite-loop regression.
// ════════════════════════════════════════════════════════════════════════





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
