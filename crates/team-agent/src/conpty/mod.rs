//! 0.5.x Windows-native transport Phase 1: ConPTY backend.
//!
//! Wire protocol + portable shim state machine live in the
//! `conpty-transport` crate (portable, no team-agent runtime deps —
//! consumed directly by the Windows `windows-shim` binary). This
//! module wires that portable machinery into team-agent's `Transport`
//! trait via `ConPtyBackend`.
//!
//! Truth sources (READ-ONLY):
//! - Design:    `.team/artifacts/0.5.x-windows-native-transport-design.md`
//! - CR verdict: `.team/artifacts/0.5.x-windows-transport-cr-verdict.md`
//!
//! CR anchors:
//! - **C-1 pipe_token**: only lives in `ConPtyBackend.pipe_token`
//!   (Mutex) and the shim's in-memory state; never persisted. Grep
//!   guard test (`conpty_pipe_token_no_persist_guard.rs`) locks this.
//! - **C-2 kill_server**: forwards to `Op::Shutdown` per-team, not
//!   global. See CR pre-check in the gate report.
//! - **C-3 stale event**: emitted by the backend when `state.shim_pid`
//!   disagrees with the shim's `hello` response.
//! - **C-5 pipe_token_mismatch**: `ProtocolError::PipeTokenMismatch`
//!   maps to `TransportError::MuxUnavailable`, NEVER silent retry.

pub mod backend;

pub use backend::{ConPtyBackend, PipeClientAdapter, PipeClientTrait};
// Re-export the portable protocol + shim types from the standalone
// crate so downstream code can `use team_agent::conpty::{Shim,
// FakePaneRuntime, ...}` without a second dependency.
pub use conpty_transport::{
    protocol, shim, FakePaneRuntime, LocalShimClient, PaneRuntime, PipeClient, Shim,
};
