use std::ops::Range;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::time::Duration;

use super::journal::*;
use super::probes::{
    control_surface_allowed, evaluate_protocol_gate, ProtocolRequirement, ProtocolSnapshot,
};
use crate::contract::delivery::*;
use crate::contract::fork::NativeControl;
use crate::contract::plan::ResolvedLaunch;
use crate::contract::probe::*;
use crate::contract::types::*;
use crate::host::clock::{remaining, Clock};
use crate::host::transport::*;
use crate::host::{digest, digest_hex, HostError, HostErrorKind};

fn safe_text(bytes: &[u8]) -> bool {
    !bytes
        .iter()
        .any(|byte| (*byte < 0x20 && !matches!(*byte, 9 | 10 | 13)) || *byte == 0x7f)
}

/// Shared renderer output plus an authoritative token location supplied by the caller.
/// Never infer the message id from the first/last marker found in user content.
pub struct PreparedEnvelope {
    rendered: String,
    message: MessageId,
    token: Range<usize>,
}
impl PreparedEnvelope {
    /// The shared Team protocol renderer (same header/task/token grammar as
    /// legacy messaging::delivery::render_message), never adapter-specific.
    pub fn from_logical(envelope: &LogicalEnvelope) -> Result<Self, ContractError> {
        let mut header = format!("Team Agent message from {}", envelope.sender.as_str());
        if let Some(task) = envelope.task.as_deref().filter(|task| !task.is_empty()) {
            header.push_str(&format!(" for {task}"));
        }
        let token = format!("[team-agent-token:{}]", envelope.message.as_str());
        let rendered = format!("{header}:\n\n{}\n\n{token}", envelope.content);
        let end = rendered.len();
        Self::from_rendered(envelope.message.clone(), rendered, end - token.len()..end)
    }

    pub fn from_rendered(
        message: MessageId,
        rendered: String,
        token: Range<usize>,
    ) -> Result<Self, ContractError> {
        let expected = format!("[team-agent-token:{}]", message.as_str());
        if !rendered.starts_with("Team Agent message from ")
            || !safe_text(rendered.as_bytes())
            || token.end != rendered.len()
            || rendered.get(token.clone()) != Some(expected.as_str())
        {
            return Err(ContractError::Invalid("rendered envelope/token boundary"));
        }
        Ok(Self {
            rendered,
            message,
            token,
        })
    }
    pub fn rendered(&self) -> &str {
        &self.rendered
    }
    pub fn message(&self) -> &MessageId {
        &self.message
    }
    pub fn token_range(&self) -> Range<usize> {
        self.token.clone()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ControlKind {
    InspectSession,
    InspectTools,
    BranchCurrent,
    BranchToTurn,
    Exit,
}

/// Native encoding is reviewable profile data, not an eighth executable provider hook.
/// The full profile hash must cover these definitions as well as its submit policy.
pub struct ControlDefinition {
    pub profile_id: &'static str,
    pub policy_sha256: Digest,
    pub operation: Operation,
    pub kind: ControlKind,
    pub command: &'static str,
}

pub struct StartupDefinition {
    pub profile_id: &'static str,
    pub policy_sha256: Digest,
    pub predicate: &'static str,
}

pub struct StartupConsent {
    owner: InstanceIdentity,
}
impl StartupConsent {
    pub fn from_launch(launch: &ResolvedLaunch) -> Result<Self, ContractError> {
        if !launch.request().bypass {
            return Err(ContractError::Invalid("startup bypass was not authorized"));
        }
        Ok(Self {
            owner: launch.request().identity.clone(),
        })
    }
}

enum InputPurpose {
    Business,
    Control {
        profile: String,
        hash: Digest,
        operation: Operation,
    },
    Startup {
        owner: InstanceIdentity,
        profile: String,
        hash: Digest,
        predicate: String,
    },
}

/// Sealed prepared input: business text, native control and a no-payload startup ack differ.
pub struct PreparedInput {
    bytes: Vec<u8>,
    correlation: Correlation,
    purpose: InputPurpose,
}
impl PreparedInput {
    pub fn business(envelope: &PreparedEnvelope) -> Self {
        Self {
            bytes: envelope.rendered.as_bytes().to_vec(),
            correlation: Correlation::Business(envelope.message.clone()),
            purpose: InputPurpose::Business,
        }
    }
    pub fn control(
        identity: OperationId,
        intent: &NativeControl,
        definition: &ControlDefinition,
    ) -> Result<Self, ContractError> {
        let kind = match intent {
            NativeControl::InspectSession => ControlKind::InspectSession,
            NativeControl::InspectTools => ControlKind::InspectTools,
            NativeControl::BranchCurrent => ControlKind::BranchCurrent,
            NativeControl::BranchToTurn(_) => ControlKind::BranchToTurn,
            NativeControl::Exit => ControlKind::Exit,
        };
        let operation_matches = match kind {
            ControlKind::InspectSession => definition.operation == Operation::SessionInspect,
            ControlKind::InspectTools => definition.operation == Operation::ToolInspect,
            ControlKind::BranchCurrent | ControlKind::BranchToTurn => {
                definition.operation == Operation::InWindowBranch
            }
            ControlKind::Exit => {
                matches!(definition.operation, Operation::Stop | Operation::Shutdown)
            }
        };
        if kind != definition.kind
            || !operation_matches
            || definition.profile_id.is_empty()
            || !definition.command.starts_with('/')
            || definition.command.len() < 2
            || !definition
                .command
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'-' | b'_'))
        {
            return Err(ContractError::Invalid("native control definition"));
        }
        let text = match intent {
            NativeControl::BranchToTurn(turn) => format!("{} {turn}", definition.command),
            _ => definition.command.to_string(),
        };
        Ok(Self {
            bytes: text.into_bytes(),
            correlation: Correlation::NativeControl(identity),
            purpose: InputPurpose::Control {
                profile: definition.profile_id.into(),
                hash: definition.policy_sha256,
                operation: definition.operation,
            },
        })
    }
    pub fn startup(
        identity: OperationId,
        consent: &StartupConsent,
        definition: &StartupDefinition,
    ) -> Result<Self, ContractError> {
        if definition.predicate.trim().is_empty() || definition.profile_id.is_empty() {
            return Err(ContractError::Invalid("startup acknowledgement definition"));
        }
        Ok(Self {
            bytes: Vec::new(),
            correlation: Correlation::Startup(identity),
            purpose: InputPurpose::Startup {
                owner: consent.owner.clone(),
                profile: definition.profile_id.into(),
                hash: definition.policy_sha256,
                predicate: definition.predicate.into(),
            },
        })
    }
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
    pub fn correlation(&self) -> &Correlation {
        &self.correlation
    }
    fn message(&self) -> Option<&MessageId> {
        match &self.correlation {
            Correlation::Business(message) => Some(message),
            _ => None,
        }
    }
}

/// K3 implements this using its durable scope/generation-fenced store. Consuming a
/// bootstrap reservation must succeed before input; there is no default/no-op success.
pub trait BootstrapCommit {
    fn consume(
        &mut self,
        owner: &InstanceIdentity,
        attempt: &AttemptId,
        clock: &dyn Clock,
        deadline: Duration,
    ) -> Result<(), HostError>;
}

/// Attempt-local borrows and the bootstrap port's object lifetime differ. A
/// long-lived borrowed store must not force local host/policy/journal values to
/// live as long as that store (mutable trait objects are lifetime-invariant).
pub struct InjectionRequest<'a, 'bootstrap: 'a> {
    pub target: &'a TargetReceipt,
    pub input: &'a PreparedInput,
    pub attempt: &'a AttemptId,
    pub operation: Operation,
    pub policy: &'a ResolvedSubmitPolicy<'a>,
    pub protocol: &'a ProtocolSnapshot,
    pub protocol_requirement: ProtocolRequirement,
    pub server_key: &'a str,
    pub deadline: Duration,
    pub freshness: Duration,
    pub bootstrap: Option<&'a mut (dyn BootstrapCommit + 'bootstrap)>,
    /// A lifecycle operation can hold the same lane across inspect/fork/stop; otherwise
    /// this executor acquires it. A held lane is not released by this call.
    pub lane: Option<&'a dyn InputLease>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ActionCounts {
    pub issued: u16,
    pub confirmed: u16,
    pub uncertain: u16,
}
impl ActionCounts {
    fn record(&mut self, outcome: StepOutcome) {
        self.issued = self.issued.saturating_add(1);
        match outcome {
            StepOutcome::Confirmed => self.confirmed = self.confirmed.saturating_add(1),
            StepOutcome::MayHaveOccurred => self.uncertain = self.uncertain.saturating_add(1),
            StepOutcome::NoEffect => {}
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PhysicalCounts {
    pub paste: ActionCounts,
    pub initial: ActionCounts,
    pub confirmation: ActionCounts,
    pub retry: ActionCounts,
    pub wrap_gap: ActionCounts,
    pub queue: ActionCounts,
    pub startup: ActionCounts,
    pub host_mode_confirmed: u32,
    pub host_mode_uncertain: u32,
    pub captures: u64,
}
impl PhysicalCounts {
    pub fn keys_issued(&self) -> u32 {
        [
            self.initial,
            self.confirmation,
            self.retry,
            self.wrap_gap,
            self.queue,
            self.startup,
        ]
        .iter()
        .map(|count| u32::from(count.issued))
        .sum()
    }
    fn record(&mut self, kind: StepKind, outcome: StepOutcome) {
        match kind {
            StepKind::Paste => &mut self.paste,
            StepKind::InitialSubmit => &mut self.initial,
            StepKind::Confirmation => &mut self.confirmation,
            StepKind::Retry => &mut self.retry,
            StepKind::WrapGap => &mut self.wrap_gap,
            StepKind::QueueFlush => &mut self.queue,
            StepKind::StartupAck => &mut self.startup,
        }
        .record(outcome);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InjectionDisposition {
    Refused,
    Deferred,
    Unresolved,
    NativeAccepted,
    StartupCleared,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExecutionProblem {
    pub code: &'static str,
    pub host: Option<HostError>,
}
impl ExecutionProblem {
    fn code(code: &'static str) -> Self {
        Self { code, host: None }
    }
    fn host(code: &'static str, host: HostError) -> Self {
        Self {
            code,
            host: Some(host),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InjectionReport {
    pub metadata: AttemptMetadata,
    pub disposition: InjectionDisposition,
    pub counts: PhysicalCounts,
    pub effect_floor: DeliveryEffect,
    pub events: Vec<DeliveryEvent>,
    pub problems: Vec<ExecutionProblem>,
    pub persistence: PersistenceState,
    pub bootstrap_consumed: bool,
    pub unreleased_buffer: Option<String>,
}

impl InjectionReport {
    pub fn business_receipt(&self) -> Option<DeliveryReceipt> {
        let Correlation::Business(message) = &self.metadata.correlation else {
            return None;
        };
        Some(DeliveryReceipt {
            identity: self.metadata.owner.clone(),
            message: message.clone(),
            attempt: self.metadata.attempt.clone(),
            operation: self.metadata.operation,
            channel: Channel::Tmux,
            policy_sha256: self.metadata.policy_sha256,
            events: self.events.clone(),
            effect_floor: self.effect_floor,
            failure: self
                .problems
                .first()
                .map(|problem| ContractError::Invalid(problem.code)),
            persistence: self.persistence,
        })
    }
}

#[derive(Clone)]
enum Guard {
    Pasted,
    Confirmation(String),
    Retry(String),
    Queue(String),
    Startup(String),
}

struct Run<'a, 'bootstrap: 'a> {
    transport: &'a mut dyn PhysicalTransport,
    request: InjectionRequest<'a, 'bootstrap>,
    journal: &'a mut dyn AttemptJournal,
    observer: Option<&'a dyn DeliveryObserver>,
    clock: &'a dyn Clock,
    report: InjectionReport,
    ordinal: u64,
    baseline: Option<CaptureBaseline>,
    last_capture: Option<CaptureBaseline>,
    latch: PasteLatch,
    after_step: Option<StepKind>,
    last_scope: Option<EvidenceScope>,
    last_key_at: Option<Duration>,
    journal_started: bool,
    execution_seen: bool,
}

/// The business caller seam delegates to exactly the same executor as native controls.
pub fn deliver_envelope<'a, 'bootstrap: 'a>(
    transport: &'a mut dyn PhysicalTransport,
    request: InjectionRequest<'a, 'bootstrap>,
    journal: &'a mut dyn AttemptJournal,
    observer: Option<&'a dyn DeliveryObserver>,
    clock: &'a dyn Clock,
) -> InjectionReport {
    inject_with_contract(transport, request, journal, observer, clock)
}

pub fn inject_with_contract<'a, 'bootstrap: 'a>(
    transport: &'a mut dyn PhysicalTransport,
    request: InjectionRequest<'a, 'bootstrap>,
    journal: &'a mut dyn AttemptJournal,
    observer: Option<&'a dyn DeliveryObserver>,
    clock: &'a dyn Clock,
) -> InjectionReport {
    let metadata = AttemptMetadata {
        owner: request.target.identity().clone(),
        attempt: request.attempt.clone(),
        correlation: request.input.correlation.clone(),
        operation: request.operation,
        policy_sha256: request.policy.profile().policy_sha256,
        payload_sha256: digest(&request.input.bytes),
        payload_bytes: request.input.bytes.len(),
    };
    let report = InjectionReport {
        metadata,
        disposition: InjectionDisposition::Refused,
        counts: PhysicalCounts::default(),
        effect_floor: DeliveryEffect::NoEffect,
        events: Vec::new(),
        problems: Vec::new(),
        persistence: PersistenceState::Pending,
        bootstrap_consumed: false,
        unreleased_buffer: None,
    };
    let mut run = Run {
        transport,
        request,
        journal,
        observer,
        clock,
        report,
        ordinal: 0,
        baseline: None,
        last_capture: None,
        latch: PasteLatch::NeverSeen,
        after_step: None,
        last_scope: None,
        last_key_at: None,
        journal_started: false,
        execution_seen: false,
    };
    let result = run.execute();
    if let Err(problem) = result {
        if !matches!(
            run.report.disposition,
            InjectionDisposition::NativeAccepted | InjectionDisposition::StartupCleared
        ) {
            run.report.disposition = if run.report.effect_floor != DeliveryEffect::NoEffect {
                InjectionDisposition::Unresolved
            } else if matches!(
                problem.code,
                "input-not-ready" | "native-lane-busy" | "protocol-not-ready"
            ) {
                InjectionDisposition::Deferred
            } else {
                InjectionDisposition::Refused
            };
        }
        run.report.problems.push(problem);
        let _ = run.record(
            JournalKind::Failure,
            None,
            None,
            None,
            Some("execution-stopped"),
        );
    }
    run.release_buffer();
    if run.journal_started && run.report.persistence != PersistenceState::Failed {
        if let Err(problem) = run.record(JournalKind::Complete, None, None, None, None) {
            run.report.problems.push(problem);
        }
    }
    run.report
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Progress {
    Guard,
    Accepted,
    OtherQueue,
}

impl Run<'_, '_> {
    fn record(
        &mut self,
        kind: JournalKind,
        step: Option<StepKind>,
        outcome: Option<StepOutcome>,
        surface: Option<InputSurface>,
        code: Option<&'static str>,
    ) -> Result<(), ExecutionProblem> {
        if !self.journal_started {
            return Ok(());
        }
        if self.report.persistence == PersistenceState::Failed {
            return Err(ExecutionProblem::code("journal-unavailable"));
        }
        self.ordinal = self
            .ordinal
            .checked_add(1)
            .ok_or_else(|| ExecutionProblem::code("journal-ordinal-overflow"))?;
        let record = JournalRecord {
            kind,
            ordinal: self.ordinal,
            at: self.clock.now(),
            effect: self.report.effect_floor,
            step,
            outcome,
            sequence: self.last_scope.as_ref().map(|scope| scope.sequence),
            surface,
            code,
        };
        if let Err(error) = self.journal.append(&record) {
            self.report.persistence = PersistenceState::Failed;
            return Err(ExecutionProblem::host("journal-write-failed", error));
        }
        self.report.persistence = PersistenceState::Durable;
        Ok(())
    }

    fn expected(&self, minimum_sequence: u64) -> EvidenceExpectation<'_> {
        let target = self.request.target;
        EvidenceExpectation {
            identity: target.identity(),
            endpoint: target.endpoint.to_str().unwrap_or(""),
            pane: &target.pane,
            binding: &target.binding,
            session: target.native_session.as_ref(),
            minimum_sequence,
            now: self.clock.now(),
            process: &target.process.identity,
            profile_id: self.request.policy.profile().id,
            server_key: self.request.server_key,
        }
    }

    fn prepare(&mut self, deadline: Duration) -> Result<(), ExecutionProblem> {
        if remaining(self.clock, deadline.min(self.request.deadline)).is_none() {
            return Err(ExecutionProblem::code("host-preparation-deadline"));
        }
        let result = self.transport.prepare_host_mode(
            self.request.target,
            self.clock,
            deadline.min(self.request.deadline),
        );
        self.report.counts.host_mode_confirmed += u32::from(result.control_commands_confirmed);
        self.report.counts.host_mode_uncertain += u32::from(result.control_commands_uncertain);
        if let Some(error) = result.error {
            return Err(ExecutionProblem::host("host-mode-blocked", error));
        }
        Ok(())
    }

    fn capture(&mut self, deadline: Duration) -> Result<InteractionObservation, ExecutionProblem> {
        let deadline = deadline.min(self.request.deadline);
        if remaining(self.clock, deadline).is_none() {
            return Err(ExecutionProblem::code("observation-deadline"));
        }
        self.report.counts.captures = self.report.counts.captures.saturating_add(1);
        let sample = self
            .transport
            .capture(
                self.request.target,
                self.clock,
                deadline,
                self.request.freshness,
            )
            .map_err(|error| ExecutionProblem::host("capture-failed", error))?;
        let next = self
            .last_scope
            .as_ref()
            .map(|scope| scope.sequence.saturating_add(1))
            .unwrap_or(0);
        if sample.mode != PaneMode::Normal || !sample.scope.is_current(&self.expected(next)) {
            return Err(ExecutionProblem::code("stale-or-noninput-capture"));
        }
        let frame = CaptureFrame {
            scope: sample.scope.clone(),
            text: sample.text,
            baseline: self.baseline.clone(),
            profile_id: self.request.policy.profile().id.into(),
            operation: self.request.operation,
            message: self.request.input.message().cloned(),
            attempt: Some(self.request.attempt.clone()),
            after_step: self.after_step,
            paste_latch: self.latch.clone(),
        };
        let observation = catch_unwind(AssertUnwindSafe(|| {
            self.request.policy.interaction().interpret(&frame)
        }))
        .map_err(|_| ExecutionProblem::code("interaction-parser-panicked"))?;
        if observation.scope != frame.scope {
            return Err(ExecutionProblem::code("interaction-scope-mismatch"));
        }
        match (&self.latch, &observation.paste_latch) {
            (_, PasteLatch::Seen { native_identity } | PasteLatch::Gone { native_identity })
                if native_identity.is_empty() =>
            {
                return Err(ExecutionProblem::code("empty-paste-identity"))
            }
            (
                PasteLatch::Seen {
                    native_identity: old,
                }
                | PasteLatch::Gone {
                    native_identity: old,
                },
                PasteLatch::Seen {
                    native_identity: new,
                }
                | PasteLatch::Gone {
                    native_identity: new,
                },
            ) if old != new => return Err(ExecutionProblem::code("paste-identity-changed")),
            (PasteLatch::Gone { .. }, PasteLatch::Seen { .. })
            | (PasteLatch::Seen { .. } | PasteLatch::Gone { .. }, PasteLatch::NeverSeen)
            | (PasteLatch::NeverSeen, PasteLatch::Gone { .. }) => {
                return Err(ExecutionProblem::code("paste-latch-regressed"))
            }
            _ => {}
        }
        self.latch = observation.paste_latch.clone();
        self.last_scope = Some(frame.scope.clone());
        self.last_capture = Some(CaptureBaseline {
            scope: frame.scope.clone(),
            text: frame.text,
        });
        if self.baseline.is_none() {
            self.baseline = self.last_capture.clone();
        }
        if observation.surface == InputSurface::Busy {
            self.execution_seen = true;
        }
        let accepted = observation.surface == InputSurface::NativeAccepted
            && self.after_step.is_some()
            && !matches!(self.request.input.purpose, InputPurpose::Startup { .. })
            && self.correlated(&observation);
        let mut acceptance_event = None;
        if accepted {
            // Preserve the stronger native fact in memory BEFORE audit/observer operations.
            self.report.effect_floor = self
                .report
                .effect_floor
                .retain_floor(DeliveryEffect::Submitted);
            self.report.disposition = InjectionDisposition::NativeAccepted;
            self.execution_seen = true;
            if let Some(message) = self.request.input.message() {
                let event = DeliveryEvent::NativeAcceptance {
                    scope: observation.scope.clone(),
                    message: message.clone(),
                    attempt: self.request.attempt.clone(),
                    source: "current-native-H6-evidence".into(),
                };
                self.report.events.push(event.clone());
                acceptance_event = Some(event);
            }
        }
        let startup_cleared = matches!(self.request.input.purpose, InputPurpose::Startup { .. })
            && self.report.counts.startup.issued > 0
            && observation.surface == InputSurface::ComposerReady;
        if startup_cleared {
            self.report.disposition = InjectionDisposition::StartupCleared;
        }
        self.record(
            JournalKind::Surface,
            None,
            None,
            Some(observation.surface),
            None,
        )?;
        if accepted || startup_cleared {
            self.record(
                JournalKind::Accepted,
                None,
                None,
                Some(observation.surface),
                None,
            )?;
        }
        if let Some(event) = acceptance_event {
            self.notify(&event)?;
        }
        Ok(observation)
    }

    fn correlated(&self, observation: &InteractionObservation) -> bool {
        observation.current_attempt.as_ref() == Some(self.request.attempt)
            && observation.current_message.as_ref() == self.request.input.message()
    }

    fn guard_matches(&self, observation: &InteractionObservation, guard: &Guard) -> bool {
        if self.execution_seen || !self.correlated(observation) {
            return false;
        }
        match guard {
            Guard::Pasted => {
                observation.surface == InputSurface::ComposerContainsPaste
                    && matches!(observation.paste_latch, PasteLatch::Seen { .. })
            }
            Guard::Confirmation(predicate) => {
                observation.surface == InputSurface::ComposerContainsPaste
                    && observation.predicate.as_ref() == Some(predicate)
                    && !matches!(observation.paste_latch, PasteLatch::NeverSeen)
            }
            Guard::Retry(predicate) => {
                observation.surface == InputSurface::ComposerContainsPaste
                    && observation.predicate.as_ref() == Some(predicate)
                    && matches!(observation.paste_latch, PasteLatch::Seen { .. })
            }
            Guard::Queue(predicate) => {
                observation.surface == InputSurface::Queued
                    && observation.predicate.as_ref() == Some(predicate)
            }
            Guard::Startup(predicate) => {
                observation.surface == InputSurface::StartupRiskWarning
                    && observation.predicate.as_ref() == Some(predicate)
            }
        }
    }

    fn accepted(&self) -> bool {
        matches!(
            self.report.disposition,
            InjectionDisposition::NativeAccepted | InjectionDisposition::StartupCleared
        )
    }

    fn pause(&self, interval: Duration, deadline: Duration) -> Result<(), ExecutionProblem> {
        let left = remaining(self.clock, deadline.min(self.request.deadline))
            .ok_or_else(|| ExecutionProblem::code("observation-deadline"))?;
        self.clock.sleep(interval.min(left));
        Ok(())
    }

    fn wait_guard(
        &mut self,
        guard: &Guard,
        deadline: Duration,
        allow_other_queue: bool,
        not_before: Duration,
    ) -> Result<Progress, ExecutionProblem> {
        let policy = self.request.policy.policy().clone();
        let mut stable = None;
        loop {
            if remaining(self.clock, deadline.min(self.request.deadline)).is_none() {
                return Err(ExecutionProblem::code("guard-deadline"));
            }
            let observation = self.capture(deadline)?;
            if self.accepted() {
                return Ok(Progress::Accepted);
            }
            if self.guard_matches(&observation, guard) {
                let since = *stable.get_or_insert(self.clock.now());
                if self.clock.now() >= not_before
                    && self.clock.now().saturating_sub(since) >= policy.timing.stable_window
                {
                    return Ok(Progress::Guard);
                }
            } else {
                stable = None;
                if allow_other_queue
                    && observation.surface == InputSurface::Queued
                    && self.correlated(&observation)
                {
                    return Ok(Progress::OtherQueue);
                }
                if matches!(
                    observation.surface,
                    InputSurface::ShellOrUnknown
                        | InputSurface::Starting
                        | InputSurface::Fault
                        | InputSurface::StartupRiskWarning
                ) && !matches!(guard, Guard::Startup(_))
                {
                    return Err(ExecutionProblem::code("unknown-or-blocked-native-surface"));
                }
            }
            self.pause(policy.timing.capture_interval, deadline)?;
        }
    }

    fn notify(&self, event: &DeliveryEvent) -> Result<(), ExecutionProblem> {
        if let Some(observer) = self.observer {
            match catch_unwind(AssertUnwindSafe(|| observer.observe(event))) {
                Ok(Ok(())) => {}
                Ok(Err(_)) => return Err(ExecutionProblem::code("observer-failed")),
                Err(_) => return Err(ExecutionProblem::code("observer-panicked")),
            }
        }
        Ok(())
    }

    fn physical_result(
        &mut self,
        kind: StepKind,
        key: Option<PhysicalKey>,
        result: ActionResult,
    ) -> Result<(), ExecutionProblem> {
        self.report.counts.record(kind, result.outcome);
        if result.outcome != StepOutcome::NoEffect {
            self.report.effect_floor =
                self.report
                    .effect_floor
                    .retain_floor(if kind == StepKind::Paste {
                        DeliveryEffect::MayHavePasted
                    } else {
                        DeliveryEffect::MayHaveSubmitted
                    });
        }
        self.after_step = Some(kind);
        if kind != StepKind::Paste {
            self.last_key_at = Some(self.clock.now());
        }
        let scope = self
            .last_scope
            .clone()
            .ok_or_else(|| ExecutionProblem::code("missing-action-scope"))?;
        let event = DeliveryEvent::PhysicalStep(PhysicalStep {
            ordinal: self.ordinal,
            scope,
            kind,
            key,
            outcome: result.outcome,
        });
        self.report.events.push(event.clone());
        self.record(
            JournalKind::ActionResult,
            Some(kind),
            Some(result.outcome),
            None,
            None,
        )?;
        self.notify(&event)?;
        if let Some(error) = result.error {
            return Err(ExecutionProblem::host("physical-action-failed", error));
        }
        if result.outcome != StepOutcome::Confirmed {
            return Err(ExecutionProblem::code("physical-action-unconfirmed"));
        }
        Ok(())
    }

    fn press(
        &mut self,
        kind: StepKind,
        key: PhysicalKey,
        guard: &Guard,
        deadline: Duration,
    ) -> Result<(), ExecutionProblem> {
        let policy = self.request.policy.policy().clone();
        if self.report.counts.keys_issued() >= u32::from(policy.max_submit_keys) {
            return Err(ExecutionProblem::code("submit-budget-exhausted"));
        }
        if let Some(previous) = self.last_key_at {
            let earliest = previous.saturating_add(
                policy
                    .timing
                    .capture_interval
                    .max(policy.timing.stable_window),
            );
            if self.clock.now() < earliest {
                self.pause(earliest - self.clock.now(), deadline)?;
            }
        }
        self.prepare(deadline)?;
        let observation = self.capture(deadline)?;
        if self.accepted() {
            return Ok(());
        }
        if !self.guard_matches(&observation, guard) {
            return Err(ExecutionProblem::code("key-guard-changed"));
        }
        self.record(JournalKind::ActionIntent, Some(kind), None, None, None)?;
        let result = self.transport.key(
            self.request.target,
            key,
            self.clock,
            deadline.min(self.request.deadline),
        );
        self.physical_result(kind, Some(key), result)
    }

    fn release_buffer(&mut self) {
        let Some(name) = self.report.unreleased_buffer.clone() else {
            return;
        };
        let result = self.transport.release_buffer(
            self.request.target,
            &name,
            self.clock,
            self.request.deadline,
        );
        if result.outcome == StepOutcome::Confirmed {
            self.report.unreleased_buffer = None;
        } else {
            self.report.problems.push(ExecutionProblem {
                code: "owned-buffer-release-unconfirmed",
                host: result.error,
            });
        }
    }

    fn input_gate(&mut self) -> Result<bool, ExecutionProblem> {
        self.prepare(self.request.deadline)?;
        let process = self
            .transport
            .validate_target(self.request.target, self.clock, self.request.deadline)
            .map_err(|error| ExecutionProblem::host("native-target-unverified", error))?;
        if process.process != self.request.target.process.identity {
            return Err(ExecutionProblem::code("native-process-mismatch"));
        }
        let observation = self.capture(self.request.deadline)?;
        if self.report.counts.paste.issued == 0
            && !matches!(observation.paste_latch, PasteLatch::NeverSeen)
        {
            return Err(ExecutionProblem::code("existing-composer-input"));
        }
        let surface_ok = match &self.request.input.purpose {
            InputPurpose::Business => observation.surface == InputSurface::ComposerReady,
            InputPurpose::Control { .. } => {
                control_surface_allowed(self.request.operation, observation.surface)
            }
            InputPurpose::Startup { predicate, .. } => {
                self.guard_matches(&observation, &Guard::Startup(predicate.clone()))
            }
        };
        if !surface_ok {
            return Err(ExecutionProblem::code("input-not-ready"));
        }
        if matches!(self.request.input.purpose, InputPurpose::Business) {
            let process = Probe {
                scope: observation.scope.clone(),
                outcome: ProbeOutcome::Observed(process),
            };
            let pane = Probe {
                scope: observation.scope.clone(),
                outcome: ProbeOutcome::Observed(PaneReadyEvidence {
                    profile_id: self.request.policy.profile().id.into(),
                    surface: observation.surface,
                }),
            };
            let allowance = if self.request.bootstrap.is_some() {
                BootstrapAllowance::AuthorizedFirstBusinessOnce { remaining: 1 }
            } else {
                BootstrapAllowance::Disabled
            };
            match evaluate_protocol_gate(
                &process,
                &pane,
                self.request.protocol,
                &self.expected(0),
                self.request.operation,
                self.request.protocol_requirement,
                allowance,
            ) {
                SendGate::ReadyForInput { consume_bootstrap } => Ok(consume_bootstrap),
                SendGate::Blocked(_) => Err(ExecutionProblem::code("protocol-not-ready")),
            }
        } else {
            Ok(false)
        }
    }

    fn execute(&mut self) -> Result<(), ExecutionProblem> {
        let policy = self.request.policy.policy().clone();
        policy
            .validate()
            .map_err(|_| ExecutionProblem::code("invalid-submit-policy"))?;
        if !matches!(self.request.input.purpose, InputPurpose::Startup { .. })
            && policy.paste_mode != PasteMode::Bracketed
        {
            return Err(ExecutionProblem::code(
                "unframed-terminal-paste-unsupported",
            ));
        }
        if matches!(self.request.input.purpose, InputPurpose::Startup { .. })
            && (!policy.confirmation_steps.is_empty()
                || !matches!(policy.retry_budget, RetryBudget::Never)
                || !policy.queue_flush.is_empty())
        {
            return Err(ExecutionProblem::code("startup-ack-sequence-unsupported"));
        }
        let profile = self.request.policy.profile();
        if profile.channel != Channel::Tmux
            || !profile.operations.contains(&self.request.operation)
            || self.request.policy.operation() != self.request.operation
            || self.request.policy.provider() != self.request.target.provider.as_str()
            || self.request.policy.evidence_kind() != self.request.target.evidence_kind
            || self.request.policy.candidate_sha256() != self.request.target.candidate_sha256
            || !profile.matches_native(&self.request.target.native)
            || self.request.freshness.is_zero()
            || self.request.target.endpoint.to_str().is_none()
        {
            return Err(ExecutionProblem::code("execution-profile-mismatch"));
        }
        match &self.request.input.purpose {
            InputPurpose::Business
                if !matches!(
                    self.request.operation,
                    Operation::FirstBusiness | Operation::OrdinarySend
                ) =>
            {
                return Err(ExecutionProblem::code("business-operation-mismatch"))
            }
            InputPurpose::Control {
                profile: id,
                hash,
                operation,
            } if id != profile.id
                || *hash != profile.policy_sha256
                || *operation != self.request.operation =>
            {
                return Err(ExecutionProblem::code("control-profile-mismatch"))
            }
            InputPurpose::Startup {
                owner,
                profile: id,
                hash,
                ..
            } if owner != self.request.target.identity()
                || id != profile.id
                || *hash != profile.policy_sha256
                || self.request.operation != Operation::StartupBypassAck =>
            {
                return Err(ExecutionProblem::code("startup-authorization-mismatch"))
            }
            _ => {}
        }
        self.request.deadline = self.request.deadline.min(
            self.clock
                .now()
                .checked_add(policy.timing.deadline)
                .ok_or_else(|| ExecutionProblem::code("execution-deadline-overflow"))?,
        );
        if remaining(self.clock, self.request.deadline).is_none() {
            return Err(ExecutionProblem::code("execution-deadline"));
        }
        let owned_lane = if let Some(lane) = self.request.lane {
            if lane.directory() != &self.request.target.directory {
                return Err(ExecutionProblem::code("native-lane-owner-mismatch"));
            }
            None
        } else {
            Some(
                self.transport
                    .acquire_lane(self.request.target)
                    .map_err(|error| {
                        ExecutionProblem::host(
                            if error.kind == HostErrorKind::Conflict {
                                "native-lane-busy"
                            } else {
                                "native-lane-refused"
                            },
                            error,
                        )
                    })?,
            )
        };
        if owned_lane
            .as_ref()
            .is_some_and(|lane| lane.directory() != &self.request.target.directory)
        {
            return Err(ExecutionProblem::code("native-lane-owner-mismatch"));
        }
        self.journal.begin(&self.report.metadata).map_err(|error| {
            self.report.persistence = PersistenceState::Failed;
            ExecutionProblem::host("attempt-journal-refused", error)
        })?;
        self.journal_started = true;
        self.report.persistence = PersistenceState::Durable;
        self.input_gate()?;
        if let InputPurpose::Startup { predicate, .. } = &self.request.input.purpose {
            let guard = Guard::Startup(predicate.clone());
            self.wait_guard(&guard, self.request.deadline, false, self.clock.now())?;
            self.press(
                StepKind::StartupAck,
                policy.initial_submit,
                &guard,
                self.request.deadline,
            )?;
            return self.observe_only();
        }
        // A fresh gate after the stable window prevents writing into a changing composer.
        self.pause(policy.timing.stable_window, self.request.deadline)?;
        self.input_gate()?;
        let mut bytes = self.request.input.bytes.clone();
        if let PayloadTrailer::VerifiedBytes { bytes: suffix, .. } = policy.payload_trailer {
            bytes.extend_from_slice(suffix);
        }
        if !safe_text(&bytes) {
            return Err(ExecutionProblem::code("payload-control-byte-unsupported"));
        }
        let name = format!(
            "tac-{}",
            digest_hex(digest(journal_name(&self.report.metadata).as_bytes()))
        );
        let staged = self.transport.stage_buffer(
            self.request.target,
            &name,
            &bytes,
            policy.paste_mode,
            self.clock,
            self.request.deadline,
        );
        if staged.outcome != StepOutcome::NoEffect {
            self.report.unreleased_buffer = Some(name.clone());
        }
        if staged.outcome != StepOutcome::Confirmed {
            return Err(ExecutionProblem {
                code: "buffer-stage-failed",
                host: staged.error,
            });
        }
        let consume_bootstrap = self.input_gate()?;
        if consume_bootstrap {
            let reservation = self
                .request
                .bootstrap
                .as_deref_mut()
                .ok_or_else(|| ExecutionProblem::code("bootstrap-reservation-missing"))?;
            reservation
                .consume(
                    self.request.target.identity(),
                    self.request.attempt,
                    self.clock,
                    self.request.deadline,
                )
                .map_err(|error| ExecutionProblem::host("bootstrap-reservation-refused", error))?;
            self.report.bootstrap_consumed = true;
            self.record(
                JournalKind::Surface,
                None,
                None,
                Some(InputSurface::ComposerReady),
                Some("bootstrap-consumed-before-input"),
            )?;
        }
        self.baseline = self.last_capture.clone();
        self.record(
            JournalKind::ActionIntent,
            Some(StepKind::Paste),
            None,
            None,
            None,
        )?;
        let pasted = self.transport.paste_buffer(
            self.request.target,
            &name,
            policy.paste_mode,
            self.clock,
            self.request.deadline,
        );
        let pasted_at = self.clock.now();
        self.physical_result(StepKind::Paste, None, pasted)?;
        self.release_buffer();
        if self.report.unreleased_buffer.is_some() {
            return Err(ExecutionProblem::code("buffer-release-failed"));
        }
        if self.wait_guard(
            &Guard::Pasted,
            self.request.deadline,
            false,
            pasted_at.saturating_add(policy.timing.paste_to_submit_floor),
        )? == Progress::Accepted
        {
            return Ok(());
        }
        self.report.effect_floor = self
            .report
            .effect_floor
            .retain_floor(DeliveryEffect::PastedUnsubmitted);
        self.press(
            StepKind::InitialSubmit,
            policy.initial_submit,
            &Guard::Pasted,
            self.request.deadline,
        )?;
        if self.accepted() {
            return Ok(());
        }
        for confirmation in policy.confirmation_steps {
            let guard = Guard::Confirmation(confirmation.predicate.into());
            let deadline = self
                .clock
                .now()
                .saturating_add(confirmation.deadline)
                .min(self.request.deadline);
            if self.wait_guard(&guard, deadline, false, self.clock.now())? == Progress::Accepted {
                return Ok(());
            }
            self.press(StepKind::Confirmation, confirmation.key, &guard, deadline)?;
            if self.accepted() {
                return Ok(());
            }
        }
        if let RetryBudget::Guarded {
            predicate,
            additional,
            wrap_gap,
        } = policy.retry_budget
        {
            let guard = Guard::Retry(predicate.into());
            let mut queued = false;
            for (kind, count) in [(StepKind::Retry, additional), (StepKind::WrapGap, wrap_gap)] {
                for _ in 0..count {
                    match self.wait_guard(&guard, self.request.deadline, true, self.clock.now())? {
                        Progress::Accepted => return Ok(()),
                        Progress::OtherQueue => {
                            queued = true;
                            break;
                        }
                        Progress::Guard => {}
                    }
                    self.press(kind, policy.initial_submit, &guard, self.request.deadline)?;
                    if self.accepted() {
                        return Ok(());
                    }
                }
                if queued {
                    break;
                }
            }
        }
        for action in policy.queue_flush {
            let guard = Guard::Queue(action.predicate.into());
            for _ in 0..action.max_presses {
                match self.wait_guard(&guard, self.request.deadline, true, self.clock.now())? {
                    Progress::Accepted => return Ok(()),
                    Progress::OtherQueue => break,
                    Progress::Guard => {}
                }
                self.press(
                    StepKind::QueueFlush,
                    action.key,
                    &guard,
                    self.request.deadline,
                )?;
                if self.accepted() {
                    return Ok(());
                }
                self.pause(action.interval, self.request.deadline)?;
            }
        }
        self.observe_only()
    }

    fn observe_only(&mut self) -> Result<(), ExecutionProblem> {
        let interval = self.request.policy.policy().timing.capture_interval;
        loop {
            let observation = self.capture(self.request.deadline)?;
            if self.accepted() {
                return Ok(());
            }
            if matches!(
                observation.surface,
                InputSurface::ShellOrUnknown | InputSurface::Fault
            ) {
                return Err(ExecutionProblem::code("acceptance-unresolved"));
            }
            self.pause(interval, self.request.deadline)?;
        }
    }
}
