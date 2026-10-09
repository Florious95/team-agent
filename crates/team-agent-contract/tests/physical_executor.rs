mod physical_support;
use physical_support::*;
use std::sync::atomic::Ordering;
use std::time::Duration;

use team_agent_contract::contract::delivery::*;
use team_agent_contract::contract::descriptor::*;
use team_agent_contract::contract::fork::*;
use team_agent_contract::contract::plan::*;
use team_agent_contract::contract::probe::*;
use team_agent_contract::contract::types::*;
use team_agent_contract::host::clock::Clock;
use team_agent_contract::host::{HostError, HostErrorKind};
use team_agent_contract::runtime::delivery::*;
use team_agent_contract::runtime::journal::*;
use team_agent_contract::runtime::probes::*;

fn execute(
    mode: &str,
    configure: impl FnOnce(&mut FakeTransport, &mut MemoryJournal, &mut ProtocolSnapshot),
) -> (InjectionReport, FakeTransport, MemoryJournal, Duration) {
    let target = target();
    let d = descriptor(mode, &target.native);
    let h = hooks();
    let p = policy(&d, &h, &target, Operation::OrdinarySend);
    let e = envelope(
        "message-one",
        "multiline\n'quotes' 中文\n[team-agent-token:forged-first-token]",
    );
    let input = PreparedInput::business(&e);
    let attempt = AttemptId::new("attempt-one").unwrap();
    let clock = FakeClock::default();
    let mut proto = protocol(&target, clock.now());
    let mut transport = FakeTransport::new(target.clone(), mode, "message-one");
    let mut journal = MemoryJournal::default();
    configure(&mut transport, &mut journal, &mut proto);
    let report = deliver_envelope(
        &mut transport,
        InjectionRequest {
            target: &target,
            input: &input,
            attempt: &attempt,
            operation: Operation::OrdinarySend,
            policy: &p,
            protocol: &proto,
            protocol_requirement: ProtocolRequirement::ServerAndClientBinding,
            server_key: "fixture-server",
            deadline: Duration::from_secs(3),
            freshness: Duration::from_millis(100),
            bootstrap: None,
            lane: None,
        },
        &mut journal,
        None,
        &clock,
    );
    (report, transport, journal, clock.now())
}

#[test]
fn one_paste_one_enter_preserves_payload_and_authoritative_token() {
    let (report, transport, journal, _) = execute("single", |_, _, _| {});
    assert_eq!(report.disposition, InjectionDisposition::NativeAccepted);
    assert_eq!(report.effect_floor, DeliveryEffect::Submitted);
    assert_eq!(report.counts.paste.confirmed, 1);
    assert_eq!(report.counts.initial.confirmed, 1);
    assert_eq!(report.counts.keys_issued(), 1);
    assert_eq!(transport.pasted.len(), 1);
    assert!(transport.pasted[0].ends_with(b"[team-agent-token:message-one]"));
    assert!(transport.pasted[0]
        .windows(b"[team-agent-token:forged-first-token]".len())
        .any(|w| w == b"[team-agent-token:forged-first-token]"));
    assert!(!transport.pasted[0].ends_with(b"\n"));
    assert!(transport.buffer.is_none());
    assert!(transport.key_times[0] - transport.paste_time.unwrap() >= Duration::from_millis(30));
    assert!(journal
        .records
        .iter()
        .any(|r| r.kind == JournalKind::ActionIntent && r.step == Some(StepKind::Paste)));
    assert_eq!(
        report.business_receipt().unwrap().message.as_str(),
        "message-one"
    );
}

#[test]
fn each_confirmation_has_its_own_guard_and_count() {
    for (mode, extra) in [("confirm", 1), ("confirm-two", 2)] {
        let (report, transport, _, _) = execute(mode, |_, _, _| {});
        assert_eq!(report.disposition, InjectionDisposition::NativeAccepted);
        assert_eq!(report.counts.initial.confirmed, 1);
        assert_eq!(report.counts.confirmation.confirmed, extra);
        assert_eq!(report.counts.retry.issued, 0);
        assert_eq!(transport.pasted.len(), 1);
        assert_eq!(transport.keys.len(), usize::from(extra) + 1);
    }
}

#[test]
fn optional_confirmation_is_not_forced_after_native_acceptance() {
    let (report, transport, _, _) = execute("confirm", |transport, _, _| {
        transport.mode = "single".into()
    });
    assert_eq!(report.disposition, InjectionDisposition::NativeAccepted);
    assert_eq!(transport.keys.len(), 1);
    assert_eq!(report.counts.confirmation.issued, 0);
}

#[test]
fn missing_confirmation_does_not_fall_back_to_retry() {
    let (report, transport, _, elapsed) =
        execute("confirm", |transport, _, _| transport.never_accept = true);
    assert_eq!(report.disposition, InjectionDisposition::Unresolved);
    assert_eq!(report.effect_floor, DeliveryEffect::MayHaveSubmitted);
    assert_eq!(transport.keys.len(), 1);
    assert_eq!(report.counts.retry.issued, 0);
    assert!(elapsed < Duration::from_secs(1));
}

#[test]
fn retry_wrap_and_queue_are_bounded_separate_single_key_actions() {
    let (report, transport, _, _) = execute("retry", |_, _, _| {});
    assert_eq!(report.disposition, InjectionDisposition::NativeAccepted);
    assert_eq!(report.counts.initial.confirmed, 1);
    assert_eq!(report.counts.retry.confirmed, 2);
    assert_eq!(report.counts.wrap_gap.confirmed, 1);
    assert_eq!(transport.keys.len(), 4);
    assert_eq!(transport.pasted.len(), 1);
    let (report, transport, _, _) = execute("queue", |_, _, _| {});
    assert_eq!(report.disposition, InjectionDisposition::NativeAccepted);
    assert_eq!(report.counts.initial.confirmed, 1);
    assert_eq!(report.counts.queue.confirmed, 2);
    assert_eq!(report.counts.retry.issued, 0);
    assert_eq!(transport.keys.len(), 3);
}

#[test]
fn busy_after_submit_never_produces_an_extra_key() {
    let (report, transport, _, _) =
        execute("retry", |transport, _, _| transport.mode = "busy".into());
    assert_eq!(report.effect_floor, DeliveryEffect::MayHaveSubmitted);
    assert_eq!(report.disposition, InjectionDisposition::Unresolved);
    assert_eq!(transport.keys.len(), 1);
    assert_eq!(transport.pasted.len(), 1);
}

#[test]
fn capture_or_fence_failure_before_paste_has_no_input_effect() {
    for fence in [false, true] {
        let (report, transport, _, _) = execute("single", |transport, _, _| {
            transport.bad_scope = fence;
            transport.capture_error = !fence;
        });
        assert_eq!(report.effect_floor, DeliveryEffect::NoEffect);
        assert!(transport.pasted.is_empty());
        assert!(transport.keys.is_empty());
    }
}

#[test]
fn post_enter_capture_failure_preserves_may_have_submitted_without_repaste() {
    let (report, transport, _, _) = execute("confirm", |transport, _, _| {
        transport.fail_capture_after_key = true
    });
    assert_eq!(report.effect_floor, DeliveryEffect::MayHaveSubmitted);
    assert_eq!(transport.pasted.len(), 1);
    assert_eq!(transport.keys.len(), 1);
}

#[test]
fn uncertain_paste_stops_before_any_enter_and_keeps_effect_floor() {
    let (report, transport, _, _) =
        execute("single", |transport, _, _| transport.uncertain_paste = true);
    assert_eq!(report.effect_floor, DeliveryEffect::MayHavePasted);
    assert_eq!(report.counts.paste.uncertain, 1);
    assert_eq!(transport.pasted.len(), 1);
    assert!(transport.keys.is_empty());
}

#[test]
fn uncertain_key_is_not_counted_as_confirmed_or_repeated() {
    let (report, transport, _, _) =
        execute("retry", |transport, _, _| transport.uncertain_key = true);
    assert_eq!(report.effect_floor, DeliveryEffect::MayHaveSubmitted);
    assert_eq!(report.counts.initial.confirmed, 0);
    assert_eq!(report.counts.initial.uncertain, 1);
    assert_eq!(transport.keys.len(), 1);
}

#[test]
fn wrong_message_after_key_cannot_authorize_confirmation() {
    let (report, transport, _, _) = execute("confirm", |transport, _, _| {
        transport.wrong_message_after_key = true
    });
    assert_ne!(report.disposition, InjectionDisposition::NativeAccepted);
    assert_eq!(transport.keys.len(), 1);
}

#[test]
fn known_copy_mode_preparation_is_separate_from_native_keys() {
    let (report, transport, _, _) =
        execute("single", |transport, _, _| transport.in_copy_mode = true);
    assert_eq!(report.disposition, InjectionDisposition::NativeAccepted);
    assert_eq!(report.counts.host_mode_confirmed, 1);
    assert_eq!(transport.keys.len(), 1);
    assert!(transport.preparations >= 4);
}

#[test]
fn unknown_host_mode_refuses_without_escape_or_control_c() {
    let (report, transport, _, _) =
        execute("single", |transport, _, _| transport.fail_prepare = true);
    assert_eq!(report.effect_floor, DeliveryEffect::NoEffect);
    assert!(transport.keys.is_empty());
    assert!(transport.pasted.is_empty());
}

#[test]
fn busy_native_lane_defers_instead_of_soft_lock_fallback() {
    let (report, transport, journal, _) = execute("single", |transport, _, _| {
        transport.locked.store(true, Ordering::SeqCst)
    });
    assert_eq!(report.disposition, InjectionDisposition::Deferred);
    assert!(transport.pasted.is_empty());
    assert!(journal.metadata.is_empty());
}

#[test]
fn journal_intent_failure_stops_before_the_corresponding_effect() {
    let (report, transport, _, _) = execute("single", |_, journal, _| {
        journal.fail_intent = Some(StepKind::Paste)
    });
    assert_eq!(report.effect_floor, DeliveryEffect::NoEffect);
    assert!(transport.pasted.is_empty());
    let (report, transport, _, _) = execute("single", |_, journal, _| {
        journal.fail_intent = Some(StepKind::InitialSubmit)
    });
    assert_eq!(report.effect_floor, DeliveryEffect::PastedUnsubmitted);
    assert_eq!(transport.pasted.len(), 1);
    assert!(transport.keys.is_empty());
}

#[test]
fn result_journal_failure_keeps_in_memory_physical_step_and_effect() {
    let (report, transport, _, _) = execute("single", |_, journal, _| {
        journal.fail_result = Some(StepKind::InitialSubmit)
    });
    assert_eq!(report.effect_floor, DeliveryEffect::MayHaveSubmitted);
    assert_eq!(report.persistence, PersistenceState::Failed);
    assert_eq!(transport.keys.len(), 1);
    assert!(report.events.iter().any(
        |e| matches!(e, DeliveryEvent::PhysicalStep(step) if step.kind == StepKind::InitialSubmit)
    ));
}

#[test]
fn acceptance_survives_later_audit_failure() {
    let (report, _, _, _) = execute("single", |_, journal, _| journal.fail_submitted = true);
    assert_eq!(report.effect_floor, DeliveryEffect::Submitted);
    assert_eq!(report.disposition, InjectionDisposition::NativeAccepted);
    assert_eq!(report.persistence, PersistenceState::Failed);
    assert!(report
        .events
        .iter()
        .any(|e| matches!(e, DeliveryEvent::NativeAcceptance { .. })));
}

#[test]
fn explicit_protocol_negatives_block_even_if_a_binding_is_positive() {
    let (report, transport, _, _) = execute("single", |_, _, protocol| {
        protocol.server.outcome = ProbeOutcome::Negative(ContraryEvidence::ProcessExited {
            pid: 400,
            exit_code: Some(1),
        })
    });
    assert_eq!(report.disposition, InjectionDisposition::Deferred);
    assert!(transport.pasted.is_empty());
}

#[test]
fn mcp_configuration_or_pending_binding_does_not_pass_without_bootstrap() {
    let (report, transport, _, _) = execute("single", |_, _, protocol| {
        protocol.binding.outcome = ProbeOutcome::Unknown(NO)
    });
    assert_eq!(report.disposition, InjectionDisposition::Deferred);
    assert!(transport.pasted.is_empty());
}

fn scoped_request<'a>(
    target: &'a team_agent_contract::host::transport::TargetReceipt,
    input: &'a PreparedInput,
    attempt: &'a AttemptId,
    policy: &'a ResolvedSubmitPolicy<'a>,
    proto: &'a ProtocolSnapshot,
    operation: Operation,
) -> InjectionRequest<'a> {
    InjectionRequest {
        target,
        input,
        attempt,
        operation,
        policy,
        protocol: proto,
        protocol_requirement: ProtocolRequirement::ClientBinding,
        server_key: "fixture-server",
        deadline: Duration::from_secs(3),
        freshness: Duration::from_millis(100),
        bootstrap: None,
        lane: None,
    }
}

#[test]
fn the_same_attempt_is_not_replayed_even_after_a_complete_receipt() {
    let target = target();
    let d = descriptor("single", &target.native);
    let h = hooks();
    let p = policy(&d, &h, &target, Operation::OrdinarySend);
    let envelope = envelope("one", "hello");
    let input = PreparedInput::business(&envelope);
    let attempt = AttemptId::new("same-attempt").unwrap();
    let clock = FakeClock::default();
    let proto = protocol(&target, clock.now());
    let mut journal = MemoryJournal::default();
    let mut first = FakeTransport::new(target.clone(), "single", "one");
    let first_report = inject_with_contract(
        &mut first,
        scoped_request(
            &target,
            &input,
            &attempt,
            &p,
            &proto,
            Operation::OrdinarySend,
        ),
        &mut journal,
        None,
        &clock,
    );
    assert_eq!(
        first_report.disposition,
        InjectionDisposition::NativeAccepted
    );
    let mut second = FakeTransport::new(target.clone(), "single", "one");
    let second_report = inject_with_contract(
        &mut second,
        scoped_request(
            &target,
            &input,
            &attempt,
            &p,
            &proto,
            Operation::OrdinarySend,
        ),
        &mut journal,
        None,
        &clock,
    );
    assert_eq!(second_report.effect_floor, DeliveryEffect::NoEffect);
    assert!(second.pasted.is_empty());
}

struct FailAfterKey;
impl DeliveryObserver for FailAfterKey {
    fn observe(&self, event: &DeliveryEvent) -> Result<(), ContractError> {
        if matches!(event, DeliveryEvent::PhysicalStep(step) if step.kind == StepKind::InitialSubmit)
        {
            Err(ContractError::Invalid("observer failure"))
        } else {
            Ok(())
        }
    }
}
#[test]
fn observer_failure_cannot_lower_effect_or_send_a_confirmation() {
    let target = target();
    let d = descriptor("confirm", &target.native);
    let h = hooks();
    let p = policy(&d, &h, &target, Operation::OrdinarySend);
    let e = envelope("one", "hi");
    let input = PreparedInput::business(&e);
    let attempt = AttemptId::new("observer").unwrap();
    let proto = protocol(&target, Duration::ZERO);
    let clock = FakeClock::default();
    let mut journal = MemoryJournal::default();
    let mut transport = FakeTransport::new(target.clone(), "confirm", "one");
    let report = inject_with_contract(
        &mut transport,
        scoped_request(
            &target,
            &input,
            &attempt,
            &p,
            &proto,
            Operation::OrdinarySend,
        ),
        &mut journal,
        Some(&FailAfterKey),
        &clock,
    );
    assert_eq!(report.effect_floor, DeliveryEffect::MayHaveSubmitted);
    assert_eq!(transport.keys.len(), 1);
}

struct OneBootstrap {
    left: u8,
    consumed: usize,
}
impl BootstrapCommit for OneBootstrap {
    fn consume(
        &mut self,
        _: &InstanceIdentity,
        _: &AttemptId,
        _: &dyn Clock,
        _: Duration,
    ) -> Result<(), HostError> {
        if self.left == 0 {
            return Err(HostError::new(
                "bootstrap already consumed",
                HostErrorKind::Conflict,
            ));
        }
        self.left -= 1;
        self.consumed += 1;
        Ok(())
    }
}
#[test]
fn bootstrap_is_committed_once_before_input_and_cannot_be_reused() {
    let target = target();
    let d = descriptor("single", &target.native);
    let h = hooks();
    let p = policy(&d, &h, &target, Operation::FirstBusiness);
    let mut proto = protocol(&target, Duration::ZERO);
    proto.binding.outcome = ProbeOutcome::Unknown(NO);
    let clock = FakeClock::default();
    let mut bootstrap = OneBootstrap {
        left: 1,
        consumed: 0,
    };
    for (id, accepted) in [("first", true), ("second", false)] {
        let e = envelope(id, "hello");
        let input = PreparedInput::business(&e);
        let attempt = AttemptId::new(id).unwrap();
        let mut transport = FakeTransport::new(target.clone(), "single", id);
        let mut journal = MemoryJournal::default();
        let mut request = scoped_request(
            &target,
            &input,
            &attempt,
            &p,
            &proto,
            Operation::FirstBusiness,
        );
        request.bootstrap = Some(&mut bootstrap);
        let report = inject_with_contract(&mut transport, request, &mut journal, None, &clock);
        if accepted {
            assert_eq!(report.disposition, InjectionDisposition::NativeAccepted);
            assert!(report.bootstrap_consumed);
        } else {
            assert!(transport.pasted.is_empty());
            assert!(!report.bootstrap_consumed);
        }
    }
    assert_eq!(bootstrap.consumed, 1);
}

#[test]
fn native_control_uses_the_same_executor_without_a_team_token() {
    let target = target();
    let d = descriptor("single", &target.native);
    let h = hooks();
    let p = policy(&d, &h, &target, Operation::SessionInspect);
    let definition = ControlDefinition {
        profile_id: "physical-fixture",
        policy_sha256: POLICY,
        operation: Operation::SessionInspect,
        kind: ControlKind::InspectSession,
        command: "/fixture-session",
    };
    let input = PreparedInput::control(
        OperationId::new("inspect").unwrap(),
        &NativeControl::InspectSession,
        &definition,
    )
    .unwrap();
    let attempt = AttemptId::new("inspect-attempt").unwrap();
    let proto = protocol(&target, Duration::ZERO);
    let clock = FakeClock::default();
    let mut transport = FakeTransport::new(target.clone(), "single", "-");
    let mut journal = MemoryJournal::default();
    let report = inject_with_contract(
        &mut transport,
        scoped_request(
            &target,
            &input,
            &attempt,
            &p,
            &proto,
            Operation::SessionInspect,
        ),
        &mut journal,
        None,
        &clock,
    );
    assert_eq!(report.disposition, InjectionDisposition::NativeAccepted);
    assert_eq!(transport.pasted, vec![b"/fixture-session".to_vec()]);
    assert_eq!(transport.keys.len(), 1);
    assert!(report.business_receipt().is_none());
    assert!(!report
        .events
        .iter()
        .any(|e| matches!(e, DeliveryEvent::NativeAcceptance { .. })));
}

#[test]
fn arbitrary_control_text_or_wrong_native_intent_is_rejected() {
    let definition = ControlDefinition {
        profile_id: "physical-fixture",
        policy_sha256: POLICY,
        operation: Operation::SessionInspect,
        kind: ControlKind::InspectSession,
        command: "/fixture-session\n/exit",
    };
    assert!(PreparedInput::control(
        OperationId::new("bad").unwrap(),
        &NativeControl::InspectSession,
        &definition
    )
    .is_err());
    assert!(PreparedEnvelope::from_rendered(
        MessageId::new("m").unwrap(),
        "/exit[team-agent-token:m]".into(),
        5..25
    )
    .is_err());
}

#[test]
fn payload_cannot_escape_bracketed_paste_with_embedded_terminal_controls() {
    let marker = "[team-agent-token:one]";
    for content in [
        "before\u{1b}[201~\r/exit",
        "before\u{3}after",
        "before\0after",
    ] {
        let rendered = format!("Team Agent message from fixture:\n\n{content}\n\n{marker}");
        let end = rendered.len();
        assert!(PreparedEnvelope::from_rendered(
            MessageId::new("one").unwrap(),
            rendered,
            end - marker.len()..end
        )
        .is_err());
    }
}

#[test]
fn policy_cannot_be_reused_for_another_operation_provider_or_evidence_kind() {
    let original = target();
    let d = descriptor("single", &original.native);
    let h = hooks();
    let p = policy(&d, &h, &original, Operation::OrdinarySend);
    let e = envelope("one", "hi");
    let input = PreparedInput::business(&e);
    let attempt = AttemptId::new("mismatch").unwrap();
    for case in 0..4 {
        let mut t = original.clone();
        let mut op = Operation::OrdinarySend;
        match case {
            0 => op = Operation::FirstBusiness,
            1 => t.provider = ProviderId::new("other-provider").unwrap(),
            2 => t.evidence_kind = EvidenceKind::Native,
            _ => t.candidate_sha256 = NATIVE,
        }
        let proto = protocol(&t, Duration::ZERO);
        let clock = FakeClock::default();
        let mut journal = MemoryJournal::default();
        let mut transport = FakeTransport::new(t.clone(), "single", "one");
        let report = inject_with_contract(
            &mut transport,
            scoped_request(&t, &input, &attempt, &p, &proto, op),
            &mut journal,
            None,
            &clock,
        );
        assert_eq!(report.disposition, InjectionDisposition::Refused);
        assert!(transport.pasted.is_empty());
        assert!(journal.metadata.is_empty());
    }
}

#[test]
fn startup_ack_requires_resolved_permission_and_reobserves_modal_clear() {
    let target = target();
    let d = descriptor("single", &target.native);
    let h = hooks();
    let mut launch_request = LaunchRequest {
        provider: target.provider.as_str().into(),
        operation: Operation::Fresh,
        mode: LaunchMode::FullWorker,
        auth: AuthMode::NativeSubscription,
        model: None,
        role_effort: None,
        team_effort: None,
        bypass: false,
        prompt: Some("fixture role".into()),
        identity: owner(),
        native: target.native.clone(),
        paths: LaunchPaths {
            executable: "/fixture/bin".into(),
            candidate: "/fixture/candidate".into(),
            cwd: target.cwd.clone(),
            runtime_root: target.directory.path.clone(),
        },
        channel: Channel::Tmux,
        input_profile: Some("physical-fixture".into()),
        evidence_kind: EvidenceKind::Fixture,
        preassigned_session: None,
        resume: None,
        fork: None,
    };
    let no_permission = resolve_launch(&d, &h, &launch_request, None).unwrap();
    assert!(StartupConsent::from_launch(&no_permission).is_err());
    launch_request.bypass = true;
    let launch = resolve_launch(&d, &h, &launch_request, None).unwrap();
    let consent = StartupConsent::from_launch(&launch).unwrap();
    let definition = StartupDefinition {
        profile_id: "physical-fixture",
        policy_sha256: POLICY,
        predicate: "consent",
    };
    let input = PreparedInput::startup(OperationId::new("startup").unwrap(), &consent, &definition)
        .unwrap();
    let attempt = AttemptId::new("startup-attempt").unwrap();
    let p = policy(&d, &h, &target, Operation::StartupBypassAck);
    let proto = protocol(&target, Duration::ZERO);
    let clock = FakeClock::default();
    let mut journal = MemoryJournal::default();
    let mut transport = FakeTransport::new(target.clone(), "startup", "-");
    let report = inject_with_contract(
        &mut transport,
        scoped_request(
            &target,
            &input,
            &attempt,
            &p,
            &proto,
            Operation::StartupBypassAck,
        ),
        &mut journal,
        None,
        &clock,
    );
    assert_eq!(report.disposition, InjectionDisposition::StartupCleared);
    assert_eq!(report.counts.startup.confirmed, 1);
    assert_eq!(report.counts.initial.issued, 0);
    assert!(transport.pasted.is_empty());
    assert!(report.business_receipt().is_none());
}
