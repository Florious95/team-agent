//! Stage 3 excision (P5: K6 pre-unified lifecycle seams + K11 state/model residue;
//! P6: K9 unwired station operation journal) must not change live lifecycle behavior.
//! Every case is GREEN on the Stage 2 accepted baseline (2801280b) and must stay GREEN.
//!
//! a) Canonical unified start: every seat activation, session policy and provider
//!    migration is planned and executed by the one `start` kernel. Dry runs are typed and
//!    read-only; conflicting policy flags are usage errors; a provider change on a
//!    stopped seat requires `--session fresh` and plans `provider_changed`; running
//!    seats are never replaced or edited by `start` (no legacy fallback);
//!    a stopped seat reactivates through the kernel; the legacy bare `restart` port and
//!    `team restart` keep working.
//! b) K9 purity: no `operations/` journal is ever materialized under `.team/station-v1`;
//!    every published seat manifest satisfies the live manifest invariant (the one
//!    `validate_manifest` keeps enforcing); a corrupted manifest is refused, never
//!    rewritten, and spawns nothing.
//! c) Global state purity: runtime state stays a canonical projection (no coordinator,
//!    plan or journal keys, status agrees with state, no temp debris) across random
//!    lifecycle sequences, and read paths never rewrite state or seat manifests.
//!
//! Properties use a seeded SplitMix64 generator (no new test dependencies).
//! Reproduce a failure with `STAGE3_PROP_SEED=<seed>`; widen the search with
//! `STAGE3_PROP_CASES=<n>`. Each failing case prints its seed and case index.
#![cfg(unix)]
#![allow(dead_code, clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use serde_json::Value;
use std::collections::BTreeMap;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

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
    fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len())]
    }
    fn slug(&mut self, len: usize) -> String {
        const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
        (0..len).map(|_| *self.pick(ALPHABET) as char).collect()
    }
}

fn for_each_case(name: &str, default_cases: usize, mut body: impl FnMut(&mut Rng, usize)) {
    let seed = std::env::var("STAGE3_PROP_SEED")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(0x5717_E0C3_2026_1106);
    let cases = std::env::var("STAGE3_PROP_CASES")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default_cases);
    for case in 0..cases {
        let mut rng = Rng(seed ^ (case as u64).wrapping_mul(0xA24B_AED4_963E_E407));
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| body(&mut rng, case)));
        if let Err(panic) = outcome {
            eprintln!("property `{name}` failed: STAGE3_PROP_SEED={seed} case={case}");
            std::panic::resume_unwind(panic);
        }
    }
}

// ───────────────────────────── process helpers ─────────────────────────────

/// Keys a canonical runtime projection must never carry after Stage 3.
const RETIRED_STATE_KEYS: &[&str] = &[
    "coordinator",
    "coordinator_tick",
    "plan",
    "plan_state",
    "operations",
    "operation_journal",
];
const SEAT_DESIRED: &[&str] = &["running", "stopped", "removed"];
const SEAT_PHASES: &[&str] = &["reserved", "starting", "ready", "stopping", "stopped", "removed", "failed"];

fn scratch_root(label: &str) -> PathBuf {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let base = if Path::new("/private/tmp").is_dir() {
        PathBuf::from("/private/tmp")
    } else {
        PathBuf::from("/tmp")
    };
    let root = base.join(format!(
        "ta-s3-{label}-{}-{}",
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
    let body = text(output);
    assert!(
        output.status.code().is_some() && output.status.code() != Some(101) && !body.contains("panicked at"),
        "{label}: CLI crashed ({:?}): {body}",
        output.status.code()
    );
}

/// Spawn in a fresh process group and wait at most `limit`.
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
            panic!("{label}: did not return within {limit:?}");
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
            Some(Proc {
                pid,
                ppid,
                pgid,
                command: fields.collect::<Vec<_>>().join(" "),
            })
        })
        .collect()
}

fn basename(command: &str) -> &str {
    let program = command.split_whitespace().next().unwrap_or("");
    program.rsplit('/').next().unwrap_or(program)
}

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

/// Anything left in the CLI's process group, or naming the workspace outside tmux.
fn residents(root: &Path, pgid: u32) -> Vec<String> {
    let table = ps_table();
    let needle = root.to_string_lossy().to_string();
    table
        .iter()
        .filter(|proc| {
            proc.pgid == pgid
                || (proc.command.contains(&needle)
                    && !lineage_matches(&table, proc, |p| basename(&p.command) == "tmux"))
        })
        .map(|proc| format!("pid={} ppid={} pgid={} {}", proc.pid, proc.ppid, proc.pgid, proc.command))
        .collect()
}

fn tmux_binary() -> PathBuf {
    std::env::var_os("CONTRACT_TMUX")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
                .map(|dir| dir.join("tmux"))
                .find(|path| path.is_file())
                .expect("tmux is required for the live lifecycle invariants")
        })
}

/// Every file under `.team` except CLI logs, with its bytes.
fn snapshot(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
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

fn snapshot_diff(before: &BTreeMap<PathBuf, Vec<u8>>, after: &BTreeMap<PathBuf, Vec<u8>>) -> Vec<String> {
    let mut changed = Vec::new();
    for (path, bytes) in after {
        match before.get(path) {
            None => changed.push(format!("+ {}", path.display())),
            Some(old) if old != bytes => changed.push(format!("~ {}", path.display())),
            _ => {}
        }
    }
    for path in before.keys().filter(|path| !after.contains_key(*path)) {
        changed.push(format!("- {}", path.display()));
    }
    changed
}

// ───────────────────────────── purity checks ─────────────────────────────

fn files_under(at: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(at).into_iter().flatten().flatten() {
        let path = entry.path();
        if path.is_dir() {
            out.extend(files_under(&path));
        }
        out.push(path);
    }
    out
}

/// K9 + K11 + validate_manifest invariants over the whole `.team` tree.
fn purity_violations(root: &Path) -> Vec<String> {
    let mut found = Vec::new();
    let station = root.join(".team/station-v1");
    for path in files_under(&root.join(".team")) {
        let relative = path.strip_prefix(root).unwrap().display().to_string();
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default();
        if path.starts_with(&station)
            && path
                .strip_prefix(&station)
                .unwrap()
                .components()
                .any(|c| c.as_os_str() == "operations")
        {
            found.push(format!("K9 operation journal materialized: {relative}"));
        }
        if name.ends_with(".tmp") || (name.starts_with('.') && name.contains(".tmp")) {
            found.push(format!("temp debris left behind: {relative}"));
        }
    }
    for path in files_under(&station) {
        let is_manifest = path.parent().is_some_and(|p| p.ends_with("seats"))
            && path.extension().is_some_and(|ext| ext == "json");
        if !is_manifest {
            continue;
        }
        let relative = path.strip_prefix(root).unwrap().display().to_string();
        let Some(seat) = std::fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        else {
            found.push(format!("unreadable seat manifest: {relative}"));
            continue;
        };
        let instance = seat["instance_id"].as_str().unwrap_or_default();
        let desired = seat["desired"].as_str().unwrap_or_default();
        let phase = seat["phase"].as_str().unwrap_or_default();
        if seat["kind"] != "seat_manifest"
            || instance.len() != 32
            || !instance.bytes().all(|b| b.is_ascii_hexdigit())
            || !SEAT_DESIRED.contains(&desired)
            || !SEAT_PHASES.contains(&phase)
            || (phase == "ready" && (desired != "running" || seat["endpoint"].is_null()))
        {
            found.push(format!(
                "seat manifest violates the live manifest invariant: {relative} kind={} desired={desired} phase={phase}",
                seat["kind"]
            ));
        }
    }
    match std::fs::read(root.join(".team/runtime/state.json")) {
        Err(_) => {}
        Ok(bytes) => match serde_json::from_slice::<Value>(&bytes) {
            Err(error) => found.push(format!("state.json is not JSON: {error}")),
            Ok(state) => {
                let mut objects = vec![("<root>".to_string(), &state)];
                if let Some(teams) = state.get("teams").and_then(Value::as_object) {
                    objects.extend(teams.iter().map(|(key, team)| (format!("teams.{key}"), team)));
                } else {
                    found.push("state.json has no teams projection".into());
                }
                for (label, object) in objects {
                    for key in RETIRED_STATE_KEYS {
                        if object.get(key).is_some() {
                            found.push(format!("state.json {label}.{key} (retired key)"));
                        }
                    }
                }
            }
        },
    }
    found
}

fn state_agents(root: &Path) -> Vec<String> {
    let Some(state) = std::fs::read(root.join(".team/runtime/state.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
    else {
        return Vec::new();
    };
    let team = state
        .get("active_team_key")
        .and_then(Value::as_str)
        .and_then(|key| state.get("teams")?.get(key));
    team.and_then(|t| t.get("agents"))
        .or_else(|| state.get("agents"))
        .and_then(Value::as_object)
        .map(|agents| agents.keys().cloned().collect())
        .unwrap_or_default()
}

// ───────────────────────────── fixture ─────────────────────────────

/// A fake-provider team under an explicit, test-owned fixture leader pane. A stub `pi`
/// on PATH lets the planner resolve a native provider for migration dry runs; it is
/// never launched (dry runs only).
struct Team {
    _serial: std::sync::MutexGuard<'static, ()>,
    root: PathBuf,
    home: PathBuf,
    name: String,
    tmux: PathBuf,
    caller_socket: PathBuf,
    caller_pane: String,
    path_env: String,
}

/// Live teams in this binary run one at a time: concurrent tmux servers churn the shared
/// per-user socket directory, which the shutdown socket-parent fence (correctly) treats
/// as a changed parent.
static LIVE: std::sync::Mutex<()> = std::sync::Mutex::new(());

impl Team {
    fn new(label: &str) -> Self {
        let serial = LIVE.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let root = scratch_root(label);
        let home = root.join("home");
        let name = format!("s3{}", label.chars().filter(char::is_ascii_alphanumeric).take(8).collect::<String>());
        let spec = root.join("spec");
        std::fs::create_dir_all(spec.join("agents")).unwrap();
        std::fs::write(
            spec.join("TEAM.md"),
            format!("---\nname: {name}\nobjective: stage3 lifecycle property\nprovider: fake\ndisplay_backend: none\n---\nTeam.\n"),
        )
        .unwrap();
        for id in ["w1", "w2"] {
            std::fs::write(
                spec.join(format!("agents/{id}.md")),
                format!(
                    "---\nname: {id}\nrole: fake {id}\nprovider: fake\nmodel: fake\nauth_mode: subscription\n\
                     dangerously_skip_permissions: false\ntools:\n  - mcp_team\n---\nFake {id}.\n"
                ),
            )
            .unwrap();
        }
        let stub = root.join("stub");
        std::fs::create_dir_all(&stub).unwrap();
        std::fs::write(
            stub.join("pi"),
            "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then echo 0.70.0; exit 0; fi\necho \"stage3 stub pi must not launch: $*\" >&2\nexit 3\n",
        )
        .unwrap();
        std::fs::set_permissions(stub.join("pi"), std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
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
        assert!(created.status.success(), "fixture caller failed: {}", text(&created));
        let caller_pane = String::from_utf8(created.stdout).unwrap().trim().to_string();
        let path_env = format!("{}:{}:/usr/bin:/bin", stub.display(), tmux.parent().unwrap().display());
        Self {
            _serial: serial,
            root,
            home,
            name,
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

    fn run(&self, args: &[&str]) -> Output {
        let label = format!("{args:?}");
        let (output, pgid) = run_bounded(self.command(args), &label, Duration::from_secs(90));
        assert_not_a_crash(&label, &output);
        let left = residents(&self.root, pgid);
        assert!(left.is_empty(), "{label}: left processes outside tmux:\n{}", left.join("\n"));
        let impure = purity_violations(&self.root);
        assert!(impure.is_empty(), "{label}: state/station purity violated:\n{}", impure.join("\n"));
        output
    }

    fn json(&self, args: &[&str]) -> (Option<i32>, Value) {
        let output = self.run(args);
        let value = stdout_json(&output).unwrap_or_else(|| panic!("{args:?}: no JSON: {}", text(&output)));
        (output.status.code(), value)
    }

    fn start_team(&self) {
        let spec = self.root.join("spec");
        let (code, value) = self.json(&["start", "--from", spec.to_str().unwrap(), "--json"]);
        assert!(code == Some(0) && value["ok"] == true, "fixture: fake team start must succeed: {value}");
        self.wait_status(&["w1", "w2"], "running");
    }

    fn status(&self) -> BTreeMap<String, String> {
        let (_, value) = self.json(&["status", "--json"]);
        value["nodes"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|node| {
                (
                    node["name"].as_str().unwrap_or_default().to_string(),
                    node["runtime_status"].as_str().unwrap_or_default().to_string(),
                )
            })
            .collect()
    }

    fn wait_status(&self, agents: &[&str], wanted: &str) {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let status = self.status();
            if agents.iter().all(|a| status.get(*a).map(String::as_str) == Some(wanted)) {
                return;
            }
            assert!(Instant::now() < deadline, "agents {agents:?} never reached {wanted}: {status:?}");
            std::thread::sleep(Duration::from_millis(300));
        }
    }

    fn team_socket(&self) -> Option<PathBuf> {
        let state: Value =
            serde_json::from_slice(&std::fs::read(self.root.join(".team/runtime/state.json")).ok()?).ok()?;
        let socket = state
            .get("tmux_socket")
            .or_else(|| state.get("teams")?.get(state.get("active_team_key")?.as_str()?)?.get("tmux_socket"))?
            .as_str()?;
        Some(PathBuf::from(socket))
    }

    /// `pane_id pane_pid` rows of the team's tmux server (empty when it is gone).
    fn panes(&self) -> Vec<String> {
        let Some(socket) = self.team_socket() else { return Vec::new() };
        let output = Command::new(&self.tmux)
            .arg("-S")
            .arg(&socket)
            .args(["list-panes", "-a", "-F", "#{pane_id} #{pane_pid}"])
            .output();
        match output {
            Ok(output) if output.status.success() => {
                let mut rows: Vec<String> = String::from_utf8_lossy(&output.stdout).lines().map(str::to_string).collect();
                rows.sort();
                rows
            }
            _ => Vec::new(),
        }
    }

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

    fn stop_and_assert_clean(&self) {
        let (mut code, mut value) = self.json(&["stop", "--json"]);
        // Pre-existing BASE race outside Stage 3 scope: another tmux server on the host can
        // change the shared socket parent between the death probe and owned-socket cleanup.
        for _ in 0..2 {
            let fenced = value.to_string().contains("shutdown socket parent changed after death probe");
            if code == Some(0) || !fenced {
                break;
            }
            std::thread::sleep(Duration::from_millis(500));
            (code, value) = self.json(&["stop", "--json"]);
        }
        assert!(code == Some(0) && value["ok"] == true, "team stop must succeed: {value}");
        let deadline = Instant::now() + Duration::from_secs(15);
        let mut left = self.leftovers();
        while !left.is_empty() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(200));
            left = self.leftovers();
        }
        assert!(left.is_empty(), "processes naming the workspace survived team stop:\n{}", left.join("\n"));
    }
}

impl Drop for Team {
    fn drop(&mut self) {
        let socket = self.team_socket();
        let _ = Command::new(env!("CARGO_BIN_EXE_team-agent"))
            .args(["stop", "--json", "--workspace"])
            .arg(&self.root)
            .env_clear()
            .env("HOME", &self.home)
            .env("PATH", &self.path_env)
            .env("TMUX", format!("{},0,0", self.caller_socket.display()))
            .env("TMUX_PANE", &self.caller_pane)
            .stdin(Stdio::null())
            .output();
        if let Some(socket) = socket {
            if socket != self.caller_socket && socket.exists() {
                let _ = Command::new(&self.tmux).arg("-S").arg(&socket).arg("kill-server").output();
            }
        }
        let _ = Command::new(&self.tmux).arg("-S").arg(&self.caller_socket).arg("kill-server").output();
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn first_step(value: &Value) -> &Value {
    &value["steps"][0]
}

// ─────────────────────── a) canonical unified start ───────────────────────

/// On a stopped team, every `start AGENT` dry run is planned by the unified kernel:
/// typed receipt, policy echoed, `op` determined by seat state and requested change,
/// provider change gated on `--session fresh`, and not one byte written.
#[test]
fn prop_unified_start_dry_run_policy_matrix_is_typed_and_read_only() {
    const POLICIES: &[&[&str]] = &[
        &[],
        &["--session", "preserve"],
        &["--session", "auto"],
        &["--session", "fresh"],
        &["--allow-fresh"],
    ];
    for_each_case("unified start dry-run matrix", 2, |rng, case| {
        let team = Team::new(&format!("dry{case}"));
        team.start_team();
        team.stop_and_assert_clean();
        for _ in 0..rng.range(8, 12) {
            let agent = *rng.pick(&["w1", "w2", "w9"]);
            let policy = *rng.pick(POLICIES);
            let provider_change = agent != "w9" && rng.below(3) == 0;
            let model = if rng.below(3) == 0 { Some(format!("fake{}", rng.slug(3))) } else { None };
            let mut args: Vec<String> = vec!["start".into(), agent.into()];
            if provider_change {
                args.extend(["--provider".into(), "pi".into(), "--model".into(), "anthropic/claude-haiku-5-5".into()]);
            } else if let Some(model) = &model {
                args.extend(["--model".into(), model.clone()]);
            }
            args.extend(policy.iter().map(|s| s.to_string()));
            args.extend(["--dry-run".into(), "--json".into()]);
            let args: Vec<&str> = args.iter().map(String::as_str).collect();
            let before = snapshot(&team.root);
            let (code, value) = team.json(&args);
            let changed = snapshot_diff(&before, &snapshot(&team.root));
            assert!(changed.is_empty(), "{args:?}: dry run wrote {changed:?}");
            let fresh = policy == ["--session", "fresh"];
            let echoed = match policy {
                [] => "preserve",
                ["--allow-fresh"] => "auto",
                [_, value] => value,
                _ => unreachable!(),
            };
            if provider_change && !fresh {
                let error = value["error"].as_str().unwrap_or_default();
                assert!(
                    code == Some(1)
                        && value["ok"] == false
                        && error.contains("provider_change_requires_fresh")
                        && error.contains("--session fresh"),
                    "{args:?}: a provider change without --session fresh must be refused with the copyable fresh command: {value}"
                );
                continue;
            }
            assert!(code == Some(0) && value["ok"] == true, "{args:?}: {value}");
            assert_eq!(value["dry_run"], true, "{args:?}: {value}");
            assert_eq!(value["surface"], "canonical", "{args:?}: {value}");
            assert_eq!(value["session_policy"], echoed, "{args:?}: {value}");
            let step = first_step(&value);
            assert_eq!(step["action"], "start", "{args:?}: {value}");
            assert_eq!(step["agent"], agent, "{args:?}: {value}");
            let expected_op = if agent == "w9" {
                "created"
            } else if provider_change {
                "provider_changed"
            } else {
                "fresh"
            };
            assert_eq!(step["op"], expected_op, "{args:?}: {value}");
            if provider_change {
                assert_eq!(step["declaration"]["provider"], "pi", "{args:?}: {value}");
            } else if let Some(model) = &model {
                assert_eq!(step["declaration"]["model"], model.as_str(), "{args:?}: {value}");
            }
            if agent == "w9" && !provider_change {
                let defaulted = step["defaulted"].as_array().cloned().unwrap_or_default();
                assert!(defaulted.iter().any(|d| d == "provider"), "{args:?}: new seat must report a defaulted provider: {value}");
            }
        }
    });
}

#[test]
fn unified_start_rejects_conflicting_session_flags_without_side_effects() {
    let team = Team::new("flags");
    team.start_team();
    // Live team: both restart forms replace the live cohort through one `restart_team`
    // step (ee19919a runtime.rs:919-947). While physical identity is still unverified
    // right after start, the planner answers with the typed planning refusal; only that
    // reply may be retried, for at most 5 s.
    let unverified = |value: &Value| {
        let error = value["error"].as_str().unwrap_or_default();
        value["ok"] == false
            && value["phase"] == "planning_or_selection"
            && (error.contains("unverified") || error.contains("unknown physical identity"))
    };
    for (args, policy) in [
        (&["team", "restart", "--dry-run", "--json"][..], "auto"),
        (&["restart", "--dry-run", "--json"][..], "preserve"),
    ] {
        let before = snapshot(&team.root);
        let deadline = Instant::now() + Duration::from_secs(5);
        let (mut code, mut value) = team.json(args);
        while code == Some(1) && unverified(&value) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(250));
            (code, value) = team.json(args);
        }
        assert!(
            code == Some(0) && value["session_policy"] == policy && first_step(&value)["action"] == "restart_team",
            "{args:?}: a live team restart plans restart_team with its {policy} default: {value}"
        );
        let changed = snapshot_diff(&before, &snapshot(&team.root));
        assert!(changed.is_empty(), "{args:?}: dry run wrote {changed:?}");
    }
    team.stop_and_assert_clean();
    for args in [
        &["start", "w1", "--allow-fresh", "--session", "fresh", "--dry-run", "--json"][..],
        &["start", "w1", "--allow-fresh", "--session", "preserve", "--json"],
        &["agent", "restart", "w1", "--session", "fresh", "--dry-run", "--json"],
    ] {
        let before = snapshot(&team.root);
        let output = team.run(args);
        assert_eq!(output.status.code(), Some(2), "{args:?} must be a usage error: {}", text(&output));
        let value = stdout_json(&output).unwrap_or_default();
        assert!(
            value["ok"] == false && value["error"].as_str().unwrap_or_default().contains("usage error"),
            "{args:?}: {}",
            text(&output)
        );
        let changed = snapshot_diff(&before, &snapshot(&team.root));
        assert!(changed.is_empty(), "{args:?}: usage error wrote {changed:?}");
    }
    let (code, value) = team.json(&["agent", "restart", "w1", "--session", "preserve", "--dry-run", "--json"]);
    assert!(code == Some(0) && value["session_policy"] == "preserve", "{value}");
    let (code, value) = team.json(&["agent", "restart", "w1", "--dry-run", "--json"]);
    assert!(
        code == Some(0) && value["session_policy"] == "auto" && first_step(&value)["action"] == "start_agent",
        "agent restart defaults to --session auto and plans a unified agent start: {value}"
    );
    // Shut-down team: since A14 (373efa16) a verified stop persists the shutdown, and
    // the restart planner then plans one `start_agent` per stopped seat instead of a
    // `restart_team` (ee19919a runtime.rs:919-947). Both forms keep their defaults.
    let status = team.status();
    assert!(
        ["w1", "w2"].iter().all(|agent| status.get(*agent).map(String::as_str) == Some("stopped")),
        "precondition: team stop leaves w1 and w2 stopped: {status:?}"
    );
    for (args, policy) in [
        (&["team", "restart", "--dry-run", "--json"][..], "auto"),
        (&["restart", "--dry-run", "--json"][..], "preserve"),
    ] {
        let before = snapshot(&team.root);
        let (code, value) = team.json(args);
        let steps = value["steps"].as_array().cloned().unwrap_or_default();
        let agents: std::collections::BTreeSet<&str> =
            steps.iter().filter_map(|step| step["agent"].as_str()).collect();
        assert!(
            code == Some(0)
                && value["session_policy"] == policy
                && steps.len() == 2
                && steps.iter().all(|step| step["action"] == "start_agent")
                && agents == std::collections::BTreeSet::from(["w1", "w2"]),
            "{args:?}: a shut-down team restart plans one start_agent per stopped seat with its {policy} default: {value}"
        );
        assert!(
            !steps.iter().any(|step| step["action"] == "restart_team"),
            "{args:?}: a shut-down team has no live cohort to restart: {value}"
        );
        let changed = snapshot_diff(&before, &snapshot(&team.root));
        assert!(changed.is_empty(), "{args:?}: dry run wrote {changed:?}");
    }
}

/// `start` never replaces or edits a running seat: an unchanged start is an
/// `already_running` no-op (or, while physical identity is still unverified, a typed
/// planning refusal), and fresh-session or configuration changes are refused until the
/// seat is stopped. No state, station or pane change either way, and no legacy fallback.
#[test]
fn running_seats_are_never_replaced_or_edited_by_start() {
    let team = Team::new("running");
    team.start_team();
    let unverified = |value: &Value| {
        let error = value["error"].as_str().unwrap_or_default();
        value["ok"] == false
            && value["phase"] == "planning_or_selection"
            && (error.contains("unverified") || error.contains("unknown physical identity"))
    };
    for _ in 0..2 {
        let before = snapshot(&team.root);
        let panes = team.panes();
        let (code, value) = team.json(&["start", "w1", "--json"]);
        let noop = code == Some(0)
            && value["ok"] == true
            && value["status"] == "already_running"
            && value["steps"].as_array().is_some_and(Vec::is_empty)
            && value["skipped"][0]["reason"] == "already_running";
        assert!(noop || (code == Some(1) && unverified(&value)), "unchanged start of a running seat: {value}");
        assert!(snapshot_diff(&before, &snapshot(&team.root)).is_empty(), "unchanged start wrote state: {value}");
        assert_eq!(team.panes(), panes, "unchanged start touched tmux panes: {value}");
    }
    for args in [
        &["start", "w2", "--session", "fresh", "--json"][..],
        &["start", "w2", "--model", "fake9", "--json"],
        &["start", "w2", "--model", "fake9", "--dry-run", "--json"],
        &["start", "w1", "--provider", "pi", "--model", "anthropic/claude-haiku-5-5", "--session", "fresh", "--json"],
    ] {
        let before = snapshot(&team.root);
        let panes = team.panes();
        let (code, value) = team.json(args);
        assert!(
            code == Some(1) && value["ok"] == false && value["error"].as_str().is_some_and(|e| e.contains("stop")),
            "{args:?}: a running seat must refuse a fresh session or configuration change and point at stop: {value}"
        );
        assert!(snapshot_diff(&before, &snapshot(&team.root)).is_empty(), "{args:?}: refusal wrote state");
        assert_eq!(team.panes(), panes, "{args:?}: refusal touched tmux panes");
    }
    team.stop_and_assert_clean();
}

/// Team creation (`start --from`), the legacy bare `restart` port, team stop and
/// unified reactivation of a stopped seat all keep working end to end.
#[test]
fn stopped_seat_reactivates_through_unified_start_and_legacy_restart_still_rebuilds() {
    let team = Team::new("cycle");
    team.start_team();
    let panes = team.panes();
    let (code, value) = team.json(&["restart", "--json"]);
    assert!(
        code == Some(0) && value["ok"] == true && value["status"] == "restarted" && value["coordinator_started"] == false,
        "legacy bare restart must rebuild the team without a coordinator: {value}"
    );
    team.wait_status(&["w1", "w2"], "running");
    assert_ne!(team.panes(), panes, "restart must replace the worker panes");
    team.stop_and_assert_clean();
    let (code, value) = team.json(&["start", "w1", "--json"]);
    let step = first_step(&value);
    assert!(
        code == Some(0) && value["ok"] == true && step["op"] == "fresh" && step["ok"] == true,
        "a stopped contextless seat reactivates through the unified kernel with op=fresh: {value}"
    );
    team.wait_status(&["w1"], "running");
    let seat = team.root.join(format!(".team/station-v1/{}/seats/w1.json", team.name));
    assert!(seat.is_file(), "unified start must publish the seat manifest at {}", seat.display());
    team.stop_and_assert_clean();
}

/// A direct agent start on a new team creates the team and the seat in one unified
/// operation and records its start source and seat manifest.
#[test]
fn direct_agent_start_creates_team_and_seat_through_the_unified_kernel() {
    let team = Team::new("direct");
    let (code, value) = team.json(&["start", "w9", "--provider", "fake", "--team", "s3direct", "--json"]);
    let step = first_step(&value);
    assert!(
        code == Some(0) && value["ok"] == true && step["op"] == "created" && step["receipt"]["agent_id"] == "w9",
        "direct start must create the seat: {value}"
    );
    assert!(team.root.join(".team/station-v1/s3direct/seats/w9.json").is_file());
    assert!(team.root.join(".team/start-source/s3direct/agents/w9.md").is_file());
    assert!(state_agents(&team.root).contains(&"w9".to_string()), "state must project the new seat");
    team.stop_and_assert_clean();
}

// ─────────────────────── b) K9 purity / manifest invariant ───────────────────────

/// A seat manifest that fails the live manifest invariant is refused by the unified
/// kernel; it is never rewritten and nothing is spawned for it.
#[test]
fn corrupted_seat_manifest_is_refused_never_rewritten_and_spawns_nothing() {
    let team = Team::new("corrupt");
    team.start_team();
    team.stop_and_assert_clean();
    let seat = team.root.join(format!(".team/station-v1/{}/seats/w1.json", team.name));
    let mut manifest: Value = serde_json::from_slice(&std::fs::read(&seat).unwrap()).unwrap();
    manifest["phase"] = Value::String("bogus".into());
    let corrupted = serde_json::to_vec(&manifest).unwrap();
    std::fs::write(&seat, &corrupted).unwrap();
    // The corrupted manifest itself is the fixture: run outside the purity wrapper.
    let (output, pgid) = run_bounded(team.command(&["start", "w1", "--json"]), "start corrupted", Duration::from_secs(90));
    assert_not_a_crash("start corrupted", &output);
    let value = stdout_json(&output).unwrap_or_default();
    assert!(
        output.status.code() != Some(0) && value["ok"] == false,
        "a seat whose manifest fails validation must not start: {}",
        text(&output)
    );
    assert!(residents(&team.root, pgid).is_empty());
    assert_eq!(std::fs::read(&seat).unwrap(), corrupted, "the corrupted manifest was rewritten");
    let (output, _) = run_bounded(team.command(&["status", "w1", "--json"]), "status corrupted", Duration::from_secs(30));
    let status = stdout_json(&output).unwrap_or_default();
    assert_ne!(status["nodes"][0]["runtime_status"], "running", "nothing may run for a refused seat: {status}");
    assert_eq!(std::fs::read(&seat).unwrap(), corrupted, "status rewrote the corrupted manifest");
}

// ─────────────────────── c) global state purity ───────────────────────

/// Read paths never rewrite runtime state, seat manifests, locks or inboxes.
#[test]
fn read_only_commands_never_rewrite_state_or_seat_manifests() {
    const READS: &[&[&str]] = &[
        &["status"],
        &["status", "--json"],
        &["status", "w1", "--json"],
        &["status", "--detail", "--json"],
        &["team", "status", "--json"],
        &["doctor", "--json"],
        &["locate", "--json"],
        &["inbox", "leader", "--json"],
        &["inbox", "w1", "--json"],
        &["schedule", "list", "--json"],
        &["approvals", "--json"],
    ];
    let team = Team::new("reads");
    team.start_team();
    for args in READS {
        let before = snapshot(&team.root);
        let output = team.run(args);
        assert_eq!(output.status.code(), Some(0), "{args:?}: {}", text(&output));
        let changed = snapshot_diff(&before, &snapshot(&team.root));
        assert!(changed.is_empty(), "{args:?}: a read path wrote {changed:?}");
    }
    team.stop_and_assert_clean();
}

/// Random lifecycle sequences keep the runtime projection canonical: purity holds after
/// every command (checked by `Team::run`), status agrees with state, and the team stops
/// clean.
#[test]
fn prop_state_projection_stays_canonical_across_random_lifecycle_sequences() {
    const COMMANDS: &[&[&str]] = &[
        &["status", "--json"],
        &["status", "w1", "--json"],
        &["team", "status", "--json"],
        &["doctor", "--json"],
        &["locate", "--json"],
        &["start", "w1", "--dry-run", "--json"],
        &["start", "w3", "--dry-run", "--json"],
        &["agent", "restart", "w2", "--dry-run", "--json"],
        &["team", "restart", "--dry-run", "--json"],
        &["restart", "--dry-run", "--json"],
        &["start", "w1", "--json"],
        &["stop", "w2", "--json"],
        &["send", "w1", "PAYLOAD", "--mailbox", "--json"],
        &["inbox", "w1", "--json"],
    ];
    for_each_case("state projection purity", 2, |rng, case| {
        let team = Team::new(&format!("seq{case}"));
        team.start_team();
        for _ in 0..rng.range(6, 10) {
            let payload = format!("stage3 {}", rng.slug(8));
            let args: Vec<&str> = rng
                .pick(COMMANDS)
                .iter()
                .map(|arg| if *arg == "PAYLOAD" { payload.as_str() } else { *arg })
                .collect();
            team.run(&args);
            let declared = state_agents(&team.root);
            let listed: Vec<String> = team.status().into_keys().collect();
            let unknown: Vec<&String> = listed.iter().filter(|name| !declared.contains(name)).collect();
            assert!(unknown.is_empty(), "{args:?}: status lists agents absent from state: {unknown:?} (state {declared:?})");
        }
        team.stop_and_assert_clean();
        let impure = purity_violations(&team.root);
        assert!(impure.is_empty(), "after stop: {impure:?}");
    });
}
