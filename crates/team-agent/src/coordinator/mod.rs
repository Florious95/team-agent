//! Legacy metadata inspection/cleanup and native host helpers.
//! M3 retired the coordinator actuator: no startup API, daemon loop, or tick engine remains.
//! Timed work belongs to `station::agenda`; native PTY hosting is not a coordination service.

#![allow(dead_code, unused_imports)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

// ── module-root REUSE imports ─────────────────────────────────────────────
// decompose 前这些 `use` 在 coordinator.rs 顶层,测试 `mod tests` 经 `use super::*`
// 继承它们。拆分后保留在 mod.rs(私有 use,子模块 tests 仍经 `use super::*` 解析)。
use crate::message_store::MessageStore;
use crate::model::enums::Provider;
use crate::provider::{ProviderAdapter, TurnId, TurnState};
use serde_json::Value;

pub mod backoff;
// 0.5.x Windows portability Batch 6 Option A: shim lifecycle
// manager. Windows-only per design §Shim Lifecycle (the shim binary
// exists only on Windows; Unix uses tmux). Cfg-gated at the module
// level so callers can `crate::coordinator::conpty_shim::...`
// uniformly on Windows and get a compile error (rather than a
// silent no-op) on Unix — Unix code paths never reach for the shim.
#[cfg(windows)]
pub mod conpty_shim;
pub mod health;
pub mod observation;
pub mod orphan;
pub mod runtime_detectors;
pub mod runtime_observation;
pub mod steps;
pub mod tick;
pub mod types;

// Preserve passive metadata/host interfaces during the staged migration.
pub use health::*;
pub use observation::*;
pub use orphan::*;
pub use runtime_detectors::*;
pub use runtime_observation::*;
pub use types::*;

#[cfg(test)]
mod tests;
