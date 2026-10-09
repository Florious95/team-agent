use std::time::Duration;

use super::descriptor::ProviderDescriptor;
use super::hooks::{InteractionHook, ProviderHooks};
use super::probe::EvidenceScope;
use super::types::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PhysicalKey {
    Enter,
    LineFeed,
    CtrlJ,
    Up,
    Down,
    Tab,
    Escape,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PasteMode {
    Bracketed,
    Plain,
    /// Literal text insertion, admitted only for a registered native control.
    DirectTyping,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PayloadTrailer {
    None,
    VerifiedBytes {
        bytes: &'static [u8],
        evidence: &'static str,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfirmationStep {
    pub predicate: &'static str,
    pub key: PhysicalKey,
    pub deadline: Duration,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RetryBudget {
    Never,
    Guarded {
        predicate: &'static str,
        additional: u16,
        wrap_gap: u16,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QueueAction {
    pub predicate: &'static str,
    pub key: PhysicalKey,
    pub max_presses: u16,
    pub interval: Duration,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InputTiming {
    pub capture_interval: Duration,
    pub stable_window: Duration,
    pub paste_to_submit_floor: Duration,
    pub deadline: Duration,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubmitPolicy {
    pub paste_mode: PasteMode,
    pub payload_trailer: PayloadTrailer,
    pub initial_submit: PhysicalKey,
    pub confirmation_steps: &'static [ConfirmationStep],
    pub retry_budget: RetryBudget,
    pub queue_flush: &'static [QueueAction],
    pub timing: InputTiming,
    pub max_submit_keys: u16,
}

impl SubmitPolicy {
    /// One initial key + confirmations + individual retries + wrap gap + queue keys.
    pub fn key_upper_bound(&self) -> Result<u16, ContractError> {
        let confirmations = u16::try_from(self.confirmation_steps.len())
            .map_err(|_| ContractError::BudgetOverflow)?;
        let (retry, wrap) = match self.retry_budget {
            RetryBudget::Never => (0, 0),
            RetryBudget::Guarded {
                additional,
                wrap_gap,
                ..
            } => (additional, wrap_gap),
        };
        let mut total = 1_u16
            .checked_add(confirmations)
            .and_then(|n| n.checked_add(retry))
            .and_then(|n| n.checked_add(wrap))
            .ok_or(ContractError::BudgetOverflow)?;
        for action in self.queue_flush {
            total = total
                .checked_add(action.max_presses)
                .ok_or(ContractError::BudgetOverflow)?;
        }
        Ok(total)
    }

    pub fn validate(&self) -> Result<(), ContractError> {
        if self.initial_submit != PhysicalKey::Enter
            || self
                .confirmation_steps
                .iter()
                .any(|step| step.key != PhysicalKey::Enter)
        {
            return Err(ContractError::Invalid(
                "submit uses a logical Enter, not a newline byte",
            ));
        }
        if self.timing.capture_interval.is_zero()
            || self.timing.deadline.is_zero()
            || self.timing.capture_interval > self.timing.deadline
            || self.timing.stable_window > self.timing.deadline
            || self.timing.paste_to_submit_floor > self.timing.deadline
        {
            return Err(ContractError::Invalid("input timing"));
        }
        if let PayloadTrailer::VerifiedBytes { bytes, evidence } = &self.payload_trailer {
            if bytes.is_empty() || !nonblank(evidence) {
                return Err(ContractError::Invalid("payload trailer evidence"));
            }
        }
        if !self.confirmation_steps.is_empty() && !matches!(self.retry_budget, RetryBudget::Never) {
            return Err(ContractError::Invalid(
                "confirmation cannot fall back to retry",
            ));
        }
        if let RetryBudget::Guarded {
            predicate,
            additional,
            wrap_gap,
        } = self.retry_budget
        {
            if !nonblank(predicate) || (additional == 0 && wrap_gap == 0) {
                return Err(ContractError::Invalid("retry guard"));
            }
        }
        let mut predicates = Vec::new();
        for step in self.confirmation_steps {
            if !nonblank(step.predicate)
                || predicates.contains(&step.predicate)
                || step.deadline.is_zero()
                || step.deadline > self.timing.deadline
            {
                return Err(ContractError::Invalid("confirmation step"));
            }
            predicates.push(step.predicate);
        }
        for action in self.queue_flush {
            if !nonblank(action.predicate)
                || predicates.contains(&action.predicate)
                || action.max_presses == 0
                || action.interval.is_zero()
                || action.interval > self.timing.deadline
            {
                return Err(ContractError::Invalid("queue action"));
            }
            predicates.push(action.predicate);
        }
        if self.key_upper_bound()? > self.max_submit_keys {
            return Err(ContractError::Invalid("total submit budget"));
        }
        Ok(())
    }
}

/// A grammar profile is not an executable attestation. RuntimeCaptured selects
/// a stable UI recipe; exact native identity remains in the generation, policy
/// evidence, process receipt and every physical fence. It does not authorize a
/// running generation to adopt a different executable after an update.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProfileIdentity {
    Exact {
        version: &'static str,
        executable_sha256: Digest,
    },
    RuntimeCaptured,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InputProfile {
    pub id: &'static str,
    pub identity: ProfileIdentity,
    pub harness: &'static str,
    pub ui: &'static str,
    pub platform: Platform,
    pub policy_sha256: Digest,
    pub operations: &'static [Operation],
    pub channel: Channel,
    pub policy: Support<SubmitPolicy>,
}

impl InputProfile {
    pub fn matches_native(&self, native: &NativeIdentity) -> bool {
        let identity_matches = match self.identity {
            ProfileIdentity::Exact {
                version,
                executable_sha256,
            } => version == native.version && executable_sha256 == native.executable_sha256,
            ProfileIdentity::RuntimeCaptured => native.validate().is_ok(),
        };
        identity_matches
            && self.harness == native.harness
            && self.ui == native.ui
            && self.platform == native.platform
    }
}

pub struct PolicyRequest<'a> {
    pub provider: &'a str,
    pub native: &'a NativeIdentity,
    pub profile_id: &'a str,
    pub operation: Operation,
    pub channel: Channel,
    pub evidence: &'a CapabilityEvidence,
    pub required_evidence_kind: EvidenceKind,
    pub candidate_sha256: Digest,
}

pub struct ResolvedSubmitPolicy<'a> {
    profile: &'a InputProfile,
    policy: &'a SubmitPolicy,
    interaction: &'a dyn InteractionHook,
    provider: &'static str,
    operation: Operation,
    evidence_kind: EvidenceKind,
    candidate_sha256: Digest,
    native: NativeIdentity,
}

impl<'a> ResolvedSubmitPolicy<'a> {
    pub fn provider(&self) -> &str {
        self.provider
    }
    pub fn operation(&self) -> Operation {
        self.operation
    }
    pub fn evidence_kind(&self) -> EvidenceKind {
        self.evidence_kind
    }
    pub fn candidate_sha256(&self) -> Digest {
        self.candidate_sha256
    }
    pub fn native(&self) -> &NativeIdentity {
        &self.native
    }
    pub fn profile(&self) -> &InputProfile {
        self.profile
    }
    pub fn policy(&self) -> &SubmitPolicy {
        self.policy
    }
    pub fn interaction(&self) -> &dyn InteractionHook {
        self.interaction
    }
}

pub fn resolve_submit_policy<'a>(
    descriptor: &'a ProviderDescriptor,
    hooks: &'a ProviderHooks<'a>,
    request: &PolicyRequest<'_>,
) -> Result<ResolvedSubmitPolicy<'a>, ContractError> {
    super::descriptor::validate_descriptor(descriptor, hooks, request.operation)?;
    request.native.validate()?;
    if request.provider != descriptor.identity.id {
        return Err(ContractError::UnknownProvider);
    }
    let profile = descriptor.input.resolve(
        request.native,
        request.profile_id,
        request.operation,
        request.channel,
    )?;
    let policy = profile.policy.require("input profile")?;
    let evidence = request.evidence;
    if evidence.provider.as_str() != request.provider
        || evidence.native != *request.native
        || evidence.profile_id != profile.id
        || evidence.policy_sha256 != profile.policy_sha256
        || evidence.channel != request.channel
        || evidence.operation != request.operation
        || evidence.kind != request.required_evidence_kind
        || evidence.candidate_sha256 != request.candidate_sha256
    {
        return Err(ContractError::Mismatch("input profile evidence"));
    }
    Ok(ResolvedSubmitPolicy {
        profile,
        policy,
        interaction: *hooks.interaction.require("H6 InteractionHook")?,
        provider: descriptor.identity.id,
        operation: request.operation,
        evidence_kind: request.required_evidence_kind,
        candidate_sha256: request.candidate_sha256,
        native: request.native.clone(),
    })
}

/// The trusted marker is carried beside immutable rendered bytes, never parsed from content.
/// Rendering itself remains the shared Team protocol, not a second implementation in K1.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogicalEnvelope {
    pub message: MessageId,
    pub sender: SeatId,
    pub task: Option<String>,
    pub content: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputSurface {
    ShellOrUnknown,
    Starting,
    StartupRiskWarning,
    ComposerReady,
    ComposerContainsPaste,
    Busy,
    Queued,
    NativeAccepted,
    Fault,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PasteLatch {
    NeverSeen,
    Seen { native_identity: String },
    Gone { native_identity: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CaptureBaseline {
    pub scope: EvidenceScope,
    pub text: String,
}

/// All context comes from this serialized attempt; H6 needs no mutable global state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CaptureFrame {
    pub scope: EvidenceScope,
    pub text: String,
    pub baseline: Option<CaptureBaseline>,
    pub profile_id: String,
    pub operation: Operation,
    pub message: Option<MessageId>,
    pub attempt: Option<AttemptId>,
    pub after_step: Option<StepKind>,
    pub paste_latch: PasteLatch,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InteractionObservation {
    pub scope: EvidenceScope,
    pub surface: InputSurface,
    pub predicate: Option<String>,
    pub current_message: Option<MessageId>,
    pub current_attempt: Option<AttemptId>,
    pub paste_latch: PasteLatch,
}

#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
pub enum DeliveryEffect {
    NoEffect,
    MayHavePasted,
    PastedUnsubmitted,
    MayHaveSubmitted,
    Submitted,
}

impl DeliveryEffect {
    /// Observation/audit failure cannot lower an already reached effect floor.
    pub fn retain_floor(self, later: Self) -> Self {
        self.max(later)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StepKind {
    Paste,
    InitialSubmit,
    Confirmation,
    Retry,
    WrapGap,
    QueueFlush,
    StartupAck,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StepOutcome {
    Confirmed,
    NoEffect,
    MayHaveOccurred,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PhysicalStep {
    pub ordinal: u64,
    pub scope: EvidenceScope,
    pub kind: StepKind,
    pub key: Option<PhysicalKey>,
    pub outcome: StepOutcome,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DeliveryEvent {
    PhysicalStep(PhysicalStep),
    NativeAcceptance {
        scope: EvidenceScope,
        message: MessageId,
        attempt: AttemptId,
        source: String,
    },
}

pub trait DeliveryObserver {
    fn observe(&self, event: &DeliveryEvent) -> Result<(), ContractError>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeliveryReceipt {
    pub identity: InstanceIdentity,
    pub message: MessageId,
    pub attempt: AttemptId,
    pub operation: Operation,
    pub channel: Channel,
    pub policy_sha256: Digest,
    pub events: Vec<DeliveryEvent>,
    pub effect_floor: DeliveryEffect,
    pub failure: Option<ContractError>,
    pub persistence: PersistenceState,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PersistenceState {
    Durable,
    Pending,
    Failed,
}
