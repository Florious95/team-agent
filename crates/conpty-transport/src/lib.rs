//! 0.5.x Windows-native transport: portable wire protocol + shim
//! state machine.
//!
//! This crate carries ONLY the portable pieces (no team-agent runtime,
//! no Unix-specific code) so it can be a direct dependency of the
//! Windows-only `windows-shim` binary without dragging in the whole
//! coordinator lib. The team-agent crate wraps this into
//! `team_agent::conpty::ConPtyBackend` which implements the
//! `Transport` trait against a boxed `PipeClient`.
//!
//! Truth sources (READ-ONLY):
//! - Design:    `.team/artifacts/0.5.x-windows-native-transport-design.md`
//! - CR verdict: `.team/artifacts/0.5.x-windows-transport-cr-verdict.md`

pub mod named_pipe_client;
pub mod protocol;
pub mod shim;

pub use protocol::{
    read_frame, read_request, read_response, write_frame, write_request, write_response,
    CaptureRequest, CaptureResult, HelloResult, InjectRequest, Op, ProtocolError, Request,
    Response, SpawnRequest, SpawnResult, MAX_FRAME_BYTES, PROTOCOL_SCHEMA,
};
pub use shim::{FakePaneRuntime, LocalShimClient, PaneEntry, PaneRuntime, PipeClient, Shim};
pub use named_pipe_client::pipe_name_for;
#[cfg(windows)]
pub use named_pipe_client::NamedPipeClient;
