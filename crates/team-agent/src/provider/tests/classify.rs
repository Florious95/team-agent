fn claude_end_turn() -> String {
    r#"{"type":"assistant","requestId":"r1","message":{"stop_reason":"end_turn"}}"#.to_string()
}
fn claude_open_turn() -> String {
    r#"{"type":"assistant","requestId":"r1","message":{"stop_reason":"tool_use"}}"#.to_string()
}

#[test]
fn classify_empty_text_is_unknown_never_idle() {
    // probe empty_text: state=unknown turn_id=None reason=unreadable_or_empty
    // source=session_file. BLOOD-LINE: unknown is NEVER idle (bug-071/077/085).
    let c = classify(Provider::ClaudeCode, "", ProcessLiveness::Unverifiable, 0.0)
        .expect("classify ok");
    assert_eq!(c.state, TurnState::Unknown);
    assert_eq!(c.reason, "unreadable_or_empty");
    assert_eq!(c.turn_id, None);
    assert_eq!(c.source, ClassifySource::SessionFile);
    // The pin the audit demands: classify(unreadable) MUST NOT be treatable as idle.
    assert!(
        !c.state.is_idle_for_takeover(),
        "unreadable/empty input must never be idle (C5)"
    );
}

#[test]
fn classify_whitespace_only_is_unknown() {
    // probe whitespace_only → unknown / unreadable_or_empty.
    let c = classify(Provider::Claude, "   \n  \t \n", ProcessLiveness::Unverifiable, 0.0)
        .expect("classify ok");
    assert_eq!(c.state, TurnState::Unknown);
    assert_eq!(c.reason, "unreadable_or_empty");
    assert!(!c.state.is_idle_for_takeover());
}

#[test]
fn classify_garbage_jsonl_is_unknown_not_idle() {
    // probe garbage_jsonl ("not json\n{broken"): parse yields diagnostics but
    // had_records=false → reason=unreadable_or_empty (the !had_records branch
    // wins over diagnostics; common.py:72-75). NEVER idle.
    let c = classify(Provider::Claude, "not json\n{broken", ProcessLiveness::Unverifiable, 0.0)
        .expect("classify ok");
    assert_eq!(c.state, TurnState::Unknown);
    assert_eq!(c.reason, "unreadable_or_empty");
    assert!(!c.state.is_idle_for_takeover());
}

#[test]
fn classify_unrecognized_format_is_unknown() {
    // probe '{"foo":"bar"}': had_records=true, no lifecycle fact, NO diagnostics
    // → reason=no_turn_lifecycle_fact (NOT unrecognized_format — golden-confirmed).
    let c = classify(Provider::Claude, r#"{"foo":"bar"}"#, ProcessLiveness::Unverifiable, 0.0)
        .expect("classify ok");
    assert_eq!(c.state, TurnState::Unknown);
    assert_eq!(c.reason, "no_turn_lifecycle_fact");
    assert!(!c.state.is_idle_for_takeover());
}

#[test]
fn classify_claude_code_alias_normalizes_to_claude_reader() {
    // 陷阱 #4: claude_code → claude reader (__init__.py:88). Both must classify an
    // end_turn transcript identically to idle / end_turn — the alias never dies.
    let txt = claude_end_turn();
    let alias = classify(Provider::ClaudeCode, &txt, ProcessLiveness::Unverifiable, 0.0)
        .expect("alias ok");
    let canon = classify(Provider::Claude, &txt, ProcessLiveness::Unverifiable, 0.0)
        .expect("canon ok");
    assert_eq!(alias.state, TurnState::Idle);
    assert_eq!(alias.reason, "end_turn");
    assert_eq!(alias, canon, "claude_code must normalize to the claude reader");
}

#[test]
fn classify_open_turn_with_no_process_is_unknown_not_working() {
    // probe open_turn_process_none: open turn + Unverifiable (None process) →
    // unknown / process_identity_unverified / source=process_guard, turn_id=r1.
    // C4: missing identity is NEVER optimistically working.
    let c = classify(Provider::Claude, &claude_open_turn(), ProcessLiveness::Unverifiable, 0.0)
        .expect("classify ok");
    assert_eq!(c.state, TurnState::Unknown);
    assert_eq!(c.reason, "process_identity_unverified");
    assert_eq!(c.source, ClassifySource::ProcessGuard);
    assert_eq!(c.turn_id, Some(TurnId::new("r1")));
    assert!(!c.state.is_idle_for_takeover());
}

#[test]
fn classify_open_turn_alive_is_working() {
    // probe open_turn_alive: open turn + Alive → working / open_turn / source=session_file.
    let c = classify(Provider::Claude, &claude_open_turn(), ProcessLiveness::Alive, 0.0)
        .expect("classify ok");
    assert_eq!(c.state, TurnState::Working);
    assert_eq!(c.reason, "open_turn");
    assert_eq!(c.source, ClassifySource::SessionFile);
    assert_eq!(c.turn_id, Some(TurnId::new("r1")));
}

#[test]
fn classify_open_turn_dead_is_abnormal_crashed_mid_turn() {
    // probe open_turn_dead: open turn + Dead → abnormal / crashed_mid_turn /
    // source=process_guard, annotations contains "crashed_mid_turn".
    let c = classify(Provider::Claude, &claude_open_turn(), ProcessLiveness::Dead, 0.0)
        .expect("classify ok");
    assert_eq!(c.state, TurnState::Abnormal);
    assert_eq!(c.reason, "crashed_mid_turn");
    assert_eq!(c.source, ClassifySource::ProcessGuard);
    assert!(c.annotations.contains(&"crashed_mid_turn".to_string()));
}

#[test]
fn classify_open_turn_unverifiable_process_is_unknown() {
    // probe open_turn_unverifiable: open turn + Unverifiable →
    // unknown / process_identity_unverified (NOT working). NEVER idle.
    let c = classify(Provider::Claude, &claude_open_turn(), ProcessLiveness::Unverifiable, 0.0)
        .expect("classify ok");
    assert_eq!(c.state, TurnState::Unknown);
    assert_eq!(c.reason, "process_identity_unverified");
    assert!(!c.state.is_idle_for_takeover());
}

#[test]
fn classify_last_lifecycle_fact_wins() {
    // probe last_fact_wins_complete (tool_use 'a' then end_turn 'b') →
    // idle / end_turn, turn_id = the LAST lifecycle fact's id ("b").
    let two = format!(
        "{}\n{}",
        r#"{"type":"assistant","requestId":"a","message":{"stop_reason":"tool_use"}}"#,
        r#"{"type":"assistant","requestId":"b","message":{"stop_reason":"end_turn"}}"#,
    );
    let c = classify(Provider::Claude, &two, ProcessLiveness::Unverifiable, 0.0)
        .expect("classify ok");
    assert_eq!(c.state, TurnState::Idle);
    assert_eq!(c.reason, "end_turn");
    assert_eq!(c.turn_id, Some(TurnId::new("b")));
}

#[test]
fn classify_c14_open_turn_beats_silence() {
    // probe c14_open_after_complete_alive (end_turn 'a' then tool_use 'b', alive,
    // file_silence=9999) → working / open_turn, turn_id="b". Silence is DISCARDED:
    // only a dead process guard can demote an open turn (common.py:93-103, C14).
    let c14 = format!(
        "{}\n{}",
        r#"{"type":"assistant","requestId":"a","message":{"stop_reason":"end_turn"}}"#,
        r#"{"type":"assistant","requestId":"b","message":{"stop_reason":"tool_use"}}"#,
    );
    let c = classify(Provider::Claude, &c14, ProcessLiveness::Alive, 9999.0)
        .expect("classify ok");
    assert_eq!(c.state, TurnState::Working);
    assert_eq!(c.reason, "open_turn");
    assert_eq!(c.turn_id, Some(TurnId::new("b")));
}

#[test]
fn classify_claude_interrupted_is_idle_interrupted_annotated() {
    // probe claude_interrupted ("[Request interrupted by user]") →
    // idle_interrupted / user_interrupt / annotations=["interrupted"], turn_id=u1.
    let txt = r#"{"type":"user","uuid":"u1","message":{"content":[{"type":"text","text":"[Request interrupted by user]"}]}}"#;
    let c = classify(Provider::Claude, txt, ProcessLiveness::Unverifiable, 0.0)
        .expect("classify ok");
    assert_eq!(c.state, TurnState::IdleInterrupted);
    assert_eq!(c.reason, "user_interrupt");
    assert_eq!(c.annotations, vec!["interrupted".to_string()]);
    assert_eq!(c.turn_id, Some(TurnId::new("u1")));
    // C12: idle_interrupted IS idle for take-over (annotated).
    assert!(c.state.is_idle_for_takeover());
}

#[test]
fn classify_claude_stop_sequence_is_idle() {
    // probe claude_stop_sequence → idle / stop_sequence.
    let txt = r#"{"type":"assistant","requestId":"r1","message":{"stop_reason":"stop_sequence"}}"#;
    let c = classify(Provider::Claude, txt, ProcessLiveness::Unverifiable, 0.0)
        .expect("classify ok");
    assert_eq!(c.state, TurnState::Idle);
    assert_eq!(c.reason, "stop_sequence");
}

#[test]
fn classify_codex_task_complete_is_idle() {
    // probe codex_task_complete → idle / task_complete, turn_id from payload (ct1).
    let txt = r#"{"type":"event_msg","payload":{"type":"task_complete","turn_id":"ct1"}}"#;
    let c = classify(Provider::Codex, txt, ProcessLiveness::Unverifiable, 0.0)
        .expect("classify ok");
    assert_eq!(c.state, TurnState::Idle);
    assert_eq!(c.reason, "task_complete");
    assert_eq!(c.turn_id, Some(TurnId::new("ct1")));
}

#[test]
fn classify_codex_turn_aborted_interrupted_is_idle_interrupted() {
    // probe codex_turn_aborted_interrupted → idle_interrupted / interrupted.
    let interrupted = r#"{"type":"event_msg","payload":{"type":"turn_aborted","turn_id":"ct2","reason":"interrupted"}}"#;
    let c = classify(Provider::Codex, interrupted, ProcessLiveness::Unverifiable, 0.0)
        .expect("classify ok");
    assert_eq!(c.state, TurnState::IdleInterrupted);
    assert_eq!(c.reason, "interrupted");
    // probe codex_turn_aborted_other (reason="error") → idle_interrupted with the
    // RAW reason string passed through ("error"), turn_id=ct3.
    let other = r#"{"type":"event_msg","payload":{"type":"turn_aborted","turn_id":"ct3","reason":"error"}}"#;
    let c2 = classify(Provider::Codex, other, ProcessLiveness::Unverifiable, 0.0)
        .expect("classify ok");
    assert_eq!(c2.state, TurnState::IdleInterrupted);
    assert_eq!(c2.reason, "error", "raw abort reason must pass through");
    assert_eq!(c2.turn_id, Some(TurnId::new("ct3")));
}

#[test]
fn classify_codex_appserver_failed_is_abnormal() {
    // probe codex_appserver_failed (turn.status==failed) → abnormal / turn_failed /
    // annotations=["turn_failed"], source=session_file, turn_id=ct4.
    let txt = r#"{"jsonrpc":"2.0","method":"turn/completed","params":{"turn":{"id":"ct4","status":"failed"}}}"#;
    let c = classify(Provider::Codex, txt, ProcessLiveness::Unverifiable, 0.0)
        .expect("classify ok");
    assert_eq!(c.state, TurnState::Abnormal);
    assert_eq!(c.reason, "turn_failed");
    assert_eq!(c.annotations, vec!["turn_failed".to_string()]);
    assert_eq!(c.turn_id, Some(TurnId::new("ct4")));
    assert!(!c.state.is_idle_for_takeover());
}

#[test]
fn classify_codex_appserver_approval_is_blocked_on_human() {
    // probe codex_appserver_approval (method endswith requestApproval) →
    // blocked_on_human / approval_required / annotations=["awaiting_approval"], turn_id=ct5.
    let txt = r#"{"jsonrpc":"2.0","method":"session/requestApproval","params":{"turnId":"ct5"}}"#;
    let c = classify(Provider::Codex, txt, ProcessLiveness::Unverifiable, 0.0)
        .expect("classify ok");
    assert_eq!(c.state, TurnState::BlockedOnHuman);
    assert_eq!(c.reason, "approval_required");
    assert_eq!(c.annotations, vec!["awaiting_approval".to_string()]);
    assert_eq!(c.turn_id, Some(TurnId::new("ct5")));
    // blocked_on_human is NOT idle for take-over.
    assert!(!c.state.is_idle_for_takeover());
}

// ---- (b) idle predicate / evaluate_takeover_reminder (NoPingReason cases) ----
//
// Golden via /tmp/probe_idle.py (idle_predicate.evaluate_takeover_reminder).
// Nodes / monitor_state passed as serde_json::Value dicts (Python dict shape).

