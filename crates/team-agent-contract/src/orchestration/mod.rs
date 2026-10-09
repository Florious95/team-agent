//! Shared lifecycle and durable messaging. No provider-specific queue or legacy runtime.

pub mod forward;
pub mod lifecycle;
pub mod mcp;
pub mod operator;
pub mod physical;
pub mod protocol;
pub mod store;
pub mod supervisor;

use crate::contract::types::ContractError;

/// Errors exclude SQL values, prompt/config bytes and native screen captures.
/// HostDiagnostic preserves only bounded, escaped tmux metadata diagnostics.
/// NativeControl renders execution facts and problem codes, never input/capture bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    Contract(ContractError),
    Fence,
    Conflict,
    Invalid(&'static str),
    Database,
    Corrupt,
    Io(std::io::ErrorKind),
    Host(&'static str),
    HostDiagnostic(crate::host::HostError),
    NativeControl(Box<crate::runtime::delivery::InjectionReport>),
    NeedsRecovery,
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::HostDiagnostic(error) => std::fmt::Display::fmt(error, f),
            Self::NativeControl(report) => write!(
                f,
                "native control {:?}: disposition={:?}; persistence={:?}; effect_floor={:?}; counts={:?}; problems={:?}",
                report.metadata.operation,
                report.disposition,
                report.persistence,
                report.effect_floor,
                report.counts,
                report.problems
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
impl From<rusqlite::Error> for Error {
    fn from(_: rusqlite::Error) -> Self {
        Self::Database
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
