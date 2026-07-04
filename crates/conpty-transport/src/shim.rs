//! Shim server logic: portable state machine that handles the 15
//! named-pipe operations against an in-memory pane registry.
//!
//! On Windows this state machine is driven by
//! `bin/windows_shim` — a real ConPTY spawn + stdout-drain thread per
//! pane. On macOS/Linux (dev host + CI Ubuntu job + tests) the same
//! state machine is driven by an in-memory `FakePaneRuntime` that
//! echoes injections back into a scrollback ring — enough to prove
//! Phase 1 acceptance bullet #3 (`send` returns delivered) and bullet
//! #4 (`capture` contains the token) without a live Windows host.
//!
//! ## Boundary
//!
//! - `Shim`: the router. Consumes `Request`, calls the matching
//!   op-handler, produces `Response`. Owns the pane registry + the
//!   in-memory pipe_token (CR C-1: never persisted).
//! - `PaneRuntime`: trait for the physical ConPTY handle. Real Windows
//!   impl (Phase 1b Windows delivery) lives in the `bin/windows_shim`
//!   binary; the `FakePaneRuntime` here is enough for Mac/CI tests.
//!
//! ## CR anchors
//!
//! - **C-1**: `pipe_token` lives in `Shim.pipe_token` (in-memory only).
//!   No `Serialize`/`Deserialize` derive on `Shim`; the field cannot
//!   accidentally end up in state.json.
//! - **C-3**: `hello` records `shim_pid` in the response — the caller
//!   compares against `state.transport.shim_pid` and emits
//!   `conpty_transport.shim_pid_stale` before returning `MuxUnavailable`
//!   if they disagree (backend-side, not shim-side).
//! - **C-5**: `Shim::rotate_pipe_token` is a distinct method (not the
//!   default token setter). Every request checks `pipe_token` FIRST
//!   (before dispatching to the op-handler) so a mismatch returns
//!   `ProtocolError::PipeTokenMismatch` unambiguously.
//! - **C-7 schema-skew**: `Request.schema != PROTOCOL_SCHEMA` returns
//!   `ProtocolError::SchemaSkew` immediately, before dispatch.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use crate::protocol::{
    HelloResult, InjectRequest, Op, ProtocolError, Request, Response, SpawnRequest, SpawnResult,
    CaptureRequest, CaptureResult, PROTOCOL_SCHEMA,
};

/// Physical-pane abstraction. The Windows `bin/windows_shim` supplies a
/// real ConPTY-backed impl; tests supply an in-memory echo impl.
pub trait PaneRuntime: Send + Sync {
    /// Write bytes to the pane's input side. Returns the number of
    /// bytes accepted. Blocks caller only briefly.
    fn write_input(&self, bytes: &[u8]) -> Result<usize, String>;

    /// Snapshot the pane's scrollback ring. `range` is one of `full`,
    /// `head:N`, or `tail:N` — same wire strings the wire protocol
    /// carries.
    fn capture(&self, range: &str) -> Result<String, String>;

    /// Best-effort child process id (Windows only; `None` for the
    /// in-memory fake).
    fn child_pid(&self) -> Option<u32>;

    /// Returns `true` while the pane's child process is still alive.
    fn is_alive(&self) -> bool;

    /// Terminate the child process. Idempotent.
    fn kill(&self);
}

/// The registered pane row. Design §Authority Model:145 field list.
pub struct PaneEntry {
    pub pane_id: String,
    pub session: String,
    pub window: String,
    pub spawn_epoch: u64,
    pub runtime: Arc<dyn PaneRuntime>,
}

/// The shim's in-memory state. Never persisted (CR C-1).
pub struct Shim {
    workspace_hash: String,
    team_key: String,
    shim_pid: u32,
    shim_version: String,
    /// CR C-1: `pipe_token` lives here in memory only. Rotated on
    /// shim restart (Phase 1b real Windows path); `rotate_pipe_token`
    /// is the only entry point that changes it.
    pipe_token: Mutex<String>,
    panes: Mutex<BTreeMap<String, PaneEntry>>,
    spawn_epoch: Mutex<u64>,
    /// Injected factory for constructing PaneRuntimes on spawn. The
    /// test-side factory returns a `FakePaneRuntime`; the Windows
    /// binary-side factory returns a real ConPTY handle.
    pane_factory: Box<dyn Fn(&SpawnRequest) -> Result<Arc<dyn PaneRuntime>, String> + Send + Sync>,
}

impl Shim {
    pub fn new(
        workspace_hash: impl Into<String>,
        team_key: impl Into<String>,
        shim_pid: u32,
        shim_version: impl Into<String>,
        pipe_token: impl Into<String>,
        pane_factory: Box<
            dyn Fn(&SpawnRequest) -> Result<Arc<dyn PaneRuntime>, String> + Send + Sync,
        >,
    ) -> Self {
        Self {
            workspace_hash: workspace_hash.into(),
            team_key: team_key.into(),
            shim_pid,
            shim_version: shim_version.into(),
            pipe_token: Mutex::new(pipe_token.into()),
            panes: Mutex::new(BTreeMap::new()),
            spawn_epoch: Mutex::new(0),
            pane_factory,
        }
    }

    /// CR C-5: rotate the token. Callers holding the OLD token get
    /// `PipeTokenMismatch` on their next request — never a silent
    /// success or silent auto-rotate.
    #[allow(dead_code)]
    pub fn rotate_pipe_token(&self, new_token: impl Into<String>) {
        *self.pipe_token.lock().unwrap() = new_token.into();
    }

    pub fn current_pipe_token(&self) -> String {
        self.pipe_token.lock().unwrap().clone()
    }

    /// Route one request to the matching op handler. Always returns
    /// a `Response` — routing itself does not fail.
    pub fn handle(&self, req: &Request) -> Response {
        // CR C-7: schema skew fail-closed BEFORE anything else.
        if req.schema != PROTOCOL_SCHEMA {
            return Response::err(
                &req.request_id,
                ProtocolError::SchemaSkew {
                    message: format!(
                        "shim expects schema {}, request has {}",
                        PROTOCOL_SCHEMA, req.schema
                    ),
                    sent: req.schema,
                    expected: PROTOCOL_SCHEMA,
                },
            );
        }
        // Hello is the ONE op that establishes the pipe_token, so it
        // does not require matching it. Every other op must match.
        if req.op != Op::Hello {
            let current = self.pipe_token.lock().unwrap().clone();
            if req.pipe_token != current {
                // CR C-5: distinguish token mismatch — never silent.
                return Response::err(
                    &req.request_id,
                    ProtocolError::PipeTokenMismatch {
                        message: "shim pipe_token has rotated; caller must \
                                  restart negotiation via `hello` — no silent \
                                  retry with rotated token allowed"
                            .to_string(),
                    },
                );
            }
        }
        // Workspace/team scope check (N18 isolation).
        if req.workspace_hash != self.workspace_hash || req.team_key != self.team_key {
            return Response::err(
                &req.request_id,
                ProtocolError::ShimUnavailable {
                    message: format!(
                        "shim serves workspace_hash={} team_key={}; \
                         request targeted workspace_hash={} team_key={}",
                        self.workspace_hash,
                        self.team_key,
                        req.workspace_hash,
                        req.team_key
                    ),
                },
            );
        }
        match req.op {
            Op::Hello => self.op_hello(req),
            Op::Spawn => self.op_spawn(req),
            Op::Inject => self.op_inject(req),
            Op::Capture => self.op_capture(req),
            Op::Liveness => self.op_liveness(req),
            Op::HasPane => self.op_has_pane(req),
            Op::ListTargets => self.op_list_targets(req),
            Op::HasSession => self.op_has_session(req),
            Op::ListWindows => self.op_list_windows(req),
            Op::KillSession => self.op_kill_session(req),
            Op::KillWindow => self.op_kill_window(req),
            Op::KillPane => self.op_kill_pane(req),
            Op::Shutdown => self.op_shutdown(req),
            // These are Ok(None)/no-op for MVP; expand in Phase 2/3.
            Op::SendKeys => Response::ok(&req.request_id, serde_json::json!({})),
            Op::Query => Response::ok(&req.request_id, serde_json::Value::Null),
            Op::SetSessionEnv => Response::ok(
                &req.request_id,
                serde_json::json!({"outcome": "internalized_at_spawn"}),
            ),
        }
    }

    fn op_hello(&self, req: &Request) -> Response {
        let result = HelloResult {
            schema: PROTOCOL_SCHEMA,
            shim_pid: self.shim_pid,
            shim_version: self.shim_version.clone(),
            pipe_token: self.current_pipe_token(),
        };
        Response::ok(&req.request_id, serde_json::to_value(result).unwrap())
    }

    fn op_spawn(&self, req: &Request) -> Response {
        let spawn: SpawnRequest = match serde_json::from_value(req.payload.clone()) {
            Ok(s) => s,
            Err(e) => {
                return Response::err(
                    &req.request_id,
                    ProtocolError::Other {
                        message: format!("bad spawn payload: {e}"),
                    },
                );
            }
        };
        let runtime = match (self.pane_factory)(&spawn) {
            Ok(r) => r,
            Err(e) => {
                return Response::err(
                    &req.request_id,
                    ProtocolError::Spawn { message: e },
                );
            }
        };
        let epoch = {
            let mut ep = self.spawn_epoch.lock().unwrap();
            *ep += 1;
            *ep
        };
        // Design §Authority Model:155 pane_id shape.
        let pane_id = format!(
            "conpty:{}:{}:{}:{}",
            self.workspace_hash,
            self.team_key,
            spawn.window,
            epoch
        );
        let child_pid = runtime.child_pid();
        let entry = PaneEntry {
            pane_id: pane_id.clone(),
            session: spawn.session.clone(),
            window: spawn.window.clone(),
            spawn_epoch: epoch,
            runtime,
        };
        self.panes.lock().unwrap().insert(pane_id.clone(), entry);
        let result = SpawnResult {
            pane_id,
            session: spawn.session,
            window: spawn.window,
            child_pid,
            spawn_epoch: epoch,
        };
        Response::ok(&req.request_id, serde_json::to_value(result).unwrap())
    }

    fn op_inject(&self, req: &Request) -> Response {
        let inject: InjectRequest = match serde_json::from_value(req.payload.clone()) {
            Ok(i) => i,
            Err(e) => {
                return Response::err(
                    &req.request_id,
                    ProtocolError::Other {
                        message: format!("bad inject payload: {e}"),
                    },
                );
            }
        };
        let panes = self.panes.lock().unwrap();
        let Some(entry) = panes.get(&inject.pane_id) else {
            return Response::err(
                &req.request_id,
                ProtocolError::TargetNotFound {
                    message: format!("pane {} not found", inject.pane_id),
                },
            );
        };
        // Write text bytes.
        let text_bytes = inject.text.as_bytes();
        let written = match entry.runtime.write_input(text_bytes) {
            Ok(n) => n,
            Err(e) => {
                return Response::err(
                    &req.request_id,
                    ProtocolError::Inject {
                        message: e,
                        stage: "text".to_string(),
                    },
                );
            }
        };
        // Submit key (CR / VT sequence).
        let submit_bytes: &[u8] = match inject.submit_key.as_deref() {
            Some("enter") | Some("Enter") => b"\r",
            Some("up") | Some("Up") => b"\x1b[A",
            Some("down") | Some("Down") => b"\x1b[B",
            Some("left") | Some("Left") => b"\x1b[D",
            Some("right") | Some("Right") => b"\x1b[C",
            Some("escape") | Some("Escape") => b"\x1b",
            Some("ctrl_c") | Some("CtrlC") => b"\x03",
            _ => b"",
        };
        if !submit_bytes.is_empty() {
            if let Err(e) = entry.runtime.write_input(submit_bytes) {
                return Response::err(
                    &req.request_id,
                    ProtocolError::Inject {
                        message: e,
                        stage: "submit".to_string(),
                    },
                );
            }
        }
        Response::ok(
            &req.request_id,
            serde_json::json!({"bytes_written": written, "submit_key_written": !submit_bytes.is_empty()}),
        )
    }

    fn op_capture(&self, req: &Request) -> Response {
        let cap: CaptureRequest = match serde_json::from_value(req.payload.clone()) {
            Ok(c) => c,
            Err(e) => {
                return Response::err(
                    &req.request_id,
                    ProtocolError::Other {
                        message: format!("bad capture payload: {e}"),
                    },
                );
            }
        };
        let panes = self.panes.lock().unwrap();
        let Some(entry) = panes.get(&cap.pane_id) else {
            return Response::err(
                &req.request_id,
                ProtocolError::TargetNotFound {
                    message: format!("pane {} not found", cap.pane_id),
                },
            );
        };
        match entry.runtime.capture(&cap.range) {
            Ok(text) => Response::ok(
                &req.request_id,
                serde_json::to_value(CaptureResult { text }).unwrap(),
            ),
            Err(e) => Response::err(&req.request_id, ProtocolError::Capture { message: e }),
        }
    }

    fn op_liveness(&self, req: &Request) -> Response {
        let pane_id = req
            .payload
            .get("pane_id")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_string();
        let panes = self.panes.lock().unwrap();
        let alive = panes.get(&pane_id).map(|e| e.runtime.is_alive());
        Response::ok(
            &req.request_id,
            serde_json::json!({
                "known": alive.is_some(),
                "alive": alive.unwrap_or(false),
            }),
        )
    }

    fn op_has_pane(&self, req: &Request) -> Response {
        let pane_id = req
            .payload
            .get("pane_id")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_string();
        Response::ok(
            &req.request_id,
            serde_json::json!({
                "present": self.panes.lock().unwrap().contains_key(&pane_id),
            }),
        )
    }

    fn op_list_targets(&self, req: &Request) -> Response {
        let panes = self.panes.lock().unwrap();
        let list: Vec<serde_json::Value> = panes
            .values()
            .map(|e| {
                serde_json::json!({
                    "pane_id": e.pane_id,
                    "session": e.session,
                    "window": e.window,
                    "spawn_epoch": e.spawn_epoch,
                    "child_pid": e.runtime.child_pid(),
                    "alive": e.runtime.is_alive(),
                })
            })
            .collect();
        Response::ok(&req.request_id, serde_json::json!({"targets": list}))
    }

    fn op_has_session(&self, req: &Request) -> Response {
        let session = req
            .payload
            .get("session")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_string();
        let present = self
            .panes
            .lock()
            .unwrap()
            .values()
            .any(|e| e.session == session);
        Response::ok(&req.request_id, serde_json::json!({"present": present}))
    }

    fn op_list_windows(&self, req: &Request) -> Response {
        let session = req
            .payload
            .get("session")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_string();
        let mut windows: Vec<String> = self
            .panes
            .lock()
            .unwrap()
            .values()
            .filter(|e| e.session == session)
            .map(|e| e.window.clone())
            .collect();
        windows.sort();
        windows.dedup();
        Response::ok(&req.request_id, serde_json::json!({"windows": windows}))
    }

    fn op_kill_session(&self, req: &Request) -> Response {
        let session = req
            .payload
            .get("session")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_string();
        let mut panes = self.panes.lock().unwrap();
        let victims: Vec<String> = panes
            .iter()
            .filter(|(_, e)| e.session == session)
            .map(|(id, _)| id.clone())
            .collect();
        for pane_id in &victims {
            if let Some(entry) = panes.remove(pane_id) {
                entry.runtime.kill();
            }
        }
        Response::ok(
            &req.request_id,
            serde_json::json!({"killed_count": victims.len()}),
        )
    }

    fn op_kill_window(&self, req: &Request) -> Response {
        let session = req
            .payload
            .get("session")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_string();
        let window = req
            .payload
            .get("window")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_string();
        let mut panes = self.panes.lock().unwrap();
        let victims: Vec<String> = panes
            .iter()
            .filter(|(_, e)| e.session == session && e.window == window)
            .map(|(id, _)| id.clone())
            .collect();
        for pane_id in &victims {
            if let Some(entry) = panes.remove(pane_id) {
                entry.runtime.kill();
            }
        }
        Response::ok(
            &req.request_id,
            serde_json::json!({"killed_count": victims.len()}),
        )
    }

    fn op_kill_pane(&self, req: &Request) -> Response {
        let pane_id = req
            .payload
            .get("pane_id")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_string();
        let mut panes = self.panes.lock().unwrap();
        let killed = panes.remove(&pane_id).is_some_and(|e| {
            e.runtime.kill();
            true
        });
        Response::ok(&req.request_id, serde_json::json!({"killed": killed}))
    }

    fn op_shutdown(&self, req: &Request) -> Response {
        let mut panes = self.panes.lock().unwrap();
        let count = panes.len();
        let ids: Vec<String> = panes.keys().cloned().collect();
        for id in ids {
            if let Some(entry) = panes.remove(&id) {
                entry.runtime.kill();
            }
        }
        Response::ok(
            &req.request_id,
            serde_json::json!({"killed_count": count}),
        )
    }
}

/// In-memory PaneRuntime for tests + Mac/Linux dev. Echoes every
/// injected byte into a scrollback buffer so `capture` retrieves what
/// was injected (Phase 1 acceptance bullets #3 delivery + #4 capture).
pub struct FakePaneRuntime {
    scrollback: Mutex<Vec<u8>>,
    alive: Mutex<bool>,
}

impl FakePaneRuntime {
    pub fn new() -> Self {
        Self {
            scrollback: Mutex::new(Vec::new()),
            alive: Mutex::new(true),
        }
    }
}

impl Default for FakePaneRuntime {
    fn default() -> Self {
        Self::new()
    }
}

impl PaneRuntime for FakePaneRuntime {
    fn write_input(&self, bytes: &[u8]) -> Result<usize, String> {
        if !*self.alive.lock().unwrap() {
            return Err("pane dead".to_string());
        }
        self.scrollback.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn capture(&self, range: &str) -> Result<String, String> {
        let sb = self.scrollback.lock().unwrap();
        let text = String::from_utf8_lossy(&sb).to_string();
        // Parse "head:N", "tail:N", or "full".
        if range == "full" {
            return Ok(text);
        }
        if let Some(n_str) = range.strip_prefix("tail:") {
            let n: usize = n_str.parse().unwrap_or(80);
            let lines: Vec<&str> = text.lines().collect();
            let start = lines.len().saturating_sub(n);
            return Ok(lines[start..].join("\n"));
        }
        if let Some(n_str) = range.strip_prefix("head:") {
            let n: usize = n_str.parse().unwrap_or(80);
            let lines: Vec<&str> = text.lines().take(n).collect();
            return Ok(lines.join("\n"));
        }
        Ok(text)
    }

    fn child_pid(&self) -> Option<u32> {
        None
    }

    fn is_alive(&self) -> bool {
        *self.alive.lock().unwrap()
    }

    fn kill(&self) {
        *self.alive.lock().unwrap() = false;
    }
}

/// Object-safe pipe-client trait so callers can hold a boxed client
/// without depending on the concrete Windows named-pipe type. The
/// team-agent crate provides a wrapper that implements its own
/// `TransportError`-returning trait against this one.
pub trait PipeClient: Send + Sync {
    fn request(&self, req: &Request) -> Response;
}

/// In-process pipe client — used by team-agent's Mac/Linux fake-worker
/// path + all portable end-to-end tests. Talks directly to the `Shim`
/// state machine without a real named-pipe.
pub struct LocalShimClient {
    shim: Arc<Shim>,
}

impl LocalShimClient {
    pub fn new(shim: Arc<Shim>) -> Self {
        Self { shim }
    }
}

impl PipeClient for LocalShimClient {
    fn request(&self, req: &Request) -> Response {
        self.shim.handle(req)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_factory() -> Box<
        dyn Fn(&SpawnRequest) -> Result<Arc<dyn PaneRuntime>, String> + Send + Sync,
    > {
        Box::new(|_spawn| Ok(Arc::new(FakePaneRuntime::new()) as Arc<dyn PaneRuntime>))
    }

    fn make_shim() -> Arc<Shim> {
        Arc::new(Shim::new(
            "wshash",
            "team-a",
            1234,
            "test-shim-0.1",
            "tok-abc",
            fake_factory(),
        ))
    }

    #[test]
    fn hello_returns_shim_pid_and_token_and_does_not_require_token_match() {
        let shim = make_shim();
        // Hello: pipe_token in the request is ignored (this is the
        // negotiation step).
        let req = Request::new("r1", "wshash", "team-a", "WRONG", Op::Hello);
        let resp = shim.handle(&req);
        assert!(resp.ok, "hello must not require token pre-match");
        let result: HelloResult = serde_json::from_value(resp.result).unwrap();
        assert_eq!(result.shim_pid, 1234);
        assert_eq!(result.pipe_token, "tok-abc");
    }

    #[test]
    fn non_hello_op_with_wrong_token_returns_pipe_token_mismatch_distinct_variant() {
        // CR C-5 anchor: mismatch must be distinct so the backend
        // cannot conflate it with routine failures.
        let shim = make_shim();
        let req = Request::new("r2", "wshash", "team-a", "WRONG", Op::ListTargets);
        let resp = shim.handle(&req);
        assert!(!resp.ok);
        match resp.error.unwrap() {
            ProtocolError::PipeTokenMismatch { message } => {
                assert!(
                    message.contains("no silent retry"),
                    "message must forbid silent rotation"
                );
            }
            other => panic!("expected PipeTokenMismatch, got {other:?}"),
        }
    }

    #[test]
    fn schema_skew_fails_closed_before_dispatch() {
        // CR C-7 anchor.
        let shim = make_shim();
        let mut req = Request::new("r3", "wshash", "team-a", "tok-abc", Op::ListTargets);
        req.schema = 99;
        let resp = shim.handle(&req);
        assert!(!resp.ok);
        assert!(matches!(
            resp.error.unwrap(),
            ProtocolError::SchemaSkew { .. }
        ));
    }

    #[test]
    fn wrong_workspace_or_team_returns_shim_unavailable_not_success() {
        // N18/N30 team isolation.
        let shim = make_shim();
        let req = Request::new("r4", "OTHER", "team-a", "tok-abc", Op::ListTargets);
        let resp = shim.handle(&req);
        assert!(!resp.ok);
        assert!(matches!(
            resp.error.unwrap(),
            ProtocolError::ShimUnavailable { .. }
        ));
    }

    #[test]
    fn spawn_registers_pane_and_inject_capture_round_trip() {
        // Phase 1 acceptance bullets #3 delivery + #4 capture on the
        // in-memory fake pane runtime.
        let shim = make_shim();
        // Establish token via hello (returns the current token).
        let hello = shim.handle(&Request::new(
            "r-hello",
            "wshash",
            "team-a",
            "unused",
            Op::Hello,
        ));
        assert!(hello.ok);
        let token: HelloResult = serde_json::from_value(hello.result).unwrap();
        let token = token.pipe_token;

        // Spawn a pane.
        let spawn_payload = serde_json::to_value(SpawnRequest {
            session: "team-a".to_string(),
            window: "w1".to_string(),
            argv: vec!["fake".to_string()],
            cwd: "/tmp".to_string(),
            env: BTreeMap::new(),
            env_unset: vec![],
            cols: 80,
            rows: 24,
        })
        .unwrap();
        let spawn_resp = shim.handle(
            &Request::new("r-spawn", "wshash", "team-a", &token, Op::Spawn)
                .with_payload(spawn_payload),
        );
        assert!(spawn_resp.ok);
        let spawn: SpawnResult = serde_json::from_value(spawn_resp.result).unwrap();
        assert!(spawn.pane_id.starts_with("conpty:wshash:team-a:w1:"));
        assert_eq!(spawn.session, "team-a");
        assert_eq!(spawn.window, "w1");

        // Inject a token.
        let unique_token = "PHASE1_ACCEPT_TOKEN_20260705";
        let inject_payload = serde_json::to_value(InjectRequest {
            pane_id: spawn.pane_id.clone(),
            text: unique_token.to_string(),
            submit_key: Some("enter".to_string()),
            bracketed: false,
        })
        .unwrap();
        let inject_resp = shim.handle(
            &Request::new("r-inj", "wshash", "team-a", &token, Op::Inject)
                .with_payload(inject_payload),
        );
        assert!(inject_resp.ok, "inject must succeed: {:?}", inject_resp);

        // Capture and check.
        let cap_payload = serde_json::to_value(CaptureRequest {
            pane_id: spawn.pane_id.clone(),
            range: "full".to_string(),
        })
        .unwrap();
        let cap_resp = shim.handle(
            &Request::new("r-cap", "wshash", "team-a", &token, Op::Capture)
                .with_payload(cap_payload),
        );
        assert!(cap_resp.ok);
        let cap: CaptureResult = serde_json::from_value(cap_resp.result).unwrap();
        assert!(
            cap.text.contains(unique_token),
            "capture must contain the injected token; got {:?}",
            cap.text
        );
    }

    #[test]
    fn chinese_token_round_trips_utf8() {
        // Design §Encoding Strategy — Chinese must survive.
        let shim = make_shim();
        let token = shim.current_pipe_token();
        let spawn_payload = serde_json::to_value(SpawnRequest {
            session: "team-a".to_string(),
            window: "w-cn".to_string(),
            argv: vec!["fake".to_string()],
            cwd: "/tmp".to_string(),
            env: BTreeMap::new(),
            env_unset: vec![],
            cols: 80,
            rows: 24,
        })
        .unwrap();
        let s = shim.handle(
            &Request::new("s", "wshash", "team-a", &token, Op::Spawn)
                .with_payload(spawn_payload),
        );
        let sp: SpawnResult = serde_json::from_value(s.result).unwrap();
        let chinese = "王小明·测试·令牌";
        shim.handle(
            &Request::new("i", "wshash", "team-a", &token, Op::Inject).with_payload(
                serde_json::to_value(InjectRequest {
                    pane_id: sp.pane_id.clone(),
                    text: chinese.to_string(),
                    submit_key: None,
                    bracketed: false,
                })
                .unwrap(),
            ),
        );
        let c = shim.handle(
            &Request::new("c", "wshash", "team-a", &token, Op::Capture).with_payload(
                serde_json::to_value(CaptureRequest {
                    pane_id: sp.pane_id.clone(),
                    range: "full".to_string(),
                })
                .unwrap(),
            ),
        );
        let cap: CaptureResult = serde_json::from_value(c.result).unwrap();
        assert!(cap.text.contains(chinese));
    }

    #[test]
    fn rotate_pipe_token_makes_old_token_reject_next_request() {
        // CR C-5 anchor: rotation is EXPLICIT (never silent auto).
        let shim = make_shim();
        let old = shim.current_pipe_token();
        shim.rotate_pipe_token("NEW_TOK");
        // Old token now fails.
        let req = Request::new("r", "wshash", "team-a", old, Op::ListTargets);
        let resp = shim.handle(&req);
        assert!(!resp.ok);
        assert!(matches!(
            resp.error.unwrap(),
            ProtocolError::PipeTokenMismatch { .. }
        ));
        // New token succeeds.
        let req2 = Request::new("r2", "wshash", "team-a", "NEW_TOK", Op::ListTargets);
        let resp2 = shim.handle(&req2);
        assert!(resp2.ok);
    }

    #[test]
    fn shutdown_kills_all_panes() {
        // Phase 1 acceptance bullet #5 (shutdown kills shim and child).
        let shim = make_shim();
        let token = shim.current_pipe_token();
        for w in ["w1", "w2", "w3"] {
            let spawn_payload = serde_json::to_value(SpawnRequest {
                session: "team-a".to_string(),
                window: w.to_string(),
                argv: vec!["fake".to_string()],
                cwd: "/tmp".to_string(),
                env: BTreeMap::new(),
                env_unset: vec![],
                cols: 80,
                rows: 24,
            })
            .unwrap();
            let _ = shim.handle(
                &Request::new("s", "wshash", "team-a", &token, Op::Spawn)
                    .with_payload(spawn_payload),
            );
        }
        let list_resp = shim.handle(&Request::new(
            "l",
            "wshash",
            "team-a",
            &token,
            Op::ListTargets,
        ));
        let targets = list_resp.result["targets"].as_array().unwrap().len();
        assert_eq!(targets, 3);
        // Shutdown.
        let sd = shim.handle(&Request::new("sd", "wshash", "team-a", &token, Op::Shutdown));
        assert!(sd.ok);
        assert_eq!(sd.result["killed_count"], serde_json::json!(3));
        // list is now empty.
        let list2 = shim.handle(&Request::new(
            "l2",
            "wshash",
            "team-a",
            &token,
            Op::ListTargets,
        ));
        assert_eq!(list2.result["targets"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn local_shim_client_routes_through_shim_handle() {
        // The LocalShimClient is used by tests + the Mac/Linux
        // fake-worker path to exercise ConPtyBackend end-to-end.
        let shim = make_shim();
        let client = LocalShimClient::new(Arc::clone(&shim));
        let req = Request::new("cli-1", "wshash", "team-a", "tok-abc", Op::Hello);
        let resp = client.request(&req);
        assert!(resp.ok);
    }
}
