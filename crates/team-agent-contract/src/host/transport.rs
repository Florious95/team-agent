use std::path::PathBuf;
use std::time::Duration;

use super::clock::Clock;
use super::files::{DirectoryReceipt, NativeLane};
use super::process::ProcessStamp;
use super::HostError;
use crate::contract::delivery::{PasteMode, PhysicalKey, StepOutcome};
use crate::contract::probe::{EvidenceScope, ProcessAliveEvidence};
use crate::contract::session::{CwdIdentity, NativeSessionId};
use crate::contract::types::{Digest, EvidenceKind, InstanceIdentity, NativeIdentity, ProviderId};

/// Captured host facts for persistence. A value alone is NOT permission to act:
/// the concrete transport checks its owned receipt, directory and live pane on every call.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TargetReceipt {
    pub directory: DirectoryReceipt,
    pub endpoint: PathBuf,
    pub socket_device: u64,
    pub socket_inode: u64,
    /// Borrowed workspace servers are never owned socket-cleanup resources.
    /// Missing on historical receipts means the original private host mode.
    #[serde(default)]
    pub shared_endpoint: bool,
    /// Only a captured new shared session may acquire its root ownership marker.
    #[serde(default)]
    pub created_session: bool,
    pub session: String,
    pub window: String,
    pub pane: String,
    pub columns: u16,
    pub rows: u16,
    pub binding: String,
    pub process: ProcessStamp,
    pub provider: ProviderId,
    pub cwd: CwdIdentity,
    pub native: NativeIdentity,
    pub evidence_kind: EvidenceKind,
    pub candidate_sha256: Digest,
    pub native_session: Option<NativeSessionId>,
}

/// Durable partial spawn observation. A pane PID is not a native executable
/// receipt: live recovery additionally requires its captured birth and image.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SpawnObservation {
    pub directory: DirectoryReceipt,
    pub endpoint: PathBuf,
    pub socket_device: u64,
    pub socket_inode: u64,
    pub shared_endpoint: bool,
    pub created_session: bool,
    pub session: String,
    pub window: String,
    pub pane: String,
    pub pane_pid: u32,
    pub birth_identity: Option<String>,
    pub columns: u16,
    pub rows: u16,
    pub binding: String,
    pub executable: PathBuf,
    pub inputs: Vec<super::process::ExecutableInputStamp>,
    pub provider: ProviderId,
    pub cwd: CwdIdentity,
    pub native: NativeIdentity,
    pub evidence_kind: EvidenceKind,
    pub candidate_sha256: Digest,
}

impl SpawnObservation {
    pub(crate) fn target(&self, process: ProcessStamp) -> TargetReceipt {
        TargetReceipt {
            directory: self.directory.clone(), endpoint: self.endpoint.clone(),
            socket_device: self.socket_device, socket_inode: self.socket_inode,
            shared_endpoint: self.shared_endpoint, created_session: self.created_session,
            session: self.session.clone(), window: self.window.clone(), pane: self.pane.clone(),
            columns: self.columns, rows: self.rows, binding: self.binding.clone(), process,
            provider: self.provider.clone(), cwd: self.cwd.clone(), native: self.native.clone(),
            evidence_kind: self.evidence_kind, candidate_sha256: self.candidate_sha256,
            native_session: None,
        }
    }
}

impl TargetReceipt {
    pub fn identity(&self) -> &InstanceIdentity {
        &self.directory.owner
    }
    pub fn scope(
        &self,
        sequence: u64,
        now: Duration,
        valid_for: Duration,
        source: &str,
    ) -> EvidenceScope {
        EvidenceScope {
            identity: self.identity().clone(),
            endpoint: self.endpoint.to_string_lossy().into_owned(),
            pane: self.pane.clone(),
            binding: self.binding.clone(),
            session: self.native_session.clone(),
            sequence,
            observed_at: now,
            valid_for,
            source: source.to_string(),
        }
    }
}

pub trait InputLease {
    fn directory(&self) -> &DirectoryReceipt;
    fn owner(&self) -> &InstanceIdentity {
        &self.directory().owner
    }
}
impl InputLease for NativeLane {
    fn directory(&self) -> &DirectoryReceipt {
        NativeLane::directory(self)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PaneMode {
    Normal,
    Copy,
    View,
    Unknown(String),
}

/// Text is only passed to H6, not dumped to a journal or treated as a protocol reply.
pub struct HostCapture {
    pub scope: EvidenceScope,
    pub text: String,
    pub mode: PaneMode,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActionResult {
    pub outcome: StepOutcome,
    pub error: Option<HostError>,
}
impl ActionResult {
    pub fn confirmed() -> Self {
        Self {
            outcome: StepOutcome::Confirmed,
            error: None,
        }
    }
    pub fn refused(error: HostError) -> Self {
        Self {
            outcome: StepOutcome::NoEffect,
            error: Some(error),
        }
    }
    pub fn uncertain(error: HostError) -> Self {
        Self {
            outcome: StepOutcome::MayHaveOccurred,
            error: Some(error),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostPreparation {
    pub mode_before: PaneMode,
    pub control_commands_confirmed: u16,
    pub control_commands_uncertain: u16,
    pub error: Option<HostError>,
}

/// Framework transport port, not a provider hook. There is no default/legacy inject.
/// Only runtime::delivery's common executor calls paste/key for native interaction.
pub trait PhysicalTransport {
    fn acquire_lane(&mut self, target: &TargetReceipt) -> Result<Box<dyn InputLease>, HostError>;
    fn validate_target(
        &mut self,
        target: &TargetReceipt,
        clock: &dyn Clock,
        deadline: Duration,
    ) -> Result<ProcessAliveEvidence, HostError>;
    fn capture(
        &mut self,
        target: &TargetReceipt,
        clock: &dyn Clock,
        deadline: Duration,
        freshness: Duration,
    ) -> Result<HostCapture, HostError>;
    fn prepare_host_mode(
        &mut self,
        target: &TargetReceipt,
        clock: &dyn Clock,
        deadline: Duration,
    ) -> HostPreparation;
    fn stage_buffer(
        &mut self,
        target: &TargetReceipt,
        name: &str,
        bytes: &[u8],
        mode: PasteMode,
        clock: &dyn Clock,
        deadline: Duration,
    ) -> ActionResult;
    fn paste_buffer(
        &mut self,
        target: &TargetReceipt,
        name: &str,
        mode: PasteMode,
        clock: &dyn Clock,
        deadline: Duration,
    ) -> ActionResult;
    fn release_buffer(
        &mut self,
        target: &TargetReceipt,
        name: &str,
        clock: &dyn Clock,
        deadline: Duration,
    ) -> ActionResult;
    fn key(
        &mut self,
        target: &TargetReceipt,
        key: PhysicalKey,
        clock: &dyn Clock,
        deadline: Duration,
    ) -> ActionResult;
}
