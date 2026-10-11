//! Controlled H6 fixtures based on the R6 folded composer. These do not execute
//! Cursor or prove native post-submit rendering/consumption. Shared guards,
//! one-Enter execution and Kiro correlation are intentionally unchanged.
use std::time::Duration;
use team_agent_contract::contract::{delivery::*, hooks::InteractionHook, probe::*, session::NativeSessionId, types::*};
use team_agent_contract::providers::cursor::{interaction::{PROFILE, PROFILES}, CursorAdapter, McpStdio};

const BLOCK: &str = "[Pasted text #1 +145 lines]";
fn adapter() -> CursorAdapter {
    CursorAdapter::new(McpStdio {
        executable: "/owned/candidate".into(), arguments: vec!["contract-mcp".into()], environment: Default::default(),
    }).unwrap()
}
fn screen(history: &str, input: &str) -> String {
    format!("Cursor Agent\nv2026.10.01-e373342\n{history}\n\n  → {input}  \n\nGPT-5.6 Luna 272K Extra High  Run Everything\n/owned/cwd\n")
}
fn scope(sequence: u64) -> EvidenceScope {
    EvidenceScope {
        identity: InstanceIdentity {
            scope: ScopeId::new("scope").unwrap(), seat: SeatId::new("worker").unwrap(),
            instance: InstanceId::new("instance-1").unwrap(), generation: Generation(1),
        },
        endpoint: "/owned/tmux.sock".into(), pane: "%0".into(), binding: "binding-1".into(),
        session: Some(NativeSessionId::new("12345678-1234-1234-1234-123456789012").unwrap()),
        sequence, observed_at: Duration::from_millis(100 * sequence), valid_for: Duration::from_secs(2),
        source: "fixture-owned-capture".into(),
    }
}
fn frame() -> CaptureFrame {
    CaptureFrame {
        scope: scope(2), text: screen("", &format!("^[{BLOCK}")),
        baseline: Some(CaptureBaseline { scope: scope(1), text: screen("", "Plan, search, build anything") }),
        profile_id: PROFILE.into(), operation: Operation::FirstBusiness,
        message: Some(MessageId::new("message-1").unwrap()), attempt: Some(AttemptId::new("attempt-1").unwrap()),
        // Production supplies this only along the confirmed-action capture path.
        after_step: Some(StepKind::Paste), paste_latch: PasteLatch::NeverSeen,
    }
}
fn assert_unseen(frame: &CaptureFrame) {
    let observed = adapter().interpret(frame);
    assert_eq!(observed.paste_latch, PasteLatch::NeverSeen);
    assert!(observed.current_message.is_none() && observed.current_attempt.is_none());
    assert_ne!(observed.surface, InputSurface::NativeAccepted);
}
fn seen_frame() -> CaptureFrame {
    let mut frame = frame();
    frame.paste_latch = adapter().interpret(&frame).paste_latch;
    assert!(matches!(frame.paste_latch, PasteLatch::Seen { .. }));
    frame.scope = scope(3);
    frame
}
fn token(frame: &CaptureFrame) -> String {
    format!("[team-agent-token:{}]", frame.message.as_ref().unwrap().as_str())
}

#[test]
fn fresh_folded_block_uses_an_attempt_receipt_not_a_claim_of_visible_token() {
    for input in [BLOCK.to_string(), format!("^[{BLOCK}")] {
        let mut frame = frame(); frame.text = screen("", &input);
        let observed = adapter().interpret(&frame);
        assert_eq!(observed.surface, InputSurface::ComposerContainsPaste);
        assert_eq!(observed.current_message, frame.message);
        assert_eq!(observed.current_attempt, frame.attempt);
        let PasteLatch::Seen { ref native_identity } = observed.paste_latch else { panic!("must latch owned fold") };
        assert!(native_identity.starts_with("cursor-owned-folded-paste-v1:"));
        assert_ne!(native_identity, &token(&frame));
        frame.paste_latch = observed.paste_latch.clone(); frame.scope = scope(3);
        assert_eq!(adapter().interpret(&frame).paste_latch, observed.paste_latch);
    }
    let policy = PROFILES[0].policy.require("fixture submit").unwrap();
    assert_eq!(policy.max_submit_keys, 1);
    assert_eq!(policy.retry_budget, RetryBudget::Never);
    assert!(policy.confirmation_steps.is_empty() && policy.queue_flush.is_empty());
}

#[test]
fn history_markers_do_not_authorize_input_and_old_message_tokens_still_refuse() {
    let mut frame = frame();
    frame.text = screen(&format!("old user text: {BLOCK}"), "Plan, search, build anything");
    assert_unseen(&frame);
    frame.text = screen("", BLOCK);
    frame.baseline.as_mut().unwrap().text = screen(BLOCK, "Plan, search, build anything");
    assert!(matches!(adapter().interpret(&frame).paste_latch, PasteLatch::Seen { .. }));
    frame.baseline.as_mut().unwrap().text = screen(&token(&frame), "Plan, search, build anything");
    assert_unseen(&frame);
}

#[test]
fn missing_paste_or_empty_baseline_evidence_cannot_latch_a_block() {
    for step in [None, Some(StepKind::InitialSubmit)] {
        let mut frame = frame(); frame.after_step = step; assert_unseen(&frame);
    }
    let mut missing = frame(); missing.baseline = None; assert_unseen(&missing);
    for before in [screen("", "old unsent text"), screen("", BLOCK), screen("⠘⠣ Working", "Plan, search, build anything"), "unknown shell".into()] {
        let mut frame = frame(); frame.baseline.as_mut().unwrap().text = before; assert_unseen(&frame);
    }
    let mut no_attempt = frame(); no_attempt.attempt = None; assert_unseen(&no_attempt);
    let mut no_message = frame(); no_message.message = None; assert_unseen(&no_message);
    let mut no_session = frame(); no_session.scope.session = None; assert_unseen(&no_session);
}

#[test]
fn malformed_blocks_extra_input_and_changed_footer_are_not_accepted() {
    for input in [
        "[Pasted text #0 +145 lines]", "[Pasted text #01 +145 lines]", "[Pasted text #1 +0 lines]",
        "[Pasted text #1 +145x lines]", "[Pasted text #1 +145 lines] extra",
        "prefix [Pasted text #1 +145 lines]", "[Pasted text #1 +145 lines][Pasted text #2 +3 lines]",
        "[Pasted text #1 +145 lines]\nextra editable line", "[Pasted text #18446744073709551616 +145 lines]",
    ] {
        let mut frame = frame(); frame.text = screen("", input); assert_unseen(&frame);
    }
    let mut frame = frame(); frame.text = frame.text.replace("/owned/cwd", "/other/cwd"); assert_unseen(&frame);
    frame.text = format!("Cursor Agent\nv2026.10.01-e373342\n→ {BLOCK}\n");
    frame.baseline.as_mut().unwrap().text = "Cursor Agent\nv2026.10.01-e373342\n→ Plan, search, build anything\n".into();
    assert_unseen(&frame);
}

#[test]
fn stale_or_different_subject_baselines_do_not_authorize_a_paste() {
    let original = frame();
    let mut cases = vec![];
    let mut changed = original.clone(); changed.baseline.as_mut().unwrap().scope.identity.generation = Generation(2); cases.push(changed);
    let mut changed = original.clone(); changed.baseline.as_mut().unwrap().scope.endpoint = "/other.sock".into(); cases.push(changed);
    let mut changed = original.clone(); changed.baseline.as_mut().unwrap().scope.pane = "%9".into(); cases.push(changed);
    let mut changed = original.clone(); changed.baseline.as_mut().unwrap().scope.binding = "other".into(); cases.push(changed);
    let mut changed = original.clone(); changed.baseline.as_mut().unwrap().scope.session = Some(NativeSessionId::new("other").unwrap()); cases.push(changed);
    let mut changed = original.clone(); changed.baseline.as_mut().unwrap().scope.sequence = changed.scope.sequence; cases.push(changed);
    let mut changed = original; changed.baseline.as_mut().unwrap().scope.observed_at = Duration::from_secs(10); cases.push(changed);
    for frame in cases { assert_unseen(&frame); }
}

#[test]
fn folded_receipts_cannot_be_reassigned_to_another_attempt_or_subject() {
    let original = seen_frame();
    let mut cases = vec![];
    let mut changed = original.clone(); changed.attempt = Some(AttemptId::new("other-attempt").unwrap()); cases.push(changed);
    let mut changed = original.clone(); changed.message = Some(MessageId::new("other-message").unwrap()); cases.push(changed);
    let mut changed = original.clone(); changed.operation = Operation::OrdinarySend; cases.push(changed);
    let mut changed = original.clone(); changed.scope.identity.generation = Generation(2); cases.push(changed);
    let mut changed = original.clone(); changed.scope.endpoint = "/other.sock".into(); cases.push(changed);
    let mut changed = original.clone(); changed.scope.pane = "%9".into(); cases.push(changed);
    let mut changed = original.clone(); changed.scope.binding = "other".into(); cases.push(changed);
    let mut changed = original; changed.scope.session = Some(NativeSessionId::new("other").unwrap()); cases.push(changed);
    for mut frame in cases {
        // Even if a new baseline is supplied for the new subject, the previous
        // opaque receipt must not be adopted by a fallback strategy.
        let mut baseline_scope = frame.scope.clone(); baseline_scope.sequence = 1;
        frame.baseline.as_mut().unwrap().scope = baseline_scope;
        let observed = adapter().interpret(&frame);
        assert!(observed.current_attempt.is_none() && observed.current_message.is_none());
        assert_ne!(observed.surface, InputSurface::NativeAccepted);
    }
}

#[test]
fn changing_the_current_block_or_step_never_refreshes_a_latch() {
    let mut frame = seen_frame(); frame.text = screen("", "[Pasted text #2 +145 lines]");
    let observed = adapter().interpret(&frame);
    assert_eq!(observed.paste_latch, frame.paste_latch);
    assert!(observed.current_message.is_none() && observed.current_attempt.is_none());
    frame = seen_frame(); frame.after_step = None;
    assert!(adapter().interpret(&frame).current_attempt.is_none());
}

#[test]
fn enter_with_visible_new_transcript_token_can_finish_the_folded_receipt() {
    let mut frame = seen_frame(); frame.after_step = Some(StepKind::InitialSubmit);
    frame.text = screen(&format!("{}\n⠘⠣ Working", token(&frame)), "Add a follow-up");
    let observed = adapter().interpret(&frame);
    assert_eq!(observed.surface, InputSurface::NativeAccepted);
    let PasteLatch::Seen { native_identity } = &frame.paste_latch else { unreachable!() };
    assert_eq!(observed.paste_latch, PasteLatch::Gone { native_identity: native_identity.clone() });
    assert_eq!(observed.current_attempt, frame.attempt);
    assert_eq!(observed.current_message, frame.message);
}

#[test]
fn disappearance_busy_or_token_still_in_composer_is_not_post_submit_acceptance() {
    let mut frame = seen_frame(); frame.after_step = Some(StepKind::InitialSubmit);
    for text in [
        screen("", "Add a follow-up"), screen("⠘⠣ Working", "Add a follow-up"),
        screen("", &token(&frame)), screen(&token(&frame), BLOCK),
    ] {
        frame.text = text;
        let observed = adapter().interpret(&frame);
        assert_ne!(observed.surface, InputSurface::NativeAccepted);
        assert_eq!(observed.paste_latch, frame.paste_latch);
        assert!(observed.current_message.is_none() && observed.current_attempt.is_none());
    }
    frame.after_step = Some(StepKind::Paste);
    frame.text = screen(&format!("{}\n⠘⠣ Working", token(&frame)), "Add a follow-up");
    assert_ne!(adapter().interpret(&frame).surface, InputSurface::NativeAccepted);
}
