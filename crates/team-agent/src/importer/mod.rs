//! Explicit offline legacy-data tooling. Runtime commands must not use this
//! module to initialize, migrate, or query framework authority on open.
#[cfg(unix)]
pub mod contract;
pub mod legacy_schema;
pub mod migration;
