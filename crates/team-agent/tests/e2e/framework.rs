//! E2E framework: TestWorkspace + run_ta + assert helpers + FakeProvider
//! support + state injection + wait_for. Zero external test-CLI deps —
//! framework uses `std::process::Command`, `serde_json`, and the existing
//! `team-agent` binary built by `cargo test`.
//!
//! All test helpers panic on programmer error (wrong binary path, write
//! failure on a temp dir we own) and return `Result` / printable diagnostics
//! when the SUT misbehaves.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::Value;

// ----------------------------------------------------------------------------
// 1. TestWorkspace — per-test temp dir with auto-cleanup (Drop)
// ----------------------------------------------------------------------------

static WORKSPACE_COUNTER: AtomicU64 = AtomicU64::new(0);

/// A self-cleaning workspace directory under `/private/tmp` (preferred on
/// macOS so it survives `/tmp -> /private/tmp` symlink resolution that some
/// runtime paths do) or `std::env::temp_dir()` elsewhere. The directory is
/// removed on `Drop` unless `TEAM_AGENT_KEEP_TEST_TMP=1` is set.
pub struct TestWorkspace {
    path: PathBuf,
    ta_binary: Mutex<Option<PathBuf>>,
    /// 0.5.43 debt-sweep (§6.1): exact test-owned tmux sockets to
    /// clean at Drop. Populated by `register_owned_tmux_socket`. Drop
    /// runs `tmux -S <sock> kill-server` on each (never a host scan)
    /// BEFORE the workspace directory removal (verified by RED
    /// `e2e_workspace_drop_cleans_exact_tmux_before_removing_workspace`).
    owned_tmux_sockets: Mutex<Vec<PathBuf>>,
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
        if let Ok(mut sockets) = self.owned_tmux_sockets.lock() {
            sockets.push(socket.to_path_buf());
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn record_ta_binary(&self, path: &Path) {
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
                "---\nname: {id}\nrole: Fake worker {id}\nprovider: fake\nmodel: fake\nauth_mode: subscription\ntools:\n  - mcp_team\n---\n\nFake worker {id}.\n",
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

    fn coordinator_pid_file(&self) -> PathBuf {
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

    fn pid_is_owned_coordinator(&self, pid: u32) -> bool {
        pid != std::process::id()
            && ps_command(pid)
                .as_deref()
                .is_some_and(|command| self.command_is_owned_coordinator(command))
    }

    fn command_is_owned_coordinator(&self, command: &str) -> bool {
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

fn read_pid(path: &Path) -> Option<u32> {
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

fn normalize_existing_path(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
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

fn pid_is_running(pid: u32) -> bool {
    // 0.5.x Windows portability Batch 5: route through
    // `platform::process::pid_is_alive` — same shape on both
    // platforms. The former inline `libc::kill(pid, 0)` +
    // last-os-error ESRCH check maps to `ProcessLiveness::Live` /
    // `Dead` from `pid_liveness`.
    team_agent::platform::process::pid_is_alive(pid)
}

fn wait_until_pid_exits(pid: u32, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if !pid_is_running(pid) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    !pid_is_running(pid)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owned_coordinator_predicate_requires_debug_binary_and_e2e_workspace() {
        let ws = TestWorkspace::new("cleanup-predicate");
        let bin = ta_binary();
        ws.record_ta_binary(&bin);
        let workspace = ws.path().to_string_lossy();

        let owned = format!("{} coordinator --workspace {workspace}", bin.display());
        assert!(
            ws.command_is_owned_coordinator(&owned),
            "debug binary plus exact e2e temp workspace should be owned"
        );

        let platform_tmp_ws = TestWorkspace {
            path: normalize_existing_path(&std::env::temp_dir())
                .join(format!("ta-e2e-platform-{}-0", std::process::id())),
            ta_binary: Mutex::new(Some(normalize_existing_path(&bin))),
            owned_tmux_sockets: Mutex::new(Vec::new()),
        };
        let platform_tmp_owned = format!(
            "{} coordinator --workspace {}",
            bin.display(),
            platform_tmp_ws.path().display()
        );
        assert!(
            platform_tmp_ws.command_is_owned_coordinator(&platform_tmp_owned),
            "debug binary plus exact platform temp e2e workspace should be owned"
        );

        let local =
            format!("/Users/alauda/.local/bin/team-agent coordinator --workspace {workspace}");
        assert!(
            !ws.command_is_owned_coordinator(&local),
            "installed local binary must never be owned by the E2E cleanup"
        );

        let runtime = format!(
            "/Users/alauda/.team-agent/runtime/0.4.8/bin/team-agent coordinator --workspace {workspace}"
        );
        assert!(
            !ws.command_is_owned_coordinator(&runtime),
            "runtime-installed binary must never be owned by the E2E cleanup"
        );

        let real_workspace = format!(
            "{} coordinator --workspace /Users/alauda/Documents/code/team-agent-public",
            bin.display()
        );
        assert!(
            !ws.command_is_owned_coordinator(&real_workspace),
            "debug binary alone is not enough without the exact private e2e workspace"
        );
    }

    #[test]
    fn drop_stops_owned_coordinator_after_worker_is_stopped() {
        let team_id = format!("cleanup{}", std::process::id());
        let (workspace, coordinator_pid) = {
            let ws = TestWorkspace::new("cleanup-drop").with_fake_spec(&["a"]);
            let qs = quick_start_fake(&ws, &team_id);
            assert!(quick_start_launched(&qs), "quick-start: {}", qs.stdout);

            let stop = run_ta(
                &ws,
                &[
                    "stop-agent",
                    "a",
                    "--workspace",
                    ws.path().to_str().unwrap(),
                    "--json",
                ],
            );
            assert!(
                stop.is_success(),
                "stop-agent exit {}; stdout={} stderr={}",
                stop.exit_code,
                stop.stdout,
                stop.stderr
            );

            wait_for_or_panic(
                "coordinator pid file",
                || read_pid(&ws.coordinator_pid_file()).is_some(),
                Duration::from_secs(3),
            );
            let pid = read_pid(&ws.coordinator_pid_file()).expect("coordinator pid");
            assert!(
                ws.pid_is_owned_coordinator(pid),
                "pid {pid} should be the workspace-owned debug coordinator"
            );
            assert!(
                pid_is_running(pid),
                "coordinator pid {pid} should be live before Drop"
            );
            (ws.path().to_path_buf(), pid)
        };

        assert!(
            wait_until_pid_exits(coordinator_pid, Duration::from_secs(3)),
            "coordinator pid {coordinator_pid} should be stopped by TestWorkspace::Drop"
        );
        assert!(
            !workspace.exists(),
            "workspace {} should be removed by TestWorkspace::Drop",
            workspace.display()
        );
    }
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
fn ta_binary() -> PathBuf {
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
        .stderr(Stdio::piped())
        // Isolate test runs from the caller's tmux state. The binary still
        // detects $TMUX when set; leaving it unset forces the framework
        // tests to opt-in via `with_tmux_pane()` (not implemented in this
        // skeleton — future addition).
        .env_remove("TMUX")
        .env_remove("TMUX_PANE");
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
    TaResult {
        argv: std::iter::once("team-agent".to_string())
            .chain(args.iter().map(|s| (*s).to_string()))
            .collect(),
        exit_code: status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&stdout).into_owned(),
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
    }
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
    let all_workers_spawned = j
        .pointer("/readiness/all_workers_spawned")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
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
    run_ta(
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
    )
}

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
