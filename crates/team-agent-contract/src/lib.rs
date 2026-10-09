//! Provider-neutral adaptation contracts, isolated from the legacy Team Agent runtime.
//!
//! `contract` is pure. `orchestration` persists provider-neutral lifecycle and
//! messaging transactions; physical effects are delegated to scoped host ports.
//! No native provider, legacy runtime or executable is included.

pub mod contract;
pub mod kiro;
pub mod orchestration;
