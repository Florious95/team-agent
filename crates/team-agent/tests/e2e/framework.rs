//! E2E framework: TestWorkspace + run_ta + assert helpers + FakeProvider
//! support + state injection + wait_for. Zero external test-CLI deps —
//! framework uses `std::process::Command`, `serde_json`, and the existing
//! `team-agent` binary built by `cargo test`.
//!
//! ---
//! purpose: Hermetic macOS E2E fixture ownership and durable delivery timeout evidence
//! contract:
//!   provides:
//!     - name: TestWorkspace
//!       what: Owns exact coordinator and tmux resources and reaps them on drop
//!     - name: wait_for_delivery_or_panic
//!       what: Persists message, coordinator, event, and physical target facts before timeout panic
//!   depends:
//!     - crate::platform::process
//!     - sqlite messages/events store
//!     - tmux per-team endpoint
//! boundary:
//!   - Test-only fixture and evidence surface; no delivery product behavior
//! maturity: wired
//! ---
//!
//! All test helpers panic on programmer error (wrong binary path, write
//! failure on a temp dir we own) and return `Result` / printable diagnostics
//! when the SUT misbehaves.

use std::collections::BTreeMap;
#[cfg(unix)]
use std::os::unix::fs::FileTypeExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use rusqlite::Connection;
use serde_json::Value;
use sha2::{Digest, Sha256};

// ----------------------------------------------------------------------------
// 1. TestWorkspace — per-test temp dir with auto-cleanup (Drop)
// ----------------------------------------------------------------------------

static WORKSPACE_COUNTER: AtomicU64 = AtomicU64::new(0);
const LAST_COMMAND_OBSERVATION: &str = ".e2e-last-command-observation.json";

const CALLER_IDENTITY_ENVS: &[&str] = &[
    "TMUX",
    "TMUX_PANE",
    "TEAM_AGENT_LEADER_PANE_ID",
    "TEAM_AGENT_LEADER_SESSION_UUID",
    "TEAM_AGENT_LEADER_SESSION_UUID_OVERRIDE",
    "TEAM_AGENT_LEADER_SESSION_NAME",
    "TEAM_AGENT_LEADER_PROVIDER",
    "TEAM_AGENT_MACHINE_FINGERPRINT",
    "TEAM_AGENT_WORKSPACE",
    "TEAM_AGENT_TEAM_ID",
    "TEAM_AGENT_OWNER_TEAM_ID",
    "TEAM_AGENT_ACTIVE_TEAM",
    "TEAM_AGENT_ID",
    "TEAM_AGENT_AGENT_ID",
    "TEAM_AGENT_AUTH_MODE",
    "TEAM_AGENT_LEADER_BYPASS",
    "TEAM_AGENT_LEADER_BYPASS_SOURCE",
    "TEAM_AGENT_LEADER_BYPASS_PROVIDER",
    "TEAM_AGENT_LEADER_BYPASS_FLAG",
    "TEAM_AGENT_MCP_AUTO_APPROVE",
    "TEAM_AGENT_MCP_AUTO_APPROVE_SOURCE",
];

/// A self-cleaning workspace directory under `/private/tmp` (preferred on
/// macOS so it survives `/tmp -> /private/tmp` symlink resolution that some
/// runtime paths do) or `std::env::temp_dir()` elsewhere. The directory is
/// removed on `Drop` unless `TEAM_AGENT_KEEP_TEST_TMP=1` is set.
pub struct TestWorkspace {
    pub(crate) path: PathBuf,
    pub(crate) ta_binary: Mutex<Option<PathBuf>>,
    /// 0.5.43 debt-sweep (§6.1): exact test-owned tmux sockets to
    /// clean at Drop. Populated by `register_owned_tmux_socket`. Drop
    /// runs `tmux -S <sock> kill-server` on each (never a host scan)
    /// BEFORE the workspace directory removal (verified by RED
    /// `e2e_workspace_drop_cleans_exact_tmux_before_removing_workspace`).
    pub(crate) owned_tmux_sockets: Mutex<Vec<PathBuf>>,
}

impl TestWorkspace {
    /// Create a workspace tagged `e2e-<tag>-<pid>-<seq>`. The tag becomes part
    /// of the dirname — pass a short label per test so kept dirs are easy to
    /// identify.
    pub fn new(tag: &str) -> Self {
        let tmp_root = if Path::new("/private/tmp").is_dir() {
            PathBuf::from("/private/tmp")
        } else {
            std::env::temp_dir()
        };
        let path = tmp_root.join(format!(
            "ta-e2e-{tag}-{}-{}",
            std::process::id(),
            WORKSPACE_COUNTER.fetch_add(1, Ordering::Relaxed),
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("create workspace dir");
        let path = std::fs::canonicalize(&path).expect("canonicalize workspace dir");
        Self {
            path,
            ta_binary: Mutex::new(None),
            owned_tmux_sockets: Mutex::new(Vec::new()),
        }
    }

    /// 0.5.43 debt-sweep (§6.1): register a test-owned tmux socket for
    /// exact Drop cleanup. Never a host-wide scan — the ledger only
    /// contains sockets THIS fixture created.
    pub fn register_owned_tmux_socket(&self, socket: &Path) {
        let socket = normalize_existing_path(socket);
        let ambient = std::env::var_os("TMUX").and_then(|value| {
            let socket = value.to_str()?.split(',').next()?;
            (!socket.is_empty()).then(|| normalize_existing_path(Path::new(socket)))
        });
        assert_ne!(
            ambient.as_deref(),
            Some(socket.as_path()),
            "refusing to register ambient TMUX endpoint as test-owned: {}",
            socket.display()
        );
        assert!(
            socket.is_absolute() && socket.exists() && is_owned_tmux_socket_path(&socket),
            "tmux endpoint must already exist as an owned platform tmux socket: socket={} workspace={}",
            socket.display(),
            self.path.display()
        );
        if let Ok(mut sockets) = self.owned_tmux_sockets.lock() {
            sockets.push(socket);
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn record_ta_binary(&self, path: &Path) {
        let normalized = normalize_existing_path(path);
        if let Ok(mut slot) = self.ta_binary.lock() {
            *slot = Some(normalized);
        }
    }

    /// Write a minimal TEAM.md + agents/<id>.md tree that uses
    /// `provider: fake` (no subscription, no real provider binary). Returns
    /// `self` for chaining.
    pub fn with_fake_spec(self, agent_ids: &[&str]) -> Self {
        let team_md = format!(
            "---\nname: e2e-{}\nobjective: E2E fake team fixture.\nprovider: fake\ndisplay_backend: none\n---\n\nTeam.\n",
            self.short_tag(),
        );
        std::fs::write(self.path.join("TEAM.md"), team_md).expect("write TEAM.md");
        let agents_dir = self.path.join("agents");
        std::fs::create_dir_all(&agents_dir).expect("create agents/");
        for id in agent_ids {
            let body = format!(
                "---\nname: {id}\nrole: Fake worker {id}\nprovider: fake\nmodel: fake\nauth_mode: subscription\ndangerously_skip_permissions: false\ntools:\n  - mcp_team\n---\n\nFake worker {id}.\n",
            );
            std::fs::write(agents_dir.join(format!("{id}.md")), body)
                .unwrap_or_else(|e| panic!("write agents/{id}.md: {e}"));
        }
        self
    }

    fn short_tag(&self) -> String {
        self.path
            .file_name()
            .and_then(|s| s.to_str())
            .map(|s| s.to_string())
            .unwrap_or_default()
    }

    /// Path to `.team/runtime/state.json` (may not exist before quick-start).
    pub fn state_json_path(&self) -> PathBuf {
        self.path.join(".team/runtime/state.json")
    }

    pub fn events_jsonl_path(&self) -> PathBuf {
        self.path.join(".team/logs/events.jsonl")
    }

    /// Read state.json as a serde_json::Value. Panics if the file doesn't
    /// exist or is malformed — those are framework-level failures, not SUT
    /// misbehaviour.
    pub fn read_state(&self) -> Value {
        let path = self.state_json_path();
        let raw = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("read state.json at {}: {e}", path.display()));
        serde_json::from_str(&raw)
            .unwrap_or_else(|e| panic!("parse state.json at {}: {e}", path.display()))
    }

    /// Inject (or override) a top-level field in state.json. Panics if
    /// state.json doesn't exist yet — callers must run at least one CLI
    /// command that creates it first, or call `seed_state()` instead.
    pub fn inject_state(&self, top_level_key: &str, value: Value) {
        let mut state = self.read_state();
        let obj = state
            .as_object_mut()
            .expect("state.json top-level must be an object");
        obj.insert(top_level_key.to_string(), value);
        let path = self.state_json_path();
        let serialized = serde_json::to_string_pretty(&state).expect("re-serialize state");
        std::fs::write(&path, serialized).expect("write state.json");
    }

    /// Inject an agent-level field. `agent_id` must already exist in
    /// `state.agents`.
    pub fn inject_agent_field(&self, agent_id: &str, field: &str, value: Value) {
        let mut state = self.read_state();
        let agents = state
            .pointer_mut("/agents")
            .and_then(Value::as_object_mut)
            .expect("state.agents must be an object");
        let agent = agents
            .get_mut(agent_id)
            .and_then(Value::as_object_mut)
            .unwrap_or_else(|| panic!("agent {agent_id} not found in state"));
        agent.insert(field.to_string(), value);
        let path = self.state_json_path();
        let serialized = serde_json::to_string_pretty(&state).expect("re-serialize state");
        std::fs::write(&path, serialized).expect("write state.json");
    }

    /// Seed state.json from scratch (creates `.team/runtime/`). Use when a
    /// test wants to start from an arbitrary pre-built state without running
    /// quick-start first.
    pub fn seed_state(&self, state: Value) {
        let runtime_dir = self.path.join(".team/runtime");
        std::fs::create_dir_all(&runtime_dir).expect("create .team/runtime/");
        let serialized = serde_json::to_string_pretty(&state).expect("serialize seed state");
        std::fs::write(runtime_dir.join("state.json"), serialized).expect("write state.json");
    }

    pub fn write_state_value(&self, state: Value) {
        let serialized = serde_json::to_string_pretty(&state).expect("serialize state");
        std::fs::write(self.state_json_path(), serialized).expect("write state.json");
    }

    pub fn mutate_state<F>(&self, f: F)
    where
        F: FnOnce(&mut Value),
    {
        let mut state = self.read_state();
        f(&mut state);
        self.write_state_value(state);
    }

    pub fn mutate_agent_everywhere<F>(&self, agent_id: &str, mut f: F)
    where
        F: FnMut(&mut serde_json::Map<String, Value>),
    {
        let mut state = self.read_state();
        if let Some(agent) = state
            .get_mut("agents")
            .and_then(Value::as_object_mut)
            .and_then(|agents| agents.get_mut(agent_id))
            .and_then(Value::as_object_mut)
        {
            f(agent);
        }
        if let Some(active) = state
            .get("active_team_key")
            .and_then(Value::as_str)
            .map(str::to_string)
        {
            if let Some(agent) = state
                .get_mut("teams")
                .and_then(Value::as_object_mut)
                .and_then(|teams| teams.get_mut(&active))
                .and_then(|team| team.get_mut("agents"))
                .and_then(Value::as_object_mut)
                .and_then(|agents| agents.get_mut(agent_id))
                .and_then(Value::as_object_mut)
            {
                f(agent);
            }
        }
        self.write_state_value(state);
    }
}

impl Drop for TestWorkspace {
    fn drop(&mut self) {
        if std::thread::panicking() {
            emit_fixture_panic_receipt(self);
        }
        // 0.5.43 debt-sweep (§6.1) Drop order = stop exact coordinator
        // tree → kill exact owned tmux server(s) + delete socket file →
        // remove workspace dir. `TEAM_AGENT_KEEP_TEST_PROCESSES/TMP`
        // debug escapes preserved (loud stderr) in both helpers.
        // Explicit non-goal: never host-scan for team-agent processes
        // via pgrep, never scan the tmux-* socket dir under /private —
        // sweep is exact-owned only (verified by RED).
        self.cleanup_owned_coordinator();
        self.cleanup_owned_tmux();
        if std::env::var("TEAM_AGENT_KEEP_TEST_TMP").as_deref() != Ok("1") {
            remove_workspace_dir(&self.path);
        }
    }
}

impl TestWorkspace {
    /// 0.5.43 debt-sweep (§6.1): kill exact registered tmux servers
    /// and delete their socket files. Skipped loudly when
    /// `TEAM_AGENT_KEEP_TEST_PROCESSES=1`. Never scans host tmux
    /// sockets — only the ledger `register_owned_tmux_socket` populated.
    fn cleanup_owned_tmux(&self) {
        if std::env::var("TEAM_AGENT_KEEP_TEST_PROCESSES").as_deref() == Ok("1") {
            if let Ok(sockets) = self.owned_tmux_sockets.lock() {
                if !sockets.is_empty() {
                    eprintln!(
                        "TEAM_AGENT_KEEP_TEST_PROCESSES=1 — skipping cleanup of {} owned tmux socket(s) for workspace {}",
                        sockets.len(),
                        self.path.display()
                    );
                }
            }
            return;
        }
        let sockets = self
            .owned_tmux_sockets
            .lock()
            .ok()
            .map(|mut guard| std::mem::take(&mut *guard))
            .unwrap_or_default();
        for socket in sockets {
            if let Some(socket_str) = socket.to_str() {
                let _ = std::process::Command::new("tmux")
                    .args(["-S", socket_str, "kill-server"])
                    .output();
            }
            let _ = std::fs::remove_file(&socket);
        }
    }

    fn cleanup_owned_coordinator(&self) {
        if std::env::var("TEAM_AGENT_KEEP_TEST_PROCESSES").as_deref() == Ok("1") {
            return;
        }
        let mut stopped_any = false;
        for pid in self.discover_owned_coordinator_pids() {
            if self.terminate_owned_pid(pid) {
                stopped_any = true;
            } else {
                eprintln!(
                    "TestWorkspace cleanup: failed to stop owned coordinator pid={pid} workspace={}",
                    self.path.display()
                );
            }
        }
        if stopped_any || !self.pid_is_owned_coordinator_from_file() {
            let _ = std::fs::remove_file(self.coordinator_pid_file());
            let _ = std::fs::remove_file(self.coordinator_meta_file());
        }
    }

    pub(crate) fn coordinator_pid_file(&self) -> PathBuf {
        self.path.join(".team/runtime/coordinator.pid")
    }

    fn coordinator_meta_file(&self) -> PathBuf {
        self.path.join(".team/runtime/coordinator.json")
    }

    fn discover_owned_coordinator_pids(&self) -> Vec<u32> {
        let mut out = Vec::new();
        if let Some(pid) = read_pid(&self.coordinator_pid_file()) {
            if self.pid_is_owned_coordinator(pid) {
                out.push(pid);
            }
        }
        for (pid, command) in ps_table() {
            if self.command_is_owned_coordinator(&command) {
                out.push(pid);
            }
        }
        out.sort_unstable();
        out.dedup();
        out
    }

    fn pid_is_owned_coordinator_from_file(&self) -> bool {
        read_pid(&self.coordinator_pid_file()).is_some_and(|pid| self.pid_is_owned_coordinator(pid))
    }

    pub(crate) fn pid_is_owned_coordinator(&self, pid: u32) -> bool {
        pid != std::process::id()
            && ps_command(pid)
                .as_deref()
                .is_some_and(|command| self.command_is_owned_coordinator(command))
    }

    pub(crate) fn command_is_owned_coordinator(&self, command: &str) -> bool {
        let tokens = command.split_whitespace().collect::<Vec<_>>();
        if tokens.len() < 3 || !self.path_is_e2e_temp_workspace() {
            return false;
        }
        let Some(binary) = tokens.first() else {
            return false;
        };
        if is_installed_team_agent_binary(binary) || !self.binary_matches_test_binary(binary) {
            return false;
        }
        tokens.iter().any(|token| *token == "coordinator")
            && workspace_arg_matches(&tokens, &workspace_match_candidates(&self.path))
    }

    fn path_is_e2e_temp_workspace(&self) -> bool {
        self.path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("ta-e2e-"))
            && test_tmp_roots()
                .iter()
                .any(|root| self.path.starts_with(root))
    }

    fn binary_matches_test_binary(&self, command_binary: &str) -> bool {
        let command_binary = normalize_existing_path(Path::new(command_binary));
        self.ta_binary_candidates()
            .iter()
            .any(|candidate| *candidate == command_binary)
    }

    fn ta_binary_candidates(&self) -> Vec<PathBuf> {
        let mut out = Vec::new();
        if let Ok(slot) = self.ta_binary.lock() {
            if let Some(path) = slot.as_ref() {
                out.push(path.clone());
            }
        }
        if let Some(path) = std::env::var_os("CARGO_BIN_EXE_team-agent").map(PathBuf::from) {
            out.push(normalize_existing_path(&path));
        }
        if let Ok(exe) = std::env::current_exe() {
            if let Some(debug_dir) = exe.parent().and_then(Path::parent) {
                out.push(normalize_existing_path(&debug_dir.join("team-agent")));
            }
        }
        out.sort();
        out.dedup();
        out
    }

    fn terminate_owned_pid(&self, pid: u32) -> bool {
        if !self.pid_is_owned_coordinator(pid) {
            return false;
        }
        let _ = team_agent::platform::process::terminate_pid(
            pid,
            team_agent::platform::process::SignalKind::TerminateGraceful,
        );
        if wait_until_pid_exits(pid, Duration::from_millis(1500)) {
            return true;
        }
        if !self.pid_is_owned_coordinator(pid) {
            return !pid_is_running(pid);
        }
        // 0.5.x Windows portability Batch 5: route the kill through
        // `platform::process::terminate_pid` so the helper compiles
        // on both platforms. Unix uses SIGKILL byte-equivalent to
        // the previous inline `libc::kill(pid, SIGKILL)`; Windows
        // uses TerminateProcess.
        let _ = team_agent::platform::process::terminate_pid(
            pid,
            team_agent::platform::process::SignalKind::TerminateForce,
        );
        wait_until_pid_exits(pid, Duration::from_millis(1500))
    }
}

fn remove_workspace_dir(path: &Path) {
    match std::fs::remove_dir_all(path) {
        Ok(()) => {}
        Err(first) if first.kind() == std::io::ErrorKind::NotFound => {}
        Err(first) => {
            std::thread::sleep(Duration::from_millis(100));
            if let Err(second) = std::fs::remove_dir_all(path) {
                if second.kind() != std::io::ErrorKind::NotFound {
                    eprintln!(
                        "TestWorkspace cleanup: remove_dir_all {} failed: first={first}; retry={second}",
                        path.display()
                    );
                }
            }
        }
    }
}

pub(crate) fn read_pid(path: &Path) -> Option<u32> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|raw| raw.trim().parse::<u32>().ok())
}

fn ps_command(pid: u32) -> Option<String> {
    let output = Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "command="])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let command = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!command.is_empty()).then_some(command)
}

fn ps_table() -> Vec<(u32, String)> {
    let output = Command::new("ps")
        .args(["-axo", "pid=,command="])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output();
    let Ok(output) = output else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(parse_ps_line)
        .collect()
}

fn parse_ps_line(line: &str) -> Option<(u32, String)> {
    let line = line.trim_start();
    let split = line.find(char::is_whitespace).unwrap_or(line.len());
    let pid = line.get(..split)?.parse::<u32>().ok()?;
    let command = line.get(split..)?.trim();
    (!command.is_empty()).then(|| (pid, command.to_string()))
}

fn workspace_match_candidates(path: &Path) -> Vec<String> {
    let mut out = vec![path.to_string_lossy().to_string()];
    if let Ok(canonical) = path.canonicalize() {
        let text = canonical.to_string_lossy().to_string();
        if !out.iter().any(|candidate| candidate == &text) {
            out.push(text);
        }
    }
    out
}

fn workspace_arg_matches(tokens: &[&str], candidates: &[String]) -> bool {
    for (index, token) in tokens.iter().enumerate() {
        if *token == "--workspace"
            && tokens
                .get(index + 1)
                .is_some_and(|workspace| candidates.iter().any(|candidate| candidate == workspace))
        {
            return true;
        }
        if let Some(workspace) = token.strip_prefix("--workspace=") {
            if candidates.iter().any(|candidate| candidate == workspace) {
                return true;
            }
        }
    }
    false
}

pub(crate) fn normalize_existing_path(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

#[cfg(unix)]
fn is_owned_tmux_socket_path(socket: &Path) -> bool {
    let Some(parent) = socket.parent() else {
        return false;
    };
    let uid_root = format!("tmux-{}", unsafe { libc::geteuid() });
    let parent = normalize_existing_path(parent);
    let in_platform_tmux_root = test_tmp_roots()
        .iter()
        .any(|root| parent == root.join(&uid_root));
    let fixture_socket_name = socket
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with("ta-"));
    in_platform_tmux_root
        && fixture_socket_name
        && socket
            .metadata()
            .is_ok_and(|metadata| metadata.file_type().is_socket())
}

#[cfg(not(unix))]
fn is_owned_tmux_socket_path(_socket: &Path) -> bool {
    false
}

fn test_tmp_roots() -> Vec<PathBuf> {
    let mut out = vec![normalize_existing_path(&std::env::temp_dir())];
    if Path::new("/private/tmp").is_dir() {
        out.push(normalize_existing_path(Path::new("/private/tmp")));
    }
    out.sort();
    out.dedup();
    out
}

fn is_installed_team_agent_binary(binary: &str) -> bool {
    binary == "/Users/alauda/.local/bin/team-agent" || binary.contains("/.team-agent/runtime/")
}

pub(crate) fn pid_is_running(pid: u32) -> bool {
    // 0.5.x Windows portability Batch 5: route through
    // `platform::process::pid_is_alive` — same shape on both
    // platforms. The former inline `libc::kill(pid, 0)` +
    // last-os-error ESRCH check maps to `ProcessLiveness::Live` /
    // `Dead` from `pid_liveness`.
    team_agent::platform::process::pid_is_alive(pid)
}

pub(crate) fn wait_until_pid_exits(pid: u32, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if !pid_is_running(pid) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    !pid_is_running(pid)
}

// ----------------------------------------------------------------------------
// 2. run_ta — invoke the team-agent binary, capture output, parse JSON
// ----------------------------------------------------------------------------

/// Structured result of a `team-agent <cmd>` invocation.
#[derive(Debug, Clone)]
pub struct TaResult {
    pub argv: Vec<String>,
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
}

impl TaResult {
    /// Parse stdout as JSON. Panics with a diagnostic showing argv + stderr
    /// when stdout is not valid JSON — the caller should EXPECT JSON because
    /// every E2E test should pass `--json`.
    pub fn json(&self) -> Value {
        serde_json::from_str(&self.stdout).unwrap_or_else(|e| {
            panic!(
                "stdout was not JSON ({e})\n  argv: {:?}\n  exit: {}\n  stdout: {}\n  stderr: {}",
                self.argv, self.exit_code, self.stdout, self.stderr
            )
        })
    }

    pub fn is_success(&self) -> bool {
        self.exit_code == 0
    }
}

/// Locate the freshly-built `team-agent` binary that Cargo produces for this
/// test. Cargo sets `CARGO_BIN_EXE_team-agent` for integration tests of the
/// owning crate, which is the recommended modern API.
pub(crate) fn ta_binary() -> PathBuf {
    let env_path = std::env::var_os("CARGO_BIN_EXE_team-agent")
        .map(PathBuf::from)
        .or_else(|| {
            // Fallback for unusual invocations: derive from the test binary's
            // path (target/debug/deps/e2e-XXXX → target/debug/team-agent).
            std::env::current_exe().ok().and_then(|exe| {
                let deps = exe.parent()?;
                let debug = deps.parent()?;
                let candidate = debug.join("team-agent");
                if candidate.exists() {
                    Some(candidate)
                } else {
                    None
                }
            })
        })
        .expect(
            "team-agent binary not found; ensure tests are invoked via `cargo test` so \
             CARGO_BIN_EXE_team-agent is set",
        );
    if !env_path.is_file() {
        panic!(
            "team-agent binary at {} is not a file (build it via cargo build first)",
            env_path.display()
        );
    }
    env_path
}

/// Run a `team-agent` CLI invocation. The first arg is the subcommand. The
/// framework does NOT auto-inject `--workspace` or `--json` — pass them
/// explicitly so test intent is visible.
pub fn run_ta(ws: &TestWorkspace, args: &[&str]) -> TaResult {
    run_ta_env(ws, args, &[])
}

/// Like `run_ta` but lets the caller splice extra env entries (key/value
/// pairs). Per-command env keeps parallel tests safe — never set process
/// globals via `std::env::set_var`.
pub fn run_ta_env(ws: &TestWorkspace, args: &[&str], extra_env: &[(&str, &str)]) -> TaResult {
    let bin = ta_binary();
    ws.record_ta_binary(&bin);
    let mut cmd = Command::new(&bin);
    cmd.args(args)
        .current_dir(ws.path())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for key in CALLER_IDENTITY_ENVS {
        cmd.env_remove(key);
    }
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    let Output {
        status,
        stdout,
        stderr,
    } = cmd
        .output()
        .unwrap_or_else(|e| panic!("spawn {:?}: {e}", bin));
    let result = TaResult {
        argv: std::iter::once("team-agent".to_string())
            .chain(args.iter().map(|s| (*s).to_string()))
            .collect(),
        exit_code: status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&stdout).into_owned(),
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
    };
    record_last_command_observation(ws, &result, extra_env);
    if args.first() == Some(&"quick-start")
        && fake_delivery_fixture_needs_real_tui_noecho(ws)
        && quick_start_spawned_workers(&result)
    {
        disable_fake_worker_tty_echo(ws);
    }
    result
}

fn json_all_workers_spawned(value: &Value) -> bool {
    value
        .pointer("/readiness/all_workers_spawned")
        .and_then(Value::as_bool)
        == Some(true)
        || value
            .pointer("/worker_readiness/all_workers_spawned")
            .and_then(Value::as_bool)
            == Some(true)
}

fn quick_start_spawned_workers(result: &TaResult) -> bool {
    serde_json::from_str::<Value>(&result.stdout)
        .ok()
        .map(|value| json_all_workers_spawned(&value))
        .unwrap_or_else(|| result.stdout.contains("\"all_workers_spawned\": true"))
}

/// These delivery contracts assert persisted/result truth and need the fake
/// pane to match a real TUI's no-echo input. Other contracts intentionally use
/// terminal echo as a pane-routing oracle, so this cannot be a global fake
/// provider setting.
fn fake_delivery_fixture_needs_real_tui_noecho(ws: &TestWorkspace) -> bool {
    let tag = ws.short_tag();
    ["send001", "gate061-send", "gate061-doc"]
        .iter()
        .any(|fixture| tag.starts_with(&format!("ta-e2e-{fixture}-")))
}

// ----------------------------------------------------------------------------
// 3. JSON field assertions
// ----------------------------------------------------------------------------

/// Assert that a JSON pointer (RFC 6901) resolves to an expected `Value`.
/// Use slash-paths: `assert_json_field(&out, "/ok", &json!(true))`.
#[track_caller]
pub fn assert_json_field(actual: &Value, pointer: &str, expected: &Value) {
    let got = actual.pointer(pointer).unwrap_or_else(|| {
        panic!(
            "JSON pointer {pointer:?} not found in:\n{}",
            serde_json::to_string_pretty(actual).unwrap_or_default()
        )
    });
    assert_eq!(
        got, expected,
        "JSON field {pointer} mismatch\n  expected: {expected}\n  got:      {got}"
    );
}

#[track_caller]
pub fn assert_json_field_eq_bool(actual: &Value, pointer: &str, expected: bool) {
    assert_json_field(actual, pointer, &Value::Bool(expected));
}

#[track_caller]
pub fn assert_json_field_eq_str(actual: &Value, pointer: &str, expected: &str) {
    assert_json_field(actual, pointer, &Value::String(expected.to_string()));
}

#[track_caller]
pub fn assert_json_field_present(actual: &Value, pointer: &str) {
    assert!(
        actual.pointer(pointer).is_some(),
        "JSON pointer {pointer:?} not present in:\n{}",
        serde_json::to_string_pretty(actual).unwrap_or_default()
    );
}

// ----------------------------------------------------------------------------
// 4. Tmux assertions — minimal subprocess wrappers
// ----------------------------------------------------------------------------

/// Return `true` if a tmux session with `name` exists on the default tmux
/// socket. Returns `false` if `tmux` is not installed or no server is
/// running (those are not assertion failures — the SUT manages its own
/// server).
pub fn tmux_session_exists(name: &str) -> bool {
    let out = Command::new("tmux")
        .args(["has-session", "-t", name])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output();
    match out {
        Ok(o) => o.status.success(),
        Err(_) => false,
    }
}

/// Return `true` if a tmux session exists on a *specific* socket (the SUT
/// uses per-team sockets like `ta-<hash>`). Pass the full `-S` or `-L`
/// argument as recorded by SUT (e.g. via state.tmux_socket).
pub fn tmux_session_exists_on_socket(socket_arg: &str, name: &str) -> bool {
    // Accept either a full socket path (use -S) or a short socket name
    // (use -L). Heuristic: path-like strings (contain '/') use -S.
    let (flag, value) = if socket_arg.contains('/') {
        ("-S", socket_arg)
    } else {
        ("-L", socket_arg)
    };
    let out = Command::new("tmux")
        .args([flag, value, "has-session", "-t", name])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output();
    match out {
        Ok(o) => o.status.success(),
        Err(_) => false,
    }
}

pub fn tmux_windows_on_socket(socket_arg: &str, name: &str) -> Vec<String> {
    let (flag, value) = if socket_arg.contains('/') {
        ("-S", socket_arg)
    } else {
        ("-L", socket_arg)
    };
    let out = Command::new("tmux")
        .args([flag, value, "list-windows", "-t", name, "-F", "#W"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output();
    match out {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout)
            .lines()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect(),
        _ => Vec::new(),
    }
}

#[track_caller]
pub fn assert_tmux_session_absent(name: &str) {
    assert!(
        !tmux_session_exists(name),
        "expected tmux session {name:?} to be absent, but it exists"
    );
}

#[track_caller]
pub fn assert_tmux_session_present(name: &str) {
    assert!(
        tmux_session_exists(name),
        "expected tmux session {name:?} to be present, but it is absent"
    );
}

/// Kill a tmux session on the default socket, ignoring errors (used in test
/// teardown belt-and-suspenders to clean up residual leader sessions a test
/// may have left if it crashed before completing shutdown).
pub fn tmux_kill_session_quiet(name: &str) {
    let _ = Command::new("tmux")
        .args(["kill-session", "-t", name])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .output();
}

// ----------------------------------------------------------------------------
// 5. File-system assertions
// ----------------------------------------------------------------------------

#[track_caller]
pub fn assert_file_exists(path: &Path) {
    assert!(path.exists(), "expected file {} to exist", path.display());
}

#[track_caller]
pub fn assert_file_absent(path: &Path) {
    assert!(
        !path.exists(),
        "expected file {} to be absent",
        path.display()
    );
}

/// Assert that a UTF-8 file contains a substring.
#[track_caller]
pub fn assert_file_contains(path: &Path, needle: &str) {
    let body =
        std::fs::read_to_string(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    assert!(
        body.contains(needle),
        "expected file {} to contain {needle:?}; full content:\n{body}",
        path.display()
    );
}

// ----------------------------------------------------------------------------
// 6. wait_for — bounded polling helper
// ----------------------------------------------------------------------------

/// Poll `predicate` until it returns `true` or `timeout` elapses. Returns
/// `true` if the predicate succeeded, `false` if it timed out. `poll_every`
/// caps how often the predicate is re-evaluated.
pub fn wait_for<F: FnMut() -> bool>(
    mut predicate: F,
    timeout: Duration,
    poll_every: Duration,
) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if predicate() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(poll_every);
    }
}

#[track_caller]
pub fn wait_for_or_panic<F: FnMut() -> bool>(description: &str, predicate: F, timeout: Duration) {
    let ok = wait_for(predicate, timeout, Duration::from_millis(100));
    assert!(
        ok,
        "timed out after {:?} waiting for: {description}",
        timeout
    );
}

/// Poll a delivery predicate and preserve enough evidence to classify a
/// timeout after `TestWorkspace` teardown removes the live fixture. The
/// snapshot intentionally lives outside the workspace and is keyed by the
/// exact message id; it is therefore safe for the controlled concurrent lane.
/// ---
/// purpose: Poll a message delivery obligation and retain timeout evidence
/// contract:
///   provides:
///     - name: wait_for_delivery_or_panic
///       what: Binds a delivery timeout to its row, coordinator, event, and physical target facts
///   depends:
///     - TestWorkspace-owned runtime files and tmux endpoint
/// boundary:
///   - Test-only timeout evidence; it does not alter delivery state
/// maturity: wired
/// ---
pub fn wait_for_delivery_or_panic<F: FnMut() -> bool>(
    ws: &TestWorkspace,
    message_id: &str,
    recipient: &str,
    description: &str,
    mut predicate: F,
    timeout: Duration,
) {
    let started = Instant::now();
    let mut assertion_count = 0_u64;
    let ok = wait_for(
        || {
            assertion_count += 1;
            predicate()
        },
        timeout,
        Duration::from_millis(100),
    );
    if ok {
        return;
    }
    let evidence = delivery_timeout_snapshot(
        ws,
        message_id,
        recipient,
        description,
        timeout,
        assertion_count,
        started.elapsed(),
    );
    let path = write_delivery_timeout_snapshot(&evidence);
    let evidence_json = serde_json::to_string_pretty(&evidence)
        .expect("serialize delivery timeout evidence for log");
    panic!(
        "timed out after {:?} waiting for: {description}; durable delivery evidence={}\nF14_DELIVERY_TIMEOUT_RECEIPT {evidence_json}",
        timeout,
        path.display()
    );
}

fn delivery_timeout_snapshot(
    ws: &TestWorkspace,
    message_id: &str,
    recipient: &str,
    description: &str,
    timeout: Duration,
    assertion_count: u64,
    duration: Duration,
) -> Value {
    let state = ws.state_json_path();
    let state_value = std::fs::read_to_string(&state)
        .ok()
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        .unwrap_or(Value::Null);
    let db_path = ws.path().join(".team/runtime/team.db");
    let same_message_rows = message_rows_snapshot(&db_path, message_id);
    let row = same_message_rows.first().cloned().unwrap_or_else(|| {
        serde_json::json!({
            "message_id": message_id,
            "recipient": null,
            "status": null,
            "error": null,
            "delivery_attempts": null,
            "delivered_at": null,
        })
    });
    let all_events = std::fs::read_to_string(ws.events_jsonl_path())
        .unwrap_or_default()
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .collect::<Vec<_>>();
    let events = all_events
        .iter()
        .rev()
        .take(64)
        .cloned()
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>();
    let message_events = all_events
        .iter()
        .filter(|event| {
            event.get("message_id").and_then(Value::as_str) == Some(message_id)
                || event
                    .get("task_id")
                    .and_then(Value::as_str)
                    .is_some_and(|task_id| task_id == message_id)
        })
        .cloned()
        .collect::<Vec<_>>();
    let coordinator_pid = read_pid(&ws.coordinator_pid_file());
    let coordinator_meta = read_json_file(&ws.path().join(".team/runtime/coordinator.json"));
    let heartbeat = read_json_file(&ws.path().join(".team/runtime/coordinator_tick.json"));
    let coordinator_boot_id = heartbeat
        .get("boot_id")
        .cloned()
        .or_else(|| coordinator_meta.get("boot_id").cloned())
        .unwrap_or(Value::Null);
    let coordinator_pid_file_process = coordinator_pid
        .into_iter()
        .map(|pid| {
            serde_json::json!({
                "pid": pid,
                "live": pid_is_running(pid),
                "source": "exact_workspace_pid_file",
            })
        })
        .collect::<Vec<_>>();
    let coordinator_wait = coordinator_wait_snapshot(coordinator_pid);
    let socket = state_value
        .get("tmux_socket")
        .and_then(Value::as_str)
        .unwrap_or("");
    let session = state_value
        .get("session_name")
        .and_then(Value::as_str)
        .unwrap_or("");
    let target = state_value
        .get("agents")
        .and_then(Value::as_object)
        .and_then(|agents| agents.get(recipient))
        .cloned()
        .unwrap_or(Value::Null);
    let pane_id = target.get("pane_id").and_then(Value::as_str).unwrap_or("");
    let pane_pid = target.get("pane_pid").cloned().unwrap_or(Value::Null);
    let window = target
        .get("window_name")
        .and_then(Value::as_str)
        .unwrap_or(recipient);
    let physical = tmux_target_snapshot(socket, session, window, pane_id);
    let fake_ready_marker = physical
        .get("capture")
        .and_then(Value::as_str)
        .is_some_and(|capture| {
            capture.contains(&format!("TEAM_AGENT_FAKE_READY agent={recipient}"))
        });
    let submit_verification =
        read_json_file(&ws.path().join(".team/runtime/submit-verification.json"));
    let submit_verification_matches_message = submit_verification
        .get("message_id")
        .and_then(Value::as_str)
        == Some(message_id);
    let resources = ws
        .owned_tmux_sockets
        .lock()
        .ok()
        .map(|sockets| {
            sockets
                .iter()
                .map(|socket| socket.to_string_lossy().to_string())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let mut snapshot = serde_json::json!({
        "schema_version": "team-agent-e2e-delivery-timeout-v3",
        "head": repository_head_sha(),
        "message_id": message_id,
        "recipient": recipient,
        "description": description,
        "timeout_ms": timeout.as_millis(),
        "row": row,
        "same_message_rows": same_message_rows,
        "coordinator": {
            "pid": coordinator_pid,
            "boot_id": coordinator_boot_id,
            "heartbeat": heartbeat,
            "tick": heartbeat.get("coordinator_tick_iteration_count").cloned().unwrap_or(Value::Null),
            "last_phase": heartbeat.get("last_phase").cloned().unwrap_or(Value::Null),
            "last_tick_status": heartbeat.get("last_tick_status").cloned().unwrap_or(Value::Null),
            "last_error": heartbeat.get("last_error").cloned().unwrap_or(Value::Null),
            "health": coordinator_meta,
            "pid_file_process": coordinator_pid_file_process,
            "wait_snapshot": coordinator_wait,
        },
        "worker": {
            "state": target.get("worker").cloned().unwrap_or(Value::Null),
            "fake_ready_marker": fake_ready_marker,
            "submit_verification": submit_verification,
            "submit_verification_matches_message": submit_verification_matches_message,
        },
        "events": events,
        "message_events": message_events,
        "target": {
            "endpoint": socket,
            "tmux_endpoint": socket,
            "session": session,
            "target_session": session,
            "window": window,
            "target_window": window,
            "pane": pane_id,
            "target_pane_id": pane_id,
            "pane_pid": pane_pid,
            "target_pane_pid": pane_pid,
            "physical": physical,
            "resolved_from": target.get("resolved_from").cloned().unwrap_or(Value::Null),
            "agent_state": target,
        },
        "fixture": {
            "workspace": ws.path(),
            "coordinator_pid_file": ws.coordinator_pid_file(),
            "owned_tmux_sockets": &resources,
            "resource_ledger": {
                "workspace": ws.path(),
                "coordinator_pid_file": ws.coordinator_pid_file(),
                "tmux_sockets": &resources,
                "resource_count": resources.len(),
                "cleanup": "exact_registered_resources_before_workspace_removal",
            },
        },
        "execution": {
            "assertion_count": assertion_count,
            "duration_ms": duration.as_millis(),
        },
    });
    let digest_material = serde_json::to_vec(&snapshot).expect("serialize receipt digest material");
    let digest = Sha256::digest(&digest_material);
    snapshot["receipt"] = serde_json::json!({
        "sha256": format!("{digest:x}"),
        "bytes_before_digest": digest_material.len(),
    });
    snapshot
}

fn repository_head_sha() -> String {
    let output = Command::new("git")
        .args(["-C", env!("CARGO_MANIFEST_DIR"), "rev-parse", "HEAD"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("resolve exact repository head for delivery-timeout receipt");
    assert!(
        output.status.success(),
        "git rev-parse HEAD failed for delivery-timeout receipt: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn read_json_file(path: &Path) -> Value {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or(Value::Null)
}

fn coordinator_wait_snapshot(pid: Option<u32>) -> Value {
    let Some(pid) = pid else {
        return serde_json::json!({"pid": null, "process": null, "children": [], "sample": null});
    };
    let pid_arg = pid.to_string();
    let process = Command::new("ps")
        .args(["-o", "pid=,ppid=,etime=,stat=,comm=", "-p", &pid_arg])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string());
    let child_ids = Command::new("pgrep")
        .args(["-P", &pid_arg])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| {
            String::from_utf8_lossy(&output.stdout)
                .split_whitespace()
                .filter_map(|raw| raw.parse::<u32>().ok())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let children = child_ids
        .iter()
        .filter_map(|child| {
            let child_arg = child.to_string();
            Command::new("ps")
                .args(["-o", "pid=,ppid=,etime=,stat=,comm=", "-p", &child_arg])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .output()
                .ok()
                .filter(|output| output.status.success())
                .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
        })
        .collect::<Vec<_>>();

    #[cfg(target_os = "macos")]
    let sample = Command::new("/usr/bin/sample")
        .args([&pid_arg, "1", "1"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .ok()
        .map(|output| {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let relevant = stdout
                .lines()
                .filter(|line| {
                    let lower = line.to_ascii_lowercase();
                    [
                        "team_agent", "tmux", "delivery", "inject", "capture", "process",
                        "wait", "flock", "sqlite", "rusqlite", "psynch", "kevent", "poll",
                    ]
                    .iter()
                    .any(|needle| lower.contains(needle))
                })
                .take(256)
                .map(str::to_string)
                .collect::<Vec<_>>();
            serde_json::json!({
                "exit_code": output.status.code(),
                "relevant_lines": relevant,
                "stderr": String::from_utf8_lossy(&output.stderr).trim(),
            })
        })
        .unwrap_or(Value::Null);
    #[cfg(not(target_os = "macos"))]
    let sample = Value::Null;

    serde_json::json!({
        "pid": pid,
        "process": process,
        "children": children,
        "sample": sample,
    })
}

fn record_last_command_observation(
    ws: &TestWorkspace,
    result: &TaResult,
    extra_env: &[(&str, &str)],
) {
    let effective_home = extra_env
        .iter()
        .rev()
        .find_map(|(key, value)| (*key == "HOME").then_some(PathBuf::from(*value)))
        .or_else(|| std::env::var_os("HOME").map(PathBuf::from));
    let command_json = serde_json::from_str::<Value>(&result.stdout).unwrap_or(Value::Null);
    let message_id = command_json
        .get("message_id")
        .and_then(Value::as_str)
        .unwrap_or("");
    let recipient = (result.argv.get(1).map(String::as_str) == Some("send"))
        .then(|| result.argv.get(2).map(String::as_str))
        .flatten()
        .unwrap_or("");
    let observation = serde_json::json!({
        "schema_version": "team-agent-e2e-last-command-v1",
        "command": {
            "argv": &result.argv,
            "exit_code": result.exit_code,
            "stdout": &result.stdout,
            "stderr": &result.stderr,
            "effective_home": effective_home.as_deref(),
            "message_id": message_id,
            "recipient": recipient,
        },
    });
    if let Ok(encoded) = serde_json::to_vec_pretty(&observation) {
        let _ = std::fs::write(ws.path().join(LAST_COMMAND_OBSERVATION), encoded);
    }
}

fn emit_fixture_panic_receipt(ws: &TestWorkspace) {
    let last_command = read_json_file(&ws.path().join(LAST_COMMAND_OBSERVATION));
    let effective_home = last_command
        .pointer("/command/effective_home")
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(PathBuf::from));
    let message_id = last_command
        .pointer("/command/message_id")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let recipient = last_command
        .pointer("/command/recipient")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let receipt = serde_json::json!({
        "schema_version": "team-agent-e2e-fixture-panic-v1",
        "last_command": last_command,
        "at_unwind": fixture_runtime_snapshot(
            ws,
            effective_home.as_deref(),
            &message_id,
            &recipient,
        ),
        "cleanup_order": "receipt_then_exact_coordinator_then_registered_tmux_then_workspace",
    });
    let encoded = serde_json::to_string_pretty(&receipt)
        .unwrap_or_else(|error| format!("{{\"serialization_error\":{error:?}}}"));
    eprintln!("F14_SHARED_FIXTURE_PANIC_RECEIPT {encoded}");
}

fn fixture_runtime_snapshot(
    ws: &TestWorkspace,
    effective_home: Option<&Path>,
    message_id: &str,
    recipient: &str,
) -> Value {
    let state = read_json_file(&ws.state_json_path());
    let coordinator_pid = read_pid(&ws.coordinator_pid_file());
    let events = std::fs::read_to_string(ws.events_jsonl_path())
        .unwrap_or_default()
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .rev()
        .take(64)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>();
    let socket = state
        .get("tmux_socket")
        .and_then(Value::as_str)
        .unwrap_or("");
    let session = state
        .get("session_name")
        .and_then(Value::as_str)
        .unwrap_or("");
    let target = state
        .get("agents")
        .and_then(Value::as_object)
        .and_then(|agents| agents.get(recipient))
        .cloned()
        .unwrap_or(Value::Null);
    let pane_id = target.get("pane_id").and_then(Value::as_str).unwrap_or("");
    let window = target
        .get("window_name")
        .and_then(Value::as_str)
        .unwrap_or(recipient);
    let owned_tmux_sockets = ws
        .owned_tmux_sockets
        .lock()
        .ok()
        .map(|sockets| sockets.clone())
        .unwrap_or_default();
    serde_json::json!({
        "workspace": ws.path(),
        "owned_tmux_sockets": owned_tmux_sockets,
        "state": {
            "active_team_key": state.get("active_team_key").cloned().unwrap_or(Value::Null),
            "session_name": state.get("session_name").cloned().unwrap_or(Value::Null),
            "tmux_socket": state.get("tmux_socket").cloned().unwrap_or(Value::Null),
            "agents": state.get("agents").cloned().unwrap_or(Value::Null),
        },
        "coordinator": {
            "pid": coordinator_pid,
            "pid_live": coordinator_pid.map(pid_is_running),
            "metadata": read_json_file(&ws.path().join(".team/runtime/coordinator.json")),
            "heartbeat": read_json_file(&ws.path().join(".team/runtime/coordinator_tick.json")),
            "log_tail": read_text_tail(&ws.path().join(".team/runtime/coordinator.log"), 64),
        },
        "message_rows": if message_id.is_empty() {
            Vec::<Value>::new()
        } else {
            message_rows_snapshot(&ws.path().join(".team/runtime/team.db"), message_id)
        },
        "events_tail": events,
        "tmux": if !recipient.is_empty() {
            tmux_target_snapshot(socket, session, window, pane_id)
        } else {
            serde_json::json!({
                "socket": if socket.is_empty() { Value::Null } else { Value::String(socket.to_string()) },
                "socket_exists": !socket.is_empty() && Path::new(socket).exists(),
                "liveness": "not_probed_without_recipient",
            })
        },
        "leader_registry": scoped_leader_registry_snapshot(effective_home, ws.path(), &state),
    })
}

fn read_text_tail(path: &Path, max_lines: usize) -> Vec<String> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .rev()
        .take(max_lines)
        .map(str::to_string)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect()
}

fn scoped_leader_registry_snapshot(
    effective_home: Option<&Path>,
    workspace: &Path,
    state: &Value,
) -> Value {
    let Some(home) = effective_home else {
        return serde_json::json!({"effective_home": null, "path": null, "entry": null});
    };
    let Some(team_key) = state.get("active_team_key").and_then(Value::as_str) else {
        return serde_json::json!({"effective_home": home, "path": null, "entry": null});
    };
    let workspace_hash = team_agent::leader::registry::workspace_hash(workspace);
    let path = home
        .join(".team-agent/leaders")
        .join(format!("{workspace_hash}__{team_key}.json"));
    serde_json::json!({
        "effective_home": home,
        "path": path,
        "entry": read_json_file(&path),
    })
}

fn message_rows_snapshot(db_path: &Path, message_id: &str) -> Vec<Value> {
    let Ok(conn) = Connection::open(db_path) else {
        return Vec::new();
    };
    let Ok(mut stmt) = conn.prepare(
        "select recipient, status, error, delivery_attempts, delivered_at \
         from messages where message_id = ?1 order by recipient",
    ) else {
        return Vec::new();
    };
    let Ok(rows) = stmt.query_map([message_id], |row| {
        Ok(serde_json::json!({
            "message_id": message_id,
            "recipient": row.get::<_, String>(0)?,
            "status": row.get::<_, String>(1)?,
            "error": row.get::<_, Option<String>>(2)?,
            "delivery_attempts": row.get::<_, i64>(3)?,
            "delivered_at": row.get::<_, Option<String>>(4)?,
        }))
    }) else {
        return Vec::new();
    };
    rows.filter_map(Result::ok).collect()
}

fn tmux_target_snapshot(socket: &str, session: &str, window: &str, pane_id: &str) -> Value {
    if socket.is_empty() {
        return serde_json::json!({"liveness": "unknown", "capture": null});
    }
    let target = if pane_id.is_empty() {
        format!("{session}:{window}")
    } else {
        pane_id.to_string()
    };
    let out = Command::new("tmux")
        .args([
            "-S",
            socket,
            "list-panes",
            "-t",
            &target,
            "-F",
            "#{session_id}|#{session_name}|#{window_id}|#{window_name}|#{pane_id}|#{pane_pid}|#{pane_dead}|#{pane_dead_status}|#{pane_current_command}|#{pane_start_command}|#{pane_current_path}",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output();
    let (liveness, tuple, list_panes_exit, stderr) = match out {
        Ok(output) => (
            if output.status.success() {
                "live"
            } else {
                "missing"
            },
            String::from_utf8_lossy(&output.stdout).trim().to_string(),
            output.status.code(),
            String::from_utf8_lossy(&output.stderr).trim().to_string(),
        ),
        Err(error) => ("unknown", String::new(), None, error.to_string()),
    };
    let capture_output = Command::new("tmux")
        .args(["-S", socket, "capture-pane", "-t", &target, "-p"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .ok();
    let capture_exit = capture_output
        .as_ref()
        .and_then(|output| output.status.code());
    let capture = capture_output
        .map(|output| String::from_utf8_lossy(&output.stdout).into_owned());
    serde_json::json!({
        "socket": socket,
        "requested_target": target,
        "liveness": liveness,
        "tuple": tuple,
        "tuple_fields": "session_id|session_name|window_id|window_name|pane_id|pane_pid|pane_dead|pane_dead_status|pane_current_command|pane_start_command|pane_current_path",
        "list_panes_exit": list_panes_exit,
        "stderr": stderr,
        "capture_exit": capture_exit,
        "capture": capture,
    })
}

fn write_delivery_timeout_snapshot(snapshot: &Value) -> PathBuf {
    let root = std::env::var_os("TEAM_AGENT_E2E_EVIDENCE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            if Path::new("/private/tmp").is_dir() {
                PathBuf::from("/private/tmp/team-agent-e2e-timeouts")
            } else {
                std::env::temp_dir().join("team-agent-e2e-timeouts")
            }
        });
    std::fs::create_dir_all(&root).expect("create durable E2E evidence directory");
    let message_id = snapshot["message_id"].as_str().unwrap_or("unknown");
    let filename = format!(
        "delivery-timeout-{}-{}-{}.json",
        std::process::id(),
        WORKSPACE_COUNTER.fetch_add(1, Ordering::Relaxed),
        message_id
            .chars()
            .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '_' })
            .collect::<String>()
    );
    let path = root.join(filename);
    let encoded = serde_json::to_vec_pretty(snapshot).expect("serialize delivery timeout evidence");
    std::fs::write(&path, encoded).expect("write durable E2E evidence");
    path
}

// ----------------------------------------------------------------------------
// 7. FakeProvider notes
// ----------------------------------------------------------------------------
//
// The team-agent binary already ships a built-in fake provider via
// `provider: fake` in the spec — it invokes `team-agent fake-worker` as the
// worker process (see crates/team-agent/src/main.rs:6 and
// crates/team-agent/src/provider/adapter.rs `fake_worker_command()`).
//
// This framework therefore does NOT ship a shell script — `TestWorkspace::
// with_fake_spec` writes a TEAM.md + agents/*.md tree that selects
// `provider: fake`. Test bodies that don't need a live worker (state-only
// tests like `restart_refuses_stale_session`) can skip
// `with_fake_spec` entirely and use `seed_state` instead.

// ----------------------------------------------------------------------------
// 8. Convenience: build a runtime workspace with a single fake-spec quick-start
// ----------------------------------------------------------------------------

/// Convenience: was the quick-start good enough for E2E to continue? Returns
/// true if the JSON shows the team was launched, even when the leader receiver
/// is unbound (which is normal under `cargo test` where no $TMUX is exported
/// — the framework strips TMUX to keep test isolation, so leader pane binding
/// fails by design). Tests that specifically need a bound leader_receiver
/// should attach manually or assert on `qs.json()["status"]` themselves.
pub fn quick_start_launched(result: &TaResult) -> bool {
    let j = result.json();
    let ok = j.pointer("/ok").and_then(|v| v.as_bool()).unwrap_or(false);
    let status = j
        .pointer("/status")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let all_workers_spawned = json_all_workers_spawned(&j);
    // ok==true is the happy path; ok==false but `leader_receiver_unbound` /
    // `pending_tool_load` is acceptable for E2E (workers spawned, only the
    // leader binding gate failed because cargo test runs outside tmux).
    if ok {
        return true;
    }
    let acceptable_degraded = [
        "leader_receiver_unbound",
        "pending_tool_load",
        "pending_session_capture",
    ];
    if all_workers_spawned && acceptable_degraded.iter().any(|s| status == *s) {
        return true;
    }
    false
}

/// Some tests want a workspace that has gone through quick-start so state.json
/// + events.jsonl exist with realistic shape. This helper does that and
/// returns the result for further inspection.
pub fn quick_start_fake(ws: &TestWorkspace, team_id: &str) -> TaResult {
    let ws_str = ws.path().to_str().expect("workspace path utf8").to_string();
    let result = run_ta(
        ws,
        &[
            "quick-start",
            &ws_str,
            "--workspace",
            &ws_str,
            "--team-id",
            team_id,
            "--yes",
            "--no-display",
            "--json",
        ],
    );
    result
}

#[cfg(unix)]
fn disable_fake_worker_tty_echo(ws: &TestWorkspace) {
    let state = ws.read_state();
    let socket = state
        .get("tmux_socket")
        .and_then(Value::as_str)
        .expect("successful fake quick-start must record its exact tmux socket");
    let agents = state
        .get("agents")
        .and_then(Value::as_object)
        .expect("successful fake quick-start must record its agents");

    for (agent_id, agent) in agents {
        let pane_id = agent
            .get("pane_id")
            .and_then(Value::as_str)
            .unwrap_or_else(|| {
                panic!("successful fake quick-start must record pane_id for {agent_id}")
            });
        eprintln!(
            "[e2e-fake-tty] query socket={socket} agent={agent_id} pane={pane_id} command=tmux -S <exact-socket> display-message -p -t <exact-pane> #{{pane_tty}}"
        );
        let tty_query = Command::new("tmux")
            .args([
                "-S",
                socket,
                "display-message",
                "-p",
                "-t",
                pane_id,
                "#{pane_tty}",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .unwrap_or_else(|error| panic!("query fake worker pane TTY for {agent_id}: {error}"));
        assert!(
            tty_query.status.success(),
            "query fake worker pane TTY failed: socket={socket} agent={agent_id} pane={pane_id} status={} stderr={}",
            tty_query.status,
            String::from_utf8_lossy(&tty_query.stderr)
        );
        let tty = String::from_utf8_lossy(&tty_query.stdout)
            .trim()
            .to_string();
        assert!(
            !tty.is_empty(),
            "query fake worker pane TTY returned empty path: socket={socket} agent={agent_id} pane={pane_id}"
        );

        #[cfg(target_os = "macos")]
        let stty_flag = "-f";
        #[cfg(not(target_os = "macos"))]
        let stty_flag = "-F";
        eprintln!(
            "[e2e-fake-tty] disable-echo socket={socket} agent={agent_id} pane={pane_id} tty={tty} command=stty {stty_flag} <exact-tty> -echo"
        );
        let disable = Command::new("stty")
            .args([stty_flag, &tty, "-echo"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .unwrap_or_else(|error| panic!("disable fake worker TTY echo for {agent_id}: {error}"));
        eprintln!(
            "[e2e-fake-tty] disable-echo-result socket={socket} agent={agent_id} pane={pane_id} tty={tty} status={}",
            disable.status
        );
        assert!(
            disable.status.success(),
            "disable fake worker TTY echo failed: socket={socket} agent={agent_id} pane={pane_id} tty={tty} status={} stderr={}",
            disable.status,
            String::from_utf8_lossy(&disable.stderr)
        );
    }
}

#[cfg(not(unix))]
fn disable_fake_worker_tty_echo(_ws: &TestWorkspace) {}

/// Sanitize team_id into the tmux session name as the runtime does:
/// session = `team-<team_id>` (lowercased, no transformation needed for our
/// safe ids). Use this everywhere to avoid scattering the convention.
pub fn worker_session_name(team_id: &str) -> String {
    format!("team-{team_id}")
}

/// Read `tmux_socket` from state.json (full path) and check whether a session
/// exists on that specific socket. Returns `false` if state.json doesn't yet
/// have a socket entry — callers should treat that as "no live tmux yet".
pub fn tmux_session_exists_for_workspace(ws: &TestWorkspace, name: &str) -> bool {
    let state_path = ws.state_json_path();
    if !state_path.exists() {
        return false;
    }
    let state = ws.read_state();
    let socket = state
        .get("tmux_socket")
        .and_then(Value::as_str)
        .unwrap_or("");
    if socket.is_empty() {
        return tmux_session_exists(name);
    }
    tmux_session_exists_on_socket(socket, name)
}

pub fn tmux_windows_for_workspace(ws: &TestWorkspace, name: &str) -> Vec<String> {
    let state_path = ws.state_json_path();
    if !state_path.exists() {
        return Vec::new();
    }
    let state = ws.read_state();
    let socket = state
        .get("tmux_socket")
        .and_then(Value::as_str)
        .unwrap_or("");
    if socket.is_empty() {
        let out = Command::new("tmux")
            .args(["list-windows", "-t", name, "-F", "#W"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output();
        return match out {
            Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout)
                .lines()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect(),
            _ => Vec::new(),
        };
    }
    tmux_windows_on_socket(socket, name)
}

pub fn tmux_window_exists_for_workspace(ws: &TestWorkspace, session: &str, window: &str) -> bool {
    tmux_windows_for_workspace(ws, session)
        .iter()
        .any(|name| name == window)
}

#[track_caller]
pub fn state_agent<'a>(state: &'a Value, agent_id: &str) -> &'a Value {
    state
        .get("agents")
        .and_then(Value::as_object)
        .and_then(|agents| agents.get(agent_id))
        .unwrap_or_else(|| panic!("state.agents.{agent_id} missing in {state}"))
}

pub fn state_has_agent(state: &Value, agent_id: &str) -> bool {
    state
        .get("agents")
        .and_then(Value::as_object)
        .is_some_and(|agents| agents.contains_key(agent_id))
}

/// Misc helper: collect a tag → value map of all keys present at the top
/// level of state.json. Useful for diagnostic prints in failing tests.
pub fn state_top_level_keys(state: &Value) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    if let Some(obj) = state.as_object() {
        for (k, v) in obj {
            out.insert(
                k.clone(),
                match v {
                    Value::Object(_) => "<object>".to_string(),
                    Value::Array(a) => format!("<array len={}>", a.len()),
                    other => other.to_string(),
                },
            );
        }
    }
    out
}

#[cfg(all(test, unix))]
mod containment_tests {
    use super::*;
    use std::os::unix::net::UnixListener;
    use std::panic::{catch_unwind, AssertUnwindSafe};

    fn socket(path: &Path) -> UnixListener {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create socket parent");
        }
        let _ = std::fs::remove_file(path);
        UnixListener::bind(path).unwrap_or_else(|error| {
            panic!(
                "bind isolated F14 control socket {}: {error}",
                path.display()
            )
        })
    }

    fn tmux_socket(path: &Path, session: &str) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create tmux socket parent");
        }
        let _ = std::fs::remove_file(path);
        let output = Command::new("tmux")
            .args([
                "-S",
                path.to_str().unwrap(),
                "new-session",
                "-d",
                "-s",
                session,
            ])
            .output()
            .expect("start isolated F14 tmux server");
        assert!(
            output.status.success(),
            "start isolated F14 tmux server: stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            path.exists(),
            "tmux must create endpoint {}",
            path.display()
        );
    }

    fn ledger_len(ws: &TestWorkspace) -> usize {
        ws.owned_tmux_sockets
            .lock()
            .expect("ownership ledger lock")
            .len()
    }

    struct AmbientTmuxGuard(Option<std::ffi::OsString>);

    impl Drop for AmbientTmuxGuard {
        fn drop(&mut self) {
            match self.0.take() {
                Some(value) => std::env::set_var("TMUX", value),
                None => std::env::remove_var("TMUX"),
            }
        }
    }

    #[test]
    fn f14_owned_socket_registration_is_exact_and_drop_spares_unregistered_socket() {
        let nonce = format!(
            "{}-{}",
            std::process::id(),
            WORKSPACE_COUNTER.fetch_add(1, Ordering::Relaxed)
        );
        let tmp_root = normalize_existing_path(&std::env::temp_dir());
        let uid_root = tmp_root.join(format!("tmux-{}", unsafe { libc::geteuid() }));
        let owned_path = uid_root.join(format!("ta-f14-owned-{nonce}"));
        let outside_path = tmp_root.join(format!("ta-f14-outside-{nonce}"));
        let unowned_path = uid_root.join(format!("foreign-f14-unowned-{nonce}"));
        // Keep the ambient control endpoint valid for every non-ambient
        // ownership predicate: only the ambient TMUX equality guard should
        // reject it.  Outside-root and bad-basename controls remain
        // independent below.
        let ambient_path = uid_root.join(format!("ta-f14-ambient-{nonce}"));
        let session = format!("f14-{nonce}");
        tmux_socket(&owned_path, &session);
        let outside_listener = socket(&outside_path);
        let unowned_listener = socket(&unowned_path);
        let ambient_listener = socket(&ambient_path);
        assert!(
            is_owned_tmux_socket_path(&ambient_path),
            "ambient control must otherwise be a valid owned-shaped socket"
        );

        let workspace_path;
        {
            let ws = TestWorkspace::new("f14-containment");
            workspace_path = ws.path().to_path_buf();
            ws.register_owned_tmux_socket(&owned_path);
            assert_eq!(
                ledger_len(&ws),
                1,
                "exactly one owned endpoint is registered"
            );
            assert_eq!(
                ws.owned_tmux_sockets.lock().unwrap()[0],
                normalize_existing_path(&owned_path),
                "ledger stores the exact canonical owned endpoint"
            );

            assert!(
                catch_unwind(AssertUnwindSafe(
                    || ws.register_owned_tmux_socket(&outside_path)
                ))
                .is_err(),
                "outside socket must fail closed"
            );
            assert_eq!(ledger_len(&ws), 1, "outside socket was not appended");
            assert!(
                catch_unwind(AssertUnwindSafe(
                    || ws.register_owned_tmux_socket(&unowned_path)
                ))
                .is_err(),
                "unowned-shaped socket must fail closed"
            );
            assert_eq!(ledger_len(&ws), 1, "unowned socket was not appended");

            let previous_tmux = std::env::var_os("TMUX");
            let _ambient_guard = AmbientTmuxGuard(previous_tmux);
            std::env::set_var(
                "TMUX",
                format!("{},f14-ambient-pane", ambient_path.display()),
            );
            assert_eq!(
                std::env::var_os("TMUX")
                    .and_then(|value| value.to_str()?.split(',').next().map(PathBuf::from))
                    .map(|path| normalize_existing_path(&path)),
                Some(normalize_existing_path(&ambient_path)),
                "ambient TMUX must identify exactly the valid ambient control endpoint"
            );
            assert!(
                catch_unwind(AssertUnwindSafe(
                    || ws.register_owned_tmux_socket(&ambient_path)
                ))
                .is_err(),
                "ambient TMUX endpoint must fail closed"
            );
            assert_eq!(ledger_len(&ws), 1, "ambient socket was not appended");
        }

        let owned_absent = !owned_path.exists();
        let workspace_absent = !workspace_path.exists();
        let outside_spared = outside_path.exists();
        let unowned_spared = unowned_path.exists();
        let ambient_spared = ambient_path.exists();
        assert!(owned_absent, "Drop must remove the exact owned endpoint");
        assert!(workspace_absent, "Drop must remove the fixture workspace");
        assert!(outside_spared, "Drop must spare the outside endpoint");
        assert!(unowned_spared, "Drop must spare the unowned endpoint");
        assert!(ambient_spared, "Drop must spare the ambient endpoint");
        println!(
            "F14_CONTAINMENT_RECEIPT {}",
            serde_json::json!({
                "ledger_count_before_drop": 1,
                "owned_endpoint": owned_path,
                "owned_endpoint_absent_after_drop": owned_absent,
                "workspace": workspace_path,
                "workspace_absent_after_drop": workspace_absent,
                "outside_endpoint_spared": outside_spared,
                "unowned_endpoint_spared": unowned_spared,
                "ambient_endpoint_spared": ambient_spared,
                "ambient_endpoint_valid_owned_shape": is_owned_tmux_socket_path(&ambient_path),
                "ambient_rejection_reason": "ambient_tmux_endpoint_matches_exact_endpoint",
                "cleanup": "exact_registered_endpoint_only"
            })
        );

        drop(outside_listener);
        drop(unowned_listener);
        drop(ambient_listener);
        std::fs::remove_file(&outside_path).expect("cleanup outside control socket");
        std::fs::remove_file(&unowned_path).expect("cleanup unowned control socket");
        std::fs::remove_file(&ambient_path).expect("cleanup ambient control socket");
        assert!(!outside_path.exists());
        assert!(!unowned_path.exists());
        assert!(!ambient_path.exists());
        let _ = std::fs::remove_dir(&uid_root);
    }
}
