//! Shared lifecycle and durable messaging. No provider-specific queue or legacy runtime.

pub mod forward;
pub mod lifecycle;
pub mod mcp;
pub mod operator;
pub mod physical;
pub mod protocol;
mod projection;
pub mod store;
pub mod supervisor;

use crate::contract::types::ContractError;

/// Errors exclude SQL values, prompt/config bytes and native screen captures.
/// HostDiagnostic preserves bounded metadata and identity-only probe diagnostics.
/// NativeControl/NativeClose render execution facts, never input/capture bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    Contract(ContractError),
    Fence,
    FenceAt { check: &'static str, entity: Option<String> },
    Conflict,
    Invalid(&'static str),
    Database,
    Corrupt,
    Io(std::io::ErrorKind),
    Host(&'static str),
    /// A read-only stop preflight refused before issuing any native mutation.
    StopNoEffect(String),
    HostDiagnostic(crate::host::HostError),
    /// The physical spawn receipt proves no process launch was attempted.
    SpawnNotStarted(crate::host::HostError),
    NativeControl(Box<crate::runtime::delivery::InjectionReport>),
    NativeClose(Box<crate::host::tmux::CloseReceipt>),
    NeedsRecovery,
}
impl Error {
    pub fn is_fence(&self) -> bool { matches!(self, Self::Fence | Self::FenceAt { .. }) }
    pub(crate) fn fence(check: &'static str) -> Self { Self::FenceAt { check, entity: None } }
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::HostDiagnostic(error) => std::fmt::Display::fmt(error, f),
            Self::NativeControl(report) => write!(
                f,
                "native control {:?}: disposition={:?}; persistence={:?}; effect_floor={:?}; counts={:?}; problems={:?}; control_paste={:?}",
                report.metadata.operation,
                report.disposition,
                report.persistence,
                report.effect_floor,
                report.counts,
                report.problems,
                report.control_paste
            ),
            Self::NativeClose(receipt) => write!(
                f,
                "owned native close incomplete: pane_outcome={:?}; pane_problem={:?}; native={:?}; socket_outcome={:?}; socket_problem={:?}; socket_preserved={}; shared_endpoint={}",
                receipt.pane_close.outcome,
                receipt.pane_close.error,
                receipt.native,
                receipt.socket_cleanup.outcome,
                receipt.socket_cleanup.error,
                receipt.socket_preserved,
                receipt.shared_endpoint
            ),
            _ => write!(f, "{self:?}"),
        }
    }
}
impl std::error::Error for Error {}
impl From<ContractError> for Error {
    fn from(value: ContractError) -> Self {
        Self::Contract(value)
    }
}
impl From<serde_json::Error> for Error {
    fn from(_: serde_json::Error) -> Self {
        Self::Corrupt
    }
}
impl From<std::io::Error> for Error {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value.kind())
    }
}
