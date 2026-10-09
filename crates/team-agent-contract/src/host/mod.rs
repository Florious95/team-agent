//! Scoped host boundary. No legacy transport/provider dispatch and no default tmux endpoint.

pub mod clock;
pub mod command;
pub mod files;
pub mod materialize;
pub mod process;
pub mod shell;
pub mod tmux;
pub mod transport;

use std::fmt;
use std::io;

use crate::contract::types::Digest;
use sha2::{Digest as _, Sha256};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostErrorKind {
    Invalid,
    Unsupported,
    Conflict,
    Ownership,
    Unknown,
    Deadline,
    Io(io::ErrorKind),
    Command,
}

/// Errors intentionally omit argv/env/native capture contents.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostError {
    pub operation: &'static str,
    pub kind: HostErrorKind,
}

impl HostError {
    pub fn new(operation: &'static str, kind: HostErrorKind) -> Self {
        Self { operation, kind }
    }
    pub fn io(operation: &'static str, error: io::Error) -> Self {
        Self::new(operation, HostErrorKind::Io(error.kind()))
    }
}
impl fmt::Display for HostError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {:?}", self.operation, self.kind)
    }
}
impl std::error::Error for HostError {}

pub fn digest(bytes: &[u8]) -> Digest {
    Digest(Sha256::digest(bytes).into())
}

pub fn digest_hex(value: Digest) -> String {
    value.0.iter().map(|byte| format!("{byte:02x}")).collect()
}
