//! PID-reuse lifecycle deadlock — frozen RED/GUARD suite (repro-opus).
//!
//! Field incident (codex-effort-fix/test-opus, 2026-11-10): the leader stopped the
//! claude seat test-opus at 12:21:59Z (status=stopped, explicit_stop=true, pane_id and
//! window removed, pane_pid=41569 retained, spawned_at=09:40:24Z). The host rebooted at
//! 15:27:51Z and at 16:06:29Z the leader's own `pi` process was born with PID 41569.
//! From then on:
//!   * `team-agent start test-opus` -> "agent ownership/liveness is unverified; run
//!     `team-agent stop test-opus ...`"
//!   * `team-agent stop test-opus`  -> "unknown physical identity; no lifecycle action
//!     performed" (restart likewise) — no CLI exit at all.
//!
//! Every case below reproduces that row shape in a hermetic fake-provider team and then
//! rewrites ONLY the fixture's own state (never a real workspace) so the recorded
//! `pane_pid` names a live, unrelated, test-owned "sentinel" process tree shaped like the
//! field process (a root without `--workspace`, plus a child carrying
//! `--workspace <root>` like the leader's `team-agent mcp-server`).
//!
//! RED  (must fail on c9e83c8f, pass after the fix):
//!   R1 field_shape_stopped_fenced_seat_with_reused_pid_resumes_without_state_surgery
//!   R2 cross_boot_reused_pid_on_stopped_seat_is_stopped_not_unknown
//!   R3 reused_pid_on_stopped_seat_is_classified_for_legacy_and_native_provider_rows
//!   R4 running_row_whose_carrier_died_and_pid_was_reused_is_recoverable_and_does_not_block_team_start
//!   R5 start_refusal_never_points_at_a_stop_that_refuses_for_the_same_reason
//!   R6 team_stop_never_signals_a_process_tree_that_reused_a_stopped_rows_pid   (adjacent H1)
//!   R7 new_spawns_persist_pane_birth_identity_matching_their_carrier           (schema)
//!   R8 recorded_birth_identity_mismatch_overrides_an_in_window_spawned_at      (schema)
//! GUARD (green on c9e83c8f, must stay green):
//!   G1 guard_pid_born_inside_the_spawn_window_is_never_adopted_restarted_or_signalled
//!   G2 guard_live_matched_seat_is_never_replaced_or_signalled
//!   G3 guard_recorded_birth_identity_match_on_a_stopped_row_is_never_adopted_or_signalled
//!
//! Schema contract (agreed with dev-astra via leader): `pane_birth_identity` is persisted
//! beside `pane_pid`, holding exactly `process_birth_identity(pane_pid)`
//! (`macos:<start_tvsec>:<start_tvusec>` / `linux:<boot_id>:<start_ticks>`). When present
//! the exact comparison decides; when absent (legacy rows such as the field row) boot
//! time and the `spawned_at` window decide; an unreadable live identity stays Unknown.
//! Cases R1-R6/G1 simulate the field's LEGACY row, so they strip `pane_birth_identity`.
//! Window assumptions: born >= 1800 s after `spawned_at` => reused; born ~0.5 s after
//! `spawned_at` => plausibly the carrier.
//!
//! Process-lifetime rules: every sentinel is a test-owned child in its own process
//! group with a stdin lifeline AND a hard 100 s alarm; Drop SIGKILLs the group before
//! reaping (the unreaped leader pins the PGID, so the kill cannot hit a recycled group).
//! The SUT's tmux servers live under a test-owned TMUX_TMPDIR; the fixture caller pane
//! exits by itself after 120 s.
#![cfg(unix)]
#![allow(dead_code, clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use serde_json::Value;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const IDENTITY_REFUSALS: &[&str] = &[
    "unknown physical identity",
    "ownership/liveness is unverified",
];
/// Far outside any plausible spawn latency, well inside the host uptime we require.
const LONG_AGO_SECS: i64 = 1800;

fn identity_refusal(value: &Value) -> bool {
    let body = value.to_string();
    IDENTITY_REFUSALS.iter().any(|needle| body.contains(needle))
}

// ───────────────────────────── host facts ─────────────────────────────

fn now_secs() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs_f64()
}

fn boot_secs() -> i64 {
    if cfg!(target_os = "macos") {
        let out = Command::new("/usr/sbin/sysctl").args(["-n", "kern.boottime"]).output().unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        let sec = text
            .split("sec =")
            .nth(1)
            .and_then(|rest| rest.split(',').next())
            .and_then(|n| n.trim().parse::<i64>().ok());
        sec.unwrap_or_else(|| panic!("cannot parse kern.boottime: {text}"))
    } else {
        let stat = std::fs::read_to_string("/proc/stat").unwrap();
        stat.lines()
            .find_map(|line| line.strip_prefix("btime ").and_then(|n| n.trim().parse().ok()))
            .expect("btime in /proc/stat")
    }
}

fn rfc3339(epoch_secs: f64) -> String {
    let micros = (epoch_secs * 1_000_000.0) as i64;
    chrono::DateTime::<chrono::Utc>::from_timestamp_micros(micros)
        .unwrap()
        .to_rfc3339_opts(chrono::SecondsFormat::Micros, false)
}

/// `ps` start-time column: same PID + same start time + alive = the same process.
fn lstart(pid: u32) -> Option<String> {
    let out = Command::new("/bin/ps").args(["-o", "lstart=", "-p", &pid.to_string()]).output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (out.status.success() && !text.is_empty()).then_some(text)
}

fn children_of(pid: u32) -> Vec<u32> {
    let out = Command::new("/bin/ps").args(["-axo", "pid=,ppid="]).output().unwrap();
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let child: u32 = fields.next()?.parse().ok()?;
            let parent: u32 = fields.next()?.parse().ok()?;
            (parent == pid).then_some(child)
        })
        .collect()
}

/// Exactly what `team_agent_contract::host::process::process_birth_identity` returns.
#[cfg(target_os = "macos")]
fn birth_token(pid: u32) -> Option<String> {
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
    let read = unsafe {
        libc::proc_pidinfo(
            pid as libc::c_int,
            libc::PROC_PIDTBSDINFO,
            0,
            (&mut info as *mut libc::proc_bsdinfo).cast(),
            size,
        )
    };
    (read == size).then(|| format!("macos:{}:{}", info.pbi_start_tvsec, info.pbi_start_tvusec))
}

#[cfg(target_os = "linux")]
fn birth_token(pid: u32) -> Option<String> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let ticks = stat[stat.rfind(')')? + 1..].split_whitespace().nth(19)?.to_string();
    let boot = std::fs::read_to_string("/proc/sys/kernel/random/boot_id").ok()?;
    Some(format!("linux:{}:{ticks}", boot.trim()))
}

/// A well-formed token of the same platform shape that names a different birth.
fn foreign_token(token: &str) -> String {
    let (head, tail) = token.rsplit_once(':').unwrap();
    let altered: u64 = tail.parse::<u64>().unwrap() ^ 1;
    format!("{head}:{altered}")
}

// ───────────────────────────── sentinel ─────────────────────────────

/// A live process tree unrelated to the team, shaped like the field PID holder:
/// root `sh` (no `--workspace`) -> child `perl` whose argv carries `--workspace <root>`.
struct Sentinel {
    child: Child,
    root_pid: u32,
    root_birth: String,
    leaf_pid: u32,
    leaf_birth: String,
    born_after: f64,
}

impl Sentinel {
    fn spawn(workspace: &Path) -> Self {
        let born_after = now_secs();
        let mut command = Command::new("/bin/sh");
        command
            .arg("-c")
            .arg(r#"/usr/bin/perl -e 'alarm 100; <STDIN>' pidreuse-mcp --workspace "$1"; exit 0"#)
            .arg("pidreuse-leader")
            .arg(workspace)
            .current_dir(workspace)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0);
        let child = command.spawn().unwrap();
        let root_pid = child.id();
        let deadline = Instant::now() + Duration::from_secs(10);
        let leaf_pid = loop {
            if let Some(&leaf) = children_of(root_pid).first() {
                break leaf;
            }
            assert!(Instant::now() < deadline, "sentinel child never appeared");
            std::thread::sleep(Duration::from_millis(50));
        };
        let root_birth = lstart(root_pid).expect("sentinel root start time");
        let leaf_birth = lstart(leaf_pid).expect("sentinel leaf start time");
        Self { child, root_pid, root_birth, leaf_pid, leaf_birth, born_after }
    }

    fn intact(&self) -> bool {
        lstart(self.root_pid).as_deref() == Some(self.root_birth.as_str())
            && lstart(self.leaf_pid).as_deref() == Some(self.leaf_birth.as_str())
    }

    fn assert_intact(&self, after: &str) {
        assert!(
            self.intact(),
            "{after}: the unrelated process tree holding the reused PID was signalled or replaced \
             (root {} {:?} -> {:?}, leaf {} {:?} -> {:?})",
            self.root_pid,
            self.root_birth,
            lstart(self.root_pid),
            self.leaf_pid,
            self.leaf_birth,
            lstart(self.leaf_pid)
        );
    }
}

impl Drop for Sentinel {
    fn drop(&mut self) {
        unsafe {
            libc::killpg(self.root_pid as libc::pid_t, libc::SIGKILL);
        }
        let _ = self.child.wait();
    }
}

// ───────────────────────────── fixture ─────────────────────────────

fn scratch_root(label: &str) -> PathBuf {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let base = if Path::new("/private/tmp").is_dir() { "/private/tmp" } else { "/tmp" };
    let root = PathBuf::from(base).join(format!(
        "ta-pr-{label}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("home")).unwrap();
    std::fs::create_dir_all(root.join("tt")).unwrap();
    root.canonicalize().unwrap()
}

fn text(output: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr))
}

fn run_bounded(mut command: Command, label: &str, limit: Duration) -> Output {
    command.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).process_group(0);
    let child = command.spawn().unwrap();
    let pgid = child.id();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(child.wait_with_output());
    });
    match rx.recv_timeout(limit) {
        Ok(output) => output.unwrap(),
        Err(_) => {
            unsafe {
                libc::killpg(pgid as libc::pid_t, libc::SIGKILL);
            }
            panic!("{label}: did not return within {limit:?}");
        }
    }
}

fn tmux_binary() -> PathBuf {
    std::env::var_os("CONTRACT_TMUX").map(PathBuf::from).unwrap_or_else(|| {
        std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
            .map(|dir| dir.join("tmux"))
            .find(|path| path.is_file())
            .expect("tmux is required")
    })
}

static LIVE: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Own-group reaper on a stdin lifeline: whenever the test process goes away (panic,
/// SIGKILL, normal Drop) it kills every tmux server under this root and removes it.
/// In-flight CLI calls run in their own process group, so they are matched by the
/// unique `--workspace <root>` marker; two passes catch a server spawned mid-teardown.
const REAPER: &str = r#"trap '' INT HUP TERM; cat >/dev/null
for pass in 1 2; do
  /usr/bin/pkill -KILL -f -- "--workspace $1" >/dev/null 2>&1
  for s in "$1"/tt/tmux-*/* "$1"/c.sock; do [ -S "$s" ] && "$2" -S "$s" kill-server >/dev/null 2>&1; done
  sleep 0.3
done
[ -n "$3" ] || rm -rf "$1""#;

struct Team {
    reaper: Option<Child>,
    _serial: std::sync::MutexGuard<'static, ()>,
    root: PathBuf,
    home: PathBuf,
    tmux_tmpdir: PathBuf,
    name: String,
    tmux: PathBuf,
    caller_socket: PathBuf,
    caller_pane: String,
    path_env: String,
}

impl Team {
    fn new(label: &str) -> Self {
        let serial = LIVE.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let root = scratch_root(label);
        let home = root.join("home");
        let tmux_tmpdir = root.join("tt");
        let name = format!("pr{}", label.chars().filter(char::is_ascii_alphanumeric).take(8).collect::<String>());
        let spec = root.join("spec");
        std::fs::create_dir_all(spec.join("agents")).unwrap();
        std::fs::write(
            spec.join("TEAM.md"),
            format!("---\nname: {name}\nobjective: pid reuse lifecycle\nprovider: fake\ndisplay_backend: none\n---\nTeam.\n"),
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
        for provider in ["pi", "claude", "codex"] {
            std::fs::write(
                stub.join(provider),
                format!("#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then echo 0.70.0; exit 0; fi\necho \"pid-reuse stub {provider} must not launch: $*\" >&2\nexit 3\n"),
            )
            .unwrap();
            std::fs::set_permissions(stub.join(provider), std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        }
        let tmux = tmux_binary();
        let keep = if std::env::var_os("TEAM_AGENT_KEEP_TEST_TMP").is_some() { "keep" } else { "" };
        let reaper = Command::new("/bin/sh")
            .arg("-c")
            .arg(REAPER)
            .arg("pidreuse-reaper")
            .arg(&root)
            .arg(&tmux)
            .arg(keep)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()
            .unwrap();
        let caller_socket = root.join("c.sock");
        let created = Command::new(&tmux)
            .args(["-u", "-S"])
            .arg(&caller_socket)
            .args(["-f", "/dev/null", "new-session", "-d", "-s", "fixture-caller", "-c"])
            .arg(&home)
            .args(["-P", "-F", "#{pane_id}", "exec sleep 120"])
            .env("HOME", &home)
            .env("TMUX_TMPDIR", &tmux_tmpdir)
            .env_remove("TMUX")
            .env_remove("TMUX_PANE")
            .output()
            .unwrap();
        assert!(created.status.success(), "fixture caller failed: {}", text(&created));
        let caller_pane = String::from_utf8(created.stdout).unwrap().trim().to_string();
        let path_env = format!("{}:{}:/usr/bin:/bin", stub.display(), tmux.parent().unwrap().display());
        let team = Self { reaper: Some(reaper), _serial: serial, root, home, tmux_tmpdir, name, tmux, caller_socket, caller_pane, path_env };
        let spec = team.root.join("spec");
        let (code, value) = team.json(&["start", "--from", spec.to_str().unwrap(), "--json"]);
        assert!(code == Some(0) && value["ok"] == true, "fixture: fake team start must succeed: {value}");
        team.wait_running(&["w1", "w2"]);
        team
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
            .env("TMUX_TMPDIR", &self.tmux_tmpdir)
            .env("TMUX", format!("{},0,0", self.caller_socket.display()))
            .env("TMUX_PANE", &self.caller_pane)
            .env("TEAM_AGENT_LEADER_PROVIDER", "fake");
        command
    }

    fn json(&self, args: &[&str]) -> (Option<i32>, Value) {
        let label = format!("{args:?}");
        let output = run_bounded(self.command(args), &label, Duration::from_secs(60));
        let body = text(&output);
        assert!(
            output.status.code().is_some() && output.status.code() != Some(101) && !body.contains("panicked at"),
            "{label}: CLI crashed ({:?}): {body}",
            output.status.code()
        );
        let value = serde_json::from_slice(&output.stdout).unwrap_or_else(|_| panic!("{label}: no JSON: {body}"));
        (output.status.code(), value)
    }

    fn state_path(&self) -> PathBuf {
        self.root.join(".team/runtime/state.json")
    }

    fn state(&self) -> Value {
        serde_json::from_slice(&std::fs::read(self.state_path()).unwrap()).unwrap()
    }

    fn row(&self, agent: &str) -> Value {
        let state = self.state();
        state
            .pointer(&format!("/teams/{}/agents/{agent}", self.name))
            .or_else(|| state.pointer(&format!("/agents/{agent}")))
            .cloned()
            .unwrap_or(Value::Null)
    }

    /// Rewrite the fixture's OWN persisted row(s) for `agent` (root projection and
    /// `teams.<name>`), simulating rows written by an earlier binary/incarnation.
    fn patch_row(&self, agent: &str, patch: impl Fn(&mut serde_json::Map<String, Value>)) {
        let mut state = self.state();
        let mut patched = 0;
        if state.get("team_key").and_then(Value::as_str) == Some(self.name.as_str()) {
            if let Some(row) = state.pointer_mut(&format!("/agents/{agent}")).and_then(Value::as_object_mut) {
                patch(row);
                patched += 1;
            }
        }
        if let Some(row) = state
            .pointer_mut(&format!("/teams/{}/agents/{agent}", self.name))
            .and_then(Value::as_object_mut)
        {
            patch(row);
            patched += 1;
        }
        assert!(patched > 0, "fixture row {agent} not found in {}", self.state_path().display());
        let tmp = self.state_path().with_extension("json.fixture-tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(&state).unwrap()).unwrap();
        std::fs::rename(&tmp, self.state_path()).unwrap();
    }

    /// Make `agent`'s recorded carrier PID name the sentinel, recorded at `spawned_at`,
    /// as a LEGACY row (no `pane_birth_identity`), exactly like the field row.
    fn point_row_at(&self, agent: &str, sentinel: &Sentinel, spawned_at: f64) {
        let pid = sentinel.root_pid;
        let at = rfc3339(spawned_at);
        self.patch_row(agent, |row| {
            row.insert("pane_pid".into(), Value::from(pid));
            row.insert("spawned_at".into(), Value::from(at.clone()));
            row.remove("pane_birth_identity");
        });
    }

    fn set_birth_identity(&self, agent: &str, token: &str) {
        self.patch_row(agent, |row| {
            row.insert("pane_birth_identity".into(), Value::from(token));
        });
    }

    fn status(&self) -> std::collections::BTreeMap<String, String> {
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

    fn wait_running(&self, agents: &[&str]) {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let status = self.status();
            if agents.iter().all(|a| status.get(*a).map(String::as_str) == Some("running")) {
                return;
            }
            assert!(Instant::now() < deadline, "agents {agents:?} never reached running: {status:?}");
            std::thread::sleep(Duration::from_millis(300));
        }
    }

    fn team_socket(&self) -> Option<PathBuf> {
        let state = self.state();
        let socket = state
            .pointer(&format!("/teams/{}/tmux_socket", self.name))
            .or_else(|| state.get("tmux_socket"))?
            .as_str()?;
        Some(PathBuf::from(socket))
    }

    /// `pane_id pane_pid` rows of the team's tmux server (sorted; empty when gone).
    fn panes(&self) -> Vec<String> {
        let Some(socket) = self.team_socket() else { return Vec::new() };
        match Command::new(&self.tmux).arg("-S").arg(&socket).args(["list-panes", "-a", "-F", "#{pane_id} #{pane_pid}"]).output() {
            Ok(output) if output.status.success() => {
                let mut rows: Vec<String> = String::from_utf8_lossy(&output.stdout).lines().map(str::to_string).collect();
                rows.sort();
                rows
            }
            _ => Vec::new(),
        }
    }

    /// Stop `agent` through the CLI and return its persisted (field-shaped) row.
    fn stop_agent(&self, agent: &str) -> Value {
        let (code, value) = self.json(&["stop", agent, "--json"]);
        assert!(code == Some(0) && value["ok"] == true, "fixture: stop {agent} must succeed: {value}");
        let row = self.row(agent);
        assert!(
            row["status"] == "stopped" && row["explicit_stop"] == true && row.get("pane_id").is_none_or(Value::is_null),
            "fixture: stopped row must have the field shape (stopped, explicit_stop, no pane_id): {row}"
        );
        row
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
            .env("TMUX_TMPDIR", &self.tmux_tmpdir)
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
        if let Some(mut reaper) = self.reaper.take() {
            drop(reaper.stdin.take());
            let _ = reaper.wait();
        }
    }
}

fn plans_start_for(value: &Value, agent: &str) -> bool {
    value["ok"] == true
        && value["steps"].as_array().into_iter().flatten().any(|step| {
            step["agent"] == agent && step["action"].as_str().is_some_and(|a| a.starts_with("start"))
        })
}

fn require_uptime_beyond_long_ago() {
    let uptime = now_secs() as i64 - boot_secs();
    assert!(
        uptime > LONG_AGO_SECS + 120,
        "precondition: host uptime {uptime}s must exceed {}s so a same-boot 'long ago' spawn exists",
        LONG_AGO_SECS + 120
    );
}

// ─────────────────────────────── RED ───────────────────────────────

/// R1 — the exact field shape. A deliberately stopped, fenced seat whose retained
/// `pane_pid` now belongs to an unrelated live process born long after `spawned_at`
/// must be classified as stopped: start plans and executes it, restart plans it, stop
/// is an `already_stopped` no-op, and the unrelated process is never signalled.
#[test]
fn field_shape_stopped_fenced_seat_with_reused_pid_resumes_without_state_surgery() {
    require_uptime_beyond_long_ago();
    let team = Team::new("r1field");
    team.stop_agent("w1");
    let sentinel = Sentinel::spawn(&team.root);
    team.point_row_at("w1", &sentinel, sentinel.born_after - LONG_AGO_SECS as f64);

    let (_, dry) = team.json(&["start", "w1", "--dry-run", "--json"]);
    assert!(
        plans_start_for(&dry, "w1") && !identity_refusal(&dry),
        "start dry-run of a stopped seat whose PID was reused must plan a start: {dry}"
    );
    let (_, restart) = team.json(&["agent", "restart", "w1", "--dry-run", "--json"]);
    assert!(
        restart["ok"] == true && !identity_refusal(&restart),
        "restart dry-run must not refuse on a reused PID: {restart}"
    );
    let (_, stop) = team.json(&["stop", "w1", "--json"]);
    assert!(
        stop["ok"] == true && !identity_refusal(&stop) && stop["status"] == "already_stopped",
        "stop of a stopped seat whose PID was reused must be an already_stopped no-op: {stop}"
    );
    sentinel.assert_intact("start/restart dry-runs and stop");
    let row = team.row("w1");
    assert!(row["explicit_stop"] == true, "a no-op stop must keep the fence: {row}");

    let (code, started) = team.json(&["start", "w1", "--json"]);
    assert!(
        code == Some(0) && started["ok"] == true && !identity_refusal(&started),
        "start of a stopped seat whose PID was reused must execute: {started}"
    );
    team.wait_running(&["w1"]);
    let row = team.row("w1");
    assert!(
        row["pane_pid"].as_u64() != Some(u64::from(sentinel.root_pid)) && row["explicit_stop"] == false,
        "the started seat must record its own new carrier, not adopt the unrelated PID: {row}"
    );
    sentinel.assert_intact("real start");
}

/// R2 — reuse across a reboot: `spawned_at` predates the host boot, so the recorded PID
/// cannot be the carrier no matter when the live process was born.
#[test]
fn cross_boot_reused_pid_on_stopped_seat_is_stopped_not_unknown() {
    let team = Team::new("r2boot");
    team.stop_agent("w1");
    let sentinel = Sentinel::spawn(&team.root);
    team.point_row_at("w1", &sentinel, (boot_secs() - 3600) as f64);

    let (_, dry) = team.json(&["start", "w1", "--dry-run", "--json"]);
    assert!(
        plans_start_for(&dry, "w1") && !identity_refusal(&dry),
        "a PID recorded before the last boot is never the carrier; start must plan: {dry}"
    );
    let (_, stop) = team.json(&["stop", "w1", "--json"]);
    assert!(
        stop["ok"] == true && stop["status"] == "already_stopped" && !identity_refusal(&stop),
        "stop must be an already_stopped no-op: {stop}"
    );
    sentinel.assert_intact("cross-boot start dry-run and stop");
}

/// R3 — the same classification for every persisted provider family: legacy claude and
/// pi rows, and a codex row that takes the native-registry branch for rows without a
/// contract instance (seat_observation.rs:117-131 on c9e83c8f). Only the persisted row
/// is rewritten; stop never consults the role provider.
#[test]
fn reused_pid_on_stopped_seat_is_classified_for_legacy_and_native_provider_rows() {
    require_uptime_beyond_long_ago();
    let team = Team::new("r3prov");
    team.stop_agent("w1");
    let sentinel = Sentinel::spawn(&team.root);
    let mut failures = Vec::new();
    for provider in ["claude", "pi", "codex"] {
        team.point_row_at("w1", &sentinel, sentinel.born_after - LONG_AGO_SECS as f64);
        team.patch_row("w1", |row| {
            row.insert("provider".into(), Value::from(provider));
        });
        let (_, stop) = team.json(&["stop", "w1", "--json"]);
        if !(stop["ok"] == true && stop["status"] == "already_stopped" && !identity_refusal(&stop)) {
            failures.push(format!("{provider}: stop -> {stop}"));
        }
        let (_, dry) = team.json(&["start", "w1", "--dry-run", "--json"]);
        if identity_refusal(&dry) {
            failures.push(format!("{provider}: start --dry-run -> {dry}"));
        }
        sentinel.assert_intact(&format!("{provider} stop/start dry-run"));
    }
    team.patch_row("w1", |row| {
        row.insert("provider".into(), Value::from("fake"));
    });
    assert!(failures.is_empty(), "reused PID misclassified for:\n{}", failures.join("\n"));
}

/// R4 — the running-row variant (what a reboot leaves behind): the row still says
/// running, its tmux pane is gone, and the recorded PID was reused. That is a dead
/// carrier, not an unknown one: the seat must be recoverable and must not make the
/// whole `team start` refuse.
#[test]
fn running_row_whose_carrier_died_and_pid_was_reused_is_recoverable_and_does_not_block_team_start() {
    require_uptime_beyond_long_ago();
    let team = Team::new("r4run");
    let row = team.row("w1");
    let pane = row["pane_id"].as_str().expect("running w1 has a pane").to_string();
    let socket = team.team_socket().expect("team tmux socket");
    let killed = Command::new(&team.tmux).arg("-S").arg(&socket).args(["kill-pane", "-t", &pane]).output().unwrap();
    assert!(killed.status.success(), "fixture: kill w1 pane: {}", text(&killed));
    let sentinel = Sentinel::spawn(&team.root);
    team.point_row_at("w1", &sentinel, sentinel.born_after - LONG_AGO_SECS as f64);
    let row = team.row("w1");
    assert!(row["status"] == "running" && row["explicit_stop"] != true, "fixture: running unfenced row: {row}");

    let (_, dry) = team.json(&["start", "w1", "--dry-run", "--json"]);
    assert!(
        plans_start_for(&dry, "w1") && !identity_refusal(&dry),
        "a running row whose pane is gone and whose PID was reused must plan recovery: {dry}"
    );
    let (_, team_dry) = team.json(&["team", "start", "--dry-run", "--json"]);
    assert!(
        plans_start_for(&team_dry, "w1") && !identity_refusal(&team_dry),
        "one reused PID must not make `team start` refuse; it must plan w1: {team_dry}"
    );
    sentinel.assert_intact("dry-runs");
    let (code, started) = team.json(&["start", "w1", "--json"]);
    assert!(code == Some(0) && started["ok"] == true && !identity_refusal(&started), "start must recover w1: {started}");
    team.wait_running(&["w1"]);
    assert!(
        team.row("w1")["pane_pid"].as_u64() != Some(u64::from(sentinel.root_pid)),
        "recovered seat must record its own carrier"
    );
    sentinel.assert_intact("real start");
}

/// R5 (Bug 2) — whatever start refuses, the remedy it names must not refuse for the
/// same reason. Fixture: a live PID born inside the spawn window (it COULD be an
/// escaped carrier, so start must keep refusing — see G1); if start's refusal points at
/// `team-agent stop`, that stop must not answer "unknown physical identity".
#[test]
fn start_refusal_never_points_at_a_stop_that_refuses_for_the_same_reason() {
    let team = Team::new("r5loop");
    team.stop_agent("w1");
    let sentinel = Sentinel::spawn(&team.root);
    team.point_row_at("w1", &sentinel, sentinel.born_after - 0.5);

    let (_, dry) = team.json(&["start", "w1", "--dry-run", "--json"]);
    assert!(dry["ok"] == false, "precondition (G1): an in-window live PID must still be refused: {dry}");
    let remedy = dry.to_string();
    if remedy.contains("team-agent stop") {
        let (_, stop) = team.json(&["stop", "w1", "--json"]);
        assert!(
            !identity_refusal(&stop),
            "start names `team-agent stop` as the remedy, but stop refuses for the same identity reason \
             (circular deadlock):\nstart: {dry}\nstop: {stop}"
        );
    }
    sentinel.assert_intact("start/stop");
}

/// R6 (adjacent H1) — team shutdown must not use a stopped row's stale `pane_pid` as a
/// kill root. The sentinel's child carries `--workspace <root>` exactly like the field
/// holder's `team-agent mcp-server`; it is reachable ONLY through the stale root.
#[test]
fn team_stop_never_signals_a_process_tree_that_reused_a_stopped_rows_pid() {
    require_uptime_beyond_long_ago();
    let team = Team::new("r6shut");
    team.stop_agent("w1");
    let sentinel = Sentinel::spawn(&team.root);
    team.point_row_at("w1", &sentinel, sentinel.born_after - LONG_AGO_SECS as f64);
    let (code, value) = team.json(&["stop", "--json"]);
    std::thread::sleep(Duration::from_millis(500));
    sentinel.assert_intact(&format!("team stop (shutdown reaper; receipt: {value})"));
    assert!(code == Some(0) && value["ok"] == true, "team stop must succeed: {value}");
}

// ────────────────────────────── GUARD ──────────────────────────────

/// G1 — do not over-relax: a live PID born inside the spawn window of a stopped row
/// could be an escaped carrier. Start must not adopt it or launch a second carrier,
/// and no lifecycle verb may signal it.
#[test]
fn guard_pid_born_inside_the_spawn_window_is_never_adopted_restarted_or_signalled() {
    let team = Team::new("g1win");
    team.stop_agent("w1");
    let sentinel = Sentinel::spawn(&team.root);
    team.point_row_at("w1", &sentinel, sentinel.born_after - 0.5);
    let panes = team.panes();

    let (_, started) = team.json(&["start", "w1", "--json"]);
    assert!(started["ok"] == false, "an in-window live PID must keep start refused: {started}");
    let (_, restarted) = team.json(&["restart", "w1", "--json"]);
    assert!(restarted["ok"] == false, "an in-window live PID must keep restart refused: {restarted}");
    let _ = team.json(&["stop", "w1", "--json"]);
    let (_, again) = team.json(&["start", "w1", "--json"]);
    assert!(again["ok"] == false, "stop must not launder an unresolved PID into a startable seat: {again}");
    assert_eq!(team.panes(), panes, "no carrier may be launched for an unresolved seat");
    assert!(team.row("w1")["explicit_stop"] == true, "the fence must survive: {}", team.row("w1"));
    sentinel.assert_intact("start/restart/stop/start of an in-window PID");
}

/// G2 — a genuinely live seat whose pane and carrier match is never replaced, restarted
/// by `start`, or signalled.
#[test]
fn guard_live_matched_seat_is_never_replaced_or_signalled() {
    let team = Team::new("g2live");
    let row = team.row("w2");
    let pid = row["pane_pid"].as_u64().expect("running w2 pane_pid") as u32;
    let birth = lstart(pid).expect("w2 carrier alive");
    let panes = team.panes();
    for args in [&["start", "w2", "--json"][..], &["start", "w2", "--dry-run", "--json"], &["team", "start", "--json"]] {
        let (code, value) = team.json(args);
        let noop = code == Some(0) && value["ok"] == true;
        let refused = value["ok"] == false && identity_refusal(&value);
        assert!(noop || refused, "{args:?}: a live seat must be a no-op (or an identity refusal): {value}");
        assert_eq!(team.panes(), panes, "{args:?}: live panes changed");
        assert_eq!(lstart(pid).as_deref(), Some(birth.as_str()), "{args:?}: live carrier was replaced or signalled");
    }
}

// ─────────────────────── schema: pane_birth_identity ───────────────────────

/// R7 — every carrier the binary spawns records `pane_birth_identity` beside
/// `pane_pid`, byte-identical to `process_birth_identity(pane_pid)`.
#[test]
fn new_spawns_persist_pane_birth_identity_matching_their_carrier() {
    let team = Team::new("r7schema");
    let state = team.state();
    let mut problems = Vec::new();
    for agent in ["w1", "w2"] {
        let mut rows = Vec::new();
        if let Some(row) = state.pointer(&format!("/teams/{}/agents/{agent}", team.name)) {
            rows.push(("teams", row));
        }
        if state.get("team_key").and_then(Value::as_str) == Some(team.name.as_str()) {
            if let Some(row) = state.pointer(&format!("/agents/{agent}")) {
                rows.push(("root", row));
            }
        }
        assert!(!rows.is_empty(), "no persisted row for {agent}");
        for (projection, row) in rows {
            let pid = row["pane_pid"].as_u64().map(|p| p as u32);
            let expected = pid.and_then(birth_token);
            let recorded = row["pane_birth_identity"].as_str();
            if expected.is_none() || recorded != expected.as_deref() {
                problems.push(format!(
                    "{projection}.{agent}: pane_pid={pid:?} pane_birth_identity={recorded:?} expected={expected:?}"
                ));
            }
        }
    }
    assert!(problems.is_empty(), "pane_birth_identity must be persisted with pane_pid:\n{}", problems.join("\n"));
}

/// R8 — an exact recorded identity beats the heuristic: the live PID was born inside
/// the `spawned_at` window, but `pane_birth_identity` names a different birth, so the
/// PID was provably reused and the stopped seat is startable.
#[test]
fn recorded_birth_identity_mismatch_overrides_an_in_window_spawned_at() {
    let team = Team::new("r8exact");
    team.stop_agent("w1");
    let sentinel = Sentinel::spawn(&team.root);
    team.point_row_at("w1", &sentinel, sentinel.born_after - 0.5);
    let token = birth_token(sentinel.root_pid).expect("sentinel birth identity");
    team.set_birth_identity("w1", &foreign_token(&token));

    let (_, dry) = team.json(&["start", "w1", "--dry-run", "--json"]);
    assert!(
        plans_start_for(&dry, "w1") && !identity_refusal(&dry),
        "a recorded birth identity that differs from the live PID's proves reuse; start must plan: {dry}"
    );
    let (_, stop) = team.json(&["stop", "w1", "--json"]);
    assert!(
        stop["ok"] == true && stop["status"] == "already_stopped" && !identity_refusal(&stop),
        "stop must be an already_stopped no-op: {stop}"
    );
    sentinel.assert_intact("exact-mismatch start dry-run and stop");
}

/// G3 — an exact recorded identity that MATCHES the live PID wins over the heuristic in
/// the other direction too: even with `spawned_at` long ago, the stopped row's PID may
/// be its own escaped carrier, so start never adopts it or launches a second carrier, and
/// the agent verbs never signal it. (Team shutdown reaping a PROVEN carrier is out of scope.)
#[test]
fn guard_recorded_birth_identity_match_on_a_stopped_row_is_never_adopted_or_signalled() {
    let team = Team::new("g3exact");
    team.stop_agent("w1");
    let sentinel = Sentinel::spawn(&team.root);
    team.point_row_at("w1", &sentinel, sentinel.born_after - LONG_AGO_SECS as f64);
    let token = birth_token(sentinel.root_pid).expect("sentinel birth identity");
    team.set_birth_identity("w1", &token);
    let panes = team.panes();

    let (_, started) = team.json(&["start", "w1", "--json"]);
    assert!(started["ok"] == false, "a stopped row whose recorded identity matches a live PID must stay refused: {started}");
    let _ = team.json(&["stop", "w1", "--json"]);
    let (_, again) = team.json(&["start", "w1", "--json"]);
    assert!(again["ok"] == false, "stop must not launder a matched live carrier into a startable seat: {again}");
    assert_eq!(team.panes(), panes, "no second carrier may be launched");
    sentinel.assert_intact("start/stop/start with an exact identity match");
}
