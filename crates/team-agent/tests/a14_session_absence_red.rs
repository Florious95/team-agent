//! A14 independent RED suite: a team stop whose own reap closed the session is still a clean stop.
//!
//! Authority: ADJUDICATION-BLUEPRINT-819331fd §12, A14 (`session_closed` = (K) killed by this
//! stop, OR (V) verified absence V1-V4; `clean_stop` replaces `session_killed` with
//! `session_closed`; all existing fences keep `clean_stop == false`). Author: repro-opus, the
//! independent tester. The implementer imports this file unmodified and does not read it.
//!
//! FROZEN. Do not edit assertions, budgets, fixtures or names. Turn red to green in production.
//!
//! Cases:
//! - t1 (RED on 0ec7b2b5): on a running fake-provider team, `stop --team` ends the panes in its
//!   own reap before any session kill. The stop must still be clean: ok, `session_closed:true`,
//!   both projections `stopped` with the dead carrier tuple retained, team status `shutdown`, and
//!   `status` reporting `stopped`, never `unknown`. A later `start wa` runs one pane with an exact
//!   birth.
//! - t2 (guard): wb records a live, test-owned process as its carrier, with no birth and a spawn
//!   time it predates (Unknown). `stop --team` is not clean: ok:false, no row re-marked
//!   `stopped`, no `shutdown` stamp, no `session_closed:true`, and the process is never
//!   signalled.
//!
//! Lifetimes: every CLI runs in an owned process group with a <=120 s stdin lifeline
//! (`FixtureGroup`). tmux servers live under the hermetic private `TMUX_TMPDIR` root, which
//! has its own reaper. No host-wide kill and no pkill. Only test-owned resources are touched.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![cfg(unix)]

#[path = "support/hermetic.rs"]
mod hermetic;

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use hermetic::proc_guard::FixtureGroup;
use hermetic::{fixture_tmux_binary, HermeticTestEnv};
use serde_json::Value;

/// Hard ceiling for any single CLI call.
const CLI_CEILING: Duration = Duration::from_secs(60);
/// Lifeline TTL for every owned process group (R1: <= 120 s).
const LIFELINE: Duration = Duration::from_secs(120);

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
            self.raw.chars().take(3000).collect::<String>()
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Pane {
    session: String,
    window: String,
    pane_id: String,
    pid: u32,
}

/// One hermetic workspace with a fake-provider team on a private tmux root.
struct Bay {
    env: HermeticTestEnv,
    ws: PathBuf,
    tmux_root: PathBuf,
    seq: std::cell::Cell<u32>,
}

impl Bay {
    fn new(tag: &str) -> Self {
        let env = HermeticTestEnv::for_subprocess_with_leader(&format!("a14-{tag}"));
        let ws = env.workspace(tag);
        let mut probe = Command::new("/bin/sh");
        probe.args(["-c", "printf %s \"$TMUX_TMPDIR\""]);
        env.configure_command(&mut probe);
        let tmux_root = PathBuf::from(String::from_utf8(probe.output().unwrap().stdout).unwrap());
        assert!(tmux_root.is_dir(), "FIXTURE: private TMUX_TMPDIR root: {}", tmux_root.display());
        Self { env, ws, tmux_root, seq: std::cell::Cell::new(0) }
    }

    /// `team-agent <args> --workspace <ws> --json` in an owned, lifelined process group.
    fn run(&self, args: &[&str]) -> Run {
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
        command
            .stdout(Stdio::from(fs::File::create(&out).unwrap()))
            .stderr(Stdio::from(fs::File::create(&err).unwrap()));
        let mut child = FixtureGroup::spawn(&mut command, &self.ws, LIFELINE);
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

    /// Create and start a fake-provider team through the canonical `start --from`.
    fn team(&self, team: &str, seats: &[&str]) {
        let dir = self.ws.join(format!("{team}-team"));
        fs::create_dir_all(dir.join("agents")).unwrap();
        fs::write(
            dir.join("TEAM.md"),
            format!("---\nname: {team}\nobjective: A14 session absence.\nprovider: fake\n---\n\nTeam.\n"),
        )
        .unwrap();
        for seat in seats {
            fs::write(
                dir.join(format!("agents/{seat}.md")),
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

    fn state(&self) -> Value {
        serde_json::from_slice(&fs::read(self.state_path()).unwrap()).unwrap()
    }

    /// The seat row in the team projection, and in the root mirror when the root is this team.
    fn rows(&self, team: &str, seat: &str) -> Vec<Value> {
        let state = self.state();
        let mut rows = vec![state.pointer(&format!("/teams/{team}/agents/{seat}")).cloned().unwrap_or(Value::Null)];
        let root_is_team = state.get("team_key").and_then(Value::as_str) == Some(team)
            || state.get("active_team_key").and_then(Value::as_str) == Some(team);
        if root_is_team {
            if let Some(row) = state.pointer(&format!("/agents/{seat}")) {
                rows.push(row.clone());
            }
        }
        rows
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
        let mut state = self.state();
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
}

impl Drop for Bay {
    fn drop(&mut self) {
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

/// Every boolean value of `key` anywhere in the JSON.
fn bools(value: &Value, key: &str, out: &mut Vec<bool>) {
    match value {
        Value::Object(map) => {
            if let Some(flag) = map.get(key).and_then(Value::as_bool) {
                out.push(flag);
            }
            map.values().for_each(|item| bools(item, key, out));
        }
        Value::Array(items) => items.iter().for_each(|item| bools(item, key, out)),
        _ => {}
    }
}

// ───────────────────────────── T1 ─────────────────────────────

/// A14 (V): the stop's own reap closed the session before any session kill. The stop is still
/// clean and persists it: status-only `stopped` rows (dead tuple retained), team `shutdown`,
/// and a public status that says `stopped`. A later single-seat start works.
#[test]
fn t1_team_stop_whose_reap_closed_the_session_persists_a_clean_shutdown() {
    let bay = Bay::new("t1");
    bay.team("a14t1", &["wa", "wb"]);
    let mut carriers = Vec::new();
    for seat in ["wa", "wb"] {
        let row = &bay.rows("a14t1", seat)[0];
        let pid = row["pane_pid"].as_u64().expect("FIXTURE: running row records its carrier") as u32;
        assert_eq!(row["pane_birth_identity"].as_str(), Some(birth(pid).as_str()), "FIXTURE: exact carrier birth recorded");
        carriers.push((seat, pid, row["pane_birth_identity"].clone()));
    }

    let stop = bay.run(&["stop", "--team", "a14t1"]);
    assert!(stop.ok(), "T1: the team stop must succeed: {}", stop.describe());
    let mut closed = Vec::new();
    bools(&stop.json, "session_closed", &mut closed);
    assert!(
        closed.contains(&true) && !closed.contains(&false),
        "T1: the receipt must report session_closed:true (verified closure): {}",
        stop.describe()
    );
    assert!(bay.panes().is_empty(), "T1: no team pane may survive the stop: {:?}", bay.panes());
    for (seat, pid, recorded_birth) in &carriers {
        assert!(gone_within(*pid, Duration::from_secs(5)), "T1: {seat}'s carrier must be gone after the stop");
        for row in bay.rows("a14t1", seat) {
            assert_eq!(row["status"], "stopped", "T1: {seat} must be persisted stopped in every projection: {row}");
            assert_eq!(row["pane_pid"].as_u64(), Some(u64::from(*pid)), "T1: stop is status-only; {seat}'s dead carrier PID is retained: {row}");
            assert_eq!(&row["pane_birth_identity"], recorded_birth, "T1: stop is status-only; {seat}'s birth is retained: {row}");
        }
    }
    let state = bay.state();
    assert_eq!(state.pointer("/teams/a14t1/status").and_then(Value::as_str), Some("shutdown"), "T1: the team must be stamped shutdown: {}", state.pointer("/teams/a14t1").cloned().unwrap_or(Value::Null));
    let status = bay.status("a14t1");
    for seat in ["wa", "wb"] {
        assert_eq!(status.get(seat).map(String::as_str), Some("stopped"), "T1: status must report {seat} stopped, never unknown: {status:?}");
    }

    let start = bay.run(&["start", "wa", "--team", "a14t1"]);
    assert!(start.ok(), "T1: starting one seat after the clean stop must succeed: {}", start.describe());
    let panes = bay.seat_panes("wa");
    assert_eq!(panes.len(), 1, "T1: wa must own exactly one live pane: {:?}", bay.panes());
    let row = &bay.rows("a14t1", "wa")[0];
    assert_eq!(row["pane_pid"].as_u64(), Some(u64::from(panes[0].pid)), "T1: wa's row records the new carrier: {row}");
    assert_eq!(row["pane_birth_identity"].as_str(), Some(birth(panes[0].pid).as_str()), "T1: wa's row records the new carrier's exact birth: {row}");
    let status = bay.status("a14t1");
    assert_eq!(status.get("wa").map(String::as_str), Some("running"), "T1: {status:?}");
    assert_eq!(status.get("wb").map(String::as_str), Some("stopped"), "T1: {status:?}");
    assert!(bay.seat_panes("wb").is_empty(), "T1: wb must not be started: {:?}", bay.panes());
}

// ───────────────────────────── T2 ─────────────────────────────

/// A14 fences: an Unknown recorded carrier keeps the stop unclean even though the session is
/// gone. Rows keep their status, no shutdown stamp, no `session_closed:true`, and the process
/// is never signalled.
#[test]
fn t2_unknown_recorded_carrier_keeps_the_stop_unclean_and_is_never_signalled() {
    let bay = Bay::new("t2");
    bay.team("a14t2", &["wa", "wb"]);
    let marker = bay.env.root().join("a14-unknown-carrier");
    fs::create_dir_all(&marker).unwrap();
    let sentinel = FixtureGroup::presence(&marker, LIFELINE);
    let pid = sentinel.id();
    assert!(alive(pid), "FIXTURE: the test-owned carrier is live");
    // No birth, and a spawn time the process predates by more than the window: Unknown.
    let spawned_at = (chrono::Utc::now() + chrono::Duration::seconds(120)).to_rfc3339();
    bay.edit_row("a14t2", "wb", |row| {
        row.insert("pane_pid".into(), Value::from(pid));
        row.remove("pane_birth_identity");
        row.insert("spawned_at".into(), Value::from(spawned_at.clone()));
    });
    let before: Vec<(String, Vec<Value>)> = ["wa", "wb"]
        .iter()
        .map(|seat| (seat.to_string(), bay.rows("a14t2", seat).iter().map(|row| row["status"].clone()).collect()))
        .collect();

    let stop = bay.run(&["stop", "--team", "a14t2"]);
    assert!(!stop.ok(), "T2: an unverified carrier must keep the stop from reporting ok: {}", stop.describe());
    assert!(stop.raw.contains("unverified carrier"), "T2: the refusal must come from the unverified-carrier fence: {}", stop.describe());
    let mut closed = Vec::new();
    bools(&stop.json, "session_closed", &mut closed);
    assert!(!closed.contains(&true), "T2: the receipt must not report session_closed:true: {}", stop.describe());
    assert!(alive(pid), "T2: the Unknown carrier must never be signalled");
    for (seat, statuses) in &before {
        let after: Vec<Value> = bay.rows("a14t2", seat).iter().map(|row| row["status"].clone()).collect();
        assert_eq!(&after, statuses, "T2: {seat}'s rows must not be re-marked by an unclean stop");
        assert!(after.iter().all(|status| status != "stopped"), "T2: {seat} must not be persisted stopped: {after:?}");
    }
    let state = bay.state();
    assert_ne!(state.pointer("/teams/a14t2/status").and_then(Value::as_str), Some("shutdown"), "T2: no shutdown stamp on an unclean stop");
    assert_ne!(state.get("status").and_then(Value::as_str), Some("shutdown"), "T2: no root shutdown stamp on an unclean stop");
    drop(sentinel);
}
