//! Space Station M2 golden property suite (Stage 2 — stateless role and seat lifecycle).
//!
//! Blueprint: `.team/artifacts/SPACE-STATION-STEP3-OPUS-REVIEW-REFINEMENT.md` (C4) and
//! `.team/artifacts/SPACE-STATION-ARCHITECTURE-OPUS-PROPOSAL.md` §4 Milestone 2.
//! L3 companion: `.team/artifacts/STAGE2-M2-HAIKU-RUNNER-CARDS.md`.
//!
//! Naming:
//! - `red_*`   must FAIL on `stage1/space-station-messaging-rebased-v0.5.128@9a2adad9` (the M1
//!   candidate) and PASS after M2. Each one was observed failing on that baseline for the stated
//!   reason before it was written down.
//! - `guard_*` PASSES on the baseline and must stay green through M2. A guard protects a property
//!   that removing the workspace lock or the shared state document could break.
//!
//! Rules for whoever turns these green:
//! - Do not weaken an assertion, lengthen a timeout, or delete a case to get green. If an oracle
//!   is wrong, change it with a written reason in the design notes of the runner cards.
//! - Tests drive public surfaces only: the `team-agent` binary and real tmux panes. Seat state is
//!   read through `status --json`, never through a file layout, so M2 may move seat records.
//! - Workers are the built-in `provider: fake` worker. Nothing needs provider credentials.
//! - Every tmux server is derived from a per-test workspace and is torn down by exact socket. The
//!   only fault injector is a `tmux` shim on the CLI's PATH that delays spawn or kill commands
//!   while a flag file exists in the test workspace; it always ends by exec-ing the real tmux.
//! - Contract-runtime-only properties (per-seat team-stop receipts for contract seats, the
//!   `NeedsRecovery` teardown wedge, legacy `provider: claude` row reset) need real providers and
//!   are covered by the L3 cards, not here. Geometry-is-not-identity is already locked by
//!   `team-agent-contract/tests/host_native.rs` (resize, then restore the owned pane).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![cfg(unix)]

#[path = "support/hermetic.rs"]
mod hermetic;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use hermetic::{fixture_tmux_binary, HermeticTestEnv};
use serde_json::Value;

/// A lifecycle verb on one seat may take this long at most while an unrelated seat is busy.
const UNRELATED_BUDGET: Duration = Duration::from_secs(5);
/// How long the shim stalls a spawn in the blocking tests. Well above `UNRELATED_BUDGET`.
const STALL_SECS: u64 = 12;
/// Hard ceiling for any single CLI call, so a hung baseline cannot hang the suite.
const CLI_CEILING: Duration = Duration::from_secs(60);

// ───────────────────────────── fixture ─────────────────────────────

struct Run {
    json: Value,
    raw: String,
    elapsed: Duration,
    timed_out: bool,
}

impl Run {
    fn ok(&self) -> bool {
        !self.timed_out && self.json.get("ok").and_then(Value::as_bool) == Some(true)
    }

    fn describe(&self) -> String {
        format!(
            "elapsed={:?} timed_out={} output={}",
            self.elapsed,
            self.timed_out,
            self.raw.chars().take(1200).collect::<String>()
        )
    }
}

/// One workspace with real fake-provider teams on a workspace-derived tmux server.
struct Bay {
    env: HermeticTestEnv,
    ws: PathBuf,
    shim_dir: PathBuf,
    teams: Vec<String>,
    /// `("-L", name)` or `("-S", path)` for every tmux server a team of this bay reported.
    sockets: std::cell::RefCell<Vec<(String, String)>>,
}

impl Bay {
    fn new(tag: &str) -> Self {
        let env = HermeticTestEnv::for_subprocess_with_leader(&format!("m2-{tag}"));
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
    if [ -f "$W/.m2-slow-spawn" ]; then echo $$ > "$W/.m2-entered"; sleep "$(cat "$W/.m2-slow-spawn")"; fi;;
  *" kill-pane "*|*" kill-window "*)
    if [ -f "$W/.m2-slow-kill" ]; then echo $$ > "$W/.m2-entered"; sleep "$(cat "$W/.m2-slow-kill")"; fi;;
esac
exec '{tmux}' "$@"
"#,
                ws = ws.display(),
                tmux = fixture_tmux_binary().display()
            ),
        )
        .unwrap();
        let mut permissions = fs::metadata(&shim).unwrap().permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, 0o755);
        fs::set_permissions(&shim, permissions).unwrap();
        Self { env, ws, shim_dir, teams: Vec::new(), sockets: Default::default() }
    }

    fn command(&self, args: &[&str], shim: bool) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_team-agent"));
        command.args(args).current_dir(&self.ws).stdin(Stdio::null());
        self.env.configure_command(&mut command);
        if shim {
            let path = std::env::var("PATH").unwrap_or_default();
            command.env("PATH", format!("{}:{path}", self.shim_dir.display()));
        }
        command
    }

    fn spawn(&self, args: &[&str], shim: bool) -> Child {
        self.command(args, shim).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap()
    }

    fn finish(mut child: Child, started: Instant, ceiling: Duration) -> Run {
        let mut timed_out = false;
        loop {
            if child.try_wait().unwrap().is_some() {
                break;
            }
            if started.elapsed() > ceiling {
                let _ = child.kill();
                timed_out = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let output = child.wait_with_output().unwrap();
        let elapsed = started.elapsed();
        let raw = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let json = serde_json::from_slice(&output.stdout).unwrap_or(Value::Null);
        Run { json, raw, elapsed, timed_out }
    }

    /// `team-agent <args> --workspace <ws> --json`, bounded by `CLI_CEILING`.
    fn ta(&self, args: &[&str]) -> Run {
        self.ta_with(args, false)
    }

    fn ta_with(&self, args: &[&str], shim: bool) -> Run {
        let ws = self.ws.to_string_lossy().to_string();
        let mut full: Vec<&str> = args.to_vec();
        full.extend(["--workspace", ws.as_str(), "--json"]);
        let started = Instant::now();
        let child = self.spawn(&full, shim);
        Self::finish(child, started, CLI_CEILING)
    }

    /// quick-start a fake-provider team; every later call names it with `--team`.
    fn team(&mut self, team: &str, seats: &[&str]) {
        let dir = self.ws.join(format!("{team}-team"));
        fs::create_dir_all(dir.join("agents")).unwrap();
        fs::write(
            dir.join("TEAM.md"),
            format!("---\nname: {team}\nobjective: Stage 2 M2 lifecycle properties.\nprovider: fake\n---\n\nTeam.\n"),
        )
        .unwrap();
        for seat in seats {
            self.role(team, seat);
        }
        let dir_s = dir.to_string_lossy().to_string();
        let run = self.ta(&["quick-start", &dir_s, "--name", team, "--yes"]);
        assert!(run.ok(), "FIXTURE: quick-start of `{team}` must succeed: {}", run.describe());
        self.remember_sockets(&run.json);
        self.teams.push(team.to_string());
        let status = self.status(team);
        for seat in seats {
            assert_eq!(status.get(*seat).map(String::as_str), Some("running"), "FIXTURE: {seat} not running after quick-start: {status:?}");
        }
    }

    fn role(&self, team: &str, seat: &str) -> PathBuf {
        let path = self.ws.join(format!("{team}-team/agents/{seat}.md"));
        fs::write(&path, format!("---\nname: {seat}\nrole: {seat}\nprovider: fake\ntools:\n  - mcp_team\n---\n\n{seat}.\n")).unwrap();
        path
    }

    fn remember_sockets(&self, value: &Value) {
        let mut found = Vec::new();
        collect_sockets(value, &mut found);
        let mut sockets = self.sockets.borrow_mut();
        for socket in found {
            if !sockets.contains(&socket) {
                sockets.push(socket);
            }
        }
    }

    /// seat → runtime_status, from the public status brief.
    fn status(&self, team: &str) -> std::collections::BTreeMap<String, String> {
        let run = self.ta(&["status", "--team", team]);
        self.remember_sockets(&run.json);
        let nodes = run.json.get("nodes").and_then(Value::as_array).unwrap_or_else(|| {
            panic!("`status --team {team}` must return nodes: {}", run.describe())
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

    /// Live (not dead) panes on every tmux server this bay's teams have reported. One server can
    /// be reported both as `-L <name>` and `-S <path>`, so panes are keyed by the server's own
    /// `#{socket_path}`.
    fn live_panes(&self) -> Vec<String> {
        let tmux = fixture_tmux_binary();
        let mut panes = std::collections::BTreeSet::new();
        for (flag, socket) in self.sockets.borrow().iter() {
            let Ok(output) = Command::new(&tmux)
                .args([
                    flag.as_str(), socket.as_str(), "list-panes", "-a", "-F",
                    "#{pane_dead} #{socket_path} #{session_name}:#{window_name}.#{pane_id}",
                ])
                .output()
            else {
                continue;
            };
            for line in String::from_utf8_lossy(&output.stdout).lines() {
                if let Some(rest) = line.strip_prefix("0 ") {
                    panes.insert(rest.to_string());
                }
            }
        }
        panes.into_iter().collect()
    }

    fn wait_until(&self, what: &str, limit: Duration, mut condition: impl FnMut() -> bool) {
        let deadline = Instant::now() + limit;
        while !condition() {
            assert!(Instant::now() < deadline, "FIXTURE: timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn flag(&self, name: &str, seconds: u64) {
        let _ = fs::remove_file(self.ws.join(".m2-entered"));
        fs::write(self.ws.join(name), seconds.to_string()).unwrap();
    }

    fn unflag(&self, name: &str) {
        let _ = fs::remove_file(self.ws.join(name));
    }

    /// PID of the shim process that is currently stalling, once it has entered the stall.
    fn wait_stalled(&self) -> u32 {
        let path = self.ws.join(".m2-entered");
        self.wait_until("the tmux shim to stall a lifecycle command", Duration::from_secs(15), || {
            fs::read_to_string(&path).map(|text| !text.trim().is_empty()).unwrap_or(false)
        });
        fs::read_to_string(&path).unwrap().trim().parse().unwrap()
    }

    fn coordinator_pid(&self) -> Option<u32> {
        fs::read_to_string(self.ws.join(".team/runtime/coordinator.pid"))
            .ok()
            .and_then(|text| text.trim().parse().ok())
    }
}

impl Drop for Bay {
    fn drop(&mut self) {
        let _ = fs::remove_file(self.ws.join(".m2-slow-spawn"));
        let _ = fs::remove_file(self.ws.join(".m2-slow-kill"));
        if let Some(pid) = self.coordinator_pid() {
            let _ = Command::new("kill").args(["-CONT", &pid.to_string()]).status();
        }
        for team in self.teams.clone() {
            let _ = self.ta(&["stop", "--team", &team]);
        }
        // Exact servers only: each was reported by this bay's own workspace-derived teams.
        let tmux = fixture_tmux_binary();
        let mut files = Vec::new();
        for (flag, socket) in self.sockets.borrow().iter() {
            if flag == "-S" {
                files.push(PathBuf::from(socket));
            } else if let Ok(output) = Command::new(&tmux).args([flag.as_str(), socket.as_str(), "list-sessions", "-F", "#{socket_path}"]).output() {
                if let Some(path) = String::from_utf8_lossy(&output.stdout).lines().next().filter(|line| !line.is_empty()) {
                    files.push(PathBuf::from(path));
                }
            }
            let _ = Command::new(&tmux).args([flag.as_str(), socket.as_str(), "kill-server"]).output();
        }
        for file in files {
            let _ = fs::remove_file(file);
        }
        if let Some(pid) = self.coordinator_pid() {
            let _ = Command::new("kill").args(["-KILL", &pid.to_string()]).status();
        }
    }
}

/// Collect `-L <name>` / `-S <path>` pairs from every string in a CLI JSON result.
fn collect_sockets(value: &Value, out: &mut Vec<(String, String)>) {
    match value {
        Value::String(text) => {
            let tokens: Vec<String> = text.split_whitespace().map(|t| t.trim_matches('\'').trim_matches('"').to_string()).collect();
            for pair in tokens.windows(2) {
                if (pair[0] == "-L" || pair[0] == "-S") && !pair[1].is_empty() {
                    out.push((pair[0].clone(), pair[1].clone()));
                }
            }
        }
        Value::Array(items) => items.iter().for_each(|item| collect_sockets(item, out)),
        Value::Object(map) => map.values().for_each(|item| collect_sockets(item, out)),
        _ => {}
    }
}

/// SIGSTOP the recorded coordinator for the guard's lifetime, if there is one.
struct Frozen(Option<u32>);

impl Frozen {
    fn coordinator(bay: &Bay) -> Self {
        let pid = bay.coordinator_pid();
        if let Some(pid) = pid {
            let _ = Command::new("kill").args(["-STOP", &pid.to_string()]).status();
        }
        Self(pid)
    }
}

impl Drop for Frozen {
    fn drop(&mut self) {
        if let Some(pid) = self.0 {
            let _ = Command::new("kill").args(["-CONT", &pid.to_string()]).status();
        }
    }
}

// ───────────────────── R-1/R-2 no workspace-wide lifecycle lock ─────────────────────

/// Blueprint C4 + proposal M2 gate 2 (isolation). While one seat's start is stuck inside its
/// spawn, a stop of a *different* seat must not wait for it.
/// Baseline: `stop-agent wb` waits for the workspace-wide `agent-lifecycle.lock` held by the
/// stalled `start-agent wa` (observed ~12 s; with a hung holder it times out at 30 s).
#[test]
fn red_m2_r1_a_stalled_spawn_on_one_seat_does_not_block_another_seats_stop() {
    let mut bay = Bay::new("r1");
    bay.team("m2r1", &["wa", "wb"]);
    assert!(bay.ta(&["stop-agent", "wa", "--team", "m2r1"]).ok());

    bay.flag(".m2-slow-spawn", STALL_SECS);
    let ws = bay.ws.to_string_lossy().to_string();
    let started = Instant::now();
    let stalled = bay.spawn(&["start-agent", "wa", "--team", "m2r1", "--workspace", &ws, "--json"], true);
    bay.wait_stalled();

    let other = bay.ta(&["stop-agent", "wb", "--team", "m2r1"]);
    bay.unflag(".m2-slow-spawn");
    let _ = Bay::finish(stalled, started, CLI_CEILING);

    assert!(other.ok(), "stop-agent wb must succeed: {}", other.describe());
    assert!(
        other.elapsed < UNRELATED_BUDGET,
        "stop-agent wb must not wait for wa's stalled spawn (budget {UNRELATED_BUDGET:?}): {}",
        other.describe()
    );
}

/// Blueprint S1/G4 (no workspace-global mutable lock; 27 teams shared one lock in the field).
/// A stalled spawn in team A must not block a lifecycle verb in team B of the same workspace.
#[test]
fn red_m2_r2_a_stalled_spawn_in_one_team_does_not_block_another_team() {
    let mut bay = Bay::new("r2");
    bay.team("m2r2a", &["wa"]);
    bay.team("m2r2b", &["v1"]);
    assert!(bay.ta(&["stop-agent", "wa", "--team", "m2r2a"]).ok());

    bay.flag(".m2-slow-spawn", STALL_SECS);
    let ws = bay.ws.to_string_lossy().to_string();
    let started = Instant::now();
    let stalled = bay.spawn(&["start-agent", "wa", "--team", "m2r2a", "--workspace", &ws, "--json"], true);
    bay.wait_stalled();

    let other = bay.ta(&["stop-agent", "v1", "--team", "m2r2b"]);
    bay.unflag(".m2-slow-spawn");
    let _ = Bay::finish(stalled, started, CLI_CEILING);

    assert!(other.ok(), "stop-agent v1 in team m2r2b must succeed: {}", other.describe());
    assert!(
        other.elapsed < UNRELATED_BUDGET,
        "team m2r2b must not wait for team m2r2a's stalled spawn (budget {UNRELATED_BUDGET:?}): {}",
        other.describe()
    );
}

// ───────────────────── R-3 explicit-stop fence (no resurrection) ─────────────────────

/// Blueprint C4 fences; `ISSUE-STOPPED-SEAT-RESURRECTION-BUG.md`. A team-level `start` must not
/// resurrect a seat the operator stopped explicitly; only an explicit `start <seat>` wakes it.
/// Baseline: team `start` emits `start_agent wa` (mode=fresh) and wa comes back `running`.
#[test]
fn red_m2_r3_team_start_keeps_an_explicitly_stopped_seat_stopped() {
    let mut bay = Bay::new("r3");
    bay.team("m2r3", &["wa", "wb"]);
    assert!(bay.ta(&["stop-agent", "wa", "--team", "m2r3"]).ok());
    let panes_before = bay.live_panes();

    let start = bay.ta(&["start", "--team", "m2r3"]);
    assert!(start.ok(), "team start must succeed: {}", start.describe());
    let status = bay.status("m2r3");
    assert_eq!(status.get("wa").map(String::as_str), Some("stopped"), "explicitly stopped wa was resurrected: {status:?}; start={}", start.describe());
    assert_eq!(status.get("wb").map(String::as_str), Some("running"), "{status:?}");
    assert_eq!(bay.live_panes().len(), panes_before.len(), "team start must not spawn a pane for wa");

    let wake = bay.ta(&["start", "wa", "--team", "m2r3"]);
    assert!(wake.ok(), "explicit `start wa` must wake the seat: {}", wake.describe());
    assert_eq!(bay.status("m2r3").get("wa").map(String::as_str), Some("running"));
}

/// Same fence across a whole-team shutdown: stop wa, stop the team, start the team.
#[test]
fn red_m2_r3b_explicit_stop_survives_team_shutdown_and_start() {
    let mut bay = Bay::new("r3b");
    bay.team("m2r3b", &["wa", "wb"]);
    assert!(bay.ta(&["stop-agent", "wa", "--team", "m2r3b"]).ok());
    let shutdown = bay.ta(&["stop", "--team", "m2r3b"]);
    assert!(shutdown.ok(), "team stop must succeed: {}", shutdown.describe());

    let start = bay.ta(&["start", "--team", "m2r3b"]);
    assert!(start.ok(), "team start must succeed: {}", start.describe());
    bay.remember_sockets(&start.json);
    let status = bay.status("m2r3b");
    assert_eq!(status.get("wa").map(String::as_str), Some("stopped"), "explicitly stopped wa was resurrected after shutdown: {status:?}");
    assert_eq!(status.get("wb").map(String::as_str), Some("running"), "{status:?}");
    assert_eq!(bay.live_panes().len(), 1, "only wb may own a live pane: {:?}", bay.live_panes());
}

// ───────────────────── R-4 crash convergence ─────────────────────

/// Blueprint C4 ("crash anywhere releases the kernel flock; the next invocation recomputes the
/// plan from tmux truth"), proposal M2 gate 1. The CLI dies after its spawn command was issued,
/// so the pane appears but the seat record was never published. Rerunning the same verb must
/// converge: one live pane for wa, status running, no manual `stop-agent` step.
/// Baseline: the rerun refuses with "already running (duplicate / active cohort); use
/// stop-agent first" while status still says wa is stopped.
#[test]
fn red_m2_r4_rerunning_start_after_a_crash_mid_spawn_converges_to_one_live_pane() {
    let mut bay = Bay::new("r4");
    bay.team("m2r4", &["wa", "wb"]);
    assert!(bay.ta(&["stop-agent", "wa", "--team", "m2r4"]).ok());
    assert_eq!(bay.live_panes().len(), 1);

    bay.flag(".m2-slow-spawn", 3);
    let ws = bay.ws.to_string_lossy().to_string();
    let mut crashed = bay.spawn(&["start-agent", "wa", "--team", "m2r4", "--workspace", &ws, "--json"], true);
    bay.wait_stalled();
    crashed.kill().unwrap(); // SIGKILL the CLI only; its already-issued tmux command completes.
    let _ = crashed.wait();
    bay.unflag(".m2-slow-spawn");
    bay.wait_until("the orphaned spawn to land", Duration::from_secs(10), || bay.live_panes().len() == 2);

    let rerun = bay.ta(&["start-agent", "wa", "--team", "m2r4"]);
    assert!(rerun.ok(), "rerunning start-agent wa after a crash must converge: {}", rerun.describe());
    assert_eq!(bay.status("m2r4").get("wa").map(String::as_str), Some("running"));
    assert_eq!(bay.live_panes().len(), 2, "exactly one live pane each for wa and wb: {:?}", bay.live_panes());
}

// ───────────────────── R-5 sibling-role isolation ─────────────────────

/// Paralysis F1/F2, blueprint S1 (validation scoped to the role it protects). An unrelated,
/// broken role file next to the new one must not veto `add-agent`.
/// Baseline: "spec compile failed … /agents/N/provider: unknown provider 'nosuchprovider'".
#[test]
fn red_m2_r5_add_agent_is_not_blocked_by_an_unrelated_broken_role_file() {
    let mut bay = Bay::new("r5");
    bay.team("m2r5", &["wa"]);
    fs::write(
        bay.ws.join("m2r5-team/agents/zz-broken.md"),
        "---\nname: zz-broken\nrole: zz-broken\nprovider: nosuchprovider\n---\n\nBroken sibling.\n",
    )
    .unwrap();
    let role = bay.role("m2r5", "wd").to_string_lossy().to_string();

    let add = bay.ta(&["add-agent", "wd", "--role-file", &role, "--team", "m2r5"]);
    assert!(add.ok(), "add-agent wd must not be vetoed by an unrelated role file: {}", add.describe());
    assert!(!add.raw.contains("nosuchprovider") && !add.raw.contains("zz-broken"), "the receipt must not be about the sibling: {}", add.describe());
    assert_eq!(bay.status("m2r5").get("wd").map(String::as_str), Some("running"));
}

// ───────────────────── R-6 restart converges to one pane per seat ─────────────────────

/// Blueprint C4 convergent verbs ("diff desired vs observed"). Restarting the team must replace
/// each seat's pane, not add a second live one beside it.
/// Baseline: `restart --team` and `team restart` each add a full new set of panes and leave the
/// old ones running (2 seats → 4, then 6 live panes).
#[test]
fn red_m2_r6_team_restart_leaves_exactly_one_live_pane_per_seat() {
    let mut bay = Bay::new("r6");
    bay.team("m2r6", &["wa", "wb"]);
    assert_eq!(bay.live_panes().len(), 2);

    let bare = bay.ta(&["restart", "--team", "m2r6"]);
    assert!(bare.ok(), "restart --team must succeed: {}", bare.describe());
    bay.remember_sockets(&bare.json);
    let after_bare = bay.live_panes();
    assert_eq!(after_bare.len(), 2, "restart --team leaked panes: {after_bare:?}");

    let explicit = bay.ta(&["team", "restart", "--team", "m2r6"]);
    assert!(explicit.ok(), "team restart must succeed: {}", explicit.describe());
    bay.remember_sockets(&explicit.json);
    let after_team = bay.live_panes();
    assert_eq!(after_team.len(), 2, "team restart leaked panes: {after_team:?}");
    let status = bay.status("m2r6");
    assert_eq!(status.get("wa").map(String::as_str), Some("running"));
    assert_eq!(status.get("wb").map(String::as_str), Some("running"));
}

// ───────────────────── R-7 the shared state document is off the critical path ─────────────────────

/// Proposal M2.2/M2.6, Astra C4 ("每席一个清单；不再写巨型 state.json"), blueprint S1. A torn
/// or corrupt workspace-wide runtime state document must not take down seat lifecycle or status.
/// The coordinator is frozen first so it cannot rewrite the document mid-test.
/// Baseline: every verb and `status` fail with "json: EOF while parsing a value".
#[test]
fn red_m2_r7_a_corrupt_shared_state_document_does_not_take_down_seat_lifecycle() {
    let mut bay = Bay::new("r7");
    bay.team("m2r7", &["wa", "wb"]);
    let _frozen = Frozen::coordinator(&bay);
    fs::write(bay.ws.join(".team/runtime/state.json"), b"{\"truncated\": ").unwrap();

    let stop = bay.ta(&["stop-agent", "wb", "--team", "m2r7"]);
    assert!(stop.ok(), "stop-agent wb with a torn shared document: {}", stop.describe());
    let start = bay.ta(&["start-agent", "wb", "--team", "m2r7"]);
    assert!(start.ok(), "start-agent wb with a torn shared document: {}", start.describe());
    let status = bay.ta(&["status", "--team", "m2r7"]);
    let nodes = status.json.get("nodes").and_then(Value::as_array).cloned().unwrap_or_default();
    assert_eq!(nodes.len(), 2, "status must still list both seats: {}", status.describe());
}

// ───────────────────── R-8 excision ratchet ─────────────────────

fn production_sources() -> Vec<(PathBuf, String)> {
    let crate_src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        for entry in fs::read_dir(dir).unwrap().flatten() {
            let path = entry.path();
            let name = path.file_name().unwrap().to_string_lossy().to_string();
            if path.is_dir() {
                if name != "tests" && name != "test_support" {
                    walk(&path, out);
                }
            } else if name.ends_with(".rs") && !name.ends_with("_tests.rs") && name != "tests.rs" {
                out.push(path);
            }
        }
    }
    walk(&crate_src, &mut files);
    files
        .into_iter()
        .map(|path| {
            let text = fs::read_to_string(&path).unwrap();
            (path, production_part(&text))
        })
        .collect()
}

/// Drop comment lines and everything from a trailing `#[cfg(test)] mod` onward.
fn production_part(text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let mut out = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        if trimmed == "#[cfg(test)]" {
            let next = lines[index + 1..].iter().map(|l| l.trim()).find(|l| !l.is_empty()).unwrap_or("");
            if next.starts_with("mod ") || next.starts_with("pub mod ") || next.starts_with("pub(crate) mod ") {
                break;
            }
        }
        out.push(if trimmed.starts_with("//") { "" } else { *line });
    }
    out.join("\n")
}

/// Proposal M2.6: the workspace-wide `agent-lifecycle.lock` and its waiters are excised, not
/// merely bypassed. R1/R2 prove the behaviour; this ratchet keeps the global lock from coming back
/// under the same entry points.
#[test]
fn red_m2_r8_no_production_caller_of_the_workspace_wide_lifecycle_lock() {
    let mut found = Vec::new();
    for (path, text) in production_sources() {
        for (number, line) in text.lines().enumerate() {
            if line.contains("fn acquire_agent_lifecycle_lock") || line.contains("fn agent_lifecycle_lock_path") {
                continue;
            }
            if ["acquire_agent_lifecycle_lock(", "agent_lifecycle_lock_path("].iter().any(|p| line.contains(p)) {
                found.push(format!("{}:{}: {}", path.display(), number + 1, line.trim()));
            }
        }
    }
    assert!(found.is_empty(), "workspace-wide lifecycle lock still taken by production code:\n{}", found.join("\n"));
}

// ───────────────────── guards (green on baseline, must stay green) ─────────────────────

/// Crash before the spawn command took effect: rerunning start lands exactly one pane.
#[test]
fn guard_m2_g1_crash_before_the_spawn_effect_rerun_starts_exactly_once() {
    let mut bay = Bay::new("g1");
    bay.team("m2g1", &["wa", "wb"]);
    assert!(bay.ta(&["stop-agent", "wa", "--team", "m2g1"]).ok());

    bay.flag(".m2-slow-spawn", 5);
    let ws = bay.ws.to_string_lossy().to_string();
    let mut crashed = bay.spawn(&["start-agent", "wa", "--team", "m2g1", "--workspace", &ws, "--json"], true);
    let shim = bay.wait_stalled();
    crashed.kill().unwrap();
    let _ = crashed.wait();
    let _ = Command::new("kill").args(["-KILL", &shim.to_string()]).status(); // the spawn never happens
    bay.unflag(".m2-slow-spawn");
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(bay.live_panes().len(), 1, "FIXTURE: no spawn may have landed");

    let rerun = bay.ta(&["start-agent", "wa", "--team", "m2g1"]);
    assert!(rerun.ok(), "{}", rerun.describe());
    assert_eq!(bay.status("m2g1").get("wa").map(String::as_str), Some("running"));
    assert_eq!(bay.live_panes().len(), 2, "{:?}", bay.live_panes());
}

/// Crash after the kill took effect but before the seat record was updated: rerunning stop and
/// then start converges.
#[test]
fn guard_m2_g2_crash_after_the_stop_effect_rerun_stop_and_start_converge() {
    let mut bay = Bay::new("g2");
    bay.team("m2g2", &["wa", "wb"]);

    bay.flag(".m2-slow-kill", 2);
    let ws = bay.ws.to_string_lossy().to_string();
    let started = Instant::now();
    let mut crashed = bay.spawn(&["stop-agent", "wb", "--team", "m2g2", "--workspace", &ws, "--json"], true);
    let entered = {
        let path = bay.ws.join(".m2-entered");
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if fs::read_to_string(&path).map(|t| !t.trim().is_empty()).unwrap_or(false) {
                break true;
            }
            if crashed.try_wait().unwrap().is_some() || Instant::now() > deadline {
                break false;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    };
    if entered {
        crashed.kill().unwrap(); // the issued kill still lands after the stall
    }
    let _ = Bay::finish(crashed, started, CLI_CEILING);
    bay.unflag(".m2-slow-kill");
    std::thread::sleep(Duration::from_secs(3));

    let stop = bay.ta(&["stop-agent", "wb", "--team", "m2g2"]);
    assert!(stop.ok(), "{}", stop.describe());
    assert_ne!(bay.status("m2g2").get("wb").map(String::as_str), Some("running"));
    assert_eq!(bay.live_panes().len(), 1, "{:?}", bay.live_panes());
    let start = bay.ta(&["start-agent", "wb", "--team", "m2g2"]);
    assert!(start.ok(), "{}", start.describe());
    assert_eq!(bay.live_panes().len(), 2, "{:?}", bay.live_panes());
}

/// Removing the global lock must keep same-seat verbs serialized: three concurrent starts of one
/// stopped seat yield exactly one live pane.
#[test]
fn guard_m2_g3_concurrent_starts_of_one_seat_yield_exactly_one_pane() {
    let mut bay = Bay::new("g3");
    bay.team("m2g3", &["wa", "wb"]);
    assert!(bay.ta(&["stop-agent", "wa", "--team", "m2g3"]).ok());
    let ws = bay.ws.to_string_lossy().to_string();
    let started = Instant::now();
    let children: Vec<Child> = (0..3)
        .map(|_| bay.spawn(&["start-agent", "wa", "--team", "m2g3", "--workspace", &ws, "--json"], false))
        .collect();
    let runs: Vec<Run> = children.into_iter().map(|child| Bay::finish(child, started, CLI_CEILING)).collect();
    assert!(runs.iter().any(Run::ok), "at least one start must succeed: {:?}", runs.iter().map(Run::describe).collect::<Vec<_>>());
    assert_eq!(bay.status("m2g3").get("wa").map(String::as_str), Some("running"));
    assert_eq!(bay.live_panes().len(), 2, "exactly one pane for wa: {:?}", bay.live_panes());
}

/// Two different new seats added in parallel both land; neither record is lost.
#[test]
fn guard_m2_g4_parallel_adds_of_two_new_seats_both_land() {
    let mut bay = Bay::new("g4");
    bay.team("m2g4", &["wa"]);
    let ws = bay.ws.to_string_lossy().to_string();
    let wx = bay.role("m2g4", "wx").to_string_lossy().to_string();
    let wy = bay.role("m2g4", "wy").to_string_lossy().to_string();
    let started = Instant::now();
    let a = bay.spawn(&["add-agent", "wx", "--role-file", &wx, "--team", "m2g4", "--workspace", &ws, "--json"], false);
    let b = bay.spawn(&["add-agent", "wy", "--role-file", &wy, "--team", "m2g4", "--workspace", &ws, "--json"], false);
    let (a, b) = (Bay::finish(a, started, CLI_CEILING), Bay::finish(b, started, CLI_CEILING));
    assert!(a.ok() && b.ok(), "{} / {}", a.describe(), b.describe());
    let status = bay.status("m2g4");
    assert_eq!(status.get("wx").map(String::as_str), Some("running"), "{status:?}");
    assert_eq!(status.get("wy").map(String::as_str), Some("running"), "{status:?}");
}

/// A removed seat stays removed across team start and team restart (tombstone kept).
#[test]
fn guard_m2_g5_removed_seat_stays_removed_across_start_and_restart() {
    let mut bay = Bay::new("g5");
    bay.team("m2g5", &["wa", "wb"]);
    let remove = bay.ta(&["remove-agent", "wb", "--from-spec", "--confirm", "--force", "--team", "m2g5"]);
    assert!(remove.ok(), "{}", remove.describe());
    assert!(bay.ta(&["start", "--team", "m2g5"]).ok());
    assert!(!bay.status("m2g5").contains_key("wb"), "removed wb came back after start");
    let restart = bay.ta(&["team", "restart", "--team", "m2g5"]);
    assert!(restart.ok(), "{}", restart.describe());
    assert!(!bay.status("m2g5").contains_key("wb"), "removed wb came back after team restart");
}

/// Seat verbs never wait on the coordinator: with it frozen, stop and start of a seat finish
/// promptly.
#[test]
fn guard_m2_g6_a_frozen_coordinator_does_not_slow_seat_verbs() {
    let mut bay = Bay::new("g6");
    bay.team("m2g6", &["wa", "wb"]);
    let _frozen = Frozen::coordinator(&bay);
    let stop = bay.ta(&["stop-agent", "wa", "--team", "m2g6"]);
    assert!(stop.ok() && stop.elapsed < UNRELATED_BUDGET, "{}", stop.describe());
    let start = bay.ta(&["start-agent", "wa", "--team", "m2g6"]);
    assert!(start.ok() && start.elapsed < UNRELATED_BUDGET, "{}", start.describe());
}
