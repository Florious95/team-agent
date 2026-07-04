//! 0.5.x Windows-native transport Phase 1: ConPTY backend + named-pipe protocol.
//!
//! Truth sources (READ-ONLY):
//! - Design:    `.team/artifacts/0.5.x-windows-native-transport-design.md`
//!              (§Transport Boundary + §Named Pipe Control + §Phase 1)
//! - CR verdict: `.team/artifacts/0.5.x-windows-transport-cr-verdict.md`
//!              (7 constraints, C-1/C-2/C-3/C-5 anchor into this module)
//!
//! Module layout:
//! - `protocol`: the wire protocol between the backend and the windows-shim.
//!   Length-prefixed UTF-8 JSON frames, 15 operations, portable pure logic
//!   (all frame encoding/decoding + request/response types are cross-OS
//!   testable on macOS / Linux).
//! - `backend`: the `Transport` implementation. Compiles on all hosts; the
//!   pipe-client wiring is `cfg(windows)` real, `cfg(not(windows))` stub
//!   that returns typed `MuxUnavailable` so tests can drive the error
//!   path without needing a live shim.
//!
//! CR anchors:
//! - **C-1 pipe_token**: never persisted. Only lives in the shim process
//!   memory + the caller's in-memory `PipeClient` after `hello`. The
//!   `state.rs` schema does NOT include a `pipe_token` field. A source
//!   grep guard test locks this invariant.
//! - **C-2 kill_server**: the ConPTY backend's `kill_server` closes the
//!   shim connection and terminates that shim (per team, not global).
//!   The existing tmux caller in `cli/mod.rs` invokes it only inside a
//!   `KillDecision::KillWholeServer` branch that already scopes to the
//!   caller's transport instance, so per-team scope is preserved.
//! - **C-3 stale event**: when state's `shim_pid` / `pipe_name` do not
//!   match the currently-connected shim, an event
//!   `conpty_transport.shim_pid_stale` (or `pipe_name_stale`) is emitted
//!   BEFORE returning a `MuxUnavailable` degradation.
//! - **C-5 pipe_token_mismatch**: shim returns
//!   `ProtocolError::PipeTokenMismatch`; caller MUST NOT silently retry
//!   with a rotated token — it must surface `MuxUnavailable` so the
//!   coordinator escalates to the honest `transport_unavailable` path.

pub mod protocol;
pub mod backend;
pub mod shim;

pub use backend::{ConPtyBackend, PipeClientTrait};
pub use shim::{FakePaneRuntime, LocalShimPipeClient, PaneRuntime, Shim};
