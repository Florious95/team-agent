use std::path::PathBuf;
use std::time::Duration;

use super::delivery::{CaptureFrame, InteractionObservation};
use super::descriptor::CatalogSource;
use super::fork::{NativeForkPlan, ResolvedFork};
use super::plan::*;
use super::probe::{EvidenceScope, Probe, SemanticEvidence};
use super::session::{CaptureOrigin, CwdIdentity, ResumeBinding};
use super::types::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReadBounds {
    pub deadline: Duration,
    pub max_output_bytes: usize,
}

impl ReadBounds {
    pub fn validate(&self) -> Result<(), ContractError> {
        if self.deadline.is_zero() || self.max_output_bytes == 0 {
            Err(ContractError::Invalid("read bounds"))
        } else {
            Ok(())
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CatalogRequest {
    pub provider: ProviderId,
    pub native: NativeIdentity,
    pub executable: PathBuf,
    pub cwd: CwdIdentity,
    pub source: CatalogSource,
    pub bounds: ReadBounds,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReadFailure {
    /// The native read requested authentication; no credentials are inspected and
    /// no model/catalog result may be inferred from the partial terminal output.
    AuthRequired,
    TimedOut { elapsed: Duration },
    OutputLimit { limit: usize },
    Exit { code: i32 },
    Unknown(Reason),
    Error(ContractError),
}

/// Bounded raw output has no Debug implementation, so it is not accidentally logged.
pub struct ReadOutput {
    pub stdout: Vec<u8>,
    pub elapsed: Duration,
    pub exit_code: i32,
}

/// A host implementation enforces its captured command whitelist and absolute deadline.
/// This port is not an eighth provider hook and does not expose a shell runner.
pub trait BoundedReadHost {
    fn read_catalog(&mut self, request: &CatalogRequest) -> Result<ReadOutput, ReadFailure>;
}

pub struct OwnedValidationRequest {
    pub resource: OwnedResourceReceipt,
    pub validator_id: &'static str,
    pub bounds: ReadBounds,
}

/// Implementations enforce captured scope/root/owner grants, no-follow, exclusive create,
/// bounded reads, and partial-effect receipts. Merely constructing OwnedPath grants no I/O.
pub trait OwnedIo {
    fn create_exclusive(
        &mut self,
        request: &OwnedResourceRequest,
    ) -> Result<OwnedResourceReceipt, PartialFailure>;
    fn read_bound_session(
        &mut self,
        binding: &ResumeBinding,
        bounds: ReadBounds,
    ) -> Result<ReadOutput, ReadFailure>;
    fn validate_configuration(
        &mut self,
        request: &OwnedValidationRequest,
    ) -> Result<ReadOutput, ReadFailure>;
}

pub struct ScopedSessionEvidence {
    pub scope: EvidenceScope,
    pub provider: ProviderId,
    pub native: NativeIdentity,
    pub cwd: CwdIdentity,
    pub origin: CaptureOrigin,
    pub evidence_kind: EvidenceKind,
    pub evidence_sha256: Digest,
    pub record: Vec<u8>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NativeRecordKind {
    Surface,
    Protocol,
    Session,
}

pub struct NativeRecord {
    pub scope: EvidenceScope,
    pub kind: NativeRecordKind,
    pub bytes: Vec<u8>,
}

// Exactly seven provider behavior interfaces. None has a default implementation.
pub trait CatalogHook: Send + Sync {
    fn discover(
        &self,
        request: &CatalogRequest,
        host: &mut dyn BoundedReadHost,
    ) -> Result<CatalogObservation, ReadFailure>;
}

pub trait PlanHook: Send + Sync {
    fn plan(&self, request: &ResolvedLaunch) -> Result<LaunchPlan, ContractError>;
}

pub trait MaterializeHook: Send + Sync {
    fn materialize(
        &self,
        requests: &[OwnedResourceRequest],
        io: &mut dyn OwnedIo,
    ) -> Result<MaterializeReceipt, PartialFailure>;
}

pub trait SessionHook: Send + Sync {
    fn bind(&self, evidence: &ScopedSessionEvidence) -> Result<ResumeBinding, ContractError>;
}

pub trait SemanticReader: Send + Sync {
    fn read(&self, record: &NativeRecord) -> Probe<SemanticEvidence>;
}

pub trait InteractionHook: Send + Sync {
    fn interpret(&self, frame: &CaptureFrame) -> InteractionObservation;
}

pub trait ForkHook: Send + Sync {
    fn stage(
        &self,
        request: &ResolvedFork,
        io: &mut dyn OwnedIo,
    ) -> Result<NativeForkPlan, PartialFailure>;
}

/// Behavior bindings are separate from the fifteen-facet data descriptor.
/// Mandatory hooks are checked for each operation before any hook is invoked.
pub struct ProviderHooks<'a> {
    pub catalog: HookBinding<&'a dyn CatalogHook>,
    pub plan: HookBinding<&'a dyn PlanHook>,
    pub materialize: HookBinding<&'a dyn MaterializeHook>,
    pub session: HookBinding<&'a dyn SessionHook>,
    pub semantic: HookBinding<&'a dyn SemanticReader>,
    pub interaction: HookBinding<&'a dyn InteractionHook>,
    pub fork: HookBinding<&'a dyn ForkHook>,
}
