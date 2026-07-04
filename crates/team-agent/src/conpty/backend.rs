//! `ConPtyBackend` — implements the existing `Transport` trait using the
//! named-pipe protocol in `super::protocol`.
//!
//! ## Design boundary (design.md §Transport Boundary:97-124)
//!
//! Every trait method has one of three flavours:
//!
//! - **Required**: forwards to the shim via `PipeClient::request`.
//! - **Exempt typed no-op**: e.g. `configure_adaptive_pane_title` — the
//!   shim has no pane title so the method returns `Ok(())` locally.
//! - **Typed unsupported**: `attach_session` returns
//!   `AttachOutcome::Unsupported { reason }` because ConPTY has no attach
//!   concept (design.md:123-124; there is no `tmux attach`).
//!
//! ## Phase 1a scope
//!
//! This file lands the Transport impl **shape** + honest degradation
//! when no pipe client is connected. The pipe transport itself
//! (Windows named-pipe socket + real shim binary) lives in Phase 1b.
//!
//! When a `PipeClient` is not available (Mac dev host, tests, or a live
//! Windows host whose shim died), every trait method returns
//! `TransportError::MuxUnavailable { backend: BackendKind::ConPty, detail }`
//! — the same honest-fail path the coordinator uses today when a tmux
//! server is missing. This satisfies MUST-NOT-13 (do not silently
//! success) + CR C-3 (stale shim → honest degradation).
//!
//! ## CR anchors
//!
//! - **C-1 pipe_token**: only lives in `PipeClient` in-memory state.
//!   `ConPtyBackend` is Send+Sync; the token field is behind
//!   `parking_lot::Mutex` and is NEVER handed out via a getter that
//!   could feed serialisation.
//! - **C-2 kill_server**: this backend's `kill_server` closes the
//!   pipe + tells the shim to `Shutdown`. Semantics are per-workspace
//!   (each `ConPtyBackend` owns exactly one team shim), so callers who
//!   invoke `kill_server` in a `KillDecision::KillWholeServer` branch
//!   still get the same "tear down this workspace's transport" effect
//!   they get from tmux.
//! - **C-3 stale event**: when `state.shim_pid` disagrees with what
//!   `hello` returns, the backend emits `conpty_transport.shim_pid_stale`
//!   BEFORE returning `MuxUnavailable`. Handled in
//!   `PipeClient::ensure_hello` (Phase 1b).
//! - **C-5 pipe_token_mismatch**: the `PipeClient::request` inspection
//!   of the response's `ProtocolError::PipeTokenMismatch` translates
//!   directly to `TransportError::MuxUnavailable`. The client MUST NOT
//!   silently rotate its own token.

use std::collections::BTreeMap;
use std::path::Path;

use crate::model::enums::PaneLiveness;
use crate::transport::{
    AttachOutcome, BackendKind, CaptureRange, CapturedText, InjectPayload, InjectReport, Key,
    PaneField, PaneId, PaneInfo, SessionName, SetEnvOutcome, SpawnResult, Target, Transport,
    TransportError, WindowName,
};

/// The `Transport` implementation for the named ConPTY backend.
///
/// The pipe client is optional so this struct can be constructed on
/// non-Windows hosts and in tests without a live shim; all trait methods
/// return `MuxUnavailable` in that case (honest degradation, not silent
/// success).
pub struct ConPtyBackend {
    /// Canonical workspace-hash + team-key key — used only for the
    /// `MuxUnavailable` diagnostic detail so operators see which shim
    /// was expected.
    workspace_hash: String,
    team_key: String,
    /// Present when a `PipeClient` was successfully connected (Phase 1b).
    /// In Phase 1a this is always `None`; every request degrades to
    /// `MuxUnavailable` with a stable `no_pipe_client` diagnostic.
    #[allow(dead_code)]
    pipe_client: Option<Box<dyn PipeClientTrait>>,
}

impl ConPtyBackend {
    /// New backend for `(workspace_hash, team_key)`. The pipe client is
    /// left unset; callers wire it in Phase 1b via `with_pipe_client`.
    pub fn new(workspace_hash: impl Into<String>, team_key: impl Into<String>) -> Self {
        Self {
            workspace_hash: workspace_hash.into(),
            team_key: team_key.into(),
            pipe_client: None,
        }
    }

    /// Build a `MuxUnavailable` error explaining that no pipe client is
    /// wired for this backend. The detail string carries the
    /// `(workspace_hash, team_key)` so operators can correlate with
    /// state.json's `transport.pipe_name`.
    fn mux_unavailable(&self, op: &str) -> TransportError {
        TransportError::MuxUnavailable {
            backend: BackendKind::ConPty,
            detail: format!(
                "no_pipe_client (op={op}, workspace_hash={ws}, team_key={team}); \
                 Phase 1a has not yet connected a live shim — this is honest \
                 degradation, not silent success",
                ws = self.workspace_hash,
                team = self.team_key,
            ),
        }
    }
}

/// Object-safe pipe-client trait so `ConPtyBackend` can hold a boxed
/// client without depending on the concrete Windows named-pipe type.
/// Phase 1b implements this against a real `\\.\pipe\...` handle;
/// Phase 1a tests can implement it against an in-memory
/// `Cursor<Vec<u8>>` pair.
///
/// The `request` method returns the raw `Response`; the backend layer
/// maps errors into `TransportError`.
pub trait PipeClientTrait: Send + Sync {
    fn request(
        &self,
        req: &super::protocol::Request,
    ) -> Result<super::protocol::Response, TransportError>;
}

impl Transport for ConPtyBackend {
    fn kind(&self) -> BackendKind {
        BackendKind::ConPty
    }

    fn probes_real_tmux_socket_roots(&self) -> bool {
        // Design §Transport Boundary:102 — ConPTY exposes pipe name via
        // a separate diagnostic field, NOT this tmux method.
        false
    }

    fn tmux_endpoint(&self) -> Option<String> {
        // Same — pipe name is exposed elsewhere, not here.
        None
    }

    fn spawn_first(
        &self,
        _session: &SessionName,
        _window: &WindowName,
        _argv: &[String],
        _cwd: &Path,
        _env: &BTreeMap<String, String>,
    ) -> Result<SpawnResult, TransportError> {
        Err(self.mux_unavailable("spawn_first"))
    }

    fn spawn_into(
        &self,
        _session: &SessionName,
        _window: &WindowName,
        _argv: &[String],
        _cwd: &Path,
        _env: &BTreeMap<String, String>,
    ) -> Result<SpawnResult, TransportError> {
        Err(self.mux_unavailable("spawn_into"))
    }

    fn inject(
        &self,
        _target: &Target,
        _payload: &InjectPayload,
        _submit: Key,
        _bracketed: bool,
    ) -> Result<InjectReport, TransportError> {
        Err(self.mux_unavailable("inject"))
    }

    fn send_keys(&self, _target: &Target, _keys: &[Key]) -> Result<(), TransportError> {
        Err(self.mux_unavailable("send_keys"))
    }

    fn capture(
        &self,
        _target: &Target,
        _range: CaptureRange,
    ) -> Result<CapturedText, TransportError> {
        Err(self.mux_unavailable("capture"))
    }

    fn query(&self, _target: &Target, _field: PaneField) -> Result<Option<String>, TransportError> {
        Err(self.mux_unavailable("query"))
    }

    fn liveness(&self, _pane: &PaneId) -> Result<PaneLiveness, TransportError> {
        Err(self.mux_unavailable("liveness"))
    }

    fn list_targets(&self) -> Result<Vec<PaneInfo>, TransportError> {
        Err(self.mux_unavailable("list_targets"))
    }

    fn has_session(&self, _session: &SessionName) -> Result<bool, TransportError> {
        Err(self.mux_unavailable("has_session"))
    }

    fn list_windows(&self, _session: &SessionName) -> Result<Vec<WindowName>, TransportError> {
        Err(self.mux_unavailable("list_windows"))
    }

    fn set_session_env(
        &self,
        _session: &SessionName,
        _key: &str,
        _value: &str,
    ) -> Result<SetEnvOutcome, TransportError> {
        // Design §Transport Boundary:118 — ConPTY internalises env at
        // spawn time; post-spawn mutation is NOT supported. This is a
        // typed capability refusal, NOT an error, so it returns
        // `Ok(InternalizedAtSpawn)` even without a live pipe client.
        Ok(SetEnvOutcome::InternalizedAtSpawn)
    }

    fn kill_session(&self, _session: &SessionName) -> Result<(), TransportError> {
        Err(self.mux_unavailable("kill_session"))
    }

    fn kill_window(&self, _target: &Target) -> Result<(), TransportError> {
        Err(self.mux_unavailable("kill_window"))
    }

    fn attach_session(&self, _session: &SessionName) -> Result<AttachOutcome, TransportError> {
        // Design §Transport Boundary:123 + §Visibility Without tmux attach:374
        // — ConPTY has NO attach concept. Typed refusal, not error.
        // This is honest and matches the tmux-only method's semantics.
        Ok(AttachOutcome::Unsupported {
            reason: "ConPTY worker backend has no interactive attach; \
                     use `team-agent capture` or the optional \
                     `windows-view` (Phase 5) for scrollback."
                .to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_is_conpty() {
        let b = ConPtyBackend::new("wshash", "team-a");
        assert_eq!(b.kind(), BackendKind::ConPty);
    }

    #[test]
    fn tmux_specific_methods_return_none_or_false() {
        let b = ConPtyBackend::new("wshash", "team-a");
        // Design §Transport Boundary:102 — ConPTY MUST NOT lie by
        // reporting a fake tmux socket.
        assert_eq!(b.tmux_endpoint(), None);
        assert!(!b.probes_real_tmux_socket_roots());
    }

    #[test]
    fn attach_session_returns_typed_unsupported() {
        // Design §Transport Boundary:123 anchor. The refusal is
        // `AttachOutcome::Unsupported`, NOT a `TransportError`.
        let b = ConPtyBackend::new("wshash", "team-a");
        let out = b.attach_session(&SessionName::new("s")).unwrap();
        match out {
            AttachOutcome::Unsupported { reason } => {
                assert!(!reason.is_empty(), "reason must be non-empty");
            }
            other => panic!("expected Unsupported, got {other:?}"),
        }
    }

    #[test]
    fn set_session_env_returns_internalized_at_spawn() {
        // Design §Transport Boundary:118. Typed refusal, not error.
        let b = ConPtyBackend::new("wshash", "team-a");
        let out = b
            .set_session_env(&SessionName::new("s"), "KEY", "value")
            .unwrap();
        assert_eq!(out, SetEnvOutcome::InternalizedAtSpawn);
    }

    #[test]
    fn no_pipe_client_returns_honest_mux_unavailable_not_success() {
        // MUST-NOT-13 + CR C-3: with no pipe client wired we must NOT
        // silently pretend the spawn/inject/capture succeeded.
        let b = ConPtyBackend::new("wshash", "team-a");
        for op_name in [
            "spawn_first",
            "spawn_into",
            "inject",
            "capture",
            "list_targets",
            "kill_session",
        ] {
            let err = match op_name {
                "spawn_first" => b
                    .spawn_first(
                        &SessionName::new("s"),
                        &WindowName::new("w"),
                        &[],
                        Path::new("/tmp"),
                        &BTreeMap::new(),
                    )
                    .unwrap_err(),
                "spawn_into" => b
                    .spawn_into(
                        &SessionName::new("s"),
                        &WindowName::new("w"),
                        &[],
                        Path::new("/tmp"),
                        &BTreeMap::new(),
                    )
                    .unwrap_err(),
                "inject" => b
                    .inject(
                        &Target::Pane(PaneId::new("conpty:test")),
                        &InjectPayload::Empty,
                        Key::Enter,
                        false,
                    )
                    .unwrap_err(),
                "capture" => b
                    .capture(
                        &Target::Pane(PaneId::new("conpty:test")),
                        CaptureRange::Tail(80),
                    )
                    .unwrap_err(),
                "list_targets" => b.list_targets().unwrap_err(),
                "kill_session" => b.kill_session(&SessionName::new("s")).unwrap_err(),
                _ => unreachable!(),
            };
            match err {
                TransportError::MuxUnavailable { backend, detail } => {
                    assert_eq!(backend, BackendKind::ConPty);
                    assert!(
                        detail.contains("no_pipe_client"),
                        "detail must anchor on `no_pipe_client` diagnostic tag, got {detail}"
                    );
                    assert!(
                        detail.contains(op_name),
                        "detail must include op={op_name}, got {detail}"
                    );
                }
                other => panic!("expected MuxUnavailable, got {other:?}"),
            }
        }
    }
}
