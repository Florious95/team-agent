//! Grammar/recipe regression fixtures derived from R0/R10 shapes. These tests do
//! not launch Kiro, consume credits, or claim candidate/native MCP acceptance.
use std::time::Duration;
use team_agent_contract::contract::{delivery::*, hooks::*, probe::*, session::*, types::*};
use team_agent_contract::host::digest;
use team_agent_contract::kiro::{interaction::*, KiroAdapter, McpStdio};

fn adapter() -> KiroAdapter {
    KiroAdapter::new(McpStdio {
        executable: "/fixture/candidate".into(),
        arguments: vec![],
        environment: Default::default(),
    })
    .unwrap()
}
fn scope() -> EvidenceScope {
    EvidenceScope {
        identity: InstanceIdentity {
            scope: ScopeId::new("scope").unwrap(),
            seat: SeatId::new("worker").unwrap(),
            instance: InstanceId::new("instance").unwrap(),
            generation: Generation(1),
        },
        endpoint: "/private/socket".into(),
        pane: "%0".into(),
        binding: "binding".into(),
        session: None,
        sequence: 1,
        observed_at: Duration::ZERO,
        valid_for: Duration::from_secs(2),
        source: "controlled-r0-shape".into(),
    }
}
fn frame(text: &str) -> CaptureFrame {
    CaptureFrame {
        scope: scope(),
        text: text.into(),
        baseline: None,
        profile_id: PROFILE.into(),
        operation: Operation::OrdinarySend,
        message: None,
        attempt: None,
        after_step: None,
        paste_latch: PasteLatch::NeverSeen,
    }
}
const READY: &str = "kiro_default · claude-sonnet-4.5 · ◔ 4%\n›  ask a question or describe a task ↵\n/copy to clipboard";
const SID: &str = "• Session ID: 94b32acb-dcc0-4bd0-a7d2-acf02a25b133\n  Resume with: kiro-cli --resume-id 94b32acb-dcc0-4bd0-a7d2-acf02a25b133";

#[test]
fn exact_unicode_composer_and_current_busy_surface_are_distinct() {
    let adapter = adapter();
    assert_eq!(
        adapter.interpret(&frame(READY)).surface,
        InputSurface::ComposerReady
    );
    assert_eq!(
        adapter.interpret(&frame("> generic shell prompt")).surface,
        InputSurface::ShellOrUnknown
    );
    assert_eq!(
        adapter
            .interpret(&frame(
                "Thinking... (esc to cancel)\nKiro is working · 1s · Type to steer"
            ))
            .surface,
        InputSurface::Busy
    );
    assert_eq!(
        adapter
            .interpret(&frame(&format!("Thinking... (old transcript)\n{READY}")))
            .surface,
        InputSurface::ComposerReady
    );
    let mut wrong = frame(READY);
    wrong.profile_id = "v3-unobserved".into();
    assert_eq!(
        adapter.interpret(&wrong).surface,
        InputSurface::ShellOrUnknown
    );
}
// R0 frame-01..19 contain these six spinner cells and this current status bar.
const WORKING: &str = "›  Kiro is working · 1s · Type to steer · Ctrl+S to queue";
const SPINNERS: [&str; 6] = ["ᗢ", "ᗣ", "ᗤ", "ᗥ", "ᗦ", "ᗧ"];

#[test]
fn native_spinner_and_working_composer_are_busy_but_old_or_inexact_shapes_are_not() {
    let adapter = adapter();
    for spinner in SPINNERS {
        let thinking = format!("{spinner} Thinking... (esc to cancel)");
        for current in [thinking.clone(), format!("{thinking}\n{WORKING}")] {
            assert_eq!(
                adapter.interpret(&frame(&current)).surface,
                InputSurface::Busy
            );
        }
        assert_eq!(
            adapter
                .interpret(&frame(&format!("{thinking}\n{READY}")))
                .surface,
            InputSurface::ComposerReady
        );
        assert_eq!(
            adapter
                .interpret(&frame(&format!("{thinking}\n› draft")))
                .surface,
            InputSurface::ComposerContainsPaste
        );
    }
    for seconds in ["0", "3", "38"] {
        assert_eq!(
            adapter
                .interpret(&frame(&WORKING.replace("1s", &format!("{seconds}s"))))
                .surface,
            InputSurface::Busy
        );
    }
    for inexact in [
        WORKING.replace("1s", "1m"),
        WORKING.replace("1s", "xs"),
        WORKING.replace("1s", "s"),
        WORKING.replace(" · Ctrl+S to queue", ""),
        format!("{WORKING} quoted text"),
        "› ᗦ Thinking... (esc to cancel)".into(),
    ] {
        assert_eq!(
            adapter.interpret(&frame(&inexact)).surface,
            InputSurface::ComposerContainsPaste
        );
    }
    assert_eq!(
        adapter
            .interpret(&frame("X Thinking... (esc to cancel)"))
            .surface,
        InputSurface::ShellOrUnknown
    );
}

#[test]
fn business_acceptance_uses_current_busy_footer_without_waiting_for_model_completion() {
    let adapter = adapter();
    let token = "[team-agent-token:msg-busy]";
    for operation in [Operation::FirstBusiness, Operation::OrdinarySend] {
        let mut capture = frame(&format!("› hello\n{token}"));
        capture.operation = operation;
        capture.message = Some(MessageId::new("msg-busy").unwrap());
        capture.attempt = Some(AttemptId::new("attempt-busy").unwrap());
        capture.baseline = Some(CaptureBaseline {
            scope: scope(),
            text: READY.into(),
        });
        capture.after_step = Some(StepKind::Paste);
        capture.paste_latch = adapter.interpret(&capture).paste_latch;
        assert!(matches!(capture.paste_latch, PasteLatch::Seen { .. }));
        let native = format!("  hello\n  {token}\nᗦ Thinking... (esc to cancel)\n{WORKING}");
        capture.text = native.clone();
        assert_ne!(
            adapter.interpret(&capture).surface,
            InputSurface::NativeAccepted
        );
        capture.after_step = Some(StepKind::InitialSubmit);
        let accepted = adapter.interpret(&capture);
        assert_eq!(accepted.surface, InputSurface::NativeAccepted);
        assert_eq!(accepted.current_message, capture.message);
        assert_eq!(accepted.current_attempt, capture.attempt);
        assert!(matches!(accepted.paste_latch, PasteLatch::Gone { .. }));
        for unproven in [
            native.replace(token, "unrelated token"),
            format!("{native}\n{token}"), // still inside the current composer
            format!("  {token}\nᗦ Thinking... (esc to cancel)\n› draft"),
        ] {
            capture.text = unproven;
            assert_ne!(
                adapter.interpret(&capture).surface,
                InputSurface::NativeAccepted
            );
        }
        capture.text = native;
        capture.paste_latch = PasteLatch::NeverSeen;
        assert_ne!(
            adapter.interpret(&capture).surface,
            InputSurface::NativeAccepted
        );
    }
}

#[test]
fn recipe_has_one_enter_no_payload_trailer_retry_or_queue_keys() {
    let policy = PROFILES[0].policy.require("policy").unwrap();
    policy.validate().unwrap();
    assert_eq!(policy.key_upper_bound().unwrap(), 1);
    assert_eq!(policy.max_submit_keys, 1);
    assert_eq!(policy.payload_trailer, PayloadTrailer::None);
    assert_eq!(policy.retry_budget, RetryBudget::Never);
    assert!(policy.queue_flush.is_empty());
    assert_eq!(PROFILES[0].identity, ProfileIdentity::RuntimeCaptured);
    assert_eq!(PROFILES[0].harness, "v2");
    assert_eq!(policy.paste_mode, PasteMode::Bracketed);
    assert!(CONTROLS.iter().all(|control| {
        control.input_mode
            == team_agent_contract::runtime::delivery::ControlInputMode::DirectTyping
            && control.policy_sha256 == POLICY
    }));
}
#[test]
fn accepted_message_requires_current_paste_and_transcript_token_not_disappearance() {
    let adapter = adapter();
    let mut capture = frame("› Team Agent message from leader:\nhello\n[team-agent-token:msg-1]");
    capture.baseline = Some(CaptureBaseline {
        scope: scope(),
        text: READY.into(),
    });
    capture.message = Some(MessageId::new("msg-1").unwrap());
    capture.attempt = Some(AttemptId::new("attempt-1").unwrap());
    capture.after_step = Some(StepKind::Paste);
    let pasted = adapter.interpret(&capture);
    assert!(matches!(pasted.paste_latch, PasteLatch::Seen { .. }));
    assert_eq!(pasted.current_message, capture.message);
    capture.paste_latch = pasted.paste_latch;
    capture.after_step = Some(StepKind::InitialSubmit);
    capture.text = READY.into();
    assert_ne!(
        adapter.interpret(&capture).surface,
        InputSurface::NativeAccepted
    );
    capture.text =
        format!("Team Agent message from leader:\nhello\n[team-agent-token:msg-1]\n• OK\n{READY}");
    let accepted = adapter.interpret(&capture);
    assert_eq!(accepted.surface, InputSurface::NativeAccepted);
    assert_eq!(accepted.current_attempt, capture.attempt);
}
#[test]
fn historical_matching_token_cannot_become_a_new_paste_receipt() {
    let mut capture = frame("› hello [team-agent-token:msg-1]");
    capture.baseline = Some(CaptureBaseline {
        scope: scope(),
        text: "old [team-agent-token:msg-1]".into(),
    });
    capture.message = Some(MessageId::new("msg-1").unwrap());
    capture.attempt = Some(AttemptId::new("attempt-1").unwrap());
    capture.after_step = Some(StepKind::Paste);
    assert_eq!(
        adapter().interpret(&capture).paste_latch,
        PasteLatch::NeverSeen
    );
}
#[test]
fn session_parser_requires_label_matching_hint_and_one_uuid() {
    assert_eq!(
        session_id(SID).unwrap().as_str(),
        "94b32acb-dcc0-4bd0-a7d2-acf02a25b133"
    );
    assert!(session_id("94b32acb-dcc0-4bd0-a7d2-acf02a25b133").is_err());
    assert!(session_id(&format!("{SID}\n{SID}")).is_err());
    assert!(session_id(&SID.replace("Resume with:", "arbitrary reply:")).is_err());
}
#[test]
fn r0_session_response_is_accepted_only_after_the_current_control_and_enter() {
    let adapter = adapter();
    let mut capture = frame("› /session-id");
    capture.operation = Operation::SessionInspect;
    capture.attempt = Some(AttemptId::new("session-control").unwrap());
    capture.baseline = Some(CaptureBaseline {
        scope: scope(),
        text: READY.into(),
    });
    capture.after_step = Some(StepKind::Paste);
    let typed = adapter.interpret(&capture);
    assert!(matches!(typed.paste_latch, PasteLatch::Seen { .. }));
    capture.paste_latch = typed.paste_latch;
    capture.after_step = Some(StepKind::InitialSubmit);
    capture.text = format!("{SID}\n{READY}");
    let accepted = adapter.interpret(&capture);
    assert_eq!(accepted.surface, InputSurface::NativeAccepted);
    assert_eq!(accepted.current_attempt, capture.attempt);
    capture.paste_latch = PasteLatch::NeverSeen;
    assert_ne!(adapter.interpret(&capture).surface, InputSurface::NativeAccepted);
}

#[test]
fn session_hook_preserves_scope_hash_and_never_lists_newest_session() {
    let native = NativeIdentity {
        version: "2.29.0".into(),
        harness: "v2".into(),
        ui: "tui".into(),
        platform: Platform::MacOs,
        executable_sha256: digest(b"runtime captured executable"),
    };
    let mut evidence = ScopedSessionEvidence {
        scope: scope(),
        provider: ProviderId::new("kiro").unwrap(),
        native,
        cwd: CwdIdentity {
            path: "/fixture/work".into(),
            identity: digest(b"cwd"),
        },
        origin: CaptureOrigin::CurrentNativeSession,
        evidence_kind: EvidenceKind::Fixture,
        evidence_sha256: digest(SID.as_bytes()),
        record: SID.as_bytes().into(),
    };
    let bound = adapter().bind(&evidence).unwrap();
    assert_eq!(bound.source, evidence.scope.identity);
    assert_eq!(bound.native, evidence.native);
    assert_eq!(bound.evidence_kind, EvidenceKind::Fixture);
    assert!(bound.backing.is_none());
    evidence.native.harness = "v3".into();
    assert!(adapter().bind(&evidence).is_err());
}
#[test]
fn mock_two_tool_registration_does_not_claim_three_tool_team_binding() {
    let panel = "Name Source Status Description\nreport_result mcp:team-agent-mock ◌ approval required\nsend_message mcp:team-agent-mock ◌ approval required\nesc to close · ↑↓ to scroll";
    assert_eq!(registry_tools(panel, "team-agent-mock").unwrap().len(), 2);
    assert!(registry_tools(panel, "team").unwrap().is_empty());
    assert_eq!(
        adapter().interpret(&frame(panel)).predicate.as_deref(),
        Some("kiro-tools-panel")
    );
    let team = panel.replace("team-agent-mock", "team").replace(
        "esc to close",
        "get_team_status mcp:team ◌ approval required\nesc to close",
    );
    assert_eq!(registry_tools(&team, "team").unwrap().len(), 3);
    assert_eq!(
        adapter().interpret(&frame(&team)).predicate.as_deref(),
        Some("kiro-team-tools-bound")
    );
    assert!(registry_tools(
        "send_message mcp:team report_result get_team_status",
        "team"
    )
    .is_none());
    assert!(registry_tools(&format!("{team}\n{READY}"), "team").is_none());
}

// Relevant rows from R10's 120x40 /tools panel; unrelated builtin rows omitted.
const R10_TEAM_TABLE: &str = concat!(
    " Name             Source      Status                Description\n",
    " get_team_status  mcp:team    ● allowed             Return machine-readable team status.\n",
    " report_result    mcp:team    ● allowed             Report task completion.\n",
    " send_message     mcp:team    ● allowed             Send a message.\n",
);

#[test]
fn both_observed_footers_preserve_header_source_and_complete_tool_requirements() {
    for footer in ["esc to close", "esc to close · ↑↓ to scroll"] {
        let panel = format!("{R10_TEAM_TABLE} {footer}\n\n");
        let tools = registry_tools(&panel, "team").unwrap();
        assert_eq!(tools.len(), TEAM_TOOLS.len());
        assert!(TEAM_TOOLS.iter().all(|tool| tools.contains(tool)));
        assert_eq!(
            adapter().interpret(&frame(&panel)).predicate.as_deref(),
            Some("kiro-team-tools-bound")
        );
        for invalid in [
            panel.replace("Name", "Other"),
            panel.replace(footer, "esc to close · unverified hint"),
            panel.replace(footer, "prefix esc to close"),
            panel.replace(footer, ""),
            format!("{panel}{READY}"),
            format!("{panel}unrelated last line"),
        ] {
            assert!(registry_tools(&invalid, "team").is_none());
        }
        for incomplete in [
            panel.replace("mcp:team", "mcp:team-other"),
            panel.replace("mcp:team", "built-in"),
            panel.replace("get_team_status", "send_message"),
        ] {
            let tools = registry_tools(&incomplete, "team").unwrap();
            assert!(!TEAM_TOOLS.iter().all(|tool| tools.contains(tool)));
            assert_eq!(
                adapter().interpret(&frame(&incomplete)).predicate.as_deref(),
                Some("kiro-tools-panel")
            );
        }
    }
}

#[test]
fn tool_panel_acceptance_requires_current_typed_control_and_enter_for_both_footers() {
    let adapter = adapter();
    for footer in ["esc to close", "esc to close · ↑↓ to scroll"] {
        let mut capture = frame("› /tools");
        capture.operation = Operation::ToolInspect;
        capture.attempt = Some(AttemptId::new("tools-control").unwrap());
        capture.baseline = Some(CaptureBaseline {
            scope: scope(),
            text: READY.into(),
        });
        capture.after_step = Some(StepKind::Paste);
        let typed = adapter.interpret(&capture);
        assert_eq!(typed.surface, InputSurface::ComposerContainsPaste);
        assert_eq!(
            typed.paste_latch,
            PasteLatch::Seen {
                native_identity: "/tools".into()
            }
        );
        capture.paste_latch = typed.paste_latch;
        capture.text = format!("{R10_TEAM_TABLE} {footer}\n\n");
        assert_ne!(
            adapter.interpret(&capture).surface,
            InputSurface::NativeAccepted
        );
        capture.after_step = Some(StepKind::InitialSubmit);
        let accepted = adapter.interpret(&capture);
        assert_eq!(accepted.surface, InputSurface::NativeAccepted);
        assert_eq!(accepted.current_attempt, capture.attempt);
        assert_eq!(accepted.predicate.as_deref(), Some("kiro-team-tools-bound"));
        assert_eq!(
            accepted.paste_latch,
            PasteLatch::Gone {
                native_identity: "/tools".into()
            }
        );
        for unrelated in [
            PasteLatch::NeverSeen,
            PasteLatch::Seen {
                native_identity: "/session-id".into(),
            },
        ] {
            capture.paste_latch = unrelated;
            assert_ne!(
                adapter.interpret(&capture).surface,
                InputSurface::NativeAccepted
            );
        }
    }
}
