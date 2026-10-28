fn claude_end_turn() -> String {
    r#"{"type":"assistant","requestId":"r1","message":{"stop_reason":"end_turn"}}"#.to_string()
}
fn claude_open_turn() -> String {
    r#"{"type":"assistant","requestId":"r1","message":{"stop_reason":"tool_use"}}"#.to_string()
}

#[test]
fn classify_state_table() {
    use ClassifySource::{ProcessGuard, SessionFile};
    use ProcessLiveness::{Alive, Unverifiable};
    use Provider::{Claude, ClaudeCode, Codex};
    use TurnState::{Abnormal, BlockedOnHuman, Idle, IdleInterrupted, Unknown, Working};

    type Input<'a> = (Provider, &'a str, ProcessLiveness, f64);
    // State, reason, turn ID, source, exact annotations, idle-for-takeover.
    // None skips an originally absent assertion; Some(None) asserts no turn ID.
    type Expected<'a> = (
        TurnState, &'a str, Option<Option<&'a str>>, Option<ClassifySource>,
        Option<&'a [&'a str]>, Option<bool>,
    );
    let open = claude_open_turn();
    let two = format!(
        "{}\n{}",
        r#"{"type":"assistant","requestId":"a","message":{"stop_reason":"tool_use"}}"#,
        r#"{"type":"assistant","requestId":"b","message":{"stop_reason":"end_turn"}}"#,
    );
    let c14 = format!(
        "{}\n{}",
        r#"{"type":"assistant","requestId":"a","message":{"stop_reason":"end_turn"}}"#,
        r#"{"type":"assistant","requestId":"b","message":{"stop_reason":"tool_use"}}"#,
    );
    let cases: &[(&str, Input<'_>, Expected<'_>)] = &[
        // Unknown is NEVER idle (bug-071/077/085, C5).
        ("empty",
            (ClaudeCode, "", Unverifiable, 0.0),
            (Unknown, "unreadable_or_empty", Some(None), Some(SessionFile), None, Some(false))),
        ("whitespace",
            (Claude, "   \n  \t \n", Unverifiable, 0.0),
            (Unknown, "unreadable_or_empty", None, None, None, Some(false))),
        // !had_records wins over parse diagnostics (common.py:72-75).
        ("garbage_jsonl",
            (Claude, "not json\n{broken", Unverifiable, 0.0),
            (Unknown, "unreadable_or_empty", None, None, None, Some(false))),
        // Valid record without lifecycle facts: NOT unrecognized_format.
        ("unrecognized_record",
            (Claude, r#"{"foo":"bar"}"#, Unverifiable, 0.0),
            (Unknown, "no_turn_lifecycle_fact", None, None, None, Some(false))),
        // C4: missing process identity is NEVER optimistically working.
        ("open_unverifiable_identity",
            (Claude, &open, Unverifiable, 0.0),
            (Unknown, "process_identity_unverified", Some(Some("r1")), Some(ProcessGuard), None, Some(false))),
        ("open_alive",
            (Claude, &open, Alive, 0.0),
            (Working, "open_turn", Some(Some("r1")), Some(SessionFile), None, None)),
        ("open_unverifiable_process",
            (Claude, &open, Unverifiable, 0.0),
            (Unknown, "process_identity_unverified", None, None, None, Some(false))),
        ("last_fact_wins",
            (Claude, &two, Unverifiable, 0.0),
            (Idle, "end_turn", Some(Some("b")), None, None, None)),
        // C14: silence is discarded; only a dead process can demote an open turn.
        ("open_beats_silence",
            (Claude, &c14, Alive, 9999.0),
            (Working, "open_turn", Some(Some("b")), None, None, None)),
        // C12: idle_interrupted IS idle for take-over, with exact annotations.
        ("claude_interrupted",
            (Claude, r#"{"type":"user","uuid":"u1","message":{"content":[{"type":"text","text":"[Request interrupted by user]"}]}}"#, Unverifiable, 0.0),
            (IdleInterrupted, "user_interrupt", Some(Some("u1")), None, Some(&["interrupted"]), Some(true))),
        ("claude_stop_sequence",
            (Claude, r#"{"type":"assistant","requestId":"r1","message":{"stop_reason":"stop_sequence"}}"#, Unverifiable, 0.0),
            (Idle, "stop_sequence", None, None, None, None)),
        ("codex_task_complete",
            (Codex, r#"{"type":"event_msg","payload":{"type":"task_complete","turn_id":"ct1"}}"#, Unverifiable, 0.0),
            (Idle, "task_complete", Some(Some("ct1")), None, None, None)),
        ("codex_abort_interrupted",
            (Codex, r#"{"type":"event_msg","payload":{"type":"turn_aborted","turn_id":"ct2","reason":"interrupted"}}"#, Unverifiable, 0.0),
            (IdleInterrupted, "interrupted", None, None, None, None)),
        // Raw abort reason must pass through, including non-interrupted errors.
        ("codex_abort_error",
            (Codex, r#"{"type":"event_msg","payload":{"type":"turn_aborted","turn_id":"ct3","reason":"error"}}"#, Unverifiable, 0.0),
            (IdleInterrupted, "error", Some(Some("ct3")), None, None, None)),
        ("codex_turn_failed",
            (Codex, r#"{"jsonrpc":"2.0","method":"turn/completed","params":{"turn":{"id":"ct4","status":"failed"}}}"#, Unverifiable, 0.0),
            (Abnormal, "turn_failed", Some(Some("ct4")), None, Some(&["turn_failed"]), Some(false))),
        // blocked_on_human is NOT idle for take-over.
        ("codex_approval",
            (Codex, r#"{"jsonrpc":"2.0","method":"session/requestApproval","params":{"turnId":"ct5"}}"#, Unverifiable, 0.0),
            (BlockedOnHuman, "approval_required", Some(Some("ct5")), None, Some(&["awaiting_approval"]), Some(false))),
    ];
    for &(id, (provider, text, liveness, silence), expected) in cases {
        let c = classify(provider, text, liveness, silence)
            .unwrap_or_else(|err| panic!("{id}: classify failed: {err:?}"));
        let (state, reason, turn_id, source, annotations, idle) = expected;
        let note = match id {
            "empty" => "unreadable/empty input must never be idle (C5)",
            "codex_abort_error" => "raw abort reason must pass through",
            _ => "",
        };
        assert_eq!(c.state, state, "{id}: state");
        assert_eq!(c.reason, reason, "{id}: reason; {note}");
        if let Some(turn_id) = turn_id {
            assert_eq!(c.turn_id.as_ref().map(TurnId::as_str), turn_id, "{id}: turn ID");
        }
        if let Some(source) = source {
            assert_eq!(c.source, source, "{id}: source");
        }
        if let Some(annotations) = annotations {
            let actual: Vec<_> = c.annotations.iter().map(String::as_str).collect();
            assert_eq!(actual.as_slice(), annotations, "{id}: exact annotations");
        }
        if let Some(idle) = idle {
            assert_eq!(c.state.is_idle_for_takeover(), idle, "{id}: idle-for-takeover; {note}");
        }
    }
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

// ---- (b) idle predicate / evaluate_takeover_reminder (NoPingReason cases) ----
//
// Golden via /tmp/probe_idle.py (idle_predicate.evaluate_takeover_reminder).
// Nodes / monitor_state passed as serde_json::Value dicts (Python dict shape).

