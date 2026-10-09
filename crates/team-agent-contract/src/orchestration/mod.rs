//! Shared lifecycle and durable messaging. No provider-specific queue or legacy runtime.

pub mod lifecycle;
pub mod mcp;
pub mod physical;
pub mod store;
pub mod supervisor;

use crate::contract::types::ContractError;

/// Stable errors intentionally exclude SQL values, prompt/config bytes and native captures.
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
    NeedsRecovery,
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
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
