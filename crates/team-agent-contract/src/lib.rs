//! Provider-neutral adaptation contracts, isolated from the legacy Team Agent runtime.
//!
//! This library performs no I/O and contains no native provider implementation.
//! Descriptors declare policies; hooks supply behavior through restricted ports;
//! observations and acceptance evidence remain separate from either declaration.

pub mod contract;
