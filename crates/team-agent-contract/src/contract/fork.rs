use super::delivery::DeliveryEffect;
use super::descriptor::{AuthMode, ProviderDescriptor, ResumeMode};
use super::hooks::ProviderHooks;
use super::plan::{LaunchPlan, MaterializeReceipt};
use super::session::{validate_resume, CwdIdentity, ResumeBinding, ResumeExpectation};
use super::types::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ForkMode { InWindowBranch, NewSeatFullSnapshot, NativeNewSeat }

impl ForkMode {
    pub fn operation(self) -> Operation {
        match self {
            Self::InWindowBranch => Operation::InWindowBranch,
            Self::NewSeatFullSnapshot => Operation::NewSeatFullSnapshot,
            Self::NativeNewSeat => Operation::NativeNewSeat,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ForkRequest {
    pub mode: ForkMode,
    pub auth: AuthMode,
    pub source: ResumeBinding,
    pub expected_source: InstanceIdentity,
    pub target: InstanceIdentity,
    pub cwd: CwdIdentity,
    pub native: NativeIdentity,
    pub evidence_kind: EvidenceKind,
    pub selected_turn: Option<u64>,
    pub operation_id: OperationId,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedFork { request: ForkRequest }

impl ResolvedFork { pub fn request(&self) -> &ForkRequest { &self.request } }

pub fn resolve_fork(descriptor: &ProviderDescriptor, hooks: &ProviderHooks<'_>, request: &ForkRequest) -> Result<ResolvedFork, ContractError> {
    // Capability rejection precedes hook invocation, staging, commands or new-seat effects.
    match request.mode {
        ForkMode::InWindowBranch => descriptor.fork.in_window.require("in-window branch")?,
        ForkMode::NewSeatFullSnapshot => descriptor.fork.full_snapshot.require("full snapshot")?,
        ForkMode::NativeNewSeat => descriptor.fork.native_new_seat.require("native new seat")?,
    };
    if !descriptor.fork.allowed_auth.contains(&request.auth) {
        return Err(ContractError::Invalid("fork authentication mode"));
    }
    descriptor.auth.mechanism(request.auth).require("authentication mode")?;
    super::descriptor::validate_descriptor(descriptor, hooks, request.mode.operation())?;
    let provider = ProviderId::new(descriptor.identity.id)?;
    let mode = if request.mode == ForkMode::NewSeatFullSnapshot { ResumeMode::ExactPath } else { ResumeMode::ExactId };
    validate_resume(&request.source, &ResumeExpectation {
        provider: &provider, source: &request.expected_source, cwd: &request.cwd,
        native: &request.native, evidence_kind: request.evidence_kind, mode,
    })?;
    let same = request.target == request.expected_source;
    if request.target.scope != request.expected_source.scope
        || (request.mode == ForkMode::InWindowBranch && !same)
        || (request.mode != ForkMode::InWindowBranch && (request.target.seat == request.expected_source.seat || request.target.instance == request.expected_source.instance))
    {
        return Err(ContractError::Mismatch("fork target identity"));
    }
    Ok(ResolvedFork { request: request.clone() })
}

/// A restricted native intent, not arbitrary terminal text and never a Team-token envelope.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NativeControl { InspectSession, BranchToTurn(u64), Exit }

/// A plan is not a committed fork. Only the lifecycle executor may produce a receipt.
#[derive(Clone, PartialEq, Eq)]
pub enum NativeForkPlan {
    InWindow { control: NativeControl },
    FullSnapshot { staging: MaterializeReceipt, launch: LaunchPlan },
    NativeNewSeat { launch: LaunchPlan },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ForkPhase { Preflight, Stage, Register, Spawn, Commit, Receipt }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BackingState { NotApplicable, Staged, Verified, Unknown }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompensationStatus { NotNeeded, Completed, Partial, Preserved, Unknown }

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ForkReceipt {
    pub mode: ForkMode,
    pub operation_id: OperationId,
    pub source: InstanceIdentity,
    pub target: InstanceIdentity,
    pub completed: Vec<ForkPhase>,
    pub new_binding: Option<ResumeBinding>,
    pub evidence_kind: EvidenceKind,
    pub backing: BackingState,
    pub resources: MaterializeReceipt,
    pub effects: DeliveryEffect,
    pub readiness: ForkReadiness,
    pub compensation: CompensationStatus,
    pub preserved: Vec<OwnedPath>,
    pub failure: Option<ContractError>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ForkReadiness { Pending, ReadyForInput, Unknown }

/// Deliberately outside ForkMode: derived text is not a full-session snapshot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContextClone { pub source: InstanceIdentity, pub text: String }
