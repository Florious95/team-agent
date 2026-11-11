//! Clean `stop --team` must persist its own outcome — frozen RED (repro-opus).
//!
//! Found during the c8639440 live gate (LC-4) and reproduced model-free: with any live
//! legacy seat, `team-agent stop --team T` kills the panes and the tmux server, then the
//! final save is refused with
//!   "state save conflict: agent_id=<seat> projection=agents
//!    conflicting_fields=pane_id,pane_pid,pane_birth_identity,window"
//! leaving the row `running` and the team without `status: shutdown` (a retry returns
//! ok:true without repairing the rows). c9e83c8f persists `stopped` + `shutdown`.
//! The frozen PID-reuse R6 cannot see this: fake seats under `display_backend: none`
//! never reach the clean-shutdown branch on either binary.
//!
//!   T1 clean_team_stop_of_a_live_seat_persists_stopped_rows_and_shutdown_status
//!      (green on c9e83c8f, red on c8639440: regression guard)
//!   T2 clean_team_stop_spares_a_reused_pid_tree_and_still_persists_shutdown
//!      (red on c9e83c8f via H1, red on c8639440 via the save conflict)
//!
//! Fixture: the frozen PID-reuse fixture's lifetime machinery (own TMUX_TMPDIR, stdin-
//! lifeline reaper in its own process group, bounded CLI calls, sentinel with alarm),
//! with seats direct-started as `claude` whose binary is a long-lived PATH stub (argv0
//! `claude`, 110 s). The
//! assertions prescribe outcomes only (ok, row status, team status, sentinel intact),
//! not whether stopped rows keep or clear their dead pane fields.
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
/// Long-lived stand-in for the native `claude` command: argv0 is `claude` (lifecycle
/// observation matches the provider command), hard 110 s lifetime via `sleep`.
const STUB_CLAUDE: &str = r#"#!/bin/sh
if [ "$1" = "--version" ]; then echo '2.1.322 (Claude Code)'; exit 0; fi
exec /usr/bin/perl -e 'exec {"/bin/sleep"} "claude", "110"'
"#;

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
        "ta-ts-{label}-{}-{}",
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
/// unique `--workspace <root>` marker (word-bounded); three passes catch a server spawned
/// mid-teardown. `ps`+`awk` instead of `pkill`: on hosts where `pkill` cannot reach
/// sysmond (seen over ssh on macOS: "Cannot get process list") it silently kills
/// nothing. The marker travels in the environment so no matcher matches itself.
const REAPER: &str = r#"trap '' INT HUP TERM; cat >/dev/null
M="--workspace $1"; export M
for pass in 1 2 3; do
  for p in $(/bin/ps -axww -o pid=,command= | /usr/bin/awk 'index($0 " ", ENVIRON["M"] " ") {print $1}'); do kill -KILL "$p" 2>/dev/null; done
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
    /// Same lifetime machinery as the frozen PID-reuse fixture, but the seats are
    /// direct-started `claude` seats whose `claude` is a long-lived stub. Only then does
    /// `stop --team` reach the clean-shutdown branch: fake seats under
    /// `display_backend: none` never do (verified on c9e83c8f and c8639440).
    fn new(label: &str, seats: &[&str]) -> Self {
        let serial = LIVE.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let root = scratch_root(label);
        let home = root.join("home");
        let tmux_tmpdir = root.join("tt");
        let name = format!("ts{}", label.chars().filter(char::is_ascii_alphanumeric).take(8).collect::<String>());
        let stub = root.join("stub");
        std::fs::create_dir_all(&stub).unwrap();
        for provider in ["pi", "codex"] {
            std::fs::write(
                stub.join(provider),
                format!("#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then echo 0.70.0; exit 0; fi\necho \"team-stop stub {provider} must not launch: $*\" >&2\nexit 3\n"),
            )
            .unwrap();
        }
        std::fs::write(stub.join("claude"), STUB_CLAUDE).unwrap();
        for provider in ["pi", "codex", "claude"] {
            std::fs::set_permissions(stub.join(provider), std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        }
        let tmux = tmux_binary();
        let keep = if std::env::var_os("TEAM_AGENT_KEEP_TEST_TMP").is_some() { "keep" } else { "" };
        let reaper = Command::new("/bin/sh")
            .arg("-c")
            .arg(REAPER)
            .arg("teamstop-reaper")
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
        for seat in seats {
            let (code, value) = team.json(&[
                "start", seat, "--provider", "claude", "--model", "claude-haiku-5-5", "--team", &team.name, "--json",
            ]);
            assert!(code == Some(0) && value["ok"] == true, "fixture: direct start {seat} must succeed: {value}");
        }
        team.wait_running(seats);
        let live = team.panes();
        assert!(live.len() >= seats.len(), "fixture: every stub seat needs a live pane: {live:?}");
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

fn assert_clean_shutdown_persisted(team: &Team, seats: &[&str], value: &Value) {
    let state = team.state();
    for seat in seats {
        let row = team.row(seat);
        assert!(row["status"] == "stopped", "teams.{}.agents.{seat} must record the stop (receipt: {value}): {row}", team.name);
        if state.get("team_key").and_then(Value::as_str) == Some(team.name.as_str()) {
            let root = &state["agents"][*seat];
            assert!(root.is_null() || root["status"] == "stopped", "root agents.{seat} must record the stop (receipt: {value}): {root}");
        }
    }
    let team_status = state.pointer(&format!("/teams/{}/status", team.name)).cloned().unwrap_or(Value::Null);
    assert!(team_status == "shutdown", "teams.{}.status must be shutdown after a clean stop (receipt: {value}): {team_status}", team.name);
}

/// T1 — a clean team stop of a live seat succeeds and persists stopped rows plus the
/// team's shutdown status; a follow-up start is not blocked by what shutdown left.
#[test]
fn clean_team_stop_of_a_live_seat_persists_stopped_rows_and_shutdown_status() {
    let team = Team::new("t1clean", &["s1"]);
    let (code, value) = team.json(&["stop", "--json"]);
    assert!(code == Some(0) && value["ok"] == true, "team stop of a live seat must succeed: {value}");
    assert!(team.panes().is_empty(), "team stop must remove the team's panes: {:?}", team.panes());
    assert_clean_shutdown_persisted(&team, &["s1"], &value);
    let (_, planned) = team.json(&["start", "s1", "--dry-run", "--json"]);
    assert!(planned["ok"] == true && !identity_refusal(&planned), "start after a clean team stop must plan: {planned}");
}

/// T2 — the same clean stop with an explicitly stopped row whose recorded pane_pid now
/// names an unrelated live tree: the tree is never signalled AND the stop still
/// succeeds and persists (H1 safety must not be bought with a failed teardown).
#[test]
fn clean_team_stop_spares_a_reused_pid_tree_and_still_persists_shutdown() {
    require_uptime_beyond_long_ago();
    let team = Team::new("t2reuse", &["s1", "s2"]);
    team.stop_agent("s2");
    let sentinel = Sentinel::spawn(&team.root);
    team.point_row_at("s2", &sentinel, sentinel.born_after - LONG_AGO_SECS as f64);
    let (code, value) = team.json(&["stop", "--json"]);
    std::thread::sleep(Duration::from_millis(500));
    sentinel.assert_intact(&format!("team stop (receipt: {value})"));
    assert!(code == Some(0) && value["ok"] == true, "team stop must succeed: {value}");
    assert!(team.panes().is_empty(), "team stop must remove the team's panes: {:?}", team.panes());
    assert_clean_shutdown_persisted(&team, &["s1", "s2"], &value);
    assert!(team.row("s2")["explicit_stop"] == true, "the explicit fence must survive team stop: {}", team.row("s2"));
}
