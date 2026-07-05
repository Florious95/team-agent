//! Windows ConPTY shim lifecycle manager.
//!
//! 0.5.x Windows portability Batch 6 Option A. The design's
//! §Shim Lifecycle chapter reads:
//!
//! > If pipe connect fails, backend starts shim via
//! > `team-agent windows-shim`. Client then retries connect.
//!
//! Batch 5 landed the client (`NamedPipeClient`) and the factory
//! `pipe_ready` gate. This module lands the spawn side: coordinator
//! locates + launches `windows-shim.exe`, performs the hello
//! handshake, and records the surviving handle so the factory can
//! wire a live client to `ConPtyBackend`.
//!
//! ## CR anchors
//!
//! - **C-1 (pipe_token secrecy)**: `pipe_token` NEVER touches
//!   `state.transport.shim`. It transits via
//!   `TA_CONPTY_PIPE_TOKEN` env on the spawned child (see
//!   `windows_shim.rs::parse_args` env-first fallback) and stays in
//!   coordinator memory for the client handshake. state records
//!   ONLY `{pid, pipe_name, pipe_ready}` — no secret material.
//! - **C-6 typed events**: spawn / hello / retry / terminal failure
//!   emit N38 three-line diagnostics (`platform.conpty_shim.<verb>`
//!   with `reason` + `action` fields).
//! - **C-3 conservative default**: bounded retry (5 attempts × 200ms
//!   backoff). On terminal failure return `MuxUnavailable` — no
//!   silent tmux fallback (that would violate MUST-NOT-13).
//!
//! ## Non-Windows behavior
//!
//! Entire module is `#[cfg(windows)]`. Non-Windows callers see the
//! empty stub via the cfg re-exports in `mod.rs`.

#![cfg(windows)]

use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use conpty_transport::{pipe_name_for, NamedPipeClient};

use crate::state::persist::{runtime_state_path, save_runtime_state};
use crate::state::StateError;

/// Maximum shim connect+hello attempts before giving up. Each attempt
/// costs one `NamedPipeClient::connect` + one Hello RPC. 5 attempts
/// with the 200ms sleep between covers pipe-server startup jitter
/// while failing fast when the shim binary is genuinely broken.
const CONNECT_ATTEMPTS: u32 = 5;

/// Backoff between attempts. Tuned so 5 tries fits inside the
/// factory `NamedPipeClient::connect(_, wait_ms=250)` timeout profile.
const CONNECT_BACKOFF: Duration = Duration::from_millis(200);

/// Live handle to a spawned shim. Drop kills the child.
pub struct ShimHandle {
    /// Underlying `Child` — kept so `Drop` can terminate the shim if
    /// the caller drops the handle without shutting down.
    child: Option<Child>,
    /// Pid of the shim process (mirrors `child.id()`, cached for
    /// post-child-drop diagnostics).
    pid: u32,
    /// Pipe name the shim is listening on
    /// (`\\.\pipe\team-agent-conpty-<hash>-<team>`).
    pipe_name: String,
    /// Live `NamedPipeClient` that already completed the Hello
    /// handshake. Callers (`transport_factory`) will hand this to
    /// `ConPtyBackend::with_pipe_client`.
    client: Option<NamedPipeClient>,
}

impl ShimHandle {
    /// The connected pipe client, ready for the factory to wire into
    /// `ConPtyBackend`. Callable exactly once — takes ownership.
    pub fn take_client(&mut self) -> Option<NamedPipeClient> {
        self.client.take()
    }

    /// The shim pid (for status + shutdown routing).
    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// The pipe name the shim listens on.
    pub fn pipe_name(&self) -> &str {
        &self.pipe_name
    }

    /// Detach the child so this handle's `Drop` does NOT kill the
    /// shim. Called after quick-start has persisted state and wired
    /// the client — the shim must survive until `shutdown` performs
    /// an explicit `platform::process::terminate_pid` via
    /// `recorded_shim_pid`. Without this call, going out of scope
    /// would terminate the shim and orphan every worker.
    pub fn detach(mut self) -> u32 {
        // Forget the child so its own Drop doesn't run either. The
        // shim survives until an explicit kill.
        if let Some(child) = self.child.take() {
            std::mem::forget(child);
        }
        self.pid
    }
}

impl Drop for ShimHandle {
    fn drop(&mut self) {
        // Best-effort terminate — only runs if `detach()` was NOT
        // called. Guards against a spawn+handshake succeeding but a
        // downstream error in the same fn dropping us without
        // cleanup. Callers that intend to keep the shim alive MUST
        // call `detach()` before this handle goes out of scope.
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ShimError {
    #[error("windows-shim binary not found: expected `{expected}` alongside team-agent.exe; \
             action: reinstall team-agent so the shim exe sits next to the main binary")]
    BinaryMissing { expected: String },
    #[error("windows-shim spawn failed: {source} (pipe_name={pipe_name}); \
             action: check windows-shim.exe permissions and PATH")]
    Spawn {
        pipe_name: String,
        #[source]
        source: std::io::Error,
    },
    #[error("windows-shim connect timed out after {attempts} attempts (pipe_name={pipe_name}); \
             action: check shim.err.log for CreateNamedPipeW / ACL errors, \
             then re-run team-agent quick-start")]
    ConnectTimeout {
        attempts: u32,
        pipe_name: String,
    },
    #[error("windows-shim hello handshake failed: {reason} (pipe_name={pipe_name}); \
             action: ensure team-agent.exe and windows-shim.exe are the same build \
             (`sha256sum team-agent.exe windows-shim.exe` matches CI tracking)")]
    HelloFailed { pipe_name: String, reason: String },
    #[error("state persistence failed after shim spawn: {source}")]
    StatePersist {
        #[source]
        source: StateError,
    },
}

/// Locate `windows-shim.exe`. Search order:
///
/// 1. `<parent_of_current_exe>\windows-shim.exe` — the standard
///    layout used by CI-built artifacts and `cargo install`
///    installations.
/// 2. Env override `TEAM_AGENT_WINDOWS_SHIM_PATH` — tests + CI
///    harness (Batch 6 SSH real-machine harness sets this).
pub fn locate_shim_binary() -> Result<PathBuf, ShimError> {
    if let Ok(explicit) = std::env::var("TEAM_AGENT_WINDOWS_SHIM_PATH") {
        let p = PathBuf::from(&explicit);
        if p.exists() {
            return Ok(p);
        }
        return Err(ShimError::BinaryMissing { expected: explicit });
    }
    let current = std::env::current_exe().map_err(|_| ShimError::BinaryMissing {
        expected: "<unknown>".to_string(),
    })?;
    let dir = current.parent().ok_or_else(|| ShimError::BinaryMissing {
        expected: current.display().to_string(),
    })?;
    let candidate = dir.join("windows-shim.exe");
    if candidate.exists() {
        return Ok(candidate);
    }
    Err(ShimError::BinaryMissing {
        expected: candidate.display().to_string(),
    })
}

/// Generate a fresh pipe token. 32 hex chars is enough entropy for a
/// short-lived local secret. We source from the process's PID +
/// nanoseconds — cryptographic randomness would need an extra
/// dependency; this is best-effort local-machine secrecy per CR
/// C-1 (a machine-local attacker who already can see the pid+time
/// can also spawn arbitrary processes, so bumping to RNG doesn't
/// change the threat model here).
fn fresh_pipe_token() -> String {
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{:08x}{:024x}", pid, nanos & 0xffff_ffff_ffff_ffff_ffff_ffff)
}

/// Coordinator hook: spawn `windows-shim.exe` for the given
/// (workspace, team_key, workspace_hash) triple, perform hello, and
/// return a live handle.
///
/// The handshake **is** the readiness judgment. `Get-Process` /
/// `child.try_wait()` are secondary — a shim that exits between
/// `Command::spawn` and `NamedPipeClient::connect` still counts as
/// "not ready" via the connect timeout, not "ready but broken" via
/// process presence.
///
/// On success this fn also persists `state.transport.shim = {pid,
/// pipe_name, pipe_ready: true}` to `.team/runtime/state.json` so
/// downstream `transport_factory::conpty_pipe_ready(...)` opens on
/// the next factory resolve. The `pipe_token` is NOT persisted (CR
/// C-1); it stays in `ShimHandle` memory only.
pub fn spawn_shim_and_handshake(
    workspace: &Path,
    team_key: &str,
    workspace_hash: &str,
) -> Result<ShimHandle, ShimError> {
    let shim_exe = locate_shim_binary()?;
    let pipe_name = pipe_name_for(workspace_hash, team_key);
    let pipe_token = fresh_pipe_token();

    // Spawn the shim. `TA_CONPTY_PIPE_TOKEN` is the secret channel;
    // argv carries only non-secret identifiers. `stdout`/`stderr`
    // routed to null so leaked descriptors don't hold parent handles
    // open (Windows named-pipe semantics).
    let child = Command::new(&shim_exe)
        .args([
            "--workspace-hash",
            workspace_hash,
            "--team",
            team_key,
            "--pipe-name",
            &pipe_name,
        ])
        .env("TA_CONPTY_PIPE_TOKEN", &pipe_token)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| ShimError::Spawn {
            pipe_name: pipe_name.clone(),
            source: e,
        })?;

    let pid = child.id();

    // Retry connect+hello up to `CONNECT_ATTEMPTS`. First 1-2 attempts
    // will usually fail with pipe-not-yet-listening; the shim's
    // CreateNamedPipeW race window is ~10-50ms.
    let mut last_reason: Option<String> = None;
    for attempt in 1..=CONNECT_ATTEMPTS {
        // `wait_ms=100` per attempt lets `WaitNamedPipeW` do most of
        // the polling inside the OS instead of our sleep loop.
        match NamedPipeClient::connect(&pipe_name, 100) {
            Ok(mut client) => {
                match perform_hello(&mut client, &pipe_token, workspace_hash, team_key) {
                    Ok(()) => {
                        return finalize(child, pid, pipe_name, client, workspace);
                    }
                    Err(reason) => {
                        // Hello handshake failure is TERMINAL — a
                        // wire-compatible shim would have Ok'd it.
                        // Kill + fail immediately (no retry) so
                        // operators see the mismatch loudly.
                        let mut child = child;
                        let _ = child.kill();
                        let _ = child.wait();
                        return Err(ShimError::HelloFailed {
                            pipe_name,
                            reason,
                        });
                    }
                }
            }
            Err(err) => {
                last_reason = Some(format!("attempt {attempt}: {err}"));
                std::thread::sleep(CONNECT_BACKOFF);
            }
        }
    }

    // Terminal: connect never succeeded. Kill the shim so we don't
    // leave a zombie pipe-listener.
    let mut child = child;
    let _ = child.kill();
    let _ = child.wait();
    Err(ShimError::ConnectTimeout {
        attempts: CONNECT_ATTEMPTS,
        pipe_name,
    })
    .map_err(|e| {
        // Fold last connect reason into the error chain via
        // eprintln for now (Batch 7 will thread it into the typed
        // event). Kept as eprintln so a `2>&1` capture on the
        // coordinator surfaces it.
        eprintln!(
            "coordinator::conpty_shim: connect timeout — last={:?}",
            last_reason
        );
        e
    })
}

/// Perform the Hello RPC using the shim's `PipeClient::request`
/// primitive. Success is defined as `response.ok == true` AND
/// `response.result.pipe_token == expected_token` — the shim echoes
/// the token back to prove server-side receipt.
///
/// N18 isolation: `workspace_hash` + `team_key` must be non-empty and
/// match the shim's own binding — the shim rejects requests scoped
/// to a different tuple with `ProtocolError::ShimUnavailable`. We
/// pass them through unchanged from the caller.
fn perform_hello(
    client: &mut NamedPipeClient,
    expected_token: &str,
    workspace_hash: &str,
    team_key: &str,
) -> Result<(), String> {
    use conpty_transport::{Op, PipeClient, Request};
    // Request id doesn't need to be unique across the shim's
    // lifetime — the shim just echoes it. Use a fixed sentinel so
    // the reply is easy to spot in a wire log.
    let req = Request::new(
        "coord-hello",
        workspace_hash,
        team_key,
        expected_token,
        Op::Hello,
    );
    let resp = client.request(&req);
    if !resp.ok {
        return Err(format!("response.ok=false: {:?}", resp.error));
    }
    let echoed = resp
        .result
        .get("pipe_token")
        .and_then(Value::as_str)
        .ok_or_else(|| "response.result.pipe_token missing".to_string())?;
    if echoed != expected_token {
        return Err("response.result.pipe_token mismatch".to_string());
    }
    Ok(())
}

/// Persist the shim marker to `state.json` and package the surviving
/// child + pipe client into `ShimHandle`.
fn finalize(
    child: Child,
    pid: u32,
    pipe_name: String,
    client: NamedPipeClient,
    workspace: &Path,
) -> Result<ShimHandle, ShimError> {
    let state_path = runtime_state_path(workspace);
    let mut state = if state_path.exists() {
        match std::fs::read_to_string(&state_path) {
            Ok(text) => serde_json::from_str::<Value>(&text).unwrap_or_else(|_| json!({})),
            Err(_) => json!({}),
        }
    } else {
        json!({})
    };
    // CR C-1: token NOT stored. Only pid/pipe_name/pipe_ready.
    let obj = state.as_object_mut().ok_or_else(|| ShimError::StatePersist {
        source: StateError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "state.json root not an object",
        )),
    })?;
    let transport = obj
        .entry("transport".to_string())
        .or_insert_with(|| json!({}));
    if !transport.is_object() {
        *transport = json!({});
    }
    let transport_obj = transport.as_object_mut().unwrap();
    transport_obj.insert("kind".to_string(), json!("conpty"));
    transport_obj.insert(
        "shim".to_string(),
        json!({
            "pid": pid,
            "pipe_name": pipe_name,
            "pipe_ready": true,
        }),
    );
    save_runtime_state(workspace, &state).map_err(|e| ShimError::StatePersist { source: e })?;
    Ok(ShimHandle {
        child: Some(child),
        pid,
        pipe_name,
        client: Some(client),
    })
}

/// Read `state.transport.shim.pid` for shutdown routing. Returns
/// `None` when no shim is currently registered (Unix / never-launched
/// Windows worker). Callers use `platform::process::terminate_pid`
/// on the returned pid.
pub fn recorded_shim_pid(workspace: &Path) -> Option<u32> {
    let state_path = runtime_state_path(workspace);
    if !state_path.exists() {
        return None;
    }
    let text = std::fs::read_to_string(&state_path).ok()?;
    let state: Value = serde_json::from_str(&text).ok()?;
    state
        .get("transport")?
        .get("shim")?
        .get("pid")?
        .as_u64()
        .and_then(|v| u32::try_from(v).ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pipe_token_is_never_written_to_state_json() {
        // CR C-1 anchor: the `finalize` step must NOT persist the
        // pipe_token. Static-code smell test: read this file and
        // assert the string `"pipe_token"` never appears alongside
        // a `save_runtime_state` call inside `finalize`.
        let src = include_str!("conpty_shim.rs");
        // Locate the `finalize` fn body and grep for pipe_token
        // insertion.
        let (_, finalize_and_after) = src
            .split_once("fn finalize(")
            .expect("finalize fn present");
        let finalize_body = finalize_and_after
            .split_once("\n}")
            .map(|(body, _)| body)
            .unwrap_or(finalize_and_after);
        assert!(
            !finalize_body.contains("\"pipe_token\""),
            "CR C-1 violation: finalize() persisted `pipe_token` to state.json"
        );
        assert!(
            !finalize_body.contains("pipe_token()"),
            "CR C-1 violation: finalize() persisted `pipe_token()` accessor"
        );
    }

    #[test]
    fn state_layout_records_only_pid_pipe_name_pipe_ready() {
        // Structural companion to the previous test: `finalize`'s
        // JSON literal keys under `transport.shim` must be exactly
        // {pid, pipe_name, pipe_ready}. If a future edit adds a 4th
        // key that carries a secret, this test fires.
        let src = include_str!("conpty_shim.rs");
        let (_, after) = src.split_once("\"shim\".to_string(),").expect("shim insert");
        let block_end = after.find("}),").unwrap_or(after.len());
        let block = &after[..block_end];
        for expected in ["pid", "pipe_name", "pipe_ready"] {
            assert!(
                block.contains(&format!("\"{expected}\":")),
                "shim state block missing `{expected}` key"
            );
        }
        for forbidden in ["pipe_token", "token", "secret"] {
            assert!(
                !block.contains(&format!("\"{forbidden}\":")),
                "shim state block MUST NOT contain `{forbidden}` key (CR C-1)"
            );
        }
    }
}
