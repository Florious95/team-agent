//! Concrete tmux `Transport` backend (SKELETON) — the real executor that runs `tmux <argv>`.
//!
//! step 9 shipped the [`crate::transport::Transport`] trait + the pure tmux argv-builders
//! (`tmux_spawn_argv`, `tmux_capture_argv`, `tmux_send_keys_argv`, `tmux_inject_text_argv`,
//! `tmux_query_argv`, `tmux_cancel_mode_argv`) but NO concrete backend that actually executes them.
//! This is that backend: each `Transport` method builds its argv via those builders, runs it through
//! a [`CommandRunner`] seam, and parses the tmux output into the trait's typed return.
//!
//! THE SEAM: [`CommandRunner`] is the single OS edge. [`RealCommandRunner`] runs
//! `std::process::Command::new("tmux") …`; tests inject a recording/canned runner so the argv
//! construction + output parsing are unit-testable in-process, while the real subprocess execution
//! stays the `#[ignore]` real-machine boundary (acceptance framework).
//!
//! §10: the implementation must be panic-free (porter adds the deny + bodies; this skeleton is
//! `unimplemented!()`). MUST-NOT-13: a transport backend has no provider-client dependency.
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::hash::{Hash, Hasher};
use std::io::{Read, Write};
// 0.5.x Windows portability Batch 1: gate the sole Unix API surface
// in this concrete tmux backend (FileTypeExt::is_socket + libc::geteuid
// tmux-socket-root derivation). The rest of the file is Command::new
// "tmux" shellouts that compile on Windows but return runtime errors
// honestly (tmux binary absent → typed subprocess error).
// Truth source: `.team/artifacts/0.5.x-windows-portability-survey-design.md` §Batch 1.
#[cfg(unix)]
use std::os::unix::fs::FileTypeExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use crate::model::enums::PaneLiveness;
use crate::transport::{
    normalize_capture, tmux_capture_argv, tmux_empty_inject_argv, tmux_inject_text_argv,
    tmux_query_argv, tmux_send_keys_argv, tmux_spawn_argv, AttachOutcome, BackendKind,
    CaptureRange, CapturedText, InjectPayload, InjectReport, InjectStage, InjectVerification, Key,
    PaneField, PaneId, PaneInfo, PaneMode, SessionName, SetEnvOutcome, SpawnResult,
    SubmitAttemptObservation, SubmitObserver, SubmitVerification, Target, Transport, TransportError,
    TurnVerification, WindowName,
};

/// Result of running an external command — the typed output of the OS edge.
#[derive(Debug, Clone)]
pub struct CommandOutput {
    /// process exit status was success (code 0).
    pub success: bool,
    /// exit code if the process exited normally.
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

/// The single OS-edge seam: run an argv vector and return its output.
/// Real impl spawns `std::process::Command`; tests inject canned/recording output so the
/// argv-construction + output-parsing of [`TmuxBackend`] is testable without a live tmux server.
pub trait CommandRunner: Send + Sync {
    fn run(&self, argv: &[String]) -> Result<CommandOutput, std::io::Error>;

    fn run_with_stdin(
        &self,
        argv: &[String],
        stdin: &str,
    ) -> Result<CommandOutput, std::io::Error> {
        let _ = stdin;
        self.run(argv)
    }
}

/// Production runner: `std::process::Command::new(argv[0]).args(argv[1..]).output()`.
pub struct RealCommandRunner;

const COMMAND_TIMEOUT: Duration = Duration::from_secs(5);

impl CommandRunner for RealCommandRunner {
    fn run(&self, argv: &[String]) -> Result<CommandOutput, std::io::Error> {
        self.run_inner(argv, None)
    }

    fn run_with_stdin(
        &self,
        argv: &[String],
        stdin: &str,
    ) -> Result<CommandOutput, std::io::Error> {
        self.run_inner(argv, Some(stdin))
    }
}

impl RealCommandRunner {
    fn run_inner(
        &self,
        argv: &[String],
        stdin_text: Option<&str>,
    ) -> Result<CommandOutput, std::io::Error> {
        let Some(program) = argv.first() else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "empty argv",
            ));
        };
        let mut child = std::process::Command::new(program)
            .args(argv.iter().skip(1))
            .stdin(if stdin_text.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        if let Some(text) = stdin_text {
            let mut stdin = child
                .stdin
                .take()
                .ok_or_else(|| std::io::Error::other("stdin pipe missing"))?;
            stdin.write_all(text.as_bytes())?;
        }
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| std::io::Error::other("stdout pipe missing"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| std::io::Error::other("stderr pipe missing"))?;
        let stdout_thread = std::thread::spawn(move || read_pipe(stdout));
        let stderr_thread = std::thread::spawn(move || read_pipe(stderr));
        let deadline = Instant::now() + COMMAND_TIMEOUT;
        let status = loop {
            if let Some(status) = child.try_wait()? {
                break status;
            }
            if Instant::now() >= deadline {
                child.kill()?;
                child.wait()?;
                let _ = join_pipe_reader(stdout_thread)?;
                let _ = join_pipe_reader(stderr_thread)?;
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    format!("{program} exceeded 5s timeout"),
                ));
            }
            std::thread::sleep(Duration::from_millis(25));
        };
        let stdout = join_pipe_reader(stdout_thread)?;
        let stderr = join_pipe_reader(stderr_thread)?;
        Ok(CommandOutput {
            success: status.success(),
            code: status.code(),
            stdout,
            stderr,
        })
    }
}

fn read_pipe<R: Read>(mut reader: R) -> Result<String, std::io::Error> {
    let mut bytes = Vec::new();
    reader.read_to_end(&mut bytes)?;
    Ok(String::from_utf8_lossy(&bytes).to_string())
}

fn join_pipe_reader(
    handle: std::thread::JoinHandle<Result<String, std::io::Error>>,
) -> Result<String, std::io::Error> {
    handle
        .join()
        .map_err(|_| std::io::Error::other("pipe reader thread panicked"))?
}

/// Concrete tmux backend: builds argv via the `transport::tmux_*_argv` builders, runs them through
/// the [`CommandRunner`], and parses tmux output into the [`Transport`] typed returns.
///
/// CP-1: a workspace-bound backend carries a PER-TEAM tmux socket (`socket = Some("ta-<hash>")`) so a
/// dying shared `default` server can no longer tear the team down. The socket is injected at the RUN
/// CHOKEPOINT ([`TmuxBackend::tmux_argv`]) — the `transport::tmux_*_argv` builders stay socket-free.
pub struct TmuxBackend {
    runner: Box<dyn CommandRunner>,
    /// `Some(name)` for a per-team socket -> every `tmux` argv gets `-L <name>` injected after the
    /// leading "tmux" token; `None` (default) -> bare `tmux` on the shared default socket.
    socket: Option<TmuxSocketEndpoint>,
    /// swallow batch 2: workspace for failure-observability events (`tmux.*_failed`);
    /// `None` for non-workspace-bound backends (no event log to write to).
    event_workspace: Option<PathBuf>,
}

enum TmuxSocketEndpoint {
    Name(String),
    Path(String),
}

impl TmuxSocketEndpoint {
    fn as_endpoint(&self) -> &str {
        match self {
            TmuxSocketEndpoint::Name(socket) | TmuxSocketEndpoint::Path(socket) => socket,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RuntimeTmuxEndpointSource {
    StateTmuxEndpoint,
    StateTmuxSocket,
    WorkspaceFallback,
}

impl RuntimeTmuxEndpointSource {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::StateTmuxEndpoint => "state.tmux_endpoint",
            Self::StateTmuxSocket => "state.tmux_socket",
            Self::WorkspaceFallback => "workspace_fallback",
        }
    }
}

pub(crate) struct RuntimeTmuxBackendSelection {
    pub(crate) backend: TmuxBackend,
    pub(crate) tmux_endpoint_used: Option<String>,
    pub(crate) tmux_endpoint_source: RuntimeTmuxEndpointSource,
}

impl TmuxBackend {
    /// Backend bound to the real `tmux` subprocess on the SHARED default socket (no `-L`).
    /// Non-team callers + existing argv/unit tests stay unaffected.
    pub fn new() -> Self {
        Self {
            runner: Box::new(RealCommandRunner),
            socket: None,
            event_workspace: None,
        }
    }

    /// CP-1 team backend: bound to the real `tmux` subprocess on a PER-WORKSPACE socket, derived
    /// deterministically from the canonicalized workspace path so the leader CLI, the daemon, and
    /// every later op (spawn / inject / has_session / kill) hit the SAME `tmux -L <socket>` server.
    pub fn for_workspace(workspace: &Path) -> Self {
        Self {
            runner: Box::new(RealCommandRunner),
            socket: Some(TmuxSocketEndpoint::Name(socket_name_for_workspace(
                workspace,
            ))),
            event_workspace: Some(workspace.to_path_buf()),
        }
    }

    pub(crate) fn for_socket_name(socket: &str) -> Self {
        if socket.is_empty() || socket == "default" {
            Self::new()
        } else {
            Self {
                runner: Box::new(RealCommandRunner),
                socket: Some(TmuxSocketEndpoint::Name(socket.to_string())),
                event_workspace: None,
            }
        }
    }

    pub(crate) fn for_tmux_endpoint(endpoint: &str) -> Self {
        if endpoint.is_empty() || endpoint == "default" {
            Self::new()
        } else if Path::new(endpoint).is_absolute() {
            Self {
                runner: Box::new(RealCommandRunner),
                socket: Some(TmuxSocketEndpoint::Path(endpoint.to_string())),
                event_workspace: None,
            }
        } else if let Some(path) = socket_path_for_name(endpoint) {
            Self {
                runner: Box::new(RealCommandRunner),
                socket: Some(TmuxSocketEndpoint::Path(path.to_string_lossy().into_owned())),
                event_workspace: None,
            }
        } else {
            Self::new()
        }
    }

    /// Backend with an injected runner (tests: canned/recording tmux output). Shared default socket.
    pub fn with_runner(runner: Box<dyn CommandRunner>) -> Self {
        Self {
            runner,
            socket: None,
            event_workspace: None,
        }
    }

    /// Backend with an injected runner bound to a per-workspace socket (tests: assert the `-L` is in
    /// the recorded argv for a workspace-bound backend).
    pub fn with_runner_for_workspace(runner: Box<dyn CommandRunner>, workspace: &Path) -> Self {
        Self {
            runner,
            socket: Some(TmuxSocketEndpoint::Name(socket_name_for_workspace(
                workspace,
            ))),
            event_workspace: Some(workspace.to_path_buf()),
        }
    }

    pub(crate) fn with_runner_for_tmux_endpoint(
        runner: Box<dyn CommandRunner>,
        endpoint: &str,
    ) -> Self {
        if Path::new(endpoint).is_absolute() {
            Self {
                runner,
                socket: Some(TmuxSocketEndpoint::Path(endpoint.to_string())),
                event_workspace: None,
            }
        } else if endpoint.is_empty() || endpoint == "default" {
            Self {
                runner,
                socket: None,
                event_workspace: None,
            }
        } else if let Some(path) = socket_path_for_name(endpoint) {
            Self {
                runner,
                socket: Some(TmuxSocketEndpoint::Path(path.to_string_lossy().into_owned())),
                event_workspace: None,
            }
        } else {
            Self {
                runner,
                socket: None,
                event_workspace: None,
            }
        }
    }

    /// Build the exact argv that a workspace-bound tmux backend will execute.
    pub fn argv_for_workspace(workspace: &Path, argv: &[String]) -> Vec<String> {
        Self::for_workspace(workspace).tmux_argv(argv)
    }

    /// THE RUN CHOKEPOINT: every executed `tmux` argv is funneled through here. When a per-team
    /// socket is set, inject `-L <socket>` right after the leading "tmux" token; otherwise pass argv
    /// through unchanged. Non-`tmux` argv (e.g. the spawned provider command) is never rewritten.
    fn tmux_argv(&self, argv: &[String]) -> Vec<String> {
        match &self.socket {
            Some(endpoint) if argv.first().map(String::as_str) == Some("tmux") => {
                let mut out = Vec::with_capacity(argv.len() + 2);
                out.push("tmux".to_string());
                match endpoint {
                    TmuxSocketEndpoint::Name(socket) => {
                        out.push("-L".to_string());
                        out.push(socket.clone());
                    }
                    TmuxSocketEndpoint::Path(socket) => {
                        out.push("-S".to_string());
                        out.push(socket.clone());
                    }
                }
                out.extend(argv.iter().skip(1).cloned());
                out
            }
            _ => argv.to_vec(),
        }
    }

    /// `tmux -L <socket> kill-server` (CP-1 cleanup): best-effort teardown of the per-team server on
    /// shutdown so per-team sockets do not orphan. No-op (and never errors) for a default-socket
    /// backend, and a "no server" failure is ignored.
    pub fn kill_server(&self) {
        if self.socket.is_none() {
            return;
        }
        let argv = self.tmux_argv(&["tmux".to_string(), "kill-server".to_string()]);
        let _ = self.runner.run(&argv);
    }
}

pub(crate) fn tmux_backend_for_runtime_state_or_workspace(
    workspace: &Path,
    state: Option<&serde_json::Value>,
) -> RuntimeTmuxBackendSelection {
    let (backend, source) =
        if let Some((endpoint, source)) = runtime_tmux_endpoint_from_state(state) {
            (TmuxBackend::for_tmux_endpoint(endpoint), source)
        } else {
            (
                TmuxBackend::for_workspace(workspace),
                RuntimeTmuxEndpointSource::WorkspaceFallback,
            )
        };
    RuntimeTmuxBackendSelection {
        tmux_endpoint_used: backend.tmux_endpoint(),
        backend,
        tmux_endpoint_source: source,
    }
}

#[cfg(test)]
pub(crate) fn tmux_backend_with_runner_for_runtime_state_or_workspace(
    runner: Box<dyn CommandRunner>,
    workspace: &Path,
    state: Option<&serde_json::Value>,
) -> RuntimeTmuxBackendSelection {
    let (backend, source) =
        if let Some((endpoint, source)) = runtime_tmux_endpoint_from_state(state) {
            (
                TmuxBackend::with_runner_for_tmux_endpoint(runner, endpoint),
                source,
            )
        } else {
            (
                TmuxBackend::with_runner_for_workspace(runner, workspace),
                RuntimeTmuxEndpointSource::WorkspaceFallback,
            )
        };
    RuntimeTmuxBackendSelection {
        tmux_endpoint_used: backend.tmux_endpoint(),
        backend,
        tmux_endpoint_source: source,
    }
}

fn runtime_tmux_endpoint_from_state(
    state: Option<&serde_json::Value>,
) -> Option<(&str, RuntimeTmuxEndpointSource)> {
    state.and_then(|state| {
        state
            .get("tmux_endpoint")
            .and_then(|v| v.as_str())
            .filter(|endpoint| !endpoint.is_empty())
            .map(|endpoint| (endpoint, RuntimeTmuxEndpointSource::StateTmuxEndpoint))
            .or_else(|| {
                state
                    .get("tmux_socket")
                    .and_then(|v| v.as_str())
                    .filter(|endpoint| !endpoint.is_empty())
                    .map(|endpoint| (endpoint, RuntimeTmuxEndpointSource::StateTmuxSocket))
            })
    })
}

/// CP-1 socket name: SHORT + DETERMINISTIC per canonical workspace path. `ta-` + 12 hex chars of a
/// stable FNV-1a hash over the canonicalized path. AF_UNIX `sun_path` is ~104 chars and the socket
/// lives at `/tmp/tmux-<uid>/<name>`, so we must NOT use the (~88-char) session name. §10: a
/// canonicalize failure falls back to the raw path (never panics).
/// Public re-export of the crate-private canonical workspace hash used
/// by the tmux short-socket derivation. `transport_factory` uses this
/// same hash for the ConPTY `workspace_hash` so both backends see
/// identical workspace identity. Adding a wrapper (not calling
/// `socket_name_for_workspace` directly outside this file) keeps the
/// existing internal API stable.
pub fn workspace_short_hash_pub(workspace: &Path) -> String {
    // The tmux short-socket name is `ta-<12 hex>`; the workspace-hash
    // used by the ConPTY workspace identity is the raw 12-hex portion
    // (no `ta-` prefix), which is what the shim/pipe name derivation
    // expects. Reuse the exact same hash so drift is impossible.
    let name = socket_name_for_workspace(workspace);
    name.strip_prefix("ta-").unwrap_or(&name).to_string()
}

/// Public re-export of the crate-private `runtime_tmux_endpoint_from_state`
/// probe. `transport_factory` uses this to spot the legacy tmux endpoint
/// in `state` without duplicating the field-precedence logic.
pub fn runtime_tmux_endpoint_from_state_pub(
    state: Option<&serde_json::Value>,
) -> Option<(String, &'static str)> {
    runtime_tmux_endpoint_from_state(state).map(|(endpoint, source)| {
        let src = match source {
            RuntimeTmuxEndpointSource::StateTmuxEndpoint => "state.tmux_endpoint",
            RuntimeTmuxEndpointSource::StateTmuxSocket => "state.tmux_socket",
            RuntimeTmuxEndpointSource::WorkspaceFallback => "workspace_fallback",
        };
        (endpoint.to_string(), src)
    })
}

pub(crate) fn socket_name_for_workspace(workspace: &Path) -> String {
    let canonical = workspace
        .canonicalize()
        .unwrap_or_else(|_| workspace.to_path_buf());
    let mut hasher = Fnv1a::default();
    canonical.as_os_str().hash(&mut hasher);
    format!("ta-{:012x}", hasher.finish() & 0xffff_ffff_ffff)
}

pub(crate) fn socket_path_for_workspace(workspace: &Path) -> Option<PathBuf> {
    socket_path_for_name(&socket_name_for_workspace(workspace))
}

pub(crate) fn socket_probe_missing_for_workspace(workspace: &Path) -> bool {
    existing_socket_path_for_workspace(workspace).is_none()
}

fn existing_socket_path_for_workspace(workspace: &Path) -> Option<PathBuf> {
    existing_socket_path_for_name(&socket_name_for_workspace(workspace))
}

pub(crate) fn socket_path_for_name(socket_name: &str) -> Option<PathBuf> {
    if socket_name.is_empty() || socket_name == "default" || Path::new(socket_name).is_absolute() {
        return None;
    }
    if let Some(existing) = existing_socket_path_for_name(socket_name) {
        return Some(existing);
    }
    // Batch 1: tmux uses `/tmp/tmux-<uid>/` on Unix; on Windows this
    // helper is dead code (Command::new "tmux" fails at spawn time
    // before this path is dereferenced). N38 typed unsupported honored
    // at the shellout boundary.
    #[cfg(unix)]
    {
        let uid = unsafe { libc::geteuid() };
        let default_root = PathBuf::from(format!("/tmp/tmux-{uid}"));
        let default_root = default_root.canonicalize().unwrap_or(default_root);
        Some(default_root.join(socket_name))
    }
    #[cfg(not(unix))]
    {
        // Windows: tmux socket-root derivation is meaningless. Return
        // None so the caller sees the honest "no such socket" branch.
        let _ = socket_name;
        None
    }
}

fn existing_socket_path_for_name(socket_name: &str) -> Option<PathBuf> {
    let roots = tmux_socket_roots();
    for root in &roots {
        let root = root.canonicalize().unwrap_or_else(|_| root.clone());
        let candidate = root.join(socket_name);
        if candidate.exists() {
            return Some(candidate.canonicalize().unwrap_or(candidate));
        }
    }
    None
}

pub(crate) fn socket_missing_hint_for_workspace(workspace: &Path) -> String {
    let socket_name = socket_name_for_workspace(workspace);
    let roots = tmux_socket_roots()
        .into_iter()
        .map(|root| root.display().to_string())
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "tmux socket {socket_name} not found under [{roots}]; run `team-agent attach-leader` or restart the team before attaching"
    )
}

pub(crate) fn attach_command_for_workspace(
    workspace: &Path,
    session_name: &SessionName,
    window_name: &str,
) -> Option<String> {
    let socket_path = socket_path_for_workspace(workspace)?;
    Some(format!(
        "tmux -S {} attach -t {}:{}",
        socket_path.display(),
        session_name.as_str(),
        window_name
    ))
}

pub(crate) fn attach_command_for_session(
    workspace: &Path,
    session_name: &SessionName,
) -> Option<String> {
    let socket_path = socket_path_for_workspace(workspace)?;
    Some(format!(
        "tmux -S {} attach -t {}",
        socket_path.display(),
        session_name.as_str()
    ))
}

pub(crate) fn attach_command_for_transport_session(
    transport: &dyn crate::transport::Transport,
    session_name: &SessionName,
) -> Option<String> {
    let endpoint = transport.tmux_endpoint()?;
    Some(attach_command_for_endpoint_session(&endpoint, session_name))
}

/// Bug #7 (prerelease 0.4.0 gate review §6): when the runtime state carries a
/// persisted `tmux_endpoint` / `tmux_socket` (e.g. `/private/tmp/tmux-501/default`),
/// the attach command MUST point at THAT endpoint, not the workspace-hash
/// socket — otherwise operators are told to attach to a socket where the
/// session does not exist. Falls back to workspace-hash when state has no
/// persisted endpoint.
pub(crate) fn attach_command_for_runtime_state_or_workspace(
    workspace: &Path,
    state: Option<&serde_json::Value>,
    session_name: &SessionName,
    window_name: &str,
) -> Option<String> {
    if let Some((endpoint, _source)) = runtime_tmux_endpoint_from_state(state) {
        let display = endpoint.to_string();
        // Distinguish absolute path (`-S <path>`) from short socket name (`-L <name>`).
        let flag = if Path::new(endpoint).is_absolute() { "-S" } else { "-L" };
        return Some(format!(
            "tmux {flag} {display} attach -t {}:{}",
            session_name.as_str(),
            window_name
        ));
    }
    attach_command_for_workspace(workspace, session_name, window_name)
}

pub(crate) fn attach_command_for_runtime_state_session_or_workspace(
    workspace: &Path,
    state: Option<&serde_json::Value>,
    session_name: &SessionName,
) -> Option<String> {
    if let Some((endpoint, _source)) = runtime_tmux_endpoint_from_state(state) {
        return Some(attach_command_for_endpoint_session(endpoint, session_name));
    }
    attach_command_for_session(workspace, session_name)
}

fn attach_command_for_endpoint_session(endpoint: &str, session_name: &SessionName) -> String {
    let flag = if Path::new(endpoint).is_absolute() {
        "-S"
    } else {
        "-L"
    };
    format!("tmux {flag} {endpoint} attach -t {}", session_name.as_str())
}

pub(crate) fn attach_commands_for_windows<'a>(
    workspace: &Path,
    session_name: &SessionName,
    window_names: impl IntoIterator<Item = &'a str>,
) -> Vec<String> {
    window_names
        .into_iter()
        .filter_map(|window_name| attach_command_for_workspace(workspace, session_name, window_name))
        .collect()
}

pub(crate) fn tmux_socket_roots() -> Vec<PathBuf> {
    // Batch 1: `/tmp/tmux-<uid>` root enumeration is Unix-only tmux
    // convention. On Windows return empty so the caller loops zero
    // times (honest "no tmux socket roots" — tmux is not deployed on
    // native Windows without WSL, and WSL is out of scope).
    #[cfg(unix)]
    {
        let uid = unsafe { libc::geteuid() };
        let mut roots = vec![PathBuf::from(format!("/tmp/tmux-{uid}"))];
        if let Some(tmpdir) = std::env::var_os("TMPDIR") {
            roots.push(PathBuf::from(tmpdir).join(format!("tmux-{uid}")));
        }
        roots.sort();
        roots.dedup();
        roots
    }
    #[cfg(not(unix))]
    {
        Vec::new()
    }
}

pub(crate) fn tmux_socket_endpoints() -> Vec<String> {
    let mut endpoints = Vec::new();
    for root in tmux_socket_roots() {
        let Ok(entries) = std::fs::read_dir(root) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            // Batch 1: `FileTypeExt::is_socket` is Unix-only. Dead code
            // on Windows because `tmux_socket_roots()` returns empty
            // there (no /tmp/tmux-<uid> convention). We still cfg the
            // method call so `cargo check --target x86_64-pc-windows-msvc`
            // sees no `std::os::unix::fs::FileTypeExt` reference.
            #[cfg(unix)]
            {
                if !file_type.is_socket() {
                    continue;
                }
            }
            #[cfg(not(unix))]
            {
                let _ = file_type;
                continue;
            }
            let path = entry.path();
            let path = path.canonicalize().unwrap_or(path);
            endpoints.push(path.to_string_lossy().to_string());
        }
    }
    endpoints.sort();
    endpoints.dedup();
    endpoints
}

pub(crate) fn socket_name_from_tmux_env() -> Option<String> {
    let tmux = std::env::var("TMUX")
        .ok()
        .filter(|value| !value.is_empty())?;
    let socket_path = tmux.split(',').next().unwrap_or("").trim();
    if socket_path.is_empty() || !Path::new(socket_path).is_absolute() {
        return None;
    }
    Some(socket_path.to_string())
}

/// Deterministic FNV-1a (64-bit) — std `DefaultHasher` is NOT stable across releases, so a fixed
/// FNV keeps the socket identical for the CLI, the daemon, and every later op on the same workspace.
struct Fnv1a(u64);

impl Default for Fnv1a {
    fn default() -> Self {
        Self(0xcbf2_9ce4_8422_2325)
    }
}

impl Hasher for Fnv1a {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 ^= u64::from(b);
            self.0 = self.0.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
}

impl Default for TmuxBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl TmuxBackend {
    fn spawn(
        &self,
        session: &SessionName,
        window: &WindowName,
        argv: &[String],
        cwd: &Path,
        env: &BTreeMap<String, String>,
        env_unset: &[String],
        first: bool,
    ) -> Result<SpawnResult, TransportError> {
        let command = shell_command(argv, cwd, env, env_unset);
        self.spawn_with_command(session, window, &command, first)
    }

    /// 0.4.x (CR C-2): spawn variant that takes a pre-built shell command
    /// (used by `spawn_first_with_leader_shell_wrapper` /
    /// `spawn_into_with_leader_shell_wrapper` to inject the leader wrapper
    /// shape without going through `shell_command`'s `exec`-only template).
    fn spawn_with_command(
        &self,
        session: &SessionName,
        window: &WindowName,
        command: &str,
        first: bool,
    ) -> Result<SpawnResult, TransportError> {
        let spawn_argv = tmux_spawn_argv(session, window, command, first);
        self.run_spawn(&spawn_argv)?;
        let pane_argv = vec![
            "tmux".to_string(),
            "display-message".to_string(),
            "-p".to_string(),
            "-t".to_string(),
            format!("{}:{}", session.as_str(), window.as_str()),
            "#{pane_id}".to_string(),
        ];
        let output = self.run_spawn(&pane_argv)?;
        let pane = output.stdout.trim();
        // T3-5 (harvest §1): never fabricate a `%0` pane id on an empty reply — a fake
        // pane id mis-addresses every later inject/capture/kill. Surface the miss.
        if pane.is_empty() {
            return Err(TransportError::Subprocess {
                argv: pane_argv,
                code: output.code,
                stderr: format!(
                    "tmux display-message returned no pane id for {}:{}",
                    session.as_str(),
                    window.as_str()
                ),
            });
        }
        let pane_id = PaneId::new(pane);
        let targets = self.list_targets()?;
        if let Some(target) = targets.iter().find(|target| {
            target.pane_id == pane_id
                && target.session.as_str() == session.as_str()
                && target
                    .window_name
                    .as_ref()
                    .is_some_and(|name| name.as_str() == window.as_str())
        }) {
            return Ok(SpawnResult {
                pane_id,
                session: session.clone(),
                window: window.clone(),
                child_pid: target.pane_pid,
            });
        }
        let observed = targets
            .iter()
            .find(|target| target.pane_id == pane_id)
            .map(|target| {
                format!(
                    "{}:{}",
                    target.session.as_str(),
                    target
                        .window_name
                        .as_ref()
                        .map(WindowName::as_str)
                        .unwrap_or("<unknown>")
                )
            })
            .unwrap_or_else(|| "<missing-from-list-targets>".to_string());
        Err(TransportError::Subprocess {
            argv: pane_argv,
            code: output.code,
            stderr: format!(
                "tmux spawn pane ownership mismatch: requested={}:{} observed_pane={} observed={}",
                session.as_str(),
                window.as_str(),
                pane_id.as_str(),
                observed
            ),
        })
    }

    fn spawn_split(
        &self,
        session: &SessionName,
        window: &WindowName,
        argv: &[String],
        cwd: &Path,
        env: &BTreeMap<String, String>,
        env_unset: &[String],
    ) -> Result<SpawnResult, TransportError> {
        let command = shell_command(argv, cwd, env, env_unset);
        let target = format!("{}:{}", session.as_str(), window.as_str());
        // E53 (0.3.26, adaptive layout same-session tabs): `-d` prevents the
        // new split pane from stealing focus from the leader's active pane.
        // Same rationale as the `-d` on `new-window` in transport.rs; for
        // adaptive layout the leader and all workers share the same tmux
        // session, and every focus-stealing spawn is a disruption.
        let split_argv = vec![
            "tmux".to_string(),
            "split-window".to_string(),
            "-d".to_string(),
            "-t".to_string(),
            target.clone(),
            "-h".to_string(),
            "-P".to_string(),
            "-F".to_string(),
            "#{pane_id}".to_string(),
            "sh".to_string(),
            "-lc".to_string(),
            command,
        ];
        let output = self.run_spawn(&split_argv)?;
        let pane = output.stdout.trim();
        if pane.is_empty() {
            return Err(TransportError::Subprocess {
                argv: split_argv,
                code: output.code,
                stderr: format!("tmux split-window returned no pane id for {target}"),
            });
        }
        let layout_argv = vec![
            "tmux".to_string(),
            "select-layout".to_string(),
            "-t".to_string(),
            target,
            "even-horizontal".to_string(),
        ];
        self.run_ok(&layout_argv)?;
        Ok(SpawnResult {
            pane_id: PaneId::new(pane),
            session: session.clone(),
            window: window.clone(),
            child_pid: None,
        })
    }

    fn run_ok(&self, argv: &[String]) -> Result<(), TransportError> {
        let argv = self.tmux_argv(argv);
        let output = self.runner.run(&argv)?;
        if output.success {
            Ok(())
        } else {
            Err(subprocess_error(argv, output))
        }
    }

    fn run_spawn(&self, argv: &[String]) -> Result<CommandOutput, TransportError> {
        let argv = self.tmux_argv(argv);
        let output = self
            .runner
            .run(&argv)
            .map_err(|source| TransportError::Spawn {
                backend: BackendKind::Tmux,
                source,
            })?;
        if output.success {
            Ok(output)
        } else {
            Err(subprocess_error(argv, output))
        }
    }

    fn run_inject_stage(&self, argv: &[String], stage: InjectStage) -> Result<(), TransportError> {
        let argv = self.tmux_argv(argv);
        let output = self
            .runner
            .run(&argv)
            .map_err(|source| TransportError::Inject { stage, source })?;
        if output.success {
            Ok(())
        } else {
            Err(subprocess_error(argv, output))
        }
    }

    fn run_inject_stage_with_stdin(
        &self,
        argv: &[String],
        stage: InjectStage,
        stdin: &str,
    ) -> Result<(), TransportError> {
        let argv = self.tmux_argv(argv);
        let output = self
            .runner
            .run_with_stdin(&argv, stdin)
            .map_err(|source| TransportError::Inject { stage, source })?;
        if output.success {
            Ok(())
        } else {
            Err(subprocess_error(argv, output))
        }
    }
}

fn subprocess_error(argv: Vec<String>, output: CommandOutput) -> TransportError {
    TransportError::Subprocess {
        argv,
        code: output.code,
        stderr: output.stderr,
    }
}

fn pane_from_target(target: &Target) -> PaneId {
    match target {
        Target::Pane(pane) => pane.clone(),
        Target::SessionWindow { session, window } => {
            PaneId::new(format!("{}:{}", session.as_str(), window.as_str()))
        }
    }
}

fn target_name(target: &Target) -> String {
    match target {
        Target::Pane(pane) => pane.as_str().to_string(),
        Target::SessionWindow { session, window } => {
            format!("{}:{}", session.as_str(), window.as_str())
        }
    }
}

fn inject_stage_for_argv(argv: &[String]) -> InjectStage {
    match argv.get(1).map(String::as_str) {
        Some("set-buffer") => InjectStage::SetBuffer,
        Some("load-buffer") => InjectStage::LoadBuffer,
        Some("paste-buffer") => InjectStage::PasteBuffer,
        Some("delete-buffer") => InjectStage::DeleteBuffer,
        Some("send-keys") => InjectStage::Submit,
        _ => InjectStage::Submit,
    }
}

fn pane_mode_from_raw(raw: Option<String>) -> Option<PaneMode> {
    match raw.as_deref().map(str::trim) {
        Some("") | Some("0") => None,
        Some("copy-mode") => Some(PaneMode::Copy),
        Some("tree-mode") => Some(PaneMode::Tree),
        Some("view-mode") => Some(PaneMode::View),
        Some("client-mode") => Some(PaneMode::Client),
        _ => Some(PaneMode::Unknown),
    }
}

fn buffer_name_for_text(text: &str) -> String {
    const PREFIX: &str = "[team-agent-token:";
    match text.find(PREFIX) {
        Some(prefix_start) => {
            let token_start = prefix_start.saturating_add(PREFIX.len());
            let Some(rest) = text.get(token_start..) else {
                return "team-agent-buf".to_string();
            };
            let Some(token_end) = rest.find(']') else {
                return "team-agent-buf".to_string();
            };
            let Some(token) = rest.get(..token_end).filter(|s| !s.is_empty()) else {
                return "team-agent-buf".to_string();
            };
            format!("team-agent-send-{token}")
        }
        None => "team-agent-buf".to_string(),
    }
}

fn inject_verification_for_payload(payload: &InjectPayload) -> InjectVerification {
    match payload {
        InjectPayload::Empty => InjectVerification::EmptyTextSendKeys,
        InjectPayload::Text(text) | InjectPayload::TextSkipConsumptionPoll(text)
            if text.contains("[team-agent-token:") =>
        {
            InjectVerification::CaptureContainsToken
        }
        InjectPayload::Text(_) | InjectPayload::TextSkipConsumptionPoll(_) => InjectVerification::NoToken,
    }
}

/// U1 #7: the exact delivery-token marker a token payload carries
/// (`[team-agent-token:<id>]`). Use the full marker, not only the prefix, so an old
/// scrollback token cannot verify a new message.
fn payload_token_marker(payload: &InjectPayload) -> Option<&str> {
    let text = payload.text()?;
    let start = text.find("[team-agent-token:")?;
    let marker = &text[start..];
    let end = marker.find(']')?;
    Some(&marker[..=end])
}

fn token_visible_in_capture(
    backend: &TmuxBackend,
    target: &Target,
    payload: &InjectPayload,
) -> Result<Option<bool>, TransportError> {
    match payload_token_marker(payload) {
        None => Ok(None),
        Some(marker) => {
            let captured = backend.capture(target, CaptureRange::Tail(80))?;
            Ok(Some(captured.text.contains(marker)))
        }
    }
}

/// U1 #7 / E31: wait briefly for the just-pasted token marker before submitting.
/// `Ok(None)` for non-token payloads (nothing to check). `Ok(Some(false))` means the
/// paste did not become visible before the Python-parity fallback delay.
fn pre_submit_token_visible(
    backend: &TmuxBackend,
    target: &Target,
    payload: &InjectPayload,
) -> Result<Option<bool>, TransportError> {
    if payload_token_marker(payload).is_none() {
        return Ok(None);
    }
    for attempt in 0..PASTED_CONTENT_APPEAR_POLLS {
        if let Some(true) = token_visible_in_capture(backend, target, payload)? {
            return Ok(Some(true));
        }
        if attempt + 1 < PASTED_CONTENT_APPEAR_POLLS {
            std::thread::sleep(Duration::from_millis(25));
        }
    }
    // Python waits 250ms between paste-buffer and Enter to let bracketed paste settle.
    std::thread::sleep(Duration::from_millis(250));
    Ok(Some(false))
}

const TOKEN_POST_SUBMIT_READBACK_POLLS: u32 = 5;

/// Some non-echo panes, including the integration harness' `stty -echo; cat`, only
/// render the injected line after the submit key. If pre-submit readback missed the
/// token, do a bounded post-submit check before reporting `CaptureMissingToken`.
fn post_submit_token_visible(
    backend: &TmuxBackend,
    target: &Target,
    payload: &InjectPayload,
) -> Result<Option<bool>, TransportError> {
    if payload_token_marker(payload).is_none() {
        return Ok(None);
    }
    for attempt in 0..TOKEN_POST_SUBMIT_READBACK_POLLS {
        if let Some(true) = token_visible_in_capture(backend, target, payload)? {
            return Ok(Some(true));
        }
        if attempt + 1 < TOKEN_POST_SUBMIT_READBACK_POLLS {
            std::thread::sleep(Duration::from_millis(25));
        }
    }
    Ok(Some(false))
}

/// U1 #7: downgrade the static token verification to `CaptureMissingToken` when the
/// pre-submit readback did not see the token in the pane. A `None` readback (non-token
/// payload, or capture unavailable) falls back to the static verification.
fn inject_verification_after_readback(
    payload: &InjectPayload,
    token_visible_before_submit: Option<bool>,
) -> InjectVerification {
    match (payload, token_visible_before_submit) {
        (_, Some(visible)) if payload_token_marker(payload).is_some() => {
            if visible {
                InjectVerification::CaptureContainsToken
            } else {
                InjectVerification::CaptureMissingToken
            }
        }
        _ => inject_verification_for_payload(payload),
    }
}

fn submit_verification_for_key(key: Key) -> SubmitVerification {
    match key {
        Key::Enter => SubmitVerification::EnterSentWithoutPlaceholderCheck,
        other => SubmitVerification::KeySentAfterVisibleToken { key: other },
    }
}

fn capture_has_pasted_content_prompt(text: &str) -> bool {
    pasted_prompt_match(text).is_some()
}

/// 0.3.27: check if a token marker is present in the bottom N non-empty lines.
/// Used by the unified submit_and_verify to detect whether the provider consumed
/// the pasted message (token scrolls out of composer region on successful submit).
fn token_in_bottom_n(text: &str, marker: &str, n: usize) -> bool {
    text.lines()
        .rev()
        .filter(|line| !line.trim().is_empty())
        .take(n)
        .any(|line| line.contains(marker))
}

fn marker_position_from_bottom(text: &str, marker: &str) -> Option<u32> {
    let mut from_bottom = 0u32;
    for line in text.lines().rev().filter(|line| !line.trim().is_empty()) {
        if line.contains(marker) {
            return Some(from_bottom);
        }
        from_bottom = from_bottom.saturating_add(1);
    }
    None
}

fn provider_busy_signal_in_tail(text: &str) -> bool {
    text.lines()
        .rev()
        .filter(|line| !line.trim().is_empty())
        .take(15)
        .any(|line| {
            let lower = line.to_ascii_lowercase();
            lower.contains("working")
                || lower.contains("thinking")
                || lower.contains("esc to interrupt")
                || line.contains('●')
                || line.contains('⏳')
                || line.contains('⠋')
                || line.contains('⠙')
                || line.contains('⠹')
                || line.contains('⠸')
                || line.contains('⠼')
                || line.contains('⠴')
                || line.contains('⠦')
                || line.contains('⠧')
                || line.contains('⠇')
                || line.contains('⠏')
                || line.contains('✶')
                || line.contains('✢')
        })
}

fn submit_attempt_observation(
    attempt_index: u32,
    captured: &CapturedText,
    marker: Option<&str>,
    elapsed_ms: u64,
) -> SubmitAttemptObservation {
    let marker_position = marker.and_then(|m| marker_position_from_bottom(&captured.text, m));
    let (matched, matched_literal, where_in_tail) = if let Some(marker) = marker {
        (
            token_in_bottom_n(&captured.text, marker, 15),
            marker_position.map(|_| marker.to_string()),
            marker_position,
        )
    } else if let Some((literal, where_in_tail)) = pasted_prompt_match(&captured.text) {
        (true, Some(literal.to_string()), Some(where_in_tail))
    } else {
        (false, None, None)
    };
    let (pane_tail_excerpt, pane_tail_lines) = scrub_pane_excerpt(&captured.text, 20);
    SubmitAttemptObservation {
        attempt_index,
        matched,
        matched_literal,
        where_in_tail,
        pane_tail_excerpt,
        pane_tail_lines,
        elapsed_ms,
    }
}

/// 0.3.27: check if a pasted-content prompt literal (`pasted content` / `pasted text`)
/// appears in the bottom N non-empty lines. Narrower than the full-Tail(80) check
/// that caused scrollback ghost matches (E50 defect B).
fn pasted_prompt_in_bottom(text: &str, n: usize) -> bool {
    text.lines()
        .rev()
        .filter(|line| !line.trim().is_empty())
        .take(n)
        .any(|line| {
            let lower = line.to_ascii_lowercase();
            lower.contains("pasted content") || lower.contains("pasted text")
        })
}

/// E50 PR-1 (0.3.24 P0, pasted-prompt 假阴诊断): factor `capture_has_pasted_content_prompt`
/// so the diagnostic layer can recover the MATCHED LITERAL and its position
/// in the tail. Returns `(literal, line_index_from_bottom)` on a match.
/// Byte-identical match semantics — `capture_has_pasted_content_prompt`'s
/// `bool` wrapper preserves the legacy `true/false` contract for the three
/// existing callers (the appear-gate poll at :1122-1128 + the legacy submit
/// loop matcher at :1138 + post-flip clearer at :1138).
///
/// **Where-in-tail rationale**: a `pasted content` literal that lives in
/// the SCROLLBACK (line 6+ from bottom) is NOT the live composer
/// placeholder — codex's successful submit scrolls the block into history
/// where it remains in the last 80 lines. The current matcher cannot
/// distinguish that from a live placeholder; this fn surfaces the data
/// the operator needs to see (PR-2 will USE it to fix the criterion).
pub(crate) fn pasted_prompt_match(text: &str) -> Option<(&'static str, u32)> {
    let lower = text.to_ascii_lowercase();
    let lit = if lower.contains("pasted content") {
        "pasted content"
    } else if lower.contains("pasted text") {
        "pasted text"
    } else {
        return None;
    };
    // Distance from the bottom of the tail in non-empty lines.
    let non_empty: Vec<&str> = text.lines().filter(|line| !line.trim().is_empty()).collect();
    let mut from_bottom: u32 = 0;
    for line in non_empty.iter().rev() {
        if line.to_ascii_lowercase().contains(lit) {
            return Some((lit, from_bottom));
        }
        from_bottom = from_bottom.saturating_add(1);
    }
    // Literal appeared only in trimmed-away whitespace lines — treat as
    // bottom (defensive; rare).
    Some((lit, 0))
}

/// E50 PR-1 (0.3.24 P0): scrub a pane capture for safe inclusion in
/// `events.jsonl`. Steps:
///   1. Strip CSI / OSC ANSI escapes.
///   2. Take the bottom `tail_lines` of non-empty lines.
///   3. Redact common secret shapes (sk-, ghp_, AKIA, Bearer ..., 32+ hex).
///   4. Cap at ~1200 bytes (UTF-8 safe truncation).
/// Returns `(excerpt, line_count)`. Designed to be CHEAP — no regex crate
/// dependency, simple byte scanning.
pub(crate) fn scrub_pane_excerpt(raw: &str, tail_lines: usize) -> (String, u32) {
    let stripped = strip_ansi_escapes_inplace(raw);
    let lines: Vec<&str> = stripped
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    let tail = if lines.len() > tail_lines {
        &lines[lines.len() - tail_lines..]
    } else {
        &lines[..]
    };
    let mut out = tail
        .iter()
        .map(|line| scrub_secrets(line))
        .collect::<Vec<_>>()
        .join("\n");
    if out.len() > 1200 {
        // Truncate at UTF-8 char boundary.
        let mut cut = 1200;
        while cut > 0 && !out.is_char_boundary(cut) {
            cut -= 1;
        }
        out.truncate(cut);
        out.push_str("…[truncated]");
    }
    (out, tail.len() as u32)
}

fn strip_ansi_escapes_inplace(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let bytes = input.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == 0x1b && i + 1 < bytes.len() {
            // CSI: ESC [ ... <final byte 0x40-0x7e>
            if bytes[i + 1] == b'[' {
                let mut j = i + 2;
                while j < bytes.len() && !(0x40..=0x7e).contains(&bytes[j]) {
                    j += 1;
                }
                i = j.saturating_add(1).min(bytes.len());
                continue;
            }
            // OSC: ESC ] ... BEL or ESC \
            if bytes[i + 1] == b']' {
                let mut j = i + 2;
                while j < bytes.len() {
                    if bytes[j] == 0x07 {
                        j += 1;
                        break;
                    }
                    if bytes[j] == 0x1b && j + 1 < bytes.len() && bytes[j + 1] == b'\\' {
                        j += 2;
                        break;
                    }
                    j += 1;
                }
                i = j.min(bytes.len());
                continue;
            }
            // Other single-char ESC sequence — skip ESC + next byte.
            i += 2;
            continue;
        }
        let ch = bytes[i];
        out.push(ch as char);
        i += 1;
    }
    // Re-decode from bytes to recover UTF-8 (since we pushed bytes as chars,
    // multi-byte UTF-8 is preserved correctly because we only skip on the
    // single-byte ESC start). For pane text this is good enough; pathological
    // UTF-8 inside CSI parameter bytes is invalid anyway.
    out
}

fn scrub_secrets(line: &str) -> String {
    // Five shapes: sk-XXXX, ghp_XXXX, AKIAXXXX (16-char uppercase id), Bearer XXXX,
    // 32+ hex (token).
    let mut out = String::with_capacity(line.len());
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        // sk- / ghp_ / AKIA prefixes: detect and redact through end-of-token.
        if matches_prefix(bytes, i, b"sk-") || matches_prefix(bytes, i, b"ghp_") {
            let prefix_len = if bytes[i] == b's' { 3 } else { 4 };
            let token_end = scan_token_end(bytes, i + prefix_len);
            out.push_str(&line[i..i + prefix_len]);
            out.push_str("REDACTED");
            i = token_end;
            continue;
        }
        if matches_prefix(bytes, i, b"AKIA") {
            let token_end = scan_token_end(bytes, i + 4);
            out.push_str("AKIA");
            out.push_str("REDACTED");
            i = token_end;
            continue;
        }
        if matches_prefix_case_insensitive(bytes, i, b"Bearer ") {
            let token_end = scan_token_end(bytes, i + 7);
            out.push_str(&line[i..i + 7]);
            out.push_str("REDACTED");
            i = token_end;
            continue;
        }
        // 32+ hex run.
        if is_hex_byte(bytes[i]) {
            let mut j = i;
            while j < bytes.len() && is_hex_byte(bytes[j]) {
                j += 1;
            }
            if j - i >= 32 {
                out.push_str("REDACTED_HEX");
                i = j;
                continue;
            }
        }
        // Default: passthrough byte.
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

fn matches_prefix(bytes: &[u8], i: usize, prefix: &[u8]) -> bool {
    bytes.get(i..i + prefix.len()).is_some_and(|s| s == prefix)
}

fn matches_prefix_case_insensitive(bytes: &[u8], i: usize, prefix: &[u8]) -> bool {
    bytes
        .get(i..i + prefix.len())
        .is_some_and(|s| s.eq_ignore_ascii_case(prefix))
}

fn scan_token_end(bytes: &[u8], start: usize) -> usize {
    let mut j = start;
    while j < bytes.len() && is_token_byte(bytes[j]) {
        j += 1;
    }
    j
}

fn is_token_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'-' || b == b'_'
}

fn is_hex_byte(b: u8) -> bool {
    b.is_ascii_hexdigit()
}

const PASTED_CONTENT_APPEAR_POLLS: u32 = 5;
const PASTED_CONTENT_SUBMIT_ATTEMPTS: u32 = 3;

/// E46 (0.3.24 bug#5): bounded resend cap for post-Enter consumption probe.
/// Mirrors PASTED_CONTENT_SUBMIT_ATTEMPTS shape: try Enter then bounded
/// re-checks of the pane's input region. Each iteration first re-checks that
/// the input still has content before resending Enter — guards against double
/// submission when the first Enter was consumed but our readback was slow.
const POST_SUBMIT_CONSUMPTION_ATTEMPTS: u32 = 3;
const POST_SUBMIT_CONSUMPTION_POLL_MS: u64 = 60;

/// E46 (0.3.24 bug#5, C5 provider-agnostic detector): the pane's input region
/// is "consumed" when the token text that was just visible BEFORE the Enter
/// is no longer present in the captured tail. Structural signal — no
/// provider-specific UI string. Works across claude / codex / copilot because
/// every provider's composer clears the input area after a successful submit
/// (the content scrolls into history, leaving the prompt empty).
///
/// Returns:
///   * `Some(true)`  — token was visible BEFORE submit and is GONE from
///     the visible input area now → consumption confirmed.
///   * `Some(false)` — token still visible (or other reason to think not yet
///     consumed).
///   * `None` — payload has no token marker (peer message without token,
///     empty payload) so we can't structurally check; caller treats this as
///     non-blocking (the pre-existing `EnterSentWithoutPlaceholderCheck`
///     path).
fn post_submit_input_consumed(
    backend: &TmuxBackend,
    target: &Target,
    payload: &InjectPayload,
) -> Result<Option<bool>, TransportError> {
    let Some(marker) = payload_token_marker(payload) else {
        return Ok(None);
    };
    let captured = backend.capture(target, CaptureRange::Tail(40))?;
    // The token may legitimately appear in scrollback (a successful submit
    // pushes it into history). We only treat the BOTTOM-of-pane region (last
    // few lines, where the input area lives) as the consumption signal. Tail
    // 30 lines is small enough that the input area still dominates if the
    // submit didn't go through, while a successful submit has pushed the
    // token marker out of the bottom 15 lines by the time the response
    // composer redraws.
    let tail_lines: Vec<&str> = captured.text.lines().rev().take(15).collect();
    let token_in_tail = tail_lines.iter().any(|line| line.contains(marker));
    Ok(Some(!token_in_tail))
}

fn shell_command(
    argv: &[String],
    cwd: &Path,
    env: &BTreeMap<String, String>,
    env_unset: &[String],
) -> String {
    let unset_set: std::collections::BTreeSet<&str> =
        env_unset.iter().map(String::as_str).collect();
    let mut parts = Vec::new();
    parts.push("cd".to_string());
    parts.push(shell_quote(&cwd.to_string_lossy()));
    parts.push("&&".to_string());
    // D9 (#264) / Python providers.py:142-145 + provider_env.py:86 — profile env_unset keys
    // must be unset in the shell itself: the `sh -lc` line inherits the tmux SERVER's stale
    // environment, which exec-prefix assignments cannot clear.
    for key in env_unset {
        parts.push("unset".to_string());
        parts.push(key.clone());
        parts.push("&&".to_string());
    }
    // 0.4.x ordering fix (env-leak symptom #3): KEY=val exports must NOT
    // re-introduce any key that was just unset. Filter env entries whose key
    // appears in env_unset so the unset wins on the final shell line. This
    // matters when inherited env (worker_spawn_env / apply_profile_launch_env)
    // contains the very keys we want to scrub (e.g. CLAUDE_EFFORT carried
    // forward from the launching shell into the env map).
    for (key, value) in env {
        if unset_set.contains(key.as_str()) {
            continue;
        }
        parts.push(format!("{key}={}", shell_quote(value)));
    }
    parts.push("exec".to_string());
    parts.extend(argv.iter().map(|arg| shell_quote(arg)));
    parts.join(" ")
}

/// 0.4.x (CR R6): single-source marker prefix. The exit marker emitted by
/// `leader_shell_wrapper_command` and the substring detected by
/// `leader_provider_health` MUST share this prefix exactly. Format:
/// `"[team-agent] {provider_label} exited with {rc}"`.
pub const LEADER_PROVIDER_EXIT_MARKER_PREFIX: &str = "[team-agent]";
pub const LEADER_PROVIDER_EXIT_MARKER_SUFFIX: &str = "exited with";

/// 0.4.x (CR R6): build the leader exit marker text for `provider_label`.
/// Used by both the shell wrapper (printf source) and the health check
/// (capture substring) so they cannot drift.
pub fn leader_provider_exit_marker(provider_label: &str) -> String {
    format!(
        "{LEADER_PROVIDER_EXIT_MARKER_PREFIX} {provider_label} {LEADER_PROVIDER_EXIT_MARKER_SUFFIX}"
    )
}

/// 0.5.39 Slice 1 (tmux-server-death-locate §7 Slice 1): ambient-tmux
/// leader-pane probe. Kept inside `tmux_backend` because it is
/// definitionally ambient — its job is to discover *which* session/pane
/// the leader process is currently inside via $TMUX/$TMUX_PANE +
/// `tmux display-message`. Everywhere else in the codebase, tmux ops
/// must go through a socket-scoped `TmuxBackend` (that constraint is
/// enforced by `n16_tmux_socket_invariant_red.rs` +
/// `tmux_server_death_0539_contract.rs::display_cleanup_...`); this
/// helper is the single controlled exception.
///
/// Returns `(session_name, Some(pane_id))` when the ambient tmux
/// responds, or `None` if `$TMUX` is unset / `display-message` fails.
pub fn probe_ambient_leader_pane_info() -> Option<(String, Option<String>)> {
    let pane = std::env::var("TMUX_PANE")
        .ok()
        .filter(|value| !value.is_empty());
    let mut commands: Vec<Vec<String>> = Vec::new();
    if let Some(pane) = pane.as_deref() {
        commands.push(vec![
            "display-message".to_string(),
            "-p".to_string(),
            "-t".to_string(),
            pane.to_string(),
            "-F".to_string(),
            "#{session_name}\t#{pane_id}".to_string(),
        ]);
        commands.push(vec![
            "display-message".to_string(),
            "-p".to_string(),
            "-t".to_string(),
            pane.to_string(),
            "-F".to_string(),
            "#{session_name}".to_string(),
        ]);
    }
    if std::env::var("TMUX").is_ok_and(|value| !value.is_empty()) {
        commands.push(vec![
            "display-message".to_string(),
            "-p".to_string(),
            "-F".to_string(),
            "#{session_name}\t#{pane_id}".to_string(),
        ]);
        commands.push(vec![
            "display-message".to_string(),
            "-p".to_string(),
            "-F".to_string(),
            "#{session_name}".to_string(),
        ]);
    }
    for command in commands {
        let output = match std::process::Command::new("tmux").args(&command).output() {
            Ok(output) if output.status.success() => output,
            _ => continue,
        };
        let stdout = String::from_utf8_lossy(&output.stdout);
        let Some(line) = stdout.lines().find(|line| !line.trim().is_empty()) else {
            continue;
        };
        let line = line.trim();
        let parts: Vec<&str> = line.split('\t').collect();
        let parsed = match parts.as_slice() {
            [session, pane_str, ..] if !session.is_empty() && !session.starts_with('%') => Some((
                (*session).to_string(),
                (!pane_str.is_empty()).then(|| (*pane_str).to_string()),
            )),
            [pane_str, session, ..] if pane_str.starts_with('%') && !session.is_empty() => {
                Some(((*session).to_string(), Some((*pane_str).to_string())))
            }
            [session] if !session.is_empty() && !session.starts_with('%') => {
                Some(((*session).to_string(), None))
            }
            _ => None,
        };
        if parsed.is_some() {
            return parsed;
        }
    }
    None
}

/// 0.5.39 Slice 2 (tmux-server-death-locate §11.2): single-source worker
/// exit marker prefix. Same envelope shape as `LEADER_PROVIDER_EXIT_MARKER_*`
/// but distinct so status/classifier code can tell "leader pane fell back
/// to shell" from "worker pane fell back to shell". Format:
/// `"[team-agent worker] {provider_label} exited with {rc}"`.
pub const WORKER_PROVIDER_EXIT_MARKER_PREFIX: &str = "[team-agent worker]";
pub const WORKER_PROVIDER_EXIT_MARKER_SUFFIX: &str = "exited with";

/// 0.5.39 Slice 2: build the worker exit marker text for `provider_label`.
/// Used by both the worker shell wrapper (printf source) and future
/// status/classifier code (capture substring) so they cannot drift.
pub fn worker_provider_exit_marker(provider_label: &str) -> String {
    format!(
        "{WORKER_PROVIDER_EXIT_MARKER_PREFIX} {provider_label} {WORKER_PROVIDER_EXIT_MARKER_SUFFIX}"
    )
}

/// 0.5.39 Slice 2 (tmux-server-death-locate §7 Slice 2): worker shell
/// wrapper. Same shape as `leader_shell_wrapper_command` — provider runs
/// as a CHILD of a long-lived shell so provider exit does NOT collapse the
/// worker pane (which under upstream tmux 3.6a private-server bugs can
/// cascade into whole-server death). When the provider exits, the worker
/// pane returns to an interactive shell with an explicit worker exit
/// marker, matching manual `tmux new-window` then `<provider>` behaviour.
pub fn worker_shell_wrapper_command(
    argv: &[String],
    cwd: &Path,
    env: &BTreeMap<String, String>,
    env_unset: &[String],
    provider_label: &str,
) -> String {
    let unset_set: std::collections::BTreeSet<&str> =
        env_unset.iter().map(String::as_str).collect();
    let mut parts = Vec::new();
    parts.push("cd".to_string());
    parts.push(shell_quote(&cwd.to_string_lossy()));
    parts.push("&&".to_string());
    for key in env_unset {
        parts.push("unset".to_string());
        parts.push(key.clone());
        parts.push("&&".to_string());
    }
    for (key, value) in env {
        if unset_set.contains(key.as_str()) {
            continue;
        }
        parts.push(format!("{key}={}", shell_quote(value)));
    }
    parts.extend(argv.iter().map(|arg| shell_quote(arg)));
    parts.push(";".to_string());
    parts.push("rc=$?;".to_string());
    parts.push("printf".to_string());
    parts.push(shell_quote(&format!(
        "\n{} %s\n",
        worker_provider_exit_marker(provider_label)
    )));
    parts.push("\"$rc\";".to_string());
    parts.push("exec".to_string());
    parts.push("\"${SHELL:-/bin/zsh}\"".to_string());
    parts.push("-l".to_string());
    parts.join(" ")
}

/// 0.4.x (CR C-2): leader shell wrapper — provider runs as a CHILD of a
/// long-lived shell, not as the pane's primary process. When the provider
/// exits, the pane returns to an interactive shell with an explicit exit
/// marker, matching manual `tmux new-session` then `claude` behaviour.
///
/// Four required envelope sections (CR C-2):
///   1. cd <cwd>                    — same as `shell_command`
///   2. unset <KEY> ...             — provider env_unset block
///   3. KEY=val ... <provider>      — env exports + provider invocation
///                                    (NO `exec` — runs as child)
///   4. printf exit marker; exec shell -l
///
/// `provider_label` is a human-readable provider name (e.g. "claude",
/// "codex") embedded in the exit marker for diagnostics.
pub fn leader_shell_wrapper_command(
    argv: &[String],
    cwd: &Path,
    env: &BTreeMap<String, String>,
    env_unset: &[String],
    provider_label: &str,
) -> String {
    let unset_set: std::collections::BTreeSet<&str> =
        env_unset.iter().map(String::as_str).collect();
    let mut parts = Vec::new();
    // 1. cd
    parts.push("cd".to_string());
    parts.push(shell_quote(&cwd.to_string_lossy()));
    parts.push("&&".to_string());
    // 2. unset
    for key in env_unset {
        parts.push("unset".to_string());
        parts.push(key.clone());
        parts.push("&&".to_string());
    }
    // 3. env exports + provider (NO `exec` so the provider is a child).
    // 0.4.x ordering fix: skip keys present in env_unset so KEY=val does not
    // re-introduce a just-unset variable from the inherited env map.
    for (key, value) in env {
        if unset_set.contains(key.as_str()) {
            continue;
        }
        parts.push(format!("{key}={}", shell_quote(value)));
    }
    parts.extend(argv.iter().map(|arg| shell_quote(arg)));
    parts.push(";".to_string());
    // 4. exit marker + fall back to interactive shell
    parts.push("rc=$?;".to_string());
    parts.push("printf".to_string());
    // CR R6: marker text comes from single-source `leader_provider_exit_marker`.
    parts.push(shell_quote(&format!(
        "\n{} %s\n",
        leader_provider_exit_marker(provider_label)
    )));
    parts.push("\"$rc\";".to_string());
    parts.push("exec".to_string());
    parts.push("\"${SHELL:-/bin/zsh}\"".to_string());
    parts.push("-l".to_string());
    parts.join(" ")
}

fn shell_quote(raw: &str) -> String {
    if raw.is_empty() {
        return "''".to_string();
    }
    if raw
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '_' | '-' | ':' | '='))
    {
        return raw.to_string();
    }
    let mut quoted = String::from("'");
    for ch in raw.chars() {
        if ch == '\'' {
            quoted.push_str("'\\''");
        } else {
            quoted.push(ch);
        }
    }
    quoted.push('\'');
    quoted
}

fn notify_submit_observer(observer: Option<&dyn SubmitObserver>) {
    if let Some(observer) = observer {
        observer.after_physical_submit();
    }
}

impl Transport for TmuxBackend {
    fn kind(&self) -> BackendKind {
        BackendKind::Tmux
    }

    fn probes_real_tmux_socket_roots(&self) -> bool {
        true
    }

    fn tmux_endpoint(&self) -> Option<String> {
        self.socket
            .as_ref()
            .map(|endpoint| endpoint.as_endpoint().to_string())
    }

    fn spawn_first(
        &self,
        session: &SessionName,
        window: &WindowName,
        argv: &[String],
        cwd: &Path,
        env: &BTreeMap<String, String>,
    ) -> Result<SpawnResult, TransportError> {
        self.spawn(session, window, argv, cwd, env, &[], true)
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
        self.spawn(session, window, argv, cwd, env, env_unset, true)
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
        self.spawn(session, window, argv, cwd, env, env_unset, false)
    }

    fn spawn_into(
        &self,
        session: &SessionName,
        window: &WindowName,
        argv: &[String],
        cwd: &Path,
        env: &BTreeMap<String, String>,
    ) -> Result<SpawnResult, TransportError> {
        self.spawn(session, window, argv, cwd, env, &[], false)
    }

    fn spawn_split_with_env_unset(
        &self,
        session: &SessionName,
        window: &WindowName,
        argv: &[String],
        cwd: &Path,
        env: &BTreeMap<String, String>,
        env_unset: &[String],
    ) -> Result<SpawnResult, TransportError> {
        self.spawn_split(session, window, argv, cwd, env, env_unset)
    }

    /// 0.5.39 Slice 2: TmuxBackend override of the worker-shell-wrapper
    /// variant. Same mechanism as the leader wrapper (child provider under
    /// long-lived shell), but the marker text is distinct so downstream
    /// classifiers can tell leader vs worker provider exit apart.
    fn spawn_first_with_worker_shell_wrapper(
        &self,
        session: &SessionName,
        window: &WindowName,
        argv: &[String],
        cwd: &Path,
        env: &BTreeMap<String, String>,
        env_unset: &[String],
        provider_label: &str,
    ) -> Result<SpawnResult, TransportError> {
        let command = worker_shell_wrapper_command(argv, cwd, env, env_unset, provider_label);
        self.spawn_with_command(session, window, &command, true)
    }

    fn spawn_into_with_worker_shell_wrapper(
        &self,
        session: &SessionName,
        window: &WindowName,
        argv: &[String],
        cwd: &Path,
        env: &BTreeMap<String, String>,
        env_unset: &[String],
        provider_label: &str,
    ) -> Result<SpawnResult, TransportError> {
        let command = worker_shell_wrapper_command(argv, cwd, env, env_unset, provider_label);
        self.spawn_with_command(session, window, &command, false)
    }

    /// 0.4.x (CR C-2): TmuxBackend override of the leader-shell-wrapper
    /// variant. Builds the wrapper shell line via
    /// `leader_shell_wrapper_command` and runs it through
    /// `spawn_with_command` (bypassing the default `exec <cmd>` shape).
    fn spawn_first_with_leader_shell_wrapper(
        &self,
        session: &SessionName,
        window: &WindowName,
        argv: &[String],
        cwd: &Path,
        env: &BTreeMap<String, String>,
        env_unset: &[String],
        provider_label: &str,
    ) -> Result<SpawnResult, TransportError> {
        let command = leader_shell_wrapper_command(argv, cwd, env, env_unset, provider_label);
        self.spawn_with_command(session, window, &command, true)
    }

    fn spawn_into_with_leader_shell_wrapper(
        &self,
        session: &SessionName,
        window: &WindowName,
        argv: &[String],
        cwd: &Path,
        env: &BTreeMap<String, String>,
        env_unset: &[String],
        provider_label: &str,
    ) -> Result<SpawnResult, TransportError> {
        let command = leader_shell_wrapper_command(argv, cwd, env, env_unset, provider_label);
        self.spawn_with_command(session, window, &command, false)
    }

    fn inject(
        &self,
        target: &Target,
        payload: &InjectPayload,
        submit: Key,
        bracketed: bool,
    ) -> Result<InjectReport, TransportError> {
        // Trait entry delegates to inject_with_submit_observer with no
        // observer. Populated InjectReport fields: stage_reached,
        // inject_verification, submit_verification, turn_verification,
        // attempts, submit_diagnostics — the 0.5.43 debt-sweep
        // governance guard (debt_sweep_0543_contract.rs::
        // coordinator_debug_eprintlns_are_deleted_but_inject_report_shape_remains)
        // scans THIS function body for those field names so debug-
        // print cleanup can't silently drop InjectReport surface area.
        self.inject_with_submit_observer(target, payload, submit, bracketed, None)
    }

    fn inject_with_submit_observer(
        &self,
        target: &Target,
        payload: &InjectPayload,
        submit: Key,
        bracketed: bool,
        observer: Option<&dyn SubmitObserver>,
    ) -> Result<InjectReport, TransportError> {
        let pane = pane_from_target(target);
        // U1 #7: pane readback signal for the non-pasted-prompt text path.
        let mut token_visible_for_report: Option<bool> = None;
        match payload {
            InjectPayload::Empty => {
                let argv = tmux_empty_inject_argv(&pane, submit);
                self.run_ok(&argv)?;
                notify_submit_observer(observer);
            }
            InjectPayload::Text(text) | InjectPayload::TextSkipConsumptionPoll(text) => {
                let buffer = buffer_name_for_text(text);
                for argv in tmux_inject_text_argv(&pane, &buffer, text, bracketed) {
                    let stage = inject_stage_for_argv(&argv);
                    if stage == InjectStage::LoadBuffer {
                        self.run_inject_stage_with_stdin(&argv, stage, text)?;
                    } else {
                        self.run_inject_stage(&argv, stage)?;
                    }
                }
                // ═══════════════════════════════════════════════════════════
                // 0.3.27 UNIFIED submit_and_verify
                //
                // Replaces the dual-branch split (saw_pasted_prompt weak loop
                // + E46 token consumption gate) with a single pipeline:
                //
                //   Phase 1 — token visibility poll (dynamic timeout based on
                //     payload size, 50ms interval, replaces the fixed 125ms
                //     appear_gate)
                //   Phase 2 — Escape (if bracketed+Text+Enter) + Enter + poll
                //     token disappeared from bottom 3 lines. On failure:
                //     re-check → Escape+Enter → poll. Up to 3 attempts.
                //
                // Design truth source: .team/artifacts/E55-delivery-architecture-design.html
                // Python parity: dynamic timeout max(2s, bytes/25000), poll 50ms.
                // ═══════════════════════════════════════════════════════════
                let inject_start = std::time::Instant::now();
                let submit_argv = tmux_send_keys_argv(&pane, &[submit]);

                // Phase 1: token visibility poll — wait for the pasted text to
                // become visible in the pane before submitting. Dynamic timeout
                // based on payload size (large codex pastes can take seconds to
                // render the bracketed-paste block).
                let token_poll_timeout_ms = {
                    let size_based = (text.len() as u64) / 25;
                    size_based.max(2000)
                };
                let poll_start = std::time::Instant::now();
                token_visible_for_report =
                    if payload_token_marker(payload).is_some() {
                        let mut visible = false;
                        while poll_start.elapsed().as_millis() < token_poll_timeout_ms as u128 {
                            match token_visible_in_capture(self, target, payload) {
                                Ok(Some(true)) => { visible = true; break; }
                                Err(_) => break, // tmux unavailable, skip poll
                                _ => {}
                            }
                            std::thread::sleep(Duration::from_millis(50));
                        }
                        Some(visible)
                    } else {
                        None
                    };

                // Phase 2: submit_and_verify — unified Escape+Enter+poll loop.
                let use_escape = bracketed
                    && payload.text().is_some()
                    && matches!(submit, Key::Enter);
                let escape_argv = if use_escape {
                    Some(tmux_send_keys_argv(&pane, &[Key::Escape]))
                } else {
                    None
                };
                // Non-Enter submit keys (Key::Down for codex menu, etc.) skip
                // the entire submit_and_verify loop — single send, no consumption
                // check, KeySentAfterVisibleToken verification.
                if !matches!(submit, Key::Enter) {
                    self.run_inject_stage(&submit_argv, InjectStage::Submit)?;
                    notify_submit_observer(observer);
                    let total_elapsed_ms = inject_start.elapsed().as_millis() as u64;
                    return Ok(InjectReport {
                        stage_reached: InjectStage::Submit,
                        inject_verification: inject_verification_after_readback(
                            payload,
                            token_visible_for_report,
                        ),
                        submit_verification: submit_verification_for_key(submit),
                        turn_verification: TurnVerification::NotYetObserved,
                        attempts: 1,
                        submit_diagnostics: Some(crate::transport::SubmitDiagnostics {
                            appear_gate_elapsed_ms: 0,
                            appear_gate_matched: false,
                            total_elapsed_ms,
                            attempts_detail: Vec::new(),
                        }),
                    });
                }

                let marker = payload_token_marker(payload);
                let max_submit_attempts: u32 = 3;
                let mut consumption_attempts: u32 = 0;
                let mut consumed: Option<bool> = None;
                let mut attempts_detail: Vec<SubmitAttemptObservation> = Vec::new();
                let mut any_attempt_matched = false;

                let poll_consumption = !payload.skip_consumption_poll();
                if !poll_consumption {
                    if self.run_inject_stage(&submit_argv, InjectStage::Submit).is_ok() {
                        notify_submit_observer(observer);
                        consumption_attempts = 1;
                    }
                }
                let submit_attempt_limit = if poll_consumption { max_submit_attempts } else { 0 };

                for attempt in 0..submit_attempt_limit {
                    let attempt_index = attempt + 1;
                    let attempt_start = std::time::Instant::now();
                    // Before resending (attempt > 0), re-check if the token
                    // already disappeared — guards against double-submit (C3).
                    // Capture failures are non-fatal (tmux may not be running
                    // in MCP sim / test env).
                    if attempt > 0 {
                        if let Some(m) = marker {
                            if let Ok(cap) = self.capture(target, CaptureRange::Tail(40)) {
                                let obs = submit_attempt_observation(
                                    attempt_index,
                                    &cap,
                                    marker,
                                    attempt_start.elapsed().as_millis() as u64,
                                );
                                if obs.matched {
                                    any_attempt_matched = true;
                                }
                                attempts_detail.push(obs);
                                if !token_in_bottom_n(&cap.text, m, 15) {
                                    consumed = Some(true);
                                    break;
                                }
                            }
                        }
                    }

                    // 0.3.28-final (E55 false-positive truth source):
                    // Escape retry is DELETED. The researcher established
                    // Escape on Claude TUI with [Pasted content] visible
                    // may CLEAR the composer content rather than exit paste
                    // mode — sending Escape+Enter on retry would submit an
                    // empty message and hide the genuine consumption failure
                    // under a fake-success path. Python parity: ONLY ever
                    // send Enter, never Escape.
                    let _ = escape_argv;

                    // Enter — send-keys failure is degraded (tmux may not have
                    // the pane in sim/test env). Break to consumed=None path
                    // which returns EnterSentWithoutPlaceholderCheck.
                    if self.run_inject_stage(&submit_argv, InjectStage::Submit).is_err() {
                        consumed = None;
                        break;
                    }
                    notify_submit_observer(observer);
                    consumption_attempts = attempt + 1;

                    // Post-submit token readback (U1 #7 parity: check token
                    // visible after Enter for no-echo panes).
                    if attempt == 0 && matches!(token_visible_for_report, Some(false)) {
                        token_visible_for_report =
                            post_submit_token_visible(self, target, payload)
                                .unwrap_or(Some(false));
                    }

                    // Poll: token disappeared from bottom 15 lines = consumed.
                    // Capture failures → consumed=None (non-blocking).
                    if let Some(m) = marker {
                        let mut found_consumed = false;
                        let mut capture_failed = false;
                        for _ in 0..12 {
                            std::thread::sleep(Duration::from_millis(100));
                            match self.capture(target, CaptureRange::Tail(40)) {
                                Ok(cap) => {
                                    let obs = submit_attempt_observation(
                                        attempt_index,
                                        &cap,
                                        marker,
                                        attempt_start.elapsed().as_millis() as u64,
                                    );
                                    if obs.matched {
                                        any_attempt_matched = true;
                                    }
                                    attempts_detail.push(obs);
                                    if !token_in_bottom_n(&cap.text, m, 15) {
                                        found_consumed = true;
                                        break;
                                    }
                                }
                                Err(_) => { capture_failed = true; break; }
                            }
                        }
                        if capture_failed {
                            consumed = None;
                            break;
                        }
                        consumed = Some(found_consumed);
                        if found_consumed {
                            break;
                        }
                    } else {
                        // Non-token payload: single Enter, no consumption check.
                        consumed = None;
                        break;
                    }
                }

                // 0.5.43 debt-sweep (§6.2): three unconditional
                // `eprintln!` submit-consumption debug lines removed.
                // The decision they narrated is already captured in
                // `InjectReport.submit_diagnostics` /
                // `submit_verification`; the prints only spammed
                // coordinator.log without additive signal. Behavior
                // is byte-identical.
                let submit_verification = match consumed {
                    Some(true) => SubmitVerification::EnterSentWithoutPlaceholderCheck,
                    Some(false) => {
                        if any_attempt_matched {
                            SubmitVerification::EnterSentWithoutPlaceholderCheck
                        } else {
                            match self.capture(target, CaptureRange::Tail(15)) {
                                Ok(cap) => {
                                    attempts_detail.push(submit_attempt_observation(
                                        consumption_attempts.max(1),
                                        &cap,
                                        marker,
                                        inject_start.elapsed().as_millis() as u64,
                                    ));
                                    let busy = provider_busy_signal_in_tail(&cap.text);
                                    if busy {
                                        SubmitVerification::EnterSentWithoutPlaceholderCheck
                                    } else {
                                        SubmitVerification::SubmitConsumptionUnverified
                                    }
                                }
                                Err(_) => SubmitVerification::SubmitConsumptionUnverified,
                            }
                        }
                    }
                    None => submit_verification_for_key(submit),
                };
                let total_elapsed_ms = inject_start.elapsed().as_millis() as u64;
                return Ok(InjectReport {
                    stage_reached: InjectStage::Submit,
                    inject_verification: inject_verification_after_readback(
                        payload,
                        token_visible_for_report,
                    ),
                    submit_verification,
                    turn_verification: match payload {
                        InjectPayload::Empty => TurnVerification::NotRequired,
                        InjectPayload::Text(_) | InjectPayload::TextSkipConsumptionPoll(_) => TurnVerification::NotYetObserved,
                    },
                    attempts: consumption_attempts,
                    submit_diagnostics: Some(crate::transport::SubmitDiagnostics {
                        appear_gate_elapsed_ms: 0,
                        appear_gate_matched: false,
                        total_elapsed_ms,
                        attempts_detail,
                    }),
                });
            }
        }
        Ok(InjectReport {
            stage_reached: InjectStage::Submit,
            inject_verification: inject_verification_after_readback(
                payload,
                token_visible_for_report,
            ),
            submit_verification: submit_verification_for_key(submit),
            turn_verification: match payload {
                InjectPayload::Empty => TurnVerification::NotRequired,
                InjectPayload::Text(_) | InjectPayload::TextSkipConsumptionPoll(_) => TurnVerification::NotYetObserved,
            },
            attempts: 1,
            // E50 PR-1: Empty payload / non-Text fallthrough path — no submit
            // diagnostics applicable.
            submit_diagnostics: None,
        })
    }

    fn send_keys(&self, target: &Target, keys: &[Key]) -> Result<(), TransportError> {
        let pane = pane_from_target(target);
        if keys.contains(&Key::CancelMode) {
            if let Some(mode) = pane_mode_from_raw(self.query(target, PaneField::PaneMode)?) {
                let argv = crate::transport::tmux_cancel_mode_argv(&pane, mode);
                return self.run_ok(&argv);
            }
            return Ok(());
        }
        let argv = tmux_send_keys_argv(&pane, keys);
        self.run_ok(&argv)
    }

    fn capture(
        &self,
        target: &Target,
        range: CaptureRange,
    ) -> Result<CapturedText, TransportError> {
        let pane = pane_from_target(target);
        let argv = self.tmux_argv(&tmux_capture_argv(&pane, range));
        let output = self
            .runner
            .run(&argv)
            .map_err(|source| TransportError::Capture { source })?;
        if !output.success {
            return Err(subprocess_error(argv, output));
        }
        Ok(CapturedText {
            text: normalize_capture(&output.stdout),
            range,
        })
    }

    fn query(&self, target: &Target, field: PaneField) -> Result<Option<String>, TransportError> {
        let pane = pane_from_target(target);
        let argv = self.tmux_argv(&tmux_query_argv(&pane, field));
        let output = self.runner.run(&argv)?;
        if !output.success {
            return Ok(None);
        }
        Ok(Some(output.stdout.trim().to_string()))
    }

    fn liveness(&self, pane: &PaneId) -> Result<PaneLiveness, TransportError> {
        let argv = self.tmux_argv(&[
            "tmux".to_string(),
            "display-message".to_string(),
            "-p".to_string(),
            "-t".to_string(),
            pane.as_str().to_string(),
            "#{pane_id}".to_string(),
        ]);
        let output = self.runner.run(&argv)?;
        if output.success {
            return Ok(PaneLiveness::Live);
        }
        if output
            .stderr
            .to_ascii_lowercase()
            .contains("can't find pane")
        {
            Ok(PaneLiveness::Dead)
        } else {
            Ok(PaneLiveness::Unknown)
        }
    }

    fn has_pane(&self, pane: &PaneId) -> Result<Option<bool>, TransportError> {
        let argv = self.tmux_argv(&[
            "tmux".to_string(),
            "display-message".to_string(),
            "-p".to_string(),
            "-t".to_string(),
            pane.as_str().to_string(),
            "#{pane_id}".to_string(),
        ]);
        let output = self.runner.run(&argv)?;
        if output.success {
            let pane_id = output.stdout.trim();
            if pane_id.is_empty() {
                return Ok(Some(false));
            }
            if pane_id == pane.as_str()
                && pane_id.starts_with('%')
                && pane_id[1..].chars().all(|ch| ch.is_ascii_digit())
            {
                return Ok(Some(true));
            }
            return Ok(None);
        }
        let stderr = output.stderr.to_ascii_lowercase();
        if stderr.contains("can't find pane")
            || stderr.contains("no such pane")
            || (stderr.contains("can't find") && stderr.contains("pane"))
        {
            Ok(Some(false))
        } else {
            Ok(None)
        }
    }

    fn list_targets(&self) -> Result<Vec<PaneInfo>, TransportError> {
        // P5 (C-P5-3): `#{pane_pid}` rides the single list-panes call (field index 11),
        // killing the per-pane display-message N+1 fallback.
        const TMUX_PANE_FORMAT: &str = "#{pane_id}\t#{session_name}\t#{window_index}\t#{window_name}\t#{pane_index}\t#{pane_tty}\t#{pane_current_command}\t#{pane_active}\t#{pane_current_path}\t#{session_attached}\t#{pane_in_mode}\t#{pane_pid}";
        let argv = self.tmux_argv(&[
            "tmux".to_string(),
            "list-panes".to_string(),
            "-a".to_string(),
            "-F".to_string(),
            TMUX_PANE_FORMAT.to_string(),
        ]);
        let output = self.runner.run(&argv)?;
        if !output.success {
            return Ok(Vec::new());
        }
        let mut panes = Vec::new();
        for line in output.stdout.lines().filter(|line| !line.is_empty()) {
            if let Some(mut pane) = parse_pane_info_line(line) {
                // 0.3.5 integration union: P5 (C-P5-3) makes `#{pane_pid}` ride the
                // single list-panes call — on real tmux the fallback below never fires.
                // swallow batch 2 ① keeps it as a RESILIENT degrade for panes whose pid
                // field came back empty (e.g. older tmux without #{pane_pid}): a single
                // pane's probe failure must not fail the WHOLE list — the pane degrades
                // to pane_pid=None and the failure is observable.
                if pane.pane_pid.is_none() {
                    match query_pane_pid(self, &pane.pane_id) {
                        Ok(pid) => pane.pane_pid = pid,
                        Err(error) => {
                            if let Some(workspace) = &self.event_workspace {
                                let _ = crate::event_log::EventLog::new(workspace).write(
                                    "tmux.pane_pid_query_failed",
                                    serde_json::json!({
                                        "pane_id": pane.pane_id.as_str(),
                                        "session": pane.session.as_str(),
                                        "error": error.to_string(),
                                    }),
                                );
                            }
                        }
                    }
                }
                panes.push(pane);
            }
        }
        Ok(panes)
    }

    fn has_session(&self, session: &SessionName) -> Result<bool, TransportError> {
        let argv = self.tmux_argv(&[
            "tmux".to_string(),
            "has-session".to_string(),
            "-t".to_string(),
            session.as_str().to_string(),
        ]);
        let output = self.runner.run(&argv)?;
        Ok(output.success)
    }

    fn list_windows(&self, session: &SessionName) -> Result<Vec<WindowName>, TransportError> {
        // golden runtime.py:1023-1029 `_tmux_window_exists`: `tmux list-windows -t <s> -F #{window_name}`;
        // returncode != 0 -> false (here: an empty window set), else the window names by line.
        let argv = self.tmux_argv(&[
            "tmux".to_string(),
            "list-windows".to_string(),
            "-t".to_string(),
            session.as_str().to_string(),
            "-F".to_string(),
            "#{window_name}".to_string(),
        ]);
        let output = self.runner.run(&argv)?;
        if !output.success {
            return Ok(Vec::new());
        }
        Ok(output
            .stdout
            .lines()
            .filter(|line| !line.is_empty())
            .map(WindowName::new)
            .collect())
    }

    fn configure_adaptive_pane_title(
        &self,
        session: &SessionName,
        window: &WindowName,
        pane: &PaneId,
        title: &str,
    ) -> Result<(), TransportError> {
        let target = format!("{}:{}", session.as_str(), window.as_str());
        self.run_ok(&[
            "tmux".to_string(),
            "set-window-option".to_string(),
            "-t".to_string(),
            target.clone(),
            "pane-border-status".to_string(),
            "bottom".to_string(),
        ])?;
        self.run_ok(&[
            "tmux".to_string(),
            "set-window-option".to_string(),
            "-t".to_string(),
            target,
            "pane-border-format".to_string(),
            " #{pane_title} ".to_string(),
        ])?;
        self.run_ok(&[
            "tmux".to_string(),
            "select-pane".to_string(),
            "-t".to_string(),
            pane.as_str().to_string(),
            "-T".to_string(),
            title.to_string(),
        ])
    }

    fn set_session_env(
        &self,
        session: &SessionName,
        key: &str,
        value: &str,
    ) -> Result<SetEnvOutcome, TransportError> {
        let argv = vec![
            "tmux".to_string(),
            "set-environment".to_string(),
            "-t".to_string(),
            session.as_str().to_string(),
            key.to_string(),
            value.to_string(),
        ];
        self.run_ok(&argv)?;
        Ok(SetEnvOutcome::Applied)
    }

    fn kill_server(&self) -> Result<(), TransportError> {
        TmuxBackend::kill_server(self);
        Ok(())
    }

    fn kill_session(&self, session: &SessionName) -> Result<(), TransportError> {
        let argv = self.tmux_argv(&[
            "tmux".to_string(),
            "kill-session".to_string(),
            "-t".to_string(),
            session.as_str().to_string(),
        ]);
        self.run_ok(&argv)
    }

    fn kill_window(&self, target: &Target) -> Result<(), TransportError> {
        let argv = self.tmux_argv(&[
            "tmux".to_string(),
            "kill-window".to_string(),
            "-t".to_string(),
            target_name(target),
        ]);
        self.run_ok(&argv)
    }

    fn kill_pane(&self, pane: &PaneId) -> Result<(), TransportError> {
        let argv = self.tmux_argv(&[
            "tmux".to_string(),
            "kill-pane".to_string(),
            "-t".to_string(),
            pane.as_str().to_string(),
        ]);
        self.run_ok(&argv)
    }

    fn attach_session(&self, session: &SessionName) -> Result<AttachOutcome, TransportError> {
        let argv = [
            "tmux".to_string(),
            "attach-session".to_string(),
            "-t".to_string(),
            session.as_str().to_string(),
        ];
        self.run_ok(&argv)?;
        Ok(AttachOutcome::Attached)
    }
}

/// swallow batch 2 ① fallback probe (only fires when `#{pane_pid}` came back empty —
/// see the P5 union note in `list_targets`).
fn query_pane_pid(backend: &TmuxBackend, pane: &PaneId) -> Result<Option<u32>, TransportError> {
    let argv = backend.tmux_argv(&[
        "tmux".to_string(),
        "display-message".to_string(),
        "-p".to_string(),
        "-t".to_string(),
        pane.as_str().to_string(),
        "#{pane_pid}".to_string(),
    ]);
    let output = backend.runner.run(&argv)?;
    if !output.success {
        return Ok(None);
    }
    Ok(parse_optional_u32(output.stdout.trim()))
}

fn parse_pane_info_line(line: &str) -> Option<PaneInfo> {
    let fields = line.split('\t').collect::<Vec<_>>();
    if fields.len() < 11 {
        return None;
    }
    Some(PaneInfo {
        pane_id: PaneId::new(fields[0]),
        session: SessionName::new(fields[1]),
        window_index: parse_optional_u32(fields[2]),
        window_name: non_empty(fields[3]).map(WindowName::new),
        pane_index: parse_optional_u32(fields[4]),
        tty: non_empty(fields[5]).map(str::to_string),
        current_command: non_empty(fields[6]).map(str::to_string),
        active: fields[7] == "1",
        current_path: non_empty(fields[8]).map(PathBuf::from),
        pane_pid: fields.get(11).and_then(|raw| parse_optional_u32(raw)),
        leader_env: BTreeMap::new(),
    })
}

fn parse_optional_u32(raw: &str) -> Option<u32> {
    if raw.is_empty() {
        return None;
    }
    raw.parse::<u32>().ok()
}

fn non_empty(raw: &str) -> Option<&str> {
    if raw.is_empty() {
        None
    } else {
        Some(raw)
    }
}

// 0.5.x Windows portability Batch 5: the `tmux_backend/tests.rs`
// module uses `std::os::unix::net::UnixListener` for its mock
// runner + verifies Unix-specific socket-root derivation. Since the
// tmux backend itself only functions on Unix (design § Route B —
// tmux is a Unix concept), the test module stays Unix-only.
#[cfg(all(test, unix))]
mod tests;
