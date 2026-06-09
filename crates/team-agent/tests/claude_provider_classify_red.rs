//! Claude provider classifier contracts for Batch A Lane 1.
//!
//! These fixtures are JSONL records shaped like Claude transcripts. They assert user-visible
//! provider state, not classifier internals.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use team_agent::model::enums::Provider;
use team_agent::provider::{classify, ProcessLiveness, TurnState};

#[test]
fn claude_assistant_content_list_without_stop_reason_is_in_flight_working() {
    let transcript = r#"{"type":"assistant","requestId":"req-open","message":{"id":"msg_1","role":"assistant","content":[{"type":"text","text":"I am working."},{"type":"tool_use","id":"toolu_1","name":"Read","input":{"file_path":"README.md"}}]}}"#;

    let result = classify(Provider::Claude, transcript, ProcessLiveness::Alive, 0.0)
        .expect("Claude classification should be fallible but successful");

    assert_eq!(
        result.state,
        TurnState::Working,
        "Claude assistant message with content=list and missing stop_reason is an open turn while the process is alive; result={result:?}"
    );
    assert_eq!(
        result.reason, "assistant_in_flight",
        "the public reason must explain that Claude assistant output is still in flight, not unknown/idle; result={result:?}"
    );
}

#[test]
fn claude_api_error_record_is_abnormal_not_unknown_or_idle() {
    let transcript = r#"{"type":"system","subtype":"api_error","level":"error","sessionId":"sess-api-error","message":"upstream provider failed"}"#;

    let result = classify(Provider::Claude, transcript, ProcessLiveness::Alive, 0.0)
        .expect("Claude api_error classification should be fallible but successful");

    assert_eq!(
        result.state,
        TurnState::Abnormal,
        "Claude api_error must surface as abnormal so lifecycle/doctor can report a provider failure; result={result:?}"
    );
    assert_eq!(
        result.reason, "api_error",
        "Claude api_error must keep a stable public reason; result={result:?}"
    );
}

#[test]
fn claude_tool_result_error_record_is_abnormal_not_turn_complete() {
    let transcript = r#"{"type":"user","uuid":"user-tool-error","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_1","is_error":true,"content":"permission denied"}]}}"#;

    let result = classify(Provider::Claude, transcript, ProcessLiveness::Alive, 0.0)
        .expect("Claude tool_result error classification should be fallible but successful");

    assert_eq!(
        result.state,
        TurnState::Abnormal,
        "Claude tool_result is_error=true must classify abnormal and must not be lost as no_turn_lifecycle_fact; result={result:?}"
    );
    assert_eq!(
        result.reason, "tool_result_is_error",
        "Claude tool_result error must keep a stable public reason; result={result:?}"
    );
}
