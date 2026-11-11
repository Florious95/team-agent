//! A13 independent RED suite: unified `start` converges a crash orphan of a stopped seat.
//!
//! Authority: ADJUDICATION-BLUEPRINT-819331fd §12, R-M2R4 -> A13 (C1-C10), its C4/dry-run
//! clarification, and the T5 tightening (a plain rerun after a team stop and a crash must
//! converge). Author: repro-opus, the independent tester. The implementer imports this file
//! unmodified and does not read it.
//!
//! FROZEN. Do not edit assertions, budgets, fixtures or names. Turn red to green in production.
//!
//! Cases:
//! - t2 (guard): a configuration-changing rerun over a crash orphan keeps today's refusal. The
//!   orphan and its carrier are untouched, and the role file and state bytes are unchanged.
//! - t3 (guard): the orphan's carrier PID is a protected root (another seat records it with a
//!   different birth identity, i.e. Reused). The rerun is refused and nothing is signalled.
//! - t4 (guard): the stopped row records the live orphan carrier with its exact birth identity
//!   (Possible). The rerun is refused and nothing is signalled.
//! - t5 (RED on 0ec7b2b5): team stop (the row keeps a dead carrier tuple), crash mid-spawn,
//!   plain rerun. Exactly one live pane for the seat with a fresh carrier; never two.
//! - t6 (RED on 0ec7b2b5): agent stop, crash mid-spawn. Dry-run predicts the replacement
//!   (ok, and names the orphan's session/window/pane_id anywhere in its receipt) with no effect. The real rerun then
//!   converges to one live pane, never adopts the orphan, and names the retired orphan.
//! - t7 (guard): rerunning start on a live, verified seat stays `already_running` and touches
//!   nothing.
//!
//! Lifetimes: every CLI runs in an owned process group with a <=120 s stdin lifeline
//! (`FixtureGroup`). tmux servers live under the hermetic private `TMUX_TMPDIR` root, which
//! has its own reaper. No host-wide kill and no pkill. Only test-owned resources are touched.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![cfg(unix)]

#[path = "support/hermetic.rs"]
mod hermetic;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use hermetic::proc_guard::FixtureGroup;
use hermetic::{fixture_tmux_binary, HermeticTestEnv};
use serde_json::Value;

/// Hard ceiling for any single CLI call.
const CLI_CEILING: Duration = Duration::from_secs(60);
/// Lifeline TTL for every owned CLI process group (R1: <= 120 s).
const LIFELINE: Duration = Duration::from_secs(120);
/// How long the tmux shim stalls the spawn command that the crashed CLI issues.
const STALL_SECS: u64 = 3;

struct Run {
    json: Value,
    raw: String,
    timed_out: bool,
}

impl Run {
    fn ok(&self) -> bool {
        !self.timed_out && self.json.get("ok").and_then(Value::as_bool) == Some(true)
    }
    fn describe(&self) -> String {
        format!(
            "timed_out={} output={}",
            self.timed_out,
            self.raw.chars().take(2400).collect::<String>()
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Pane {
    socket: PathBuf,
    session: String,
    window: String,
    pane_id: String,
    pid: u32,
}

/// One hermetic workspace with fake-provider teams on a private tmux root.
struct Bay {
    env: HermeticTestEnv,
    ws: PathBuf,
    shim_dir: PathBuf,
    tmux_root: PathBuf,
    seq: std::cell::Cell<u32>,
}

impl Bay {
    fn new(tag: &str) -> Self {
        let env = HermeticTestEnv::for_subprocess_with_leader(&format!("a13-{tag}"));
        let ws = env.workspace(tag);
        let shim_dir = env.root().join("shim");
        fs::create_dir_all(&shim_dir).unwrap();
        let shim = shim_dir.join("tmux");
        fs::write(
            &shim,
            format!(
                r#"#!/bin/sh
W='{ws}'
case " $* " in
  *" new-window "*|*" split-window "*|*" respawn-pane "*|*" respawn-window "*|*" new-session "*)
    if [ -f "$W/.a13-slow-spawn" ]; then echo $$ > "$W/.a13-entered"; sleep {stall}; fi;;
esac
exec '{tmux}' "$@"
"#,
                ws = ws.display(),
                stall = STALL_SECS,
                tmux = fixture_tmux_binary().display()
            ),
        )
        .unwrap();
        let mut permissions = fs::metadata(&shim).unwrap().permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, 0o755);
        fs::set_permissions(&shim, permissions).unwrap();
        let mut probe = Command::new("/bin/sh");
        probe.args(["-c", "printf %s \"$TMUX_TMPDIR\""]);
        env.configure_command(&mut probe);
        let tmux_root = PathBuf::from(String::from_utf8(probe.output().unwrap().stdout).unwrap());
        assert!(tmux_root.is_dir(), "FIXTURE: private TMUX_TMPDIR root: {}", tmux_root.display());
        Self { env, ws, shim_dir, tmux_root, seq: std::cell::Cell::new(0) }
    }

    /// Spawn `team-agent <args> --workspace <ws> --json` in an owned, lifelined process group.
    fn spawn(&self, args: &[&str], shim: bool) -> (FixtureGroup, PathBuf, PathBuf) {
        let n = self.seq.get();
        self.seq.set(n + 1);
        let out = self.env.root().join(format!("cli-{n}.out"));
        let err = self.env.root().join(format!("cli-{n}.err"));
        let ws = self.ws.to_string_lossy().to_string();
        let mut full: Vec<&str> = args.to_vec();
        full.extend(["--workspace", ws.as_str(), "--json"]);
        let mut command = Command::new(env!("CARGO_BIN_EXE_team-agent"));
        command.args(&full).current_dir(&self.ws);
        self.env.configure_command(&mut command);
        if shim {
            let path = std::env::var("PATH").unwrap_or_default();
            command.env("PATH", format!("{}:{path}", self.shim_dir.display()));
        }
        command
            .stdout(Stdio::from(fs::File::create(&out).unwrap()))
            .stderr(Stdio::from(fs::File::create(&err).unwrap()));
        (FixtureGroup::spawn(&mut command, &self.ws, LIFELINE), out, err)
    }

    fn run(&self, args: &[&str]) -> Run {
        let (mut child, out, err) = self.spawn(args, false);
        let started = Instant::now();
        let mut timed_out = false;
        loop {
            if child.try_wait().unwrap().is_some() {
                break;
            }
            if started.elapsed() > CLI_CEILING {
                timed_out = true;
                let _ = child.kill();
                let _ = child.wait();
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        drop(child);
        let stdout = fs::read(&out).unwrap_or_default();
        let stderr = fs::read(&err).unwrap_or_default();
        let raw = format!("{}{}", String::from_utf8_lossy(&stdout), String::from_utf8_lossy(&stderr));
        let json = serde_json::from_slice(&stdout).unwrap_or(Value::Null);
        Run { json, raw, timed_out }
    }

    fn role_path(&self, team: &str, seat: &str) -> PathBuf {
        self.ws.join(format!("{team}-team/agents/{seat}.md"))
    }

    /// Create and start a fake-provider team through the canonical `start --from`.
    fn team(&self, team: &str, seats: &[&str]) {
        let dir = self.ws.join(format!("{team}-team"));
        fs::create_dir_all(dir.join("agents")).unwrap();
        fs::write(
            dir.join("TEAM.md"),
            format!("---\nname: {team}\nobjective: A13 orphan convergence.\nprovider: fake\n---\n\nTeam.\n"),
        )
        .unwrap();
        for seat in seats {
            fs::write(
                self.role_path(team, seat),
                format!("---\nname: {seat}\nrole: {seat}\nprovider: fake\ntools:\n  - mcp_team\n---\n\n{seat}.\n"),
            )
            .unwrap();
        }
        let dir_s = dir.to_string_lossy().to_string();
        let run = self.run(&["start", "--from", &dir_s, "--team", team]);
        assert!(run.ok(), "FIXTURE: start --from of `{team}` must succeed: {}", run.describe());
        let status = self.status(team);
        for seat in seats {
            assert_eq!(status.get(*seat).map(String::as_str), Some("running"), "FIXTURE: {seat} not running: {status:?}");
            assert_eq!(self.seat_panes(seat).len(), 1, "FIXTURE: {seat} must own one live pane: {:?}", self.panes());
        }
    }

    /// seat -> runtime_status, from the public status brief.
    fn status(&self, team: &str) -> std::collections::BTreeMap<String, String> {
        let run = self.run(&["status", "--team", team]);
        let nodes = run.json.get("nodes").and_then(Value::as_array).unwrap_or_else(|| {
            panic!("FIXTURE: `status --team {team}` must return nodes: {}", run.describe())
        });
        nodes
            .iter()
            .map(|node| {
                (
                    node["name"].as_str().unwrap_or("?").to_string(),
                    node["runtime_status"].as_str().unwrap_or("?").to_string(),
                )
            })
            .collect()
    }

    /// Live panes on every tmux server under this bay's private root (the fixture caller's
    /// socket sits at the root itself and is never listed).
    fn panes(&self) -> Vec<Pane> {
        let tmux = fixture_tmux_binary();
        let mut panes = Vec::new();
        let Ok(dirs) = fs::read_dir(&self.tmux_root) else { return panes };
        for dir in dirs.flatten().map(|entry| entry.path()).filter(|path| path.is_dir()) {
            let Ok(sockets) = fs::read_dir(&dir) else { continue };
            for socket in sockets.flatten().map(|entry| entry.path()) {
                let Ok(output) = Command::new(&tmux)
                    .arg("-S")
                    .arg(&socket)
                    .args(["list-panes", "-a", "-F", "#{pane_dead}\t#{session_name}\t#{window_name}\t#{pane_id}\t#{pane_pid}"])
                    .output()
                else {
                    continue;
                };
                for line in String::from_utf8_lossy(&output.stdout).lines() {
                    let fields: Vec<&str> = line.split('\t').collect();
                    if fields.len() == 5 && fields[0] == "0" {
                        if let Ok(pid) = fields[4].parse() {
                            panes.push(Pane {
                                socket: socket.clone(),
                                session: fields[1].to_string(),
                                window: fields[2].to_string(),
                                pane_id: fields[3].to_string(),
                                pid,
                            });
                        }
                    }
                }
            }
        }
        panes
    }

    fn seat_panes(&self, seat: &str) -> Vec<Pane> {
        self.panes().into_iter().filter(|pane| pane.window == seat).collect()
    }

    fn state_path(&self) -> PathBuf {
        self.ws.join(".team/runtime/state.json")
    }

    fn state_bytes(&self) -> Vec<u8> {
        fs::read(self.state_path()).unwrap()
    }

    fn row(&self, team: &str, seat: &str) -> Value {
        let state: Value = serde_json::from_slice(&self.state_bytes()).unwrap();
        state.pointer(&format!("/teams/{team}/agents/{seat}")).cloned().unwrap_or(Value::Null)
    }

    /// Test-owned fixture edit of one seat row (team projection and its root mirror), under the
    /// runtime state-save lock.
    fn edit_row(&self, team: &str, seat: &str, edit: impl Fn(&mut serde_json::Map<String, Value>)) {
        let runtime = self.ws.join(".team/runtime");
        let lock = fs::OpenOptions::new().read(true).write(true).open(runtime.join("state-save.lock")).expect("FIXTURE: state-save lock");
        let deadline = Instant::now() + Duration::from_secs(5);
        while !team_agent::platform::file_lock::try_lock_once_nonblocking(&lock).expect("FIXTURE: lock") {
            assert!(Instant::now() < deadline, "FIXTURE: state-save lock deadline");
            std::thread::sleep(Duration::from_millis(10));
        }
        let mut state: Value = serde_json::from_slice(&self.state_bytes()).unwrap();
        let root_is_team = state.get("team_key").and_then(Value::as_str) == Some(team)
            || state.get("active_team_key").and_then(Value::as_str) == Some(team);
        let mut edited = 0;
        if let Some(row) = state.pointer_mut(&format!("/teams/{team}/agents/{seat}")).and_then(Value::as_object_mut) {
            edit(row);
            edited += 1;
        }
        if root_is_team {
            if let Some(row) = state.pointer_mut(&format!("/agents/{seat}")).and_then(Value::as_object_mut) {
                edit(row);
                edited += 1;
            }
        }
        assert!(edited > 0, "FIXTURE: no row {team}/{seat} to edit");
        fs::write(self.state_path(), serde_json::to_vec_pretty(&state).unwrap()).unwrap();
        team_agent::platform::file_lock::unlock(&lock).expect("FIXTURE: unlock");
    }

    fn wait_until(&self, what: &str, limit: Duration, mut condition: impl FnMut() -> bool) {
        let deadline = Instant::now() + limit;
        while !condition() {
            assert!(Instant::now() < deadline, "FIXTURE: timed out waiting for {what}; panes={:?}", self.panes());
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// Start `seat`, stall its spawn in the tmux shim, SIGKILL only the CLI, and return the
    /// orphaned pane that lands afterwards (never recorded in state).
    fn crash_mid_spawn(&self, team: &str, seat: &str) -> Pane {
        let before = self.panes();
        let entered = self.ws.join(".a13-entered");
        let flag = self.ws.join(".a13-slow-spawn");
        let _ = fs::remove_file(&entered);
        fs::write(&flag, "1").unwrap();
        let (mut crashed, _, _) = self.spawn(&["start", seat, "--team", team], true);
        self.wait_until("the tmux shim to stall the spawn", Duration::from_secs(30), || {
            fs::read_to_string(&entered).map(|text| !text.trim().is_empty()).unwrap_or(false)
        });
        crashed.kill().unwrap(); // SIGKILL the CLI only; the stalled tmux command still lands.
        let _ = crashed.wait();
        fs::remove_file(&flag).unwrap();
        let mut orphan = None;
        self.wait_until("the orphaned spawn to land", Duration::from_secs(30), || {
            orphan = self.panes().into_iter().find(|pane| pane.window == seat && !before.contains(pane));
            orphan.is_some()
        });
        drop(crashed);
        let orphan = orphan.unwrap();
        assert!(alive(orphan.pid), "FIXTURE: orphan carrier must be live: {orphan:?}");
        assert_eq!(self.seat_panes(seat).len(), 1, "FIXTURE: exactly the orphan lives in {seat}: {:?}", self.panes());
        orphan
    }
}

impl Drop for Bay {
    fn drop(&mut self) {
        let _ = fs::remove_file(self.ws.join(".a13-slow-spawn"));
        // Exact owned servers only: every socket under this bay's private tmux root.
        let tmux = fixture_tmux_binary();
        if let Ok(dirs) = fs::read_dir(&self.tmux_root) {
            for dir in dirs.flatten().map(|entry| entry.path()).filter(|path| path.is_dir()) {
                for socket in fs::read_dir(&dir).into_iter().flatten().flatten().map(|entry| entry.path()) {
                    let _ = Command::new(&tmux).arg("-S").arg(&socket).arg("kill-server").output();
                }
            }
        }
        // A coordinator is killed only when its live command line names this workspace.
        if let Some(pid) = fs::read_to_string(self.ws.join(".team/runtime/coordinator.pid"))
            .ok()
            .and_then(|text| text.trim().parse::<u32>().ok())
            .filter(|pid| *pid > 1)
        {
            let owned = Command::new("/bin/ps")
                .args(["-ww", "-o", "command=", "-p", &pid.to_string()])
                .output()
                .map(|output| String::from_utf8_lossy(&output.stdout).contains(&*self.ws.to_string_lossy()))
                .unwrap_or(false);
            if owned {
                let _ = nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid as i32), nix::sys::signal::Signal::SIGKILL);
            }
        }
    }
}

fn alive(pid: u32) -> bool {
    match nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid as i32), None) {
        Ok(()) => true,
        Err(nix::errno::Errno::EPERM) => true,
        Err(_) => false,
    }
}

fn gone_within(pid: u32, limit: Duration) -> bool {
    let deadline = Instant::now() + limit;
    while alive(pid) {
        if Instant::now() > deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    true
}

fn birth(pid: u32) -> String {
    team_agent_contract::host::process::process_birth_identity(pid).expect("FIXTURE: readable process birth")
}

/// A well-formed birth token that differs from the real one (observes as Reused).
fn other_birth(pid: u32) -> String {
    let real = birth(pid);
    match real.rsplit_once(':') {
        Some((head, tail)) if tail.parse::<u64>().is_ok() => format!("{head}:{}", tail.parse::<u64>().unwrap() + 1),
        _ => format!("{real}0"),
    }
}

/// Any JSON object that names this pane: session (or session_name), window (or window_name)
/// and pane_id all match.
fn names_pane(value: &Value, pane: &Pane) -> bool {
    match value {
        Value::Object(map) => {
            let field = |keys: &[&str]| keys.iter().find_map(|key| map.get(*key).and_then(Value::as_str));
            (field(&["pane_id"]) == Some(pane.pane_id.as_str())
                && field(&["session", "session_name"]) == Some(pane.session.as_str())
                && field(&["window", "window_name"]) == Some(pane.window.as_str()))
                || map.values().any(|item| names_pane(item, pane))
        }
        Value::Array(items) => items.iter().any(|item| names_pane(item, pane)),
        _ => false,
    }
}

fn events_name_pane(ws: &Path, pane: &Pane) -> bool {
    fs::read_to_string(ws.join(".team/logs/events.jsonl"))
        .unwrap_or_default()
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .any(|event| names_pane(&event, pane))
}

/// The seat converged after a crash: one live pane, a fresh carrier recorded with its exact
/// birth, the orphan retired and never adopted.
fn assert_converged(bay: &Bay, team: &str, seat: &str, orphan: &Pane, label: &str) {
    let panes = bay.seat_panes(seat);
    assert!(panes.len() <= 1, "{label}: NEVER two live panes for one seat (C8): {:?}", bay.panes());
    assert_eq!(panes.len(), 1, "{label}: the seat must own exactly one live pane after the rerun: {:?}", bay.panes());
    let pane = &panes[0];
    assert_ne!(pane.pid, orphan.pid, "{label}: the orphan must be replaced, not adopted (C9): {pane:?}");
    assert!(gone_within(orphan.pid, Duration::from_secs(5)), "{label}: the retired orphan carrier must be gone: {orphan:?}");
    assert_eq!(bay.status(team).get(seat).map(String::as_str), Some("running"), "{label}: status must say running");
    let row = bay.row(team, seat);
    assert_eq!(row["pane_id"].as_str(), Some(pane.pane_id.as_str()), "{label}: row must record the new pane: {row}");
    assert_eq!(row["pane_pid"].as_u64(), Some(u64::from(pane.pid)), "{label}: row must record the new carrier: {row}");
    assert_eq!(row["pane_birth_identity"].as_str(), Some(birth(pane.pid).as_str()), "{label}: row must record the new carrier's exact birth: {row}");
}

// ───────────────────────────── T2 ─────────────────────────────

/// C4: a rerun that changes configuration over a live crash orphan keeps today's duplicate
/// refusal byte-for-byte in substance. No kill, no spawn, no role or state write.
#[test]
fn t2_configuration_changing_rerun_over_a_crash_orphan_keeps_the_refusal_and_touches_nothing() {
    let bay = Bay::new("t2");
    bay.team("a13t2", &["wa", "wb"]);
    assert!(bay.run(&["stop", "wa", "--team", "a13t2"]).ok(), "FIXTURE: stop wa");
    let orphan = bay.crash_mid_spawn("a13t2", "wa");
    let role = fs::read(bay.role_path("a13t2", "wa")).unwrap();
    let state = bay.state_bytes();
    let panes = bay.panes();

    let rerun = bay.run(&["start", "wa", "--team", "a13t2", "--model", "fake"]);
    assert!(!rerun.ok(), "T2: a configuration-changing rerun over a live orphan must be refused: {}", rerun.describe());
    assert!(rerun.raw.contains("already running (duplicate / active cohort)"), "T2: today's duplicate refusal must be kept: {}", rerun.describe());
    assert!(rerun.raw.contains(&orphan.pane_id), "T2: the refusal's cohort proof must name the orphan pane {}: {}", orphan.pane_id, rerun.describe());
    assert!(alive(orphan.pid), "T2: the orphan carrier must not be signalled");
    assert_eq!(bay.panes(), panes, "T2: no pane may be killed or spawned");
    assert_eq!(fs::read(bay.role_path("a13t2", "wa")).unwrap(), role, "T2: a refused start must not rewrite the role file");
    assert_eq!(bay.state_bytes(), state, "T2: a refused start must not write runtime state");
}

// ───────────────────────────── T3 ─────────────────────────────

/// C6: the orphan's carrier is a protected root (another seat records that PID with a different
/// birth identity, so it observes Reused). The rerun is refused and nothing is signalled.
#[test]
fn t3_orphan_whose_carrier_is_a_protected_root_is_refused_and_never_signalled() {
    let bay = Bay::new("t3");
    bay.team("a13t3", &["wa", "wb"]);
    assert!(bay.run(&["stop", "wa", "--team", "a13t3"]).ok(), "FIXTURE: stop wa");
    let orphan = bay.crash_mid_spawn("a13t3", "wa");
    let mismatch = other_birth(orphan.pid);
    bay.edit_row("a13t3", "wb", |row| {
        row.insert("pane_pid".into(), Value::from(orphan.pid));
        row.insert("pane_birth_identity".into(), Value::from(mismatch.clone()));
    });
    let panes = bay.panes();

    let rerun = bay.run(&["start", "wa", "--team", "a13t3"]);
    assert!(!rerun.ok(), "T3: an orphan with a protected carrier must be refused: {}", rerun.describe());
    assert!(rerun.raw.contains("protected carrier"), "T3: the refusal must come from the carrier protection fence: {}", rerun.describe());
    assert!(alive(orphan.pid), "T3: a protected carrier must never be signalled");
    assert_eq!(bay.panes(), panes, "T3: no pane may be killed or spawned");
}

// ───────────────────────────── T4 ─────────────────────────────

/// C5 (49e915d2 guards): the stopped row records the live orphan carrier with its exact birth
/// identity, so the carrier observes Possible. The rerun is refused and nothing is signalled.
#[test]
fn t4_stopped_row_whose_recorded_carrier_observes_possible_is_refused_and_never_signalled() {
    let bay = Bay::new("t4");
    bay.team("a13t4", &["wa", "wb"]);
    assert!(bay.run(&["stop", "wa", "--team", "a13t4"]).ok(), "FIXTURE: stop wa");
    let orphan = bay.crash_mid_spawn("a13t4", "wa");
    let exact = birth(orphan.pid);
    bay.edit_row("a13t4", "wa", |row| {
        row.insert("pane_pid".into(), Value::from(orphan.pid));
        row.insert("pane_birth_identity".into(), Value::from(exact.clone()));
    });
    assert_eq!(bay.row("a13t4", "wa")["status"], "stopped", "FIXTURE: wa stays stopped");
    let panes = bay.panes();
    let state = bay.state_bytes();

    let rerun = bay.run(&["start", "wa", "--team", "a13t4"]);
    assert!(!rerun.ok(), "T4: a stopped row whose carrier observes Possible must be refused: {}", rerun.describe());
    assert!(alive(orphan.pid), "T4: a Possible carrier on a stopped row must never be signalled");
    assert_eq!(bay.panes(), panes, "T4: no pane may be killed or spawned");
    assert_eq!(bay.state_bytes(), state, "T4: a refused start must not write runtime state");
}

// ───────────────────────────── T5 ─────────────────────────────

/// C2/C5-C9 after a team stop (the row keeps a now-dead carrier tuple): a crash mid-spawn,
/// then a plain rerun, converges to exactly one live pane with a fresh carrier.
#[test]
fn t5_after_team_stop_a_crash_mid_spawn_then_plain_rerun_converges_to_one_pane() {
    let bay = Bay::new("t5");
    bay.team("a13t5", &["wa", "wb"]);
    let stop = bay.run(&["stop", "--team", "a13t5"]);
    assert!(stop.ok(), "FIXTURE: team stop must succeed: {}", stop.describe());
    assert!(bay.panes().is_empty(), "FIXTURE: team stop leaves no team pane: {:?}", bay.panes());
    // Whatever status text the stop persisted, any carrier the row still records is dead.
    if let Some(pid) = bay.row("a13t5", "wa")["pane_pid"].as_u64() {
        assert!(gone_within(pid as u32, Duration::from_secs(5)), "FIXTURE: wa's recorded carrier must be dead after team stop");
    }
    let orphan = bay.crash_mid_spawn("a13t5", "wa");

    let rerun = bay.run(&["start", "wa", "--team", "a13t5"]);
    assert!(bay.seat_panes("wa").len() <= 1, "T5: NEVER two live panes for one seat (C8): {:?} rerun={}", bay.panes(), rerun.describe());
    assert!(rerun.ok(), "T5: a plain rerun after a crash must converge, not refuse: {}", rerun.describe());
    assert_converged(&bay, "a13t5", "wa", &orphan, "T5");
    assert!(bay.seat_panes("wb").is_empty(), "T5: the team-stopped sibling must not be started: {:?}", bay.panes());
}

// ───────────────────────────── T6 ─────────────────────────────

/// C1-C10 + dry-run clarification: after an agent stop and a crash mid-spawn, dry-run predicts
/// the replacement without any effect; the real rerun converges and names the retired orphan.
#[test]
fn t6_dry_run_predicts_orphan_replacement_without_effect_then_real_rerun_converges() {
    let bay = Bay::new("t6");
    bay.team("a13t6", &["wa", "wb"]);
    let sibling = bay.seat_panes("wb");
    assert!(bay.run(&["stop", "wa", "--team", "a13t6"]).ok(), "FIXTURE: stop wa");
    assert!(bay.seat_panes("wa").is_empty(), "FIXTURE: stop wa leaves no wa pane");
    let orphan = bay.crash_mid_spawn("a13t6", "wa");
    assert_eq!(bay.panes().len(), 2, "FIXTURE: orphan + wb: {:?}", bay.panes());
    let state = bay.state_bytes();
    let role = fs::read(bay.role_path("a13t6", "wa")).unwrap();
    let panes = bay.panes();

    let dry = bay.run(&["start", "wa", "--team", "a13t6", "--dry-run"]);
    assert!(dry.ok(), "T6: dry-run must predict the orphan replacement, not refuse: {}", dry.describe());
    assert!(names_pane(&dry.json, &orphan), "T6: dry-run must name the orphan to retire (session/window/pane_id) {orphan:?}: {}", dry.describe());
    assert!(alive(orphan.pid), "T6: dry-run must not signal the orphan");
    assert_eq!(bay.panes(), panes, "T6: dry-run must not kill or spawn panes");
    assert_eq!(bay.state_bytes(), state, "T6: dry-run must not write runtime state");
    assert_eq!(fs::read(bay.role_path("a13t6", "wa")).unwrap(), role, "T6: dry-run must not write the role file");

    let rerun = bay.run(&["start", "wa", "--team", "a13t6"]);
    assert!(bay.seat_panes("wa").len() <= 1, "T6: NEVER two live panes for one seat (C8): {:?} rerun={}", bay.panes(), rerun.describe());
    assert!(rerun.ok(), "T6: the real rerun after a crash must converge: {}", rerun.describe());
    assert_converged(&bay, "a13t6", "wa", &orphan, "T6");
    assert_eq!(bay.seat_panes("wb"), sibling, "T6: the sibling pane and carrier must be untouched");
    assert_eq!(bay.panes().len(), 2, "T6: exactly one live pane each for wa and wb: {:?}", bay.panes());
    assert!(
        names_pane(&rerun.json, &orphan) || events_name_pane(&bay.ws, &orphan),
        "T6: the receipt or an event must name the retired orphan (C10) {orphan:?}: {}",
        rerun.describe()
    );
}

// ───────────────────────────── T7 ─────────────────────────────

/// Live rows are out of A13's scope: rerunning start on a live, verified seat stays
/// already_running and kills, spawns and writes nothing.
#[test]
fn t7_live_seat_rerun_stays_already_running_and_touches_nothing() {
    let bay = Bay::new("t7");
    bay.team("a13t7", &["wa", "wb"]);
    let wa = bay.seat_panes("wa");
    assert_eq!(bay.row("a13t7", "wa")["pane_birth_identity"].as_str(), Some(birth(wa[0].pid).as_str()), "FIXTURE: live carrier birth is recorded exactly");
    let panes = bay.panes();
    let state = bay.state_bytes();

    let rerun = bay.run(&["start", "wa", "--team", "a13t7"]);
    assert!(rerun.ok(), "T7: rerunning start on a live seat must succeed as a no-op: {}", rerun.describe());
    assert_eq!(rerun.json["status"], "already_running", "T7: {}", rerun.describe());
    assert!(alive(wa[0].pid), "T7: the live carrier must not be signalled");
    assert_eq!(bay.panes(), panes, "T7: no pane may be killed or spawned");
    assert_eq!(bay.state_bytes(), state, "T7: an already-running start must not write runtime state");
}
