use super::delivery::DeliveryEffect;
use super::descriptor::{
    AuthMode, ProviderDescriptor, ResourceDisposition, ResourceKind, ResumeMode,
};
use super::hooks::ProviderHooks;
use super::plan::{MaterializeReceipt, ResourceWriteEffect};
use super::session::{
    validate_resume, CaptureOrigin, CwdIdentity, NativeSessionId, ResumeBinding, ResumeExpectation,
};
use super::types::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ForkMode {
    InWindowBranch,
    NewSeatFullSnapshot,
    NativeNewSeat,
}

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
    pub target_native_session: Option<NativeSessionId>,
    pub target_backing: Option<OwnedPath>,
    pub channel: Channel,
    pub input_profile: Option<String>,
    pub cwd: CwdIdentity,
    pub native: NativeIdentity,
    pub evidence_kind: EvidenceKind,
    pub selected_turn: Option<u64>,
    pub operation_id: OperationId,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedFork {
    request: ForkRequest,
}

impl ResolvedFork {
    pub fn request(&self) -> &ForkRequest {
        &self.request
    }
}

pub fn resolve_fork(
    descriptor: &ProviderDescriptor,
    hooks: &ProviderHooks<'_>,
    request: &ForkRequest,
) -> Result<ResolvedFork, ContractError> {
    // Capability rejection precedes hook invocation, staging, commands or new-seat effects.
    match request.mode {
        ForkMode::InWindowBranch => descriptor.fork.in_window.require("in-window branch")?,
        ForkMode::NewSeatFullSnapshot => descriptor.fork.full_snapshot.require("full snapshot")?,
        ForkMode::NativeNewSeat => descriptor.fork.native_new_seat.require("native new seat")?,
    };
    if !descriptor.fork.allowed_auth.contains(&request.auth) {
        return Err(ContractError::Invalid("fork authentication mode"));
    }
    descriptor
        .auth
        .mechanism(request.auth)
        .require("authentication mode")?;
    super::descriptor::validate_descriptor(descriptor, hooks, request.mode.operation())?;
    let provider = ProviderId::new(descriptor.identity.id)?;
    let mode = if request.mode == ForkMode::NewSeatFullSnapshot {
        ResumeMode::ExactPath
    } else {
        ResumeMode::ExactId
    };
    validate_resume(
        &request.source,
        &ResumeExpectation {
            provider: &provider,
            source: &request.expected_source,
            cwd: &request.cwd,
            native: &request.native,
            evidence_kind: request.evidence_kind,
            mode,
        },
    )?;
    let same = request.target == request.expected_source;
    if request.target.scope != request.expected_source.scope
        || (request.mode == ForkMode::InWindowBranch && !same)
        || (request.mode != ForkMode::InWindowBranch
            && (request.target.seat == request.expected_source.seat
                || request.target.instance == request.expected_source.instance))
    {
        return Err(ContractError::Mismatch("fork target identity"));
    }
    match request.mode {
        ForkMode::InWindowBranch => {
            if request.target_native_session.is_some() || request.target_backing.is_some() {
                return Err(ContractError::Invalid(
                    "in-window branch cannot allocate a seat snapshot",
                ));
            }
            descriptor.input.resolve(
                &request.native,
                request
                    .input_profile
                    .as_deref()
                    .ok_or(ContractError::ProfileUnavailable)?,
                Operation::InWindowBranch,
                request.channel,
            )?;
        }
        ForkMode::NewSeatFullSnapshot => {
            descriptor
                .session
                .resume
                .require("snapshot target resume")?;
            let session = request
                .target_native_session
                .as_ref()
                .ok_or(ContractError::Invalid(
                    "snapshot target session must be preallocated",
                ))?;
            let path = request
                .target_backing
                .as_ref()
                .ok_or(ContractError::Invalid(
                    "snapshot target path must be preallocated",
                ))?;
            if session == &request.source.native_session
                || Some(path.path()) == request.source.backing
            {
                return Err(ContractError::Mismatch(
                    "snapshot target must not replace source",
                ));
            }
            if request.selected_turn.is_some() {
                return Err(ContractError::Invalid(
                    "full snapshot is not a selected-turn branch",
                ));
            }
        }
        ForkMode::NativeNewSeat => {
            if request.target_native_session.is_some()
                || request.target_backing.is_some()
                || request.selected_turn.is_some()
            {
                return Err(ContractError::Invalid(
                    "native new seat captures its own child session",
                ));
            }
        }
    }
    Ok(ResolvedFork {
        request: request.clone(),
    })
}

/// A restricted native intent, not arbitrary terminal text and never a Team-token envelope.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NativeControl {
    InspectSession,
    BranchCurrent,
    BranchToTurn(u64),
    Exit,
}

/// H7's stage result contains backing/intent, never argv or another LaunchPlan.
/// H2 can preflight the target from ResolvedFork before H7 performs any I/O.
/// This is not a committed fork or a readiness receipt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NativeForkPlan {
    InWindow {
        control: NativeControl,
    },
    FullSnapshot {
        source: Box<ResumeBinding>,
        target: Box<ResumeBinding>,
        staging: MaterializeReceipt,
    },
    NativeNewSeat {
        source: Box<ResumeBinding>,
        target: InstanceIdentity,
    },
}

/// Checks H7's result before register/spawn. Failure does not undo stage effects:
/// the caller must retain the returned plan/owned-I/O journal for compensation.
pub fn validate_native_fork_plan(
    resolved: &ResolvedFork,
    plan: &NativeForkPlan,
) -> Result<(), ContractError> {
    let request = resolved.request();
    match (request.mode, plan) {
        (ForkMode::InWindowBranch, NativeForkPlan::InWindow { control }) => {
            let expected = request
                .selected_turn
                .map(NativeControl::BranchToTurn)
                .unwrap_or(NativeControl::BranchCurrent);
            if control != &expected {
                return Err(ContractError::Mismatch("in-window control"));
            }
        }
        (
            ForkMode::NewSeatFullSnapshot,
            NativeForkPlan::FullSnapshot {
                source,
                target,
                staging,
            },
        ) => {
            if source.as_ref() != &request.source {
                return Err(ContractError::Mismatch("snapshot source"));
            }
            let path = request
                .target_backing
                .as_ref()
                .ok_or(ContractError::Invalid("snapshot target path"))?;
            if Some(&target.native_session) != request.target_native_session.as_ref()
                || target.backing.as_ref() != Some(&path.path())
                || target.origin != CaptureOrigin::ExactBacking
            {
                return Err(ContractError::Mismatch("staged snapshot binding"));
            }
            validate_resume(
                target,
                &ResumeExpectation {
                    provider: &request.source.provider,
                    source: &request.target,
                    cwd: &request.cwd,
                    native: &request.native,
                    evidence_kind: request.evidence_kind,
                    mode: ResumeMode::ExactPath,
                },
            )?;
            let mut paths = Vec::new();
            let mut has_backing = false;
            for resource in &staging.resources {
                let bytes_sha256 = match resource.write_effect {
                    ResourceWriteEffect::Written { bytes_sha256 } => bytes_sha256,
                    ResourceWriteEffect::MayHaveWritten => return Err(ContractError::Invalid("snapshot stage has uncertain writes")),
                };
                if resource.owner != request.target
                    || resource.operation != request.operation_id
                    || resource.path.root() != path.root()
                    || !resource.exclusive
                    || paths.contains(&resource.path)
                    || matches!(
                        resource.kind,
                        ResourceKind::GlobalSettings | ResourceKind::NativeDatabase
                    )
                    || !matches!(
                        resource.disposition,
                        ResourceDisposition::OwnedPreserved | ResourceDisposition::OwnedRemovable
                    )
                {
                    return Err(ContractError::Mismatch("snapshot stage ownership"));
                }
                if &resource.path == path {
                    if resource.kind != ResourceKind::SessionBacking
                        || resource.disposition != ResourceDisposition::OwnedPreserved
                        || bytes_sha256 != target.evidence_sha256
                    {
                        return Err(ContractError::Mismatch("snapshot backing receipt"));
                    }
                    has_backing = true;
                }
                paths.push(resource.path.clone());
            }
            if !has_backing {
                return Err(ContractError::Invalid("snapshot backing receipt required"));
            }
        }
        (ForkMode::NativeNewSeat, NativeForkPlan::NativeNewSeat { source, target }) => {
            if source.as_ref() != &request.source || target != &request.target {
                return Err(ContractError::Mismatch("native new-seat intent"));
            }
        }
        _ => return Err(ContractError::Mismatch("fork mode and stage result")),
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ForkPhase {
    Preflight,
    Stage,
    Register,
    Spawn,
    Commit,
    Receipt,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BackingState {
    NotApplicable,
    Staged,
    Verified,
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompensationStatus {
    NotNeeded,
    Completed,
    Partial,
    Preserved,
    Unknown,
}

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
pub enum ForkReadiness {
    Pending,
    ReadyForInput,
    Unknown,
}

/// Deliberately outside ForkMode: derived text is not a full-session snapshot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContextClone {
    pub source: InstanceIdentity,
    pub text: String,
}
