//! Per-agent boundary into the contract runtime. Existing provider internals
//! stay on their path, including mixed teams; a seat never has two executors.
//!
//! Selection/preflight is read-only. Physical operations require a validated
//! contract role and its real framework/native binding, not a provider guess.
pub mod config;
pub mod prompt;
pub mod registry;
