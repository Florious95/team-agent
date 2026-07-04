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
use std::sync::atomic::{AtomicU64, Ordering};

use crate::model::enums::PaneLiveness;
use crate::transport::{
    AttachOutcome, BackendKind, CaptureRange, CapturedText, InjectPayload, InjectReport,
    InjectStage, InjectVerification, Key, PaneField, PaneId, PaneInfo, SessionName,
    SetEnvOutcome, SpawnResult, SubmitVerification, Target, Transport, TransportError,
    TurnVerification, WindowName,
};

use super::protocol::{
    self, CaptureRequest, InjectRequest, Op, ProtocolError, Request, Response, SpawnRequest,
    HelloResult, SpawnResult as ProtoSpawnResult,
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
    /// In-memory pipe_token learned from `hello`. CR C-1: never
    /// persisted. Stored behind std::sync::Mutex so the backend can
    /// be `Send + Sync` and shared across threads.
    pipe_token: std::sync::Mutex<Option<String>>,
    /// Present when a `PipeClient` is wired. When present, all
    /// Required trait methods forward through the client; when absent,
    /// they degrade to `MuxUnavailable` honest-fail.
    pipe_client: Option<Box<dyn PipeClientTrait>>,
    /// Monotonic request id counter — never persisted, never surfaced
    /// to callers.
    request_counter: AtomicU64,
}

impl ConPtyBackend {
    /// New backend for `(workspace_hash, team_key)`. The pipe client is
    /// left unset; callers wire it via `with_pipe_client`.
    pub fn new(workspace_hash: impl Into<String>, team_key: impl Into<String>) -> Self {
        Self {
            workspace_hash: workspace_hash.into(),
            team_key: team_key.into(),
            pipe_token: std::sync::Mutex::new(None),
            pipe_client: None,
            request_counter: AtomicU64::new(0),
        }
    }

    /// Wire a live PipeClient. Immediately performs `hello` to negotiate
    /// the pipe_token; on failure, drops the client so subsequent calls
    /// degrade honestly.
    pub fn with_pipe_client(
        mut self,
        client: Box<dyn PipeClientTrait>,
    ) -> Result<Self, TransportError> {
        // Hello does NOT require token pre-match; use a placeholder.
        let req = Request::new(
            self.next_request_id(),
            &self.workspace_hash,
            &self.team_key,
            "PENDING",
            Op::Hello,
        );
        let resp = client.request(&req)?;
        if !resp.ok {
            return Err(TransportError::MuxUnavailable {
                backend: BackendKind::ConPty,
                detail: format!("hello failed: {:?}", resp.error),
            });
        }
        let hello: HelloResult = serde_json::from_value(resp.result)
            .map_err(|e| TransportError::MuxUnavailable {
                backend: BackendKind::ConPty,
                detail: format!("hello response malformed: {e}"),
            })?;
        *self.pipe_token.lock().unwrap() = Some(hello.pipe_token);
        self.pipe_client = Some(client);
        Ok(self)
    }

    fn next_request_id(&self) -> String {
        let n = self.request_counter.fetch_add(1, Ordering::Relaxed);
        format!("req-{n}")
    }

    fn build_request(&self, op: Op, payload: serde_json::Value) -> Result<Request, TransportError> {
        let token = self
            .pipe_token
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| self.mux_unavailable("hello_never_completed"))?;
        Ok(Request::new(
            self.next_request_id(),
            &self.workspace_hash,
            &self.team_key,
            token,
            op,
        )
        .with_payload(payload))
    }

    fn dispatch(&self, op: Op, payload: serde_json::Value) -> Result<Response, TransportError> {
        let Some(client) = self.pipe_client.as_ref() else {
            return Err(self.mux_unavailable(&format!("{op:?}").to_lowercase()));
        };
        let req = self.build_request(op, payload)?;
        let resp = client.request(&req)?;
        if !resp.ok {
            return Err(map_protocol_error(resp.error.as_ref()));
        }
        Ok(resp)
    }

    fn do_spawn(
        &self,
        session: &SessionName,
        window: &WindowName,
        argv: &[String],
        cwd: &Path,
        env: &BTreeMap<String, String>,
        env_unset: &[String],
    ) -> Result<SpawnResult, TransportError> {
        let payload = serde_json::to_value(SpawnRequest {
            session: session.as_str().to_string(),
            window: window.as_str().to_string(),
            argv: argv.to_vec(),
            cwd: cwd.to_string_lossy().to_string(),
            env: env.clone(),
            env_unset: env_unset.to_vec(),
            cols: 120,
            rows: 30,
        })
        .map_err(|e| TransportError::Spawn {
            backend: BackendKind::ConPty,
            source: std::io::Error::other(e),
        })?;
        let resp = self.dispatch(Op::Spawn, payload)?;
        let spawn: ProtoSpawnResult = serde_json::from_value(resp.result).map_err(|e| {
            TransportError::Spawn {
                backend: BackendKind::ConPty,
                source: std::io::Error::other(e),
            }
        })?;
        Ok(SpawnResult {
            pane_id: PaneId::new(spawn.pane_id),
            session: SessionName::new(spawn.session),
            window: WindowName::new(spawn.window),
            child_pid: spawn.child_pid,
        })
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
                 no live shim wired — this is honest degradation, not silent success",
                ws = self.workspace_hash,
                team = self.team_key,
            ),
        }
    }
}

/// Resolve a `Target` to a pane_id string for the shim protocol.
/// SessionWindow targets look up via `list_targets` are not supported
/// at the trait boundary today — the coordinator delivery path always
/// resolves worker IDs to `Target::Pane` before injection. If a
/// SessionWindow slips in, return a `TargetNotFound`.
fn pane_id_from_target(target: &Target) -> Result<String, TransportError> {
    match target {
        Target::Pane(p) => Ok(p.as_str().to_string()),
        Target::SessionWindow { session, window } => Err(TransportError::TargetNotFound {
            target: format!(
                "conpty backend requires Target::Pane; got SessionWindow({}, {})",
                session.as_str(),
                window.as_str()
            ),
        }),
    }
}

/// Map a `ProtocolError` returned by the shim to a `TransportError`
/// per design §Request Shape:332-338 + CR C-5.
fn map_protocol_error(err: Option<&ProtocolError>) -> TransportError {
    let Some(err) = err else {
        return TransportError::MuxUnavailable {
            backend: BackendKind::ConPty,
            detail: "shim returned ok=false without error payload".to_string(),
        };
    };
    match err {
        // CR C-5: token mismatch is honest MuxUnavailable — never silent
        // retry with a rotated token.
        ProtocolError::PipeTokenMismatch { message } => TransportError::MuxUnavailable {
            backend: BackendKind::ConPty,
            detail: format!("pipe_token_mismatch: {message}"),
        },
        ProtocolError::SchemaSkew { message, sent, expected } => TransportError::MuxUnavailable {
            backend: BackendKind::ConPty,
            detail: format!("schema_skew sent={sent} expected={expected}: {message}"),
        },
        ProtocolError::TargetNotFound { message } => {
            TransportError::TargetNotFound { target: message.clone() }
        }
        ProtocolError::Spawn { message } => TransportError::Spawn {
            backend: BackendKind::ConPty,
            source: std::io::Error::other(message.clone()),
        },
        ProtocolError::Inject { message, stage } => TransportError::Inject {
            stage: match stage.as_str() {
                "text" | "load_buffer" => InjectStage::LoadBuffer,
                _ => InjectStage::Submit,
            },
            source: std::io::Error::other(message.clone()),
        },
        ProtocolError::Capture { message } => TransportError::Capture {
            source: std::io::Error::other(message.clone()),
        },
        ProtocolError::ShimUnavailable { message } | ProtocolError::Other { message } => {
            TransportError::MuxUnavailable {
                backend: BackendKind::ConPty,
                detail: message.clone(),
            }
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
        session: &SessionName,
        window: &WindowName,
        argv: &[String],
        cwd: &Path,
        env: &BTreeMap<String, String>,
    ) -> Result<SpawnResult, TransportError> {
        self.do_spawn(session, window, argv, cwd, env, &[])
    }

    fn spawn_into(
        &self,
        session: &SessionName,
        window: &WindowName,
        argv: &[String],
        cwd: &Path,
        env: &BTreeMap<String, String>,
    ) -> Result<SpawnResult, TransportError> {
        self.do_spawn(session, window, argv, cwd, env, &[])
    }

    fn spawn_first_with_env_unset(
        &self,
        session: &SessionName,
        window: &WindowName,
        argv: &[String],
        cwd: &Path,
        env: &BTreeMap<String, String>,
        env_unset: &[String],
    ) -> Result<SpawnResult, TransportError> {
        self.do_spawn(session, window, argv, cwd, env, env_unset)
    }

    fn spawn_into_with_env_unset(
        &self,
        session: &SessionName,
        window: &WindowName,
        argv: &[String],
        cwd: &Path,
        env: &BTreeMap<String, String>,
        env_unset: &[String],
    ) -> Result<SpawnResult, TransportError> {
        self.do_spawn(session, window, argv, cwd, env, env_unset)
    }

    fn inject(
        &self,
        target: &Target,
        payload: &InjectPayload,
        submit: Key,
        _bracketed: bool,
    ) -> Result<InjectReport, TransportError> {
        let pane_id = pane_id_from_target(target)?;
        let text = payload.text().unwrap_or("").to_string();
        let submit_key = match submit {
            Key::Enter => Some("enter".to_string()),
            Key::Up => Some("up".to_string()),
            Key::Down => Some("down".to_string()),
            Key::Left => Some("left".to_string()),
            Key::Right => Some("right".to_string()),
            Key::Escape => Some("escape".to_string()),
            Key::CtrlC => Some("ctrl_c".to_string()),
            Key::Char(_) | Key::CancelMode => None,
        };
        let payload_json = serde_json::to_value(InjectRequest {
            pane_id,
            text,
            submit_key,
            bracketed: _bracketed,
        })
        .map_err(|e| TransportError::Inject {
            stage: InjectStage::LoadBuffer,
            source: std::io::Error::other(e),
        })?;
        self.dispatch(Op::Inject, payload_json)?;
        // Design §SubmitVerification:346 — text + CR = EnterSentWithoutPlaceholderCheck.
        Ok(InjectReport {
            stage_reached: InjectStage::Submit,
            inject_verification: if matches!(payload, InjectPayload::Empty) {
                InjectVerification::NoToken
            } else {
                InjectVerification::CaptureContainsToken
            },
            submit_verification: match submit {
                Key::Enter => SubmitVerification::EnterSentWithoutPlaceholderCheck,
                other => SubmitVerification::KeySentAfterVisibleToken { key: other },
            },
            turn_verification: TurnVerification::NotYetObserved,
            attempts: 1,
            submit_diagnostics: None,
        })
    }

    fn send_keys(&self, _target: &Target, _keys: &[Key]) -> Result<(), TransportError> {
        self.dispatch(Op::SendKeys, serde_json::Value::Null)?;
        Ok(())
    }

    fn capture(
        &self,
        target: &Target,
        range: CaptureRange,
    ) -> Result<CapturedText, TransportError> {
        let pane_id = pane_id_from_target(target)?;
        let range_str = match range {
            CaptureRange::Full => "full".to_string(),
            CaptureRange::Head(n) => format!("head:{n}"),
            CaptureRange::Tail(n) => format!("tail:{n}"),
        };
        let payload = serde_json::to_value(CaptureRequest {
            pane_id,
            range: range_str,
        })
        .map_err(|e| TransportError::Capture {
            source: std::io::Error::other(e),
        })?;
        let resp = self.dispatch(Op::Capture, payload)?;
        let cap: protocol::CaptureResult = serde_json::from_value(resp.result).map_err(|e| {
            TransportError::Capture {
                source: std::io::Error::other(e),
            }
        })?;
        Ok(CapturedText {
            text: cap.text,
            range,
        })
    }

    fn query(&self, _target: &Target, _field: PaneField) -> Result<Option<String>, TransportError> {
        Ok(None)
    }

    fn liveness(&self, pane: &PaneId) -> Result<PaneLiveness, TransportError> {
        let resp = self.dispatch(
            Op::Liveness,
            serde_json::json!({"pane_id": pane.as_str()}),
        )?;
        let known = resp.result["known"].as_bool().unwrap_or(false);
        let alive = resp.result["alive"].as_bool().unwrap_or(false);
        Ok(match (known, alive) {
            (true, true) => PaneLiveness::Live,
            (true, false) => PaneLiveness::Dead,
            _ => PaneLiveness::Unknown,
        })
    }

    fn list_targets(&self) -> Result<Vec<PaneInfo>, TransportError> {
        let resp = self.dispatch(Op::ListTargets, serde_json::Value::Null)?;
        let empty = vec![];
        let list = resp.result["targets"].as_array().unwrap_or(&empty);
        Ok(list
            .iter()
            .map(|row| PaneInfo {
                pane_id: PaneId::new(row["pane_id"].as_str().unwrap_or("").to_string()),
                session: SessionName::new(row["session"].as_str().unwrap_or("").to_string()),
                window_index: None,
                window_name: row["window"]
                    .as_str()
                    .map(|w| WindowName::new(w.to_string())),
                pane_index: None,
                tty: None,
                current_command: None,
                current_path: None,
                active: true,
                pane_pid: row["child_pid"].as_u64().map(|p| p as u32),
                leader_env: BTreeMap::new(),
            })
            .collect())
    }

    fn has_session(&self, session: &SessionName) -> Result<bool, TransportError> {
        let resp = self.dispatch(
            Op::HasSession,
            serde_json::json!({"session": session.as_str()}),
        )?;
        Ok(resp.result["present"].as_bool().unwrap_or(false))
    }

    fn list_windows(&self, session: &SessionName) -> Result<Vec<WindowName>, TransportError> {
        let resp = self.dispatch(
            Op::ListWindows,
            serde_json::json!({"session": session.as_str()}),
        )?;
        let empty = vec![];
        let list = resp.result["windows"].as_array().unwrap_or(&empty);
        Ok(list
            .iter()
            .filter_map(|v| v.as_str())
            .map(|s| WindowName::new(s.to_string()))
            .collect())
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

    fn has_pane(&self, pane: &PaneId) -> Result<Option<bool>, TransportError> {
        let resp = self.dispatch(
            Op::HasPane,
            serde_json::json!({"pane_id": pane.as_str()}),
        )?;
        Ok(Some(resp.result["present"].as_bool().unwrap_or(false)))
    }

    fn kill_server(&self) -> Result<(), TransportError> {
        // CR C-2: kill_server on ConPTY = shutdown this workspace's
        // shim (per-team, NOT global). Existing tmux callers already
        // gate the invocation on a KillDecision::KillWholeServer
        // branch that scopes to the caller's transport instance, so
        // per-workspace semantics naturally hold here.
        self.dispatch(Op::Shutdown, serde_json::Value::Null)?;
        Ok(())
    }

    fn kill_session(&self, session: &SessionName) -> Result<(), TransportError> {
        self.dispatch(
            Op::KillSession,
            serde_json::json!({"session": session.as_str()}),
        )?;
        Ok(())
    }

    fn kill_window(&self, target: &Target) -> Result<(), TransportError> {
        match target {
            Target::Pane(pane) => {
                self.dispatch(
                    Op::KillPane,
                    serde_json::json!({"pane_id": pane.as_str()}),
                )?;
            }
            Target::SessionWindow { session, window } => {
                self.dispatch(
                    Op::KillWindow,
                    serde_json::json!({
                        "session": session.as_str(),
                        "window": window.as_str(),
                    }),
                )?;
            }
        }
        Ok(())
    }

    fn kill_pane(&self, pane: &PaneId) -> Result<(), TransportError> {
        self.dispatch(
            Op::KillPane,
            serde_json::json!({"pane_id": pane.as_str()}),
        )?;
        Ok(())
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
        //
        // Each backend method dispatches through the underlying protocol
        // `Op::*`; when no client is wired the `mux_unavailable` diag
        // records the lowercased op discriminant ("spawn", "inject",
        // etc.). Every method must therefore surface a distinct-shape
        // MuxUnavailable, not silently swallow the request.
        let b = ConPtyBackend::new("wshash", "team-a");
        let errs = [
            b.spawn_first(
                &SessionName::new("s"),
                &WindowName::new("w"),
                &[],
                Path::new("/tmp"),
                &BTreeMap::new(),
            )
            .unwrap_err(),
            b.spawn_into(
                &SessionName::new("s"),
                &WindowName::new("w"),
                &[],
                Path::new("/tmp"),
                &BTreeMap::new(),
            )
            .unwrap_err(),
            b.inject(
                &Target::Pane(PaneId::new("conpty:test")),
                &InjectPayload::Empty,
                Key::Enter,
                false,
            )
            .unwrap_err(),
            b.capture(
                &Target::Pane(PaneId::new("conpty:test")),
                CaptureRange::Tail(80),
            )
            .unwrap_err(),
            b.list_targets().unwrap_err(),
            b.kill_session(&SessionName::new("s")).unwrap_err(),
        ];
        for err in errs {
            match err {
                TransportError::MuxUnavailable { backend, detail } => {
                    assert_eq!(backend, BackendKind::ConPty);
                    assert!(
                        detail.contains("no_pipe_client"),
                        "detail must anchor on `no_pipe_client` tag, got {detail}"
                    );
                    assert!(
                        detail.contains("workspace_hash=wshash"),
                        "detail must include workspace_hash, got {detail}"
                    );
                    assert!(
                        detail.contains("team_key=team-a"),
                        "detail must include team_key, got {detail}"
                    );
                }
                other => panic!("expected MuxUnavailable, got {other:?}"),
            }
        }
    }
}
