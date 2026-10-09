//! Provider-neutral adaptation contracts, isolated from the legacy Team Agent runtime.
//!
//! `contract` is pure; `host` owns scoped OS/tmux effects; `runtime` owns physical
//! delivery and probes; `orchestration` owns durable lifecycle/MCP transactions.
//! Kiro-specific behavior is confined to `kiro`.

pub mod contract;
pub mod host;
pub mod kiro;
pub mod orchestration;
pub mod runtime;
