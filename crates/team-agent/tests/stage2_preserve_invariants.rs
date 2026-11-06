//! Stage 2 excision (P3 coordinator tick engine; P4 attach/takeover/rediscovery,
//! fork shims and the dead ProviderAdapter surface, plan orchestrator, CLI residue,
//! legacy packaging) must not change current single-point CLI behavior. Every case is
//! GREEN on the Stage 1 final baseline (0cd6e76e) and must stay GREEN after excision.
//!
//! a) Pure CLI single point: start, stop, restart, send, status and doctor (plus inbox,
//!    locate, approvals, schedule) each return on their own, leave no resident
//!    team-agent process outside the tmux pane tree, never materialize coordinator or
//!    tick artifacts, never claim a coordinator was started, and never signal a
//!    reused legacy coordinator PID.
//! b) ProviderAdapter purity: native providers execute the public command resolved
//!    from the caller's PATH (user wrappers respected); Claude fresh/resume/fork plans
//!    are the native `claude` argv with native `--resume` / `--fork-session`; Codex,
//!    Grok and Pi get no legacy adapter command.
//! c) Fail-closed barriers: retired leader verbs refuse before parsing and name
//!    `claim-leader`; removed P3/P4 verbs and plan flags are unknown/usage errors;
//!    help never advertises them; nothing is written or spawned.
//!
//! Properties use a seeded SplitMix64 generator (no new test dependencies).
//! Reproduce a failure with `STAGE2_PROP_SEED=<seed>`; widen the search with
//! `STAGE2_PROP_CASES=<n>`. Each failing case prints its seed and case index.
#![cfg(unix)]
#![allow(dead_code, clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};
use team_agent::model::enums::ProviderEffort;
use team_agent::provider::{
    get_adapter, AuthMode, Provider, ProviderAdapter, ProviderCommandContext, SessionId,
};

// ───────────────────────────── generator ─────────────────────────────

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    fn range(&mut self, low: usize, high: usize) -> usize {
        low + self.below(high - low + 1)
    }
    fn chance(&mut self, percent: u64) -> bool {
        self.next() % 100 < percent
    }
    fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len())]
    }
    fn slug(&mut self, len: usize) -> String {
        const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
        (0..len).map(|_| *self.pick(ALPHABET) as char).collect()
    }
    fn uuid(&mut self) -> String {
        let hex = format!("{:016x}{:016x}", self.next(), self.next());
        format!(
            "{}-{}-{}-{}-{}",
            &hex[0..8],
            &hex[8..12],
            &hex[12..16],
            &hex[16..20],
            &hex[20..32]
        )
    }
}

fn prop_seed() -> u64 {
    std::env::var("STAGE2_PROP_SEED")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(0x5717_E0C2_2026_1105)
}

fn prop_cases(default: usize) -> usize {
    std::env::var("STAGE2_PROP_CASES")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

/// Run `body` for independent seeded cases; report the reproducer on failure.
fn for_each_case(name: &str, default_cases: usize, mut body: impl FnMut(&mut Rng, usize)) {
    let seed = prop_seed();
    for case in 0..prop_cases(default_cases) {
        let mut rng = Rng(seed ^ (case as u64).wrapping_mul(0xA24B_AED4_963E_E407));
        let outcome =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| body(&mut rng, case)));
        if let Err(panic) = outcome {
            eprintln!("property `{name}` failed: STAGE2_PROP_SEED={seed} case={case}");
            std::panic::resume_unwind(panic);
        }
    }
}

// ───────────────────────────── shared helpers ─────────────────────────────

const TEAM: &str = "alpha";
const AGENTS: &[&str] = &["w1", "w2"];
/// Legacy daemon files and the retired tick heartbeat (all under `.team/runtime`).
const TICK_ARTIFACTS: &[&str] = &[
    "coordinator.pid",
    "coordinator.json",
    "coordinator.log",
    "coordinator.sock",
    "coordinator_tick.json",
];
/// Subcommands a resident supervisor would run under; none may stay alive.
const RESIDENT_VERBS: &[&str] = &[
    "coordinator",
    "supervise",
    "supervisor",
    "watch",
    "tick",
    "daemon",
    "plan",
];
const COORDINATOR_START_ALLOWED: &[&str] = &["not_required", "not_attempted", "not_applicable"];
const COORDINATOR_CLAIMS: &[&str] = &["running", "started", "running_confirmed", "ready", "healthy"];

/// Short, unique, canonical scratch root (tmux sockets need short paths on macOS).
fn scratch_root(label: &str) -> PathBuf {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let base = if Path::new("/private/tmp").is_dir() {
        PathBuf::from("/private/tmp")
    } else {
        PathBuf::from("/tmp")
    };
    let root = base.join(format!(
        "ta-s2-{label}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("home")).unwrap();
    root.canonicalize().unwrap()
}

fn text(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn stdout_json(output: &Output) -> Option<Value> {
    serde_json::from_slice(&output.stdout).ok()
}

fn assert_not_a_crash(label: &str, output: &Output) {
    assert_ne!(
        output.status.code(),
        Some(101),
        "{label}: CLI panicked: {}",
        text(output)
    );
    assert!(
        output.status.code().is_some(),
        "{label}: CLI was killed by a signal: {}",
        text(output)
    );
    assert!(
        !text(output).contains("panicked at"),
        "{label}: CLI panicked: {}",
        text(output)
    );
}

/// Spawn in a fresh process group and wait at most `limit`. Returns the output and
/// the group id (== child pid), so callers can look for processes left in it.
fn run_bounded(mut command: Command, label: &str, limit: Duration) -> (Output, u32) {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    let child = command.spawn().unwrap();
    let pgid = child.id();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(child.wait_with_output());
    });
    match rx.recv_timeout(limit) {
        Ok(output) => (output.unwrap(), pgid),
        Err(_) => {
            unsafe {
                libc::killpg(pgid as libc::pid_t, libc::SIGKILL);
            }
            panic!("{label}: did not return within {limit:?}; a single-point CLI must not wait on a supervisor");
        }
    }
}

#[derive(Debug, Clone)]
struct Proc {
    pid: u32,
    ppid: u32,
    pgid: u32,
    command: String,
}

fn ps_table() -> Vec<Proc> {
    let output = Command::new("ps")
        .args(["-axww", "-o", "pid=,ppid=,pgid=,command="])
        .output()
        .unwrap();
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let pid = fields.next()?.parse().ok()?;
            let ppid = fields.next()?.parse().ok()?;
            let pgid = fields.next()?.parse().ok()?;
            let command = fields.collect::<Vec<_>>().join(" ");
            Some(Proc {
                pid,
                ppid,
                pgid,
                command,
            })
        })
        .collect()
}

fn basename(command: &str) -> &str {
    let program = command.split_whitespace().next().unwrap_or("");
    program.rsplit('/').next().unwrap_or(program)
}

/// True when `proc` or one of its ancestors satisfies `predicate`.
fn lineage_matches(table: &[Proc], proc: &Proc, predicate: impl Fn(&Proc) -> bool) -> bool {
    let mut current = Some(proc);
    for _ in 0..64 {
        let Some(node) = current else { return false };
        if predicate(node) {
            return true;
        }
        if node.ppid <= 1 {
            return false;
        }
        current = table.iter().find(|candidate| candidate.pid == node.ppid);
    }
    false
}

fn runs_resident_verb(command: &str) -> bool {
    let mut tokens = command.split_whitespace();
    let program = tokens.next().unwrap_or("");
    program.rsplit('/').next().unwrap_or("").starts_with("team-agent")
        && tokens
            .next()
            .is_some_and(|verb| RESIDENT_VERBS.contains(&verb))
}

/// Processes that outlived a CLI call: anything left in its process group, any
/// process naming the workspace outside a tmux pane tree, and any team-agent
/// supervisor-style verb for the workspace even inside tmux.
fn resident_violations(root: &Path, pgid: Option<u32>) -> Vec<String> {
    let table = ps_table();
    let needle = root.to_string_lossy().to_string();
    table
        .iter()
        .filter(|proc| {
            let names_workspace = proc.command.contains(&needle);
            let in_group = pgid.is_some_and(|group| proc.pgid == group);
            let outside_tmux =
                names_workspace && !lineage_matches(&table, proc, |p| basename(&p.command) == "tmux");
            in_group || outside_tmux || (names_workspace && runs_resident_verb(&proc.command))
        })
        .map(|proc| {
            format!(
                "pid={} ppid={} pgid={} {}",
                proc.pid, proc.ppid, proc.pgid, proc.command
            )
        })
        .collect()
}

/// Coordinator/tick footprints: daemon files, any `coordinator*` file under
/// `.team`, and a `coordinator` object written into runtime state.
fn tick_footprints(root: &Path) -> Vec<String> {
    let mut found: Vec<String> = TICK_ARTIFACTS
        .iter()
        .map(|name| root.join(".team/runtime").join(name))
        .filter(|path| path.exists())
        .map(|path| path.display().to_string())
        .collect();
    fn walk(at: &Path, found: &mut Vec<String>) {
        for entry in std::fs::read_dir(at).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, found);
            } else if path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("coordinator"))
            {
                found.push(path.display().to_string());
            }
        }
    }
    walk(&root.join(".team"), &mut found);
    if let Some(state) = std::fs::read(root.join(".team/runtime/state.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
    {
        let mut objects = vec![("<root>".to_string(), &state)];
        if let Some(teams) = state.get("teams").and_then(Value::as_object) {
            objects.extend(teams.iter().map(|(key, team)| (format!("teams.{key}"), team)));
        }
        for (label, object) in objects {
            for key in ["coordinator", "coordinator_tick"] {
                if object.get(key).is_some() {
                    found.push(format!("state.json {label}.{key}"));
                }
            }
        }
    }
    found.sort();
    found.dedup();
    found
}

/// JSON fields that would claim a coordinator was started or is running.
fn coordinator_claims(value: &Value, path: &str, out: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                let here = format!("{path}/{key}");
                if key == "coordinator_start" {
                    if let Some(status) = child.as_str() {
                        if !COORDINATOR_START_ALLOWED.contains(&status) {
                            out.push(format!("{here}={status}"));
                        }
                    }
                }
                if key == "coordinator" {
                    if let Some(status) = child.get("status").and_then(Value::as_str) {
                        if COORDINATOR_CLAIMS.contains(&status) {
                            out.push(format!("{here}/status={status}"));
                        }
                    }
                }
                coordinator_claims(child, &here, out);
            }
        }
        Value::Array(items) => {
            for (index, child) in items.iter().enumerate() {
                coordinator_claims(child, &format!("{path}/{index}"), out);
            }
        }
        _ => {}
    }
}

fn assert_single_point(label: &str, root: &Path, output: &Output, pgid: u32) {
    assert_not_a_crash(label, output);
    let residents = resident_violations(root, Some(pgid));
    assert!(
        residents.is_empty(),
        "{label}: left resident processes behind (no supervisor may outlive a CLI call):\n{}",
        residents.join("\n")
    );
    let footprints = tick_footprints(root);
    assert!(
        footprints.is_empty(),
        "{label}: materialized coordinator/tick artifacts: {footprints:?}"
    );
    if let Some(value) = stdout_json(output) {
        let mut claims = Vec::new();
        coordinator_claims(&value, "", &mut claims);
        assert!(
            claims.is_empty(),
            "{label}: output claims a coordinator was started/running: {claims:?}"
        );
    }
}

/// The public `tmux` resolved from the caller's PATH (or `CONTRACT_TMUX` in CI).
fn tmux_binary() -> PathBuf {
    std::env::var_os("CONTRACT_TMUX")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
                .map(|dir| dir.join("tmux"))
                .find(|path| path.is_file())
                .expect("tmux is required for the live single-point invariants")
        })
}

// ───────────────────────────── fixtures ─────────────────────────────

/// A stopped-seat runtime snapshot: no tmux, no live pane, no coordinator.
struct Stopped {
    root: PathBuf,
}

impl Stopped {
    fn new(label: &str) -> Self {
        let root = scratch_root(label);
        std::fs::create_dir_all(root.join(".team/runtime")).unwrap();
        let agents: serde_json::Map<String, Value> = AGENTS
            .iter()
            .map(|id| (id.to_string(), json!({"status": "stopped"})))
            .collect();
        std::fs::write(
            root.join(".team/runtime/state.json"),
            serde_json::to_vec(&json!({
                "active_team_key": TEAM,
                "teams": {TEAM: {"team_id": TEAM, "status": "stopped", "agents": agents}}
            }))
            .unwrap(),
        )
        .unwrap();
        Self { root }
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_team-agent"));
        command
            .args(args)
            .arg("--workspace")
            .arg(&self.root)
            .current_dir(&self.root)
            .env_clear()
            .env("HOME", self.root.join("home"))
            .env("PATH", "/usr/bin:/bin");
        command
    }

    fn run(&self, args: &[&str]) -> (Output, u32) {
        run_bounded(self.command(args), &format!("{args:?}"), Duration::from_secs(30))
    }

    /// Every file under `.team` except CLI error logs.
    fn snapshot(&self) -> BTreeMap<PathBuf, Vec<u8>> {
        snapshot_team_dir(&self.root)
    }
}

impl Drop for Stopped {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn snapshot_team_dir(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn walk(root: &Path, at: &Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) {
        for entry in std::fs::read_dir(at).into_iter().flatten().flatten() {
            let path = entry.path();
            let relative = path.strip_prefix(root).unwrap().to_path_buf();
            if relative.starts_with(".team/logs") {
                continue;
            }
            if path.is_dir() {
                walk(root, &path, out);
            } else {
                out.insert(relative, std::fs::read(&path).unwrap_or_default());
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, &root.join(".team"), &mut out);
    out
}

/// A real fake-provider team under an explicit, test-owned fixture leader pane.
struct LiveTeam {
    root: PathBuf,
    home: PathBuf,
    tmux: PathBuf,
    caller_socket: PathBuf,
    caller_pane: String,
    path_env: String,
}

impl LiveTeam {
    fn new(label: &str, agents: &[&str], extra_path: Option<&Path>) -> Self {
        let root = scratch_root(label);
        let home = root.join("home");
        let spec = root.join("spec");
        std::fs::create_dir_all(spec.join("agents")).unwrap();
        std::fs::write(
            spec.join("TEAM.md"),
            format!(
                "---\nname: s2{}\nobjective: stage2 single-point property\nprovider: fake\ndisplay_backend: none\n---\nTeam.\n",
                label.chars().filter(char::is_ascii_alphanumeric).take(8).collect::<String>()
            ),
        )
        .unwrap();
        for id in agents {
            std::fs::write(
                spec.join(format!("agents/{id}.md")),
                format!(
                    "---\nname: {id}\nrole: fake {id}\nprovider: fake\nmodel: fake\nauth_mode: subscription\n\
                     dangerously_skip_permissions: false\ntools:\n  - mcp_team\n---\nFake {id}.\n"
                ),
            )
            .unwrap();
        }
        let tmux = tmux_binary();
        let caller_socket = root.join("c.sock");
        let created = Command::new(&tmux)
            .args(["-u", "-S"])
            .arg(&caller_socket)
            .args(["-f", "/dev/null", "new-session", "-d", "-s", "fixture-caller", "-c"])
            .arg(&home)
            .args(["-P", "-F", "#{pane_id}", "exec sleep 86400"])
            .env("HOME", &home)
            .env_remove("TMUX")
            .env_remove("TMUX_PANE")
            .output()
            .unwrap();
        assert!(
            created.status.success(),
            "fixture caller failed: {}",
            text(&created)
        );
        let caller_pane = String::from_utf8(created.stdout).unwrap().trim().to_string();
        let mut path_env = format!("{}:/usr/bin:/bin", tmux.parent().unwrap().display());
        if let Some(extra) = extra_path {
            path_env = format!("{}:{path_env}", extra.display());
        }
        Self {
            root,
            home,
            tmux,
            caller_socket,
            caller_pane,
            path_env,
        }
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_team-agent"));
        command
            .args(args)
            .arg("--workspace")
            .arg(&self.root)
            .current_dir(&self.root)
            .env_clear()
            .env("HOME", &self.home)
            .env("PATH", &self.path_env)
            .env("TERM", "xterm-256color")
            .env("TMUX", format!("{},0,0", self.caller_socket.display()))
            .env("TMUX_PANE", &self.caller_pane)
            .env("TEAM_AGENT_LEADER_PROVIDER", "fake");
        command
    }

    fn run(&self, args: &[&str], limit: Duration) -> (Output, u32) {
        run_bounded(self.command(args), &format!("{args:?}"), limit)
    }

    fn start_team(&self) {
        let spec = self.root.join("spec");
        let spec = spec.to_str().unwrap();
        let (output, pgid) = self.run(&["start", "--from", spec, "--json"], Duration::from_secs(90));
        assert!(
            output.status.success() && stdout_json(&output).is_some_and(|v| v["ok"] == true),
            "fixture precondition: fake team start must succeed: {}",
            text(&output)
        );
        assert_single_point("start --from", &self.root, &output, pgid);
    }

    fn team_socket(&self) -> Option<PathBuf> {
        let state: Value =
            serde_json::from_slice(&std::fs::read(self.root.join(".team/runtime/state.json")).ok()?)
                .ok()?;
        let socket = state
            .get("tmux_socket")
            .or_else(|| {
                state
                    .get("teams")?
                    .get(state.get("active_team_key")?.as_str()?)?
                    .get("tmux_socket")
            })?
            .as_str()?;
        Some(PathBuf::from(socket))
    }

    /// Processes naming the workspace other than the fixture caller's own tree.
    fn leftovers(&self) -> Vec<String> {
        let table = ps_table();
        let needle = self.root.to_string_lossy().to_string();
        let caller = self.caller_socket.to_string_lossy().to_string();
        table
            .iter()
            .filter(|proc| proc.command.contains(&needle))
            .filter(|proc| !lineage_matches(&table, proc, |p| p.command.contains(&caller)))
            .map(|proc| format!("pid={} ppid={} {}", proc.pid, proc.ppid, proc.command))
            .collect()
    }
}

impl Drop for LiveTeam {
    fn drop(&mut self) {
        let socket = self.team_socket();
        let _ = run_bounded_quiet(self.command(&["stop", "--json"]), Duration::from_secs(60));
        if let Some(socket) = socket {
            if socket != self.caller_socket && socket.exists() {
                let _ = Command::new(&self.tmux).arg("-S").arg(&socket).arg("kill-server").output();
            }
        }
        let _ = Command::new(&self.tmux)
            .arg("-S")
            .arg(&self.caller_socket)
            .arg("kill-server")
            .output();
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn run_bounded_quiet(mut command: Command, limit: Duration) -> Option<Output> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    let child = command.spawn().ok()?;
    let pgid = child.id();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(child.wait_with_output());
    });
    match rx.recv_timeout(limit) {
        Ok(output) => output.ok(),
        Err(_) => {
            unsafe {
                libc::killpg(pgid as libc::pid_t, libc::SIGKILL);
            }
            None
        }
    }
}

// ───────────────────── a) pure CLI single point ─────────────────────

/// Without tmux or any live pane, every single-point command answers by itself:
/// whatever its outcome, it returns promptly, leaves nothing running and writes no
/// coordinator/tick state.
#[test]
fn prop_stopped_seat_commands_return_alone_without_residents_or_tick_artifacts() {
    const COMMANDS: &[&[&str]] = &[
        &["status"],
        &["status", "--json"],
        &["status", "--detail", "--json"],
        &["status", "w1", "--json"],
        &["team", "status", "--json"],
        &["doctor", "--json"],
        &["doctor"],
        &["send", "w1", "PAYLOAD", "--mailbox", "--json"],
        &["send", "w2", "PAYLOAD", "--json"],
        &["inbox", "w1", "--json"],
        &["inbox", "leader", "--json"],
        &["locate", "--json"],
        &["approvals", "--json"],
        &["schedule", "list", "--json"],
        &["stop", "w1", "--json"],
        &["stop", "--json"],
        &["restart", "w1", "--dry-run", "--json"],
        &["restart", "w1", "--json"],
        &["start", "w1", "--dry-run", "--json"],
    ];
    for_each_case("stopped-seat single point", 12, |rng, _| {
        let ws = Stopped::new("stopped");
        for _ in 0..rng.range(3, 6) {
            let payload = format!("stage2 {}", rng.slug(8));
            let args: Vec<&str> = rng
                .pick(COMMANDS)
                .iter()
                .map(|arg| if *arg == "PAYLOAD" { payload.as_str() } else { *arg })
                .collect();
            let started = Instant::now();
            let (output, pgid) = ws.run(&args);
            assert!(
                started.elapsed() < Duration::from_secs(20),
                "{args:?} took {:?}",
                started.elapsed()
            );
            assert_single_point(&format!("{args:?}"), &ws.root, &output, pgid);
        }
    });
}

/// A real team lifecycle driven by single-point commands: start, status, send,
/// restart, stop/start one seat, doctor, then stop. Only tmux and its pane tree may
/// outlive a command; after the final stop nothing naming the workspace remains.
#[test]
fn prop_live_team_lifecycle_is_single_point_and_leaves_nothing_after_stop() {
    const COMMANDS: &[&[&str]] = &[
        &["status", "--json"],
        &["status", "w1", "--json"],
        &["status", "--detail", "--json"],
        &["team", "status", "--json"],
        &["doctor", "--json"],
        &["send", "w1", "PAYLOAD", "--json"],
        &["send", "w2", "PAYLOAD", "--mailbox", "--json"],
        &["inbox", "w1", "--json"],
        &["inbox", "leader", "--json"],
        &["locate", "--json"],
        &["approvals", "--json"],
        &["schedule", "list", "--json"],
        &["restart", "w1", "--json"],
        &["restart", "--dry-run", "--json"],
        &["stop", "w2", "--json"],
        &["start", "w2", "--json"],
    ];
    for_each_case("live-team single point", 3, |rng, case| {
        let team = LiveTeam::new(&format!("live{case}"), AGENTS, None);
        team.start_team();
        for _ in 0..rng.range(5, 8) {
            let payload = format!("stage2 live {}", rng.slug(8));
            let args: Vec<&str> = rng
                .pick(COMMANDS)
                .iter()
                .map(|arg| if *arg == "PAYLOAD" { payload.as_str() } else { *arg })
                .collect();
            let (output, pgid) = team.run(&args, Duration::from_secs(90));
            assert_single_point(&format!("{args:?}"), &team.root, &output, pgid);
        }
        let (output, pgid) = team.run(&["stop", "--json"], Duration::from_secs(90));
        assert!(
            output.status.success() && stdout_json(&output).is_some_and(|v| v["ok"] == true),
            "team stop must succeed: {}",
            text(&output)
        );
        assert_single_point("stop --json", &team.root, &output, pgid);
        let deadline = Instant::now() + Duration::from_secs(15);
        let mut leftovers = team.leftovers();
        while !leftovers.is_empty() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(200));
            leftovers = team.leftovers();
        }
        assert!(
            leftovers.is_empty(),
            "processes naming the workspace survived team stop:\n{}",
            leftovers.join("\n")
        );
    });
}

/// A recorded legacy coordinator PID that now belongs to an unrelated process is
/// stale bookkeeping: no command signals it, read paths never edit the record, and
/// cleanup paths may only remove the unchanged record.
#[test]
fn legacy_coordinator_pid_reuse_is_never_signalled_and_read_paths_never_edit_it() {
    const READ_ONLY: &[&[&str]] = &[
        &["status"],
        &["status", "--json"],
        &["status", "--detail", "--json"],
        &["status", "w1", "--json"],
        &["doctor", "--json"],
        &["inbox", "w1", "--json"],
        &["locate", "--json"],
        &["approvals", "--json"],
        &["restart", "w1", "--dry-run", "--json"],
    ];
    const MAY_CLEAN: &[&[&str]] = &[
        &["send", "w1", "stage2 legacy pid", "--mailbox", "--json"],
        &["schedule", "list", "--json"],
        &["stop", "--json"],
    ];
    let ws = Stopped::new("legacypid");
    let mut bystander = Command::new("/bin/sleep")
        .arg("600")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let pid = bystander.id();
    let runtime = ws.root.join(".team/runtime");
    let plant = || {
        std::fs::write(runtime.join("coordinator.pid"), format!("{pid}\n")).unwrap();
        std::fs::write(
            runtime.join("coordinator.json"),
            serde_json::to_vec(&json!({"pid": pid, "workspace": ws.root})).unwrap(),
        )
        .unwrap();
    };
    let record = || {
        ["coordinator.pid", "coordinator.json"]
            .map(|name| std::fs::read(runtime.join(name)).ok())
    };
    let mut failures = Vec::new();
    for (args, read_only) in READ_ONLY
        .iter()
        .map(|args| (*args, true))
        .chain(MAY_CLEAN.iter().map(|args| (*args, false)))
    {
        plant();
        let before = record();
        let (output, _) = ws.run(args);
        assert_not_a_crash(&format!("{args:?}"), &output);
        let after = record();
        let alive = bystander.try_wait().unwrap().is_none();
        if !alive {
            failures.push(format!("{args:?} killed the unrelated process {pid}"));
            break;
        }
        if read_only && after != before {
            failures.push(format!("{args:?} edited the legacy coordinator record"));
        }
        if !read_only && after != before && after.iter().any(Option::is_some) {
            failures.push(format!(
                "{args:?} rewrote the legacy coordinator record instead of leaving or removing it"
            ));
        }
        if args[0] == "status" && !output.status.success() {
            failures.push(format!(
                "{args:?} failed beside a stale legacy record: {}",
                text(&output)
            ));
        }
    }
    let _ = bystander.kill();
    let _ = bystander.wait();
    assert!(failures.is_empty(), "{failures:#?}");
}

// ───────────────────── b) ProviderAdapter purity ─────────────────────

fn flag_value<'a>(argv: &'a [String], flag: &str) -> Option<&'a str> {
    argv.iter()
        .position(|arg| arg == flag)
        .and_then(|at| argv.get(at + 1))
        .map(String::as_str)
}

fn has_flag(argv: &[String], flag: &str) -> bool {
    argv.iter().any(|arg| arg == flag)
}

/// Claude fresh, resume and fork plans are the public `claude` command with the
/// provider's own session flags: no materialized copy, verifier or wrapper.
#[test]
fn prop_claude_plans_are_the_native_command_with_native_resume_and_fork() {
    const MODELS: &[&str] = &[
        "claude-haiku-5-5",
        "claude-sonnet-5-5",
        "claude-opus-5-5",
        "haiku",
        "opus",
    ];
    const EFFORTS: &[ProviderEffort] = &[
        ProviderEffort::Low,
        ProviderEffort::Medium,
        ProviderEffort::High,
        ProviderEffort::XHigh,
        ProviderEffort::Max,
    ];
    for_each_case("claude native plans", 64, |rng, _| {
        let provider = *rng.pick(&[Provider::Claude, Provider::ClaudeCode]);
        let adapter = get_adapter(provider);
        let model = rng.chance(70).then(|| rng.pick(MODELS).to_string());
        // No newline or double quote: those switch on native MCP config resolution.
        let prompt = rng
            .chance(60)
            .then(|| format!("You are {} working on {}", rng.slug(6), rng.slug(12)));
        let agent = rng.chance(50).then(|| format!("w{}", rng.slug(3)));
        let effort = rng.chance(50).then(|| *rng.pick(EFFORTS));
        let bypass = rng.chance(50);
        let auth = *rng.pick(&[AuthMode::Subscription, AuthMode::OfficialApi]);
        let ctx = |auth_mode| ProviderCommandContext {
            auth_mode,
            mcp_config: None,
            system_prompt: prompt.as_deref(),
            model: model.as_deref(),
            dangerously_skip_permissions: bypass,
            profile_launch: None,
            agent_id_hint: agent.as_deref(),
            effort,
        };
        let source = SessionId::new(rng.uuid());
        let check_common = |label: &str, argv: &[String]| {
            assert_eq!(argv.first().map(String::as_str), Some("claude"), "{label}: {argv:?}");
            if bypass {
                assert!(has_flag(argv, "--dangerously-skip-permissions"), "{label}: {argv:?}");
                assert!(!has_flag(argv, "--permission-mode"), "{label}: {argv:?}");
            } else {
                assert_eq!(flag_value(argv, "--permission-mode"), Some("default"), "{label}: {argv:?}");
            }
            assert_eq!(flag_value(argv, "--model"), model.as_deref(), "{label}: {argv:?}");
        };

        let fresh = adapter.build_command_plan(ctx(auth)).expect("fresh plan");
        check_common("fresh", &fresh.argv);
        assert_eq!(
            flag_value(&fresh.argv, "--session-id"),
            fresh.expected_session_id.as_ref().map(SessionId::as_str),
            "fresh plan must pre-assign its own session id: {:?}",
            fresh.argv
        );
        assert!(
            !has_flag(&fresh.argv, "--resume") && !has_flag(&fresh.argv, "--fork-session"),
            "fresh plan resumes or forks: {:?}",
            fresh.argv
        );
        assert_eq!(
            flag_value(&fresh.argv, "--effort"),
            effort.map(ProviderEffort::as_str),
            "{:?}",
            fresh.argv
        );
        assert_eq!(flag_value(&fresh.argv, "--name"), agent.as_deref(), "{:?}", fresh.argv);

        let resume = adapter
            .build_resume_command_plan(Some(&source), ctx(auth))
            .expect("resume plan");
        check_common("resume", &resume.argv);
        assert_eq!(flag_value(&resume.argv, "--resume"), Some(source.as_str()), "{:?}", resume.argv);
        assert!(
            !has_flag(&resume.argv, "--fork-session") && !has_flag(&resume.argv, "--session-id"),
            "resume must continue the same session: {:?}",
            resume.argv
        );

        let fork = adapter.fork_plan(Some(&source), ctx(auth)).expect("fork plan");
        check_common("fork", &fork.argv);
        assert_eq!(flag_value(&fork.argv, "--resume"), Some(source.as_str()), "{:?}", fork.argv);
        assert!(has_flag(&fork.argv, "--fork-session"), "fork must use the native flag: {:?}", fork.argv);
        let child = flag_value(&fork.argv, "--session-id");
        assert!(
            child.is_some()
                && child == fork.expected_session_id.as_ref().map(SessionId::as_str)
                && child != Some(source.as_str()),
            "fork must pre-assign a distinct child session: {:?}",
            fork.argv
        );

        assert!(
            adapter.fork_plan(None, ctx(auth)).is_err(),
            "fork without a source session must refuse"
        );
        assert!(
            adapter.fork_plan(Some(&source), ctx(AuthMode::CompatibleApi)).is_err(),
            "compatible-API fork must refuse instead of degrading"
        );
    });
}

/// Codex and Grok run only through their contract leaves and Pi through the
/// lifecycle materializer: the adapter facade must refuse rather than synthesize
/// a legacy worker command, resume or fork for them.
#[test]
fn contract_leaf_and_materializer_providers_get_no_legacy_adapter_command() {
    let source = SessionId::new("00000000-0000-4000-8000-000000000002");
    for provider in [Provider::Codex, Provider::Grok, Provider::Pi] {
        let adapter = get_adapter(provider);
        let ctx = ProviderCommandContext {
            auth_mode: AuthMode::Subscription,
            mcp_config: None,
            system_prompt: Some("You are a worker"),
            model: Some("some-model"),
            dangerously_skip_permissions: false,
            profile_launch: None,
            agent_id_hint: Some("w1"),
            effort: None,
        };
        assert!(
            adapter.build_command_plan(ctx).is_err(),
            "{provider:?} got a legacy adapter launch command"
        );
        assert!(
            adapter.build_resume_command_plan(Some(&source), ctx).is_err(),
            "{provider:?} got a legacy adapter resume command"
        );
        assert!(
            adapter.fork_plan(Some(&source), ctx).is_err(),
            "{provider:?} got a legacy adapter fork command"
        );
    }
}

/// Native providers execute the public command name resolved through the caller's
/// PATH, so a user wrapper is always honoured; nothing probes a private binary. The
/// wrappers here fail, and every start must refuse without leaving residents.
#[test]
fn native_providers_execute_the_public_command_through_the_callers_path() {
    let stubs = scratch_root("stubs");
    let log = stubs.join("calls.log");
    for name in ["claude", "pi", "codex"] {
        let stub = stubs.join(name);
        std::fs::write(
            &stub,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"{name} $*\" >> '{}'\nexit 1\n",
                log.display()
            ),
        )
        .unwrap();
        std::process::Command::new("/bin/chmod")
            .arg("755")
            .arg(&stub)
            .status()
            .unwrap();
    }
    let team = LiveTeam::new("native", &[], Some(&stubs));
    for (agent, provider, model) in [
        ("wc", "codex", "gpt-5.1-codex-mini"),
        ("wp", "pi", "anthropic/claude-haiku-4-5"),
        ("wl", "claude", "claude-haiku-4-5"),
    ] {
        let args = ["start", agent, "--provider", provider, "--model", model, "--json"];
        let (output, pgid) = team.run(&args, Duration::from_secs(150));
        assert_single_point(&format!("{args:?}"), &team.root, &output, pgid);
        assert!(
            !output.status.success()
                && stdout_json(&output).is_none_or(|value| value["ok"] != true),
            "{args:?} must refuse when the provider wrapper fails: {}",
            text(&output)
        );
    }
    let calls = std::fs::read_to_string(&log).unwrap_or_default();
    for provider in ["codex", "pi"] {
        assert!(
            calls.lines().any(|line| line.starts_with(&format!("{provider} "))),
            "{provider} was not resolved through the caller's PATH wrapper; wrapper log:\n{calls}"
        );
    }
    // The worker pane runs under a login shell; only assert when that shell resolves
    // `claude` to the same wrapper the caller sees (a host /etc/paths may differ).
    let login_claude = Command::new("/bin/sh")
        .args(["-lc", "command -v claude"])
        .env_clear()
        .env("HOME", &team.home)
        .env("PATH", &team.path_env)
        .output()
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .unwrap_or_default();
    if Path::new(&login_claude) == stubs.join("claude") {
        assert!(
            calls.lines().any(|line| line.starts_with("claude ")),
            "claude was not launched through the caller's PATH wrapper; wrapper log:\n{calls}"
        );
        assert!(
            calls.contains("--session-id") && !calls.contains("--fork-session") && !calls.contains("--resume "),
            "a fresh claude seat must be a native fresh launch: {calls}"
        );
    } else {
        eprintln!("login shell resolves claude to {login_claude:?}; native claude launch check skipped on this host");
    }
    drop(team);
    let _ = std::fs::remove_dir_all(&stubs);
}

// ───────────────────── c) fail-closed barriers ─────────────────────

/// `attach-leader` and `takeover` refuse before any parsing: whatever follows them
/// (help, unknown flags, scope, a missing workspace) the answer is the same
/// retirement naming `claim-leader`, and nothing is written.
#[test]
fn prop_retired_leader_verbs_refuse_before_parsing_and_name_claim_leader() {
    const TAIL: &[&str] = &[
        "--help", "-h", "--json", "--confirm", "--team", "alpha", "w1", "garbage", "--no-such-flag",
        "--detail", "--yes",
    ];
    for_each_case("retired leader verbs", 24, |rng, _| {
        let ws = Stopped::new("retired");
        let before = ws.snapshot();
        let verb = *rng.pick(&["attach-leader", "takeover"]);
        let mut args = vec![verb.to_string()];
        for _ in 0..rng.range(0, 4) {
            args.push(rng.pick(TAIL).to_string());
        }
        let missing = ws.root.join(format!("missing-{}", rng.slug(6)));
        let mut command = ws.command(&args.iter().map(String::as_str).collect::<Vec<_>>());
        if rng.chance(30) {
            command.arg("--workspace").arg(&missing);
        }
        let (output, pgid) = run_bounded(command, &format!("{args:?}"), Duration::from_secs(20));
        assert_single_point(&format!("{args:?}"), &ws.root, &output, pgid);
        assert_eq!(output.status.code(), Some(2), "{args:?}: {}", text(&output));
        let message = text(&output);
        assert!(
            message.contains("retired") && message.contains("claim-leader"),
            "{args:?}: {message}"
        );
        assert!(
            !message.contains(&format!("team-agent {verb} --help")),
            "{args:?} sends the user back to the retired verb: {message}"
        );
        if args.iter().any(|arg| arg == "--json") {
            let value = stdout_json(&output)
                .unwrap_or_else(|| panic!("{args:?}: stdout is not JSON: {message}"));
            assert_eq!(value["ok"], false, "{args:?}: {value}");
        }
        assert_eq!(ws.snapshot(), before, "{args:?} mutated the workspace");
        assert!(!missing.exists(), "{args:?} created the requested workspace");
    });
}

#[test]
fn removed_p3_p4_verbs_are_unknown_commands_without_side_effects() {
    let ws = Stopped::new("removed");
    let before = ws.snapshot();
    for verb in [
        "plan",
        "tick",
        "supervise",
        "rediscover",
        "readopt",
        "install",
        "uninstall",
        "repair",
        "fork-session",
        "verify-session",
        "halt-plan",
        "plan-status",
        "classify",
    ] {
        for json in [false, true] {
            let mut args = vec![verb];
            if json {
                args.push("--json");
            }
            let (output, pgid) = ws.run(&args);
            assert_single_point(&format!("{args:?}"), &ws.root, &output, pgid);
            assert!(
                !output.status.success(),
                "{verb} must not run: {}",
                text(&output)
            );
            assert!(
                text(&output).contains("Unknown command") && text(&output).contains("team-agent --help"),
                "{args:?} must be an unknown command pointing to help: {}",
                text(&output)
            );
        }
    }
    assert_eq!(ws.snapshot(), before);
}

#[test]
fn retired_plan_flags_and_operations_are_usage_errors_without_side_effects() {
    let ws = Stopped::new("planflags");
    let before = ws.snapshot();
    for args in [
        vec!["start", "--plan", "plan.json", "--json"],
        vec!["start", "--plan", "plan.json"],
        vec!["team", "plan", "--json"],
        vec!["agent", "plan", "w1"],
    ] {
        let (output, pgid) = ws.run(&args);
        assert_single_point(&format!("{args:?}"), &ws.root, &output, pgid);
        assert_eq!(output.status.code(), Some(2), "{args:?}: {}", text(&output));
        assert!(
            text(&output).contains("usage error") || text(&output).contains("unknown"),
            "{args:?}: {}",
            text(&output)
        );
    }
    assert_eq!(ws.snapshot(), before);
}

#[test]
fn help_catalog_never_advertises_retired_p3_p4_verbs() {
    let ws = Stopped::new("help");
    let (output, _) = ws.run(&["--help"]);
    assert!(output.status.success(), "{}", text(&output));
    let listed: Vec<String> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|line| line.starts_with("  ") && !line.starts_with("   "))
        .filter_map(|line| line.split_whitespace().next().map(str::to_string))
        .collect();
    assert!(listed.iter().any(|name| name == "claim-leader"), "{listed:?}");
    for retired in [
        "attach-leader",
        "takeover",
        "coordinator",
        "plan",
        "tick",
        "rediscover",
        "wait",
        "install",
    ] {
        assert!(
            !listed.iter().any(|name| name == retired),
            "help lists the retired verb {retired}: {listed:?}"
        );
    }
}
