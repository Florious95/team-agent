//! Provider-neutral adaptation contracts, isolated from the legacy Team Agent runtime.
//!
//! `contract` is pure; `host` contains the scoped OS/tmux boundary and `runtime`
//! owns physical delivery, journaling and evidence collection. There is no native
//! provider implementation, supervisor, store or CLI in this package yet.

pub mod contract;
pub mod host;
pub mod runtime;
