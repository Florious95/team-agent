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
        Self { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
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
}

impl Drop for TestWorkspace {
    fn drop(&mut self) {
        if std::env::var("TEAM_AGENT_KEEP_TEST_TMP").as_deref() != Ok("1") {
            let _ = std::fs::remove_dir_all(&self.path);
        }
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
    let bin = ta_binary();
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
    let Output { status, stdout, stderr } = cmd
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
    let body = std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
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
pub fn wait_for_or_panic<F: FnMut() -> bool>(
    description: &str,
    predicate: F,
    timeout: Duration,
) {
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
