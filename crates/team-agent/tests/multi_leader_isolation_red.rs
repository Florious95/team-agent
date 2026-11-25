//! Multi-leader auto-isolation RED suite (GraphEngineering incident, 2026-11-25).
//!
//! Incident: two Pi leaders in ONE workspace on one tmux socket. The second leader got
//! `claim_refused(previous_owner_alive_refused)`, shared the first leader's
//! `leader_session_uuid`, and created a second team with an explicit `--team`. From then on:
//! - `start <seat> --team current` returned "running" while `state.json teams.current`
//!   silently kept the stopped/explicit_stop row (lifecycle authority of the literal team
//!   `current` was mapped to the top-level peer team);
//! - every send to that live worker returned `ok:true status:"stored" live:false
//!   recipient_not_running`;
//! - the next `start` orphan-killed the live, seat-bound pane;
//! - bypass audit events named the peer team's start-source;
//! - `team-agent leaders` listed a dead provider shell as LIVE and had no pane locator.
//!
//! Locked user decisions under test (see `CONTRACT.md`):
//! - D1: the first CLI-launched leader keeps `current`. Every later CLI-launched leader that
//!   finds `current` owned by a live leader allocates an unoccupied `leader-<uuid_prefix>`
//!   team, plus a fresh random `leader_session_uuid`, BEFORE the provider spawns.
//! - D2: bare `start` from a worker pane is refused. A plain external shell selects the
//!   only alive team, and refuses with the `--team` choices when two or more are alive.
//! - D3: an explicit `--team` owned by a live peer leader is refused for start and stop.
//! - D4: inside a leader, unscoped commands stay on its own team. A qualified
//!   `<workspace>::<team>/leader` address always resolves.
//! - D5: a bare relaunch after a leader exit is a new leader and a new team. It never
//!   hijacks the old one.
//! - D6: a split-brain projection is repaired by `start` without replaying stored history.
//! - D7 (live run #3 finding): a leader's bare lifecycle commands never claim, register or
//!   unregister a peer team, and never touch its owner, receiver or workers, even after the
//!   peer leader's provider has exited.
//! - K1: a stored-only send keeps its exit-0 JSON contract and says so in plain text.
//! - K2: every leader provider gets a per-launch team MCP binding; inherited identity is
//!   scrubbed.
//! - K4: `leaders --json` carries verified physical locators and never the binding nonce.
//!
//! Hermetic fixture:
//! - An isolated root under /private/tmp holds HOME, TMUX_TMPDIR, provider stubs and the
//!   workspace.
//! - An outer tmux server plays the user's terminals. Each leader is launched with the
//!   public `team-agent <provider>` from its own outer window, so the launcher has a real
//!   tty.
//! - The provider stubs (`pi`, `claude`, `codex`) record argv and env and then sleep. A
//!   leader's recorded env is replayed as its "tool shell" for CLI calls.
//! - Workers are the built-in fake provider.
//! - No real model, credential or host tmux server is touched.
#![cfg(unix)]
#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

type Env = BTreeMap<String, String>;

static SEQ: AtomicUsize = AtomicUsize::new(0);
const FAKE: &[&str] = &["--provider", "fake", "--model", "fake", "--bypass", "false"];
const NOT_LIVE_PHRASE: &str = "stored in inbox only — NOT delivered live";
const LEADER_UP: Duration = Duration::from_secs(45);
const CLI_LIMIT: Duration = Duration::from_secs(150);
const SETTLE: Duration = Duration::from_secs(20);
const POLL: Duration = Duration::from_millis(150);
const POISON_SESSION_FILE: &str = "/poison/leader-session.jsonl";
const POISON_SESSION_ID: &str = "poison-leader-session-id";
const POISON_MODEL: &str = "poison/leader-model";

// ───────────────────────────── fixture ─────────────────────────────

fn ta_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_team-agent"))
}

fn which(name: &str) -> PathBuf {
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path)
        .chain(["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin"].iter().map(PathBuf::from))
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file())
        .unwrap_or_else(|| panic!("{name} must be installed for this hermetic suite"))
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn wait_until(what: &str, limit: Duration, mut probe: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + limit;
    loop {
        if probe() {
            return true;
        }
        if Instant::now() >= deadline {
            eprintln!("wait_until timed out: {what}");
            return false;
        }
        std::thread::sleep(POLL);
    }
}

#[derive(Debug)]
struct Out {
    argv: Vec<String>,
    code: i32,
    stdout: String,
    stderr: String,
}

impl Out {
    fn json(&self) -> Value {
        let text = self.stdout.trim();
        serde_json::from_str(text)
            .or_else(|_| {
                let start = text.find('{').unwrap_or(0);
                serde_json::from_str(&text[start..])
            })
            .unwrap_or_else(|e| panic!("stdout is not JSON ({e}): {self:?}"))
    }
    fn all(&self) -> String {
        format!("{}\n{}", self.stdout, self.stderr)
    }
    fn refused(&self) -> bool {
        self.code != 0 || serde_json::from_str::<Value>(self.stdout.trim()).is_ok_and(|v| v["ok"] == json!(false))
    }
}

#[derive(Clone, Debug)]
struct Leader {
    provider: String,
    pid: u32,
    pane: String,
    socket: String,
    env: Env,
    argv: Vec<String>,
}

impl Leader {
    fn team(&self) -> String {
        self.env.get("TEAM_AGENT_TEAM_ID").cloned().unwrap_or_default()
    }
    fn uuid(&self) -> String {
        self.env.get("TEAM_AGENT_LEADER_SESSION_UUID").cloned().unwrap_or_default()
    }
    fn arg_after(&self, flag: &str) -> Option<String> {
        self.argv.iter().position(|a| a == flag).and_then(|i| self.argv.get(i + 1)).cloned()
    }
}

#[derive(Clone, Debug)]
struct Pane {
    id: String,
    session: String,
    window: String,
    pid: u32,
}

struct Rig {
    root: PathBuf,
    ws: PathBuf,
    home: PathBuf,
    tmux_tmpdir: PathBuf,
    stub: PathBuf,
    log: PathBuf,
    outer: PathBuf,
    tmux: PathBuf,
    windows: AtomicUsize,
}

impl Drop for Rig {
    fn drop(&mut self) {
        if std::env::var("TEAM_AGENT_KEEP_TEST_PROCESSES").as_deref() == Ok("1") {
            eprintln!("keeping multi-leader rig {}", self.root.display());
            return;
        }
        for socket in sockets_under(&self.tmux_tmpdir) {
            let _ = Command::new(&self.tmux).arg("-S").arg(&socket).arg("kill-server").output();
        }
        let _ = Command::new(&self.tmux).arg("-S").arg(&self.outer).arg("kill-server").output();
        if std::env::var("TEAM_AGENT_KEEP_TEST_TMP").as_deref() != Ok("1") {
            // Processes that lost their tmux server may still flush a last write into the
            // root; retry until the tree is really gone (bounded).
            for _ in 0..50 {
                let _ = fs::remove_dir_all(&self.root);
                if !self.root.exists() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    }
}

fn sockets_under(dir: &Path) -> Vec<PathBuf> {
    use std::os::unix::fs::FileTypeExt;
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(next) = stack.pop() {
        let Ok(entries) = fs::read_dir(&next) else { continue };
        for entry in entries.flatten() {
            let Ok(kind) = entry.file_type() else { continue };
            if kind.is_dir() {
                stack.push(entry.path());
            } else if kind.is_socket() {
                out.push(entry.path());
            }
        }
    }
    out
}

const STUB: &str = r#"#!/bin/sh
case "$1" in
  --version) echo "@VERSION@"; exit 0;;
  --list-models) printf 'provider  model  context\nfake  fake-model  1000\n'; exit 0;;
esac
[ -n "$TMUX_PANE" ] || exit 0
d="@LOG@/@NAME@-$$"
mkdir -p "$d"
for a in "$@"; do printf '%s\0' "$a"; done > "$d/argv"
env > "$d/env.tmp" && mv "$d/env.tmp" "$d/env"
exec sleep 600
"#;

impl Rig {
    fn new(tag: &str) -> Self {
        let base = if Path::new("/private/tmp").is_dir() { PathBuf::from("/private/tmp") } else { std::env::temp_dir() };
        let root = base.join(format!("mlai-{tag}-{}-{}", std::process::id(), SEQ.fetch_add(1, Ordering::Relaxed)));
        let _ = fs::remove_dir_all(&root);
        for sub in ["ws", "home/.team-agent/leaders", "tmux", "stub", "log"] {
            fs::create_dir_all(root.join(sub)).unwrap();
        }
        let root = fs::canonicalize(root).unwrap();
        let rig = Self {
            ws: root.join("ws"),
            home: root.join("home"),
            tmux_tmpdir: root.join("tmux"),
            stub: root.join("stub"),
            log: root.join("log"),
            outer: root.join("outer.sock"),
            tmux: which("tmux"),
            windows: AtomicUsize::new(0),
            root,
        };
        for (name, version) in [("pi", "1.6.0"), ("claude", "2.1.0 (Claude Code)"), ("codex", "codex-cli 0.80.0")] {
            let script = STUB
                .replace("@VERSION@", version)
                .replace("@LOG@", rig.log.to_str().unwrap())
                .replace("@NAME@", name);
            let path = rig.stub.join(name);
            fs::write(&path, script).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        }
        rig
    }

    fn base_env(&self) -> Env {
        let tmux_dir = self.tmux.parent().unwrap().display().to_string();
        let mut env = Env::new();
        env.insert("HOME".into(), self.home.display().to_string());
        env.insert("TMUX_TMPDIR".into(), self.tmux_tmpdir.display().to_string());
        env.insert("PATH".into(), format!("{}:{tmux_dir}:/usr/bin:/bin:/usr/sbin:/sbin", self.stub.display()));
        env.insert("LANG".into(), "en_US.UTF-8".into());
        env.insert("TERM".into(), "xterm-256color".into());
        env.insert("USER".into(), std::env::var("USER").unwrap_or_else(|_| "tester".into()));
        env
    }

    fn workspace(&self, name: &str) -> PathBuf {
        let path = self.root.join(name);
        fs::create_dir_all(&path).unwrap();
        fs::canonicalize(path).unwrap()
    }

    fn provider_dirs(&self, provider: &str) -> BTreeSet<PathBuf> {
        fs::read_dir(&self.log)
            .map(|entries| {
                entries
                    .flatten()
                    .map(|e| e.path())
                    .filter(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with(&format!("{provider}-"))))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Open one user terminal (an outer tmux window) and run the public
    /// `team-agent <provider>` launcher in it, exactly as a user would.
    fn launch(&self, provider: &str, extra_env: &[(&str, &str)]) -> Leader {
        self.launch_in(&self.ws, provider, extra_env)
    }

    fn launch_in(&self, workspace: &Path, provider: &str, extra_env: &[(&str, &str)]) -> Leader {
        let before = self.provider_dirs(provider);
        let mut env = self.base_env();
        for (k, v) in extra_env {
            env.insert((*k).into(), (*v).into());
        }
        let assignments = env.iter().map(|(k, v)| format!("{k}={}", shell_quote(v))).collect::<Vec<_>>().join(" ");
        let command = format!(
            "env -i {assignments} {} {provider}; echo LAUNCHER_EXIT=$?; exec sleep 600",
            shell_quote(ta_binary().to_str().unwrap())
        );
        let n = self.windows.fetch_add(1, Ordering::SeqCst);
        let window = format!("L{n}");
        // The outer "user terminal" server is hermetic too: it never inherits the test
        // runner's HOME, TMUX_TMPDIR, TMUX or TEAM_AGENT_* identity.
        let mut cmd = Command::new(&self.tmux);
        cmd.env_clear()
            .env("HOME", &self.home)
            .env("TMUX_TMPDIR", &self.tmux_tmpdir)
            .env("PATH", env.get("PATH").cloned().unwrap_or_default())
            .env("LANG", "en_US.UTF-8")
            .env("TERM", "xterm-256color");
        cmd.arg("-S").arg(&self.outer);
        if n == 0 {
            cmd.args(["-f", "/dev/null", "new-session", "-d", "-s", "user", "-n", &window, "-x", "200", "-y", "50", "-c"]);
        } else {
            cmd.args(["new-window", "-d", "-t", "user", "-n", &window, "-c"]);
        }
        let out = cmd.arg(workspace).arg(&command).output().unwrap();
        assert!(out.status.success(), "outer terminal {window} failed: {out:?}");
        let workspace_text = workspace.display().to_string();
        let mut found: Option<PathBuf> = None;
        let up = wait_until(&format!("{provider} leader {window} up"), LEADER_UP, || {
            found = self.provider_dirs(provider).difference(&before).find(|dir| {
                fs::read_to_string(dir.join("env"))
                    .is_ok_and(|env| env.lines().any(|l| l == format!("TEAM_AGENT_WORKSPACE={workspace_text}")))
            }).cloned();
            found.is_some()
        });
        if !up {
            let screen = Command::new(&self.tmux).arg("-S").arg(&self.outer)
                .args(["capture-pane", "-p", "-J", "-S", "-80", "-t", &format!("user:{window}")]).output().unwrap();
            panic!("{provider} leader in outer window {window} never reached its provider; terminal:\n{}",
                String::from_utf8_lossy(&screen.stdout));
        }
        let dir = found.unwrap();
        let pid = dir.file_name().unwrap().to_str().unwrap().rsplit('-').next().unwrap().parse().unwrap();
        let env = parse_env(&fs::read_to_string(dir.join("env")).unwrap());
        let argv = fs::read(dir.join("argv")).unwrap()
            .split(|b| *b == 0).filter(|s| !s.is_empty())
            .map(|s| String::from_utf8_lossy(s).into_owned()).collect();
        let pane = env.get("TMUX_PANE").cloned().unwrap_or_default();
        let socket = env.get("TMUX").and_then(|t| t.split(',').next()).unwrap_or_default().to_string();
        assert!(!pane.is_empty() && !socket.is_empty(), "leader provider must run inside a tmux pane: {env:?}");
        Leader { provider: provider.into(), pid, pane, socket, env, argv }
    }

    fn dual_pi(&self) -> (Leader, Leader) {
        let l1 = self.launch("pi", &[]);
        let l2 = self.launch("pi", &[]);
        (l1, l2)
    }

    fn ta_in(&self, cwd: &Path, env: &Env, args: &[&str]) -> Out {
        let mut child = Command::new(ta_binary())
            .args(args)
            .current_dir(cwd)
            .env_clear()
            .envs(env)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let stdout = drain(child.stdout.take().unwrap());
        let stderr = drain(child.stderr.take().unwrap());
        let deadline = Instant::now() + CLI_LIMIT;
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                panic!("team-agent {args:?} exceeded {CLI_LIMIT:?}");
            }
            std::thread::sleep(Duration::from_millis(50));
        };
        Out {
            argv: std::iter::once("team-agent".to_string()).chain(args.iter().map(|s| (*s).to_string())).collect(),
            code: status.code().unwrap_or(-1),
            stdout: stdout.join().unwrap(),
            stderr: stderr.join().unwrap(),
        }
    }

    /// Run a CLI command from inside `leader`'s tool shell (its provider's env, cwd = workspace).
    fn as_leader(&self, leader: &Leader, args: &[&str]) -> Out {
        self.ta_in(&self.ws, &leader.env, args)
    }

    fn as_leader_with(&self, leader: &Leader, extra: &[(&str, &str)], args: &[&str]) -> Out {
        let mut env = leader.env.clone();
        for (k, v) in extra {
            env.insert((*k).into(), (*v).into());
        }
        self.ta_in(&self.ws, &env, args)
    }

    /// Run a CLI command from a plain external shell (no tmux, no Team Agent identity).
    fn plain(&self, args: &[&str]) -> Out {
        self.ta_in(&self.ws, &self.base_env(), args)
    }

    fn start_fake(&self, leader: &Leader, agent: &str, team: Option<&str>) -> Out {
        let mut args = vec!["start", agent];
        args.extend_from_slice(FAKE);
        if let Some(team) = team {
            args.extend_from_slice(&["--team", team]);
        }
        args.push("--json");
        self.as_leader(leader, &args)
    }

    fn state(&self) -> Value {
        read_json(&self.ws.join(".team/runtime/state.json"))
    }

    fn events(&self) -> Vec<Value> {
        fs::read_to_string(self.ws.join(".team/logs/events.jsonl"))
            .unwrap_or_default()
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect()
    }

    fn row(&self, team: &str, agent: &str) -> Value {
        self.state().pointer(&format!("/teams/{team}/agents/{agent}")).cloned().unwrap_or(Value::Null)
    }

    fn panes(&self, socket: &str) -> Vec<Pane> {
        let out = Command::new(&self.tmux)
            .args(["-S", socket, "list-panes", "-a", "-F", "#{pane_id}\t#{session_name}\t#{window_name}\t#{pane_pid}\t#{pane_dead}"])
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|l| {
                let f: Vec<&str> = l.split('\t').collect();
                (f.len() == 5 && f[4] == "0").then(|| Pane {
                    id: f[0].into(),
                    session: f[1].into(),
                    window: f[2].into(),
                    pid: f[3].parse().unwrap_or(0),
                })
            })
            .collect()
    }

    fn pane_alive(&self, socket: &str, pane: &str) -> bool {
        self.panes(socket).iter().any(|p| p.id == pane)
    }

    /// The live tmux pane of `agent` in `team`'s worker session (window named after the agent).
    fn agent_pane(&self, socket: &str, team: &str, agent: &str) -> Option<Pane> {
        let suffix = format!("-{team}");
        self.panes(socket).into_iter().find(|p| p.window == agent && p.session.ends_with(&suffix))
    }

    fn pane_text(&self, socket: &str, pane: &str) -> String {
        let out = Command::new(&self.tmux)
            .args(["-S", socket, "capture-pane", "-p", "-J", "-S", "-400", "-t", pane])
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    fn wait_worker_ready(&self, socket: &str, team: &str, agent: &str) -> Pane {
        let mut pane = None;
        let marker = format!("TEAM_AGENT_FAKE_READY agent={agent}");
        let ok = wait_until(&format!("{team}/{agent} fake pane ready"), SETTLE, || {
            pane = self.agent_pane(socket, team, agent).filter(|p| self.pane_text(socket, &p.id).contains(&marker));
            pane.is_some()
        });
        assert!(ok, "{team}/{agent}: no live, ready worker pane in a `-{team}` session; panes={:?} state_row={}",
            self.panes(socket), self.row(team, agent));
        pane.unwrap()
    }

    fn seat_endpoint(&self, team: &str, agent: &str) -> Option<String> {
        let path = self.ws.join(format!(".team/station-v1/{team}/seats/{agent}.json"));
        path.exists().then(|| read_json(&path).pointer("/endpoint/pane_id").and_then(Value::as_str).map(str::to_string)).flatten()
    }

    fn leaders(&self) -> (Out, Vec<Value>) {
        let out = self.plain(&["leaders", "--json"]);
        let ws = self.ws.display().to_string();
        let entries = out.json()["leaders"].as_array().cloned().unwrap_or_default()
            .into_iter().filter(|e| e["workspace"].as_str() == Some(ws.as_str())).collect();
        (out, entries)
    }

    fn kill_provider(&self, leader: &Leader) {
        let _ = Command::new("kill").args(["-9", &leader.pid.to_string()]).output();
        let gone = wait_until("provider process gone", SETTLE, || {
            Command::new("kill").args(["-0", &leader.pid.to_string()]).output().is_ok_and(|o| !o.status.success())
        });
        assert!(gone, "fixture: provider pid {} did not exit", leader.pid);
    }
}

fn drain(mut stream: impl Read + Send + 'static) -> std::thread::JoinHandle<String> {
    std::thread::spawn(move || {
        let mut text = String::new();
        let _ = stream.read_to_string(&mut text);
        text
    })
}

fn parse_env(text: &str) -> Env {
    let mut env = Env::new();
    let mut last: Option<String> = None;
    for line in text.lines() {
        match line.split_once('=') {
            Some((k, v)) if !k.is_empty() && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') => {
                env.insert(k.into(), v.into());
                last = Some(k.into());
            }
            _ => {
                if let Some(k) = &last {
                    let value = env.get_mut(k).unwrap();
                    value.push('\n');
                    value.push_str(line);
                }
            }
        }
    }
    env
}

fn read_json(path: &Path) -> Value {
    fs::read_to_string(path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or(Value::Null)
}

fn inbox_text(ws: &Path, team: &str, agent: &str) -> String {
    let dir = ws.join(format!(".team/inbox/{team}/{agent}"));
    fs::read_dir(dir)
        .map(|entries| {
            entries.flatten().filter(|e| e.path().extension().is_some_and(|x| x == "jsonl"))
                .map(|e| fs::read_to_string(e.path()).unwrap_or_default()).collect::<Vec<_>>().join("\n")
        })
        .unwrap_or_default()
}

fn all_inbox_text(ws: &Path) -> String {
    let mut text = String::new();
    let mut stack = vec![ws.join(".team/inbox")];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(dir).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|x| x == "jsonl") {
                text.push_str(&fs::read_to_string(&path).unwrap_or_default());
            }
        }
    }
    text
}

fn agents_everywhere(state: &Value) -> BTreeSet<String> {
    let mut ids = BTreeSet::new();
    if let Some(agents) = state["agents"].as_object() {
        ids.extend(agents.keys().cloned());
    }
    for team in state["teams"].as_object().into_iter().flat_map(|t| t.values()) {
        if let Some(agents) = team["agents"].as_object() {
            ids.extend(agents.keys().cloned());
        }
    }
    ids
}

fn status_names(out: &Out) -> BTreeSet<String> {
    out.json()["nodes"].as_array().unwrap_or_else(|| panic!("status must return nodes: {out:?}"))
        .iter().filter_map(|n| n["name"].as_str().map(str::to_string)).collect()
}

#[track_caller]
fn ok(out: &Out, what: &str) -> Value {
    assert!(out.code == 0, "{what}: argv={:?} exit={} stdout={} stderr={}", out.argv, out.code, out.stdout, out.stderr);
    let body = out.json();
    assert_eq!(body["ok"], json!(true), "{what}: {body}");
    body
}

#[track_caller]
fn assert_live_receipt(receipt: &Value, pane: &str, what: &str) {
    assert_eq!(receipt["live"], json!(true), "{what}: a send to a live, verified seat must be delivered live: {receipt}");
    assert_eq!(receipt["delivery"], json!("physical"), "{what}: {receipt}");
    assert_eq!(receipt["pane_id"].as_str(), Some(pane), "{what}: delivered to the wrong pane: {receipt}");
}

#[track_caller]
fn assert_own_team_key(leader: &Leader, what: &str) -> String {
    let team = leader.team();
    assert!(!team.is_empty(), "{what}: launcher must bake TEAM_AGENT_TEAM_ID into the leader env: {:?}", leader.env);
    team
}

/// D1 shape: `leader-<prefix of the leader's own random leader_session_uuid>`.
#[track_caller]
fn assert_auto_leader_team(l1: &Leader, l2: &Leader) -> String {
    assert_eq!(l1.team(), "current", "D1: the first CLI-launched leader keeps `current`: {:?}", l1.env);
    let t2 = l2.team();
    assert_ne!(t2, "current",
        "D1: a second CLI-launched leader while `current` has a live owner must get its own team before its provider \
         spawns; it was baked with TEAM_AGENT_TEAM_ID={t2:?} (GraphEngineering: both leaders bound to `current`)");
    let suffix = t2.strip_prefix("leader-").unwrap_or_else(|| panic!("D1: auto team must be `leader-<uuid_prefix>`, got {t2:?}"));
    assert!(suffix.len() >= 4 && suffix.chars().all(|c| c.is_ascii_hexdigit()),
        "D1: auto team must be `leader-<hex uuid prefix>`, got {t2:?}");
    assert_ne!(l2.uuid(), l1.uuid(),
        "D1: the second leader needs its own random leader_session_uuid (GraphEngineering: both shared {})", l1.uuid());
    assert!(l2.uuid().to_ascii_lowercase().starts_with(&suffix.to_ascii_lowercase()),
        "D1: auto team {t2:?} must be derived from the leader's own uuid {}", l2.uuid());
    t2
}

fn pi_extension_definition(leader: &Leader) -> (PathBuf, Value) {
    let path = PathBuf::from(leader.arg_after("-e").unwrap_or_else(|| panic!("pi leader argv must load a team extension: {:?}", leader.argv)));
    let text = fs::read_to_string(&path).unwrap_or_else(|e| panic!("read pi extension {}: {e}", path.display()));
    let line = text.lines().find(|l| l.contains("teamMcpDefinition") && l.contains('{'))
        .unwrap_or_else(|| panic!("pi extension carries no teamMcpDefinition: {}", path.display()));
    let start = line.find('{').unwrap();
    let end = line.rfind('}').unwrap();
    (path, serde_json::from_str(&line[start..=end]).expect("teamMcpDefinition JSON"))
}

/// A minimal newline-delimited JSON-RPC client for `team-agent mcp-server`.
struct Mcp {
    child: Child,
    rx: mpsc::Receiver<Value>,
    next: u64,
}

impl Mcp {
    fn spawn(definition: &Value, base_env: &Env, cwd: &Path) -> Self {
        let mut env = base_env.clone();
        for (k, v) in definition["env"].as_object().into_iter().flatten() {
            env.insert(k.clone(), v.as_str().unwrap_or_default().to_string());
        }
        let args: Vec<String> = definition["args"].as_array().unwrap().iter().map(|a| a.as_str().unwrap().to_string()).collect();
        let mut child = Command::new(definition["command"].as_str().unwrap())
            .args(&args).current_dir(cwd).env_clear().envs(&env)
            .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null())
            .spawn().unwrap();
        let stdout = child.stdout.take().unwrap();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if let Ok(value) = serde_json::from_str::<Value>(&line) {
                    if tx.send(value).is_err() {
                        break;
                    }
                }
            }
        });
        let mut mcp = Self { child, rx, next: 1 };
        mcp.call("initialize", json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "mlai-red", "version": "0"}}));
        mcp.notify("notifications/initialized");
        mcp
    }
    fn notify(&mut self, method: &str) {
        let stdin = self.child.stdin.as_mut().unwrap();
        writeln!(stdin, "{}", json!({"jsonrpc": "2.0", "method": method})).unwrap();
        stdin.flush().unwrap();
    }
    fn call(&mut self, method: &str, params: Value) -> Value {
        let id = self.next;
        self.next += 1;
        let stdin = self.child.stdin.as_mut().unwrap();
        writeln!(stdin, "{}", json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})).unwrap();
        stdin.flush().unwrap();
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let frame = self.rx.recv_timeout(left).unwrap_or_else(|_| panic!("mcp {method} timed out"));
            if frame["id"] == json!(id) {
                return frame;
            }
        }
    }
    /// tools/call → parsed JSON of the first text content block.
    fn tool(&mut self, name: &str, arguments: Value) -> Value {
        let frame = self.call("tools/call", json!({"name": name, "arguments": arguments}));
        let text = frame.pointer("/result/content/0/text").and_then(Value::as_str)
            .unwrap_or_else(|| panic!("mcp {name}: no text result: {frame}"));
        serde_json::from_str(text).unwrap_or_else(|_| json!({"raw": text, "frame": frame}))
    }
}

impl Drop for Mcp {
    fn drop(&mut self) {
        drop(self.child.stdin.take());
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// All `%N` pane references at `pane_id`/`target`/`pane` keys anywhere in `value`.
fn pane_refs(value: &Value, out: &mut BTreeSet<String>) {
    match value {
        Value::Object(map) => {
            for (k, v) in map {
                if matches!(k.as_str(), "pane_id" | "target" | "pane") {
                    if let Some(s) = v.as_str().filter(|s| s.starts_with('%')) {
                        out.insert(s.to_string());
                    }
                }
                pane_refs(v, out);
            }
        }
        Value::Array(items) => items.iter().for_each(|v| pane_refs(v, out)),
        _ => {}
    }
}

fn descendants(root: u32) -> Vec<u32> {
    let out = Command::new("ps").args(["-A", "-o", "pid=,ppid="]).output().unwrap();
    let table: Vec<(u32, u32)> = String::from_utf8_lossy(&out.stdout).lines().filter_map(|l| {
        let mut it = l.split_whitespace();
        Some((it.next()?.parse().ok()?, it.next()?.parse().ok()?))
    }).collect();
    let mut found = vec![root];
    let mut i = 0;
    while i < found.len() {
        let parent = found[i];
        found.extend(table.iter().filter(|(_, pp)| *pp == parent).map(|(p, _)| *p));
        i += 1;
    }
    found
}

fn process_env_text(pid: u32) -> String {
    if let Ok(raw) = fs::read(format!("/proc/{pid}/environ")) {
        return String::from_utf8_lossy(&raw).replace('\0', "\n");
    }
    let out = Command::new("ps").args(["-E", "-ww", "-o", "command=", "-p", &pid.to_string()]).output().unwrap();
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// GraphEngineering shape: L1 owns `current` and has a stopped seat; a peer leader then
/// creates a second team with an explicit `--team`, which becomes the top-level team.
fn peer_team_on_top(rig: &Rig) -> (Leader, Leader) {
    let (l1, l2) = rig.dual_pi();
    ok(&rig.start_fake(&l1, "worker", Some("current")), "L1 start worker --team current");
    rig.wait_worker_ready(&l1.socket, "current", "worker");
    ok(&rig.as_leader(&l1, &["stop", "worker", "--team", "current", "--json"]), "L1 stop worker --team current");
    assert!(wait_until("worker pane gone after stop", SETTLE, || rig.agent_pane(&l1.socket, "current", "worker").is_none()),
        "fixture: stop must remove the worker pane");
    ok(&rig.start_fake(&l2, "t1", Some("morph")), "L2 start t1 --team morph");
    rig.wait_worker_ready(&l2.socket, "morph", "t1");
    (l1, l2)
}

/// Reactivate the stopped seat and return its live pane after the projection settles.
fn reactivate(rig: &Rig, l1: &Leader) -> Pane {
    ok(&rig.as_leader(l1, &["start", "worker", "--team", "current", "--json"]), "L1 reactivate worker --team current");
    rig.wait_worker_ready(&l1.socket, "current", "worker")
}

// ───────────────────────────── guards (must stay green) ─────────────────────────────

#[test]
fn g1_single_cli_pi_leader_keeps_current_and_bare_start_send_status_work() {
    let rig = Rig::new("g1");
    let l1 = rig.launch("pi", &[]);
    assert_eq!(assert_own_team_key(&l1, "G1"), "current", "single-leader workspaces keep team `current`");
    let started = ok(&rig.start_fake(&l1, "worker", None), "G1 bare start");
    assert_eq!(started["scope"]["team"], json!("current"), "{started}");
    let pane = rig.wait_worker_ready(&l1.socket, "current", "worker");
    let receipt = ok(&rig.as_leader(&l1, &["send", "worker", "G1-TOKEN", "--json"]), "G1 bare send");
    assert_live_receipt(&receipt, &pane.id, "G1 bare send");
    assert_eq!(status_names(&rig.as_leader(&l1, &["status", "--json"])), BTreeSet::from(["worker".to_string()]));
}

#[test]
fn g2_stored_only_json_receipt_contract_is_unchanged() {
    let rig = Rig::new("g2");
    let l1 = rig.launch("pi", &[]);
    ok(&rig.start_fake(&l1, "worker", None), "G2 start");
    rig.wait_worker_ready(&l1.socket, "current", "worker");
    ok(&rig.as_leader(&l1, &["stop", "worker", "--json"]), "G2 stop");
    let out = rig.as_leader(&l1, &["send", "worker", "G2-STORED", "--json"]);
    let receipt = ok(&out, "K1: a persisted-but-undelivered send keeps exit 0 / ok:true");
    assert_eq!(receipt["status"], json!("stored"), "{receipt}");
    assert_eq!(receipt["stored"], json!(true), "{receipt}");
    assert_eq!(receipt["live"], json!(false), "{receipt}");
    assert!(receipt["delivery_error"].as_str().is_some_and(|s| !s.is_empty()), "K1 keeps delivery_error: {receipt}");
}

#[test]
fn g3_worker_pane_bare_start_is_refused_and_creates_nothing() {
    let rig = Rig::new("g3");
    let l1 = rig.launch("pi", &[]);
    ok(&rig.start_fake(&l1, "worker", None), "G3 start");
    let pane = rig.wait_worker_ready(&l1.socket, "current", "worker");
    let mut env = rig.base_env();
    for (k, v) in [
        ("TMUX", format!("{},0,0", l1.socket)), ("TMUX_PANE", pane.id.clone()),
        ("TEAM_AGENT_ID", "worker".into()), ("TEAM_AGENT_AGENT_ID", "worker".into()),
        ("TEAM_AGENT_OWNER_TEAM_ID", "current".into()), ("TEAM_AGENT_WORKSPACE", rig.ws.display().to_string()),
        ("TEAM_AGENT_CALLER_PROVIDER", "fake".into()), ("TEAM_AGENT_CALLER_PANE_ID", pane.id.clone()),
        ("TEAM_AGENT_CALLER_TMUX_ENDPOINT", l1.socket.clone()),
    ] {
        env.insert(k.into(), v);
    }
    let mut args = vec!["start", "helper"];
    args.extend_from_slice(FAKE);
    args.push("--json");
    let out = rig.ta_in(&rig.ws, &env, &args);
    assert!(out.refused(), "D2: a worker pane's bare `start` must be refused: {out:?}");
    assert!(!agents_everywhere(&rig.state()).contains("helper"), "D2: refused start must not admit a seat: {}", rig.state());
    assert!(rig.panes(&l1.socket).iter().all(|p| p.window != "helper"), "D2: refused start must not spawn a window");
}

#[test]
fn g4_external_shell_selects_the_only_alive_team_and_refuses_ambiguity_with_team_choices() {
    let rig = Rig::new("g4");
    let (l1, l2) = rig.dual_pi();
    ok(&rig.start_fake(&l1, "worker", Some("current")), "G4 L1 start");
    rig.wait_worker_ready(&l1.socket, "current", "worker");
    ok(&rig.start_fake(&l2, "t1", Some("morph")), "G4 L2 start --team morph");
    rig.wait_worker_ready(&l2.socket, "morph", "t1");
    let mut args = vec!["start", "plainx"];
    args.extend_from_slice(FAKE);
    args.push("--json");
    let out = rig.plain(&args);
    let text = out.all();
    assert!(out.refused(), "D2: plain-shell bare start with 2+ alive teams must refuse: {out:?}");
    assert!(text.contains("--team") && text.contains("current") && text.contains("morph"),
        "D2: the ambiguity refusal must list the `--team <key>` choices: {text}");
    assert!(!agents_everywhere(&rig.state()).contains("plainx"), "D2: ambiguity refusal must not admit a seat");
}

#[test]
fn g5_explicit_team_owned_by_a_live_peer_leader_is_refused_for_start_and_stop() {
    let rig = Rig::new("g5");
    let (l1, l2) = rig.dual_pi();
    ok(&rig.start_fake(&l2, "t1", Some("morph")), "G5 L2 start --team morph");
    let t1 = rig.wait_worker_ready(&l2.socket, "morph", "t1");
    let start = rig.start_fake(&l1, "intruder", Some("morph"));
    assert!(start.refused(), "D3: L1 must not start a seat in a team owned by live L2: {start:?}");
    assert!(!agents_everywhere(&rig.state()).contains("intruder"), "D3: {}", rig.state());
    let stop = rig.as_leader(&l1, &["stop", "t1", "--team", "morph", "--json"]);
    assert!(stop.refused(), "D3: L1 must not stop a seat in a team owned by live L2: {stop:?}");
    assert!(rig.pane_alive(&l2.socket, &t1.id), "D3: the peer's seat must stay alive");
}

// ───────────────────────────── RED: launch, identity, discovery ─────────────────────────────

#[test]
fn r01_second_cli_pi_leader_gets_its_own_leader_team_and_uuid_before_provider_spawn() {
    let rig = Rig::new("r01");
    let (l1, l2) = rig.dual_pi();
    let t2 = assert_auto_leader_team(&l1, &l2);
    let (ext1, _) = pi_extension_definition(&l1);
    let (ext2, _) = pi_extension_definition(&l2);
    assert!(ext1.ends_with(".team/runtime/pi/current/leader/team-mcp.ts"), "L1 extension: {}", ext1.display());
    assert!(ext2.ends_with(format!(".team/runtime/pi/{t2}/leader/team-mcp.ts")),
        "K2: L2 must load its own team's extension, not `current`'s: {}", ext2.display());
    let state = rig.state();
    assert_eq!(state.pointer("/teams/current/team_owner/pane_id").and_then(Value::as_str), Some(l1.pane.as_str()),
        "L1 keeps ownership of current: {}", state["teams"]["current"]["team_owner"]);
    assert_eq!(state.pointer(&format!("/teams/{t2}/team_owner/pane_id")).and_then(Value::as_str), Some(l2.pane.as_str()),
        "D1: L2 owns its auto team: {}", state["teams"]);
    let refused: Vec<Value> = rig.events().into_iter()
        .filter(|e| e["event"] == "leader_receiver.claim_refused" && e["new_pane_id"].as_str() == Some(l2.pane.as_str())).collect();
    assert!(refused.is_empty(), "D1: launching a second leader is not a refused claim: {refused:?}");
}

#[test]
fn r02_leaders_json_lists_each_live_leader_with_physical_locators_and_never_the_nonce() {
    let rig = Rig::new("r02");
    let (l1, l2) = rig.dual_pi();
    let (out, entries) = rig.leaders();
    assert!(!out.stdout.contains("binding_nonce"), "K4: leaders output must never expose binding_nonce: {}", out.stdout);
    let live: Vec<&Value> = entries.iter().filter(|e| e["status"] == "LIVE").collect();
    assert_eq!(live.len(), 2, "K4: both live leaders of the workspace must be listed LIVE: {entries:?}");
    let keys: BTreeSet<&str> = live.iter().filter_map(|e| e["team_key"].as_str()).collect();
    assert_eq!(keys.len(), 2, "K4: the two leaders are distinguished by distinct team keys: {entries:?}");
    for leader in [&l1, &l2] {
        let entry = live.iter().find(|e| e["pane_id"].as_str() == Some(leader.pane.as_str()))
            .unwrap_or_else(|| panic!("K4: no LIVE entry carries pane_id {} : {entries:?}", leader.pane));
        assert_eq!(entry["provider"].as_str(), Some(leader.provider.as_str()), "K4 provider: {entry}");
        assert_eq!(entry["leader_session_uuid"].as_str(), Some(leader.uuid().as_str()), "K4 uuid: {entry}");
        let endpoint = entry["tmux_endpoint"].as_str().unwrap_or_else(|| panic!("K4 tmux_endpoint missing: {entry}"));
        assert_eq!(fs::canonicalize(endpoint).ok(), fs::canonicalize(&leader.socket).ok(), "K4 endpoint: {entry}");
        assert_eq!(entry["team_key"].as_str(), Some(leader.team().as_str()), "K4 team: {entry}");
        let hint = format!("{}::{}/leader", rig.ws.display(), leader.team());
        assert!(entry["send_hint"].as_str().is_some_and(|h| h.contains(&hint)), "K4 qualified send_hint {hint}: {entry}");
    }
}

#[test]
fn r03_same_name_worker_started_bare_by_each_leader_is_two_isolated_seats() {
    let rig = Rig::new("r03");
    let (l1, l2) = rig.dual_pi();
    let s1 = ok(&rig.start_fake(&l1, "worker", None), "L1 bare start worker");
    let s2 = ok(&rig.start_fake(&l2, "worker", None), "L2 bare start worker");
    assert_eq!(s1["scope"]["team"], json!("current"), "{s1}");
    let t2 = assert_auto_leader_team(&l1, &l2);
    assert_eq!(s2["scope"]["team"].as_str(), Some(t2.as_str()),
        "D1/D4: L2's bare start belongs to L2's team (GraphEngineering: `already_running` = L1's worker): {s2}");
    assert_ne!(s2["status"], json!("already_running"), "L2 must not see L1's seat as its own: {s2}");
    let p1 = rig.wait_worker_ready(&l1.socket, "current", "worker");
    let p2 = rig.wait_worker_ready(&l2.socket, &t2, "worker");
    assert_ne!(p1.id, p2.id, "two seats, two panes");
    assert_ne!(p1.session, p2.session, "each team has its own worker session");
    assert_eq!(rig.row("current", "worker")["pane_id"].as_str(), Some(p1.id.as_str()), "state current/worker");
    assert_eq!(rig.row(&t2, "worker")["pane_id"].as_str(), Some(p2.id.as_str()), "state {t2}/worker");
}

#[test]
fn r04_bare_status_and_unscoped_names_are_scoped_to_the_calling_leader() {
    let rig = Rig::new("r04");
    let (l1, l2) = rig.dual_pi();
    ok(&rig.start_fake(&l1, "worker", None), "L1 bare start worker");
    ok(&rig.start_fake(&l2, "bravo", None), "L2 bare start bravo (GraphEngineering: owner gate refused)");
    assert_eq!(status_names(&rig.as_leader(&l1, &["status", "--json"])), BTreeSet::from(["worker".to_string()]),
        "D4: L1's bare status lists only L1's team");
    assert_eq!(status_names(&rig.as_leader(&l2, &["status", "--json"])), BTreeSet::from(["bravo".to_string()]),
        "D4: L2's bare status lists only L2's team");
    let leak = rig.as_leader(&l1, &["send", "bravo", "R04-LEAK-TOKEN", "--json"]);
    assert!(leak.refused(), "D4: an unscoped peer-only name must be refused inside L1: {leak:?}");
    assert!(!all_inbox_text(&rig.ws).contains("R04-LEAK-TOKEN"), "D4: the refused send must not be stored anywhere");
}

#[test]
fn r05_bare_cli_send_from_each_leader_reaches_only_its_own_same_name_worker() {
    let rig = Rig::new("r05");
    let (l1, l2) = rig.dual_pi();
    ok(&rig.start_fake(&l1, "worker", None), "L1 start");
    ok(&rig.start_fake(&l2, "worker", None), "L2 start");
    let t2 = assert_own_team_key(&l2, "R05");
    let p1 = rig.wait_worker_ready(&l1.socket, "current", "worker");
    let p2 = rig.wait_worker_ready(&l2.socket, &t2, "worker");
    assert_ne!(p1.id, p2.id, "precondition: two isolated worker seats");
    let r1 = ok(&rig.as_leader(&l1, &["send", "worker", "R05-L1-TOKEN", "--json"]), "L1 send");
    let r2 = ok(&rig.as_leader(&l2, &["send", "worker", "R05-L2-TOKEN", "--json"]), "L2 send");
    assert_live_receipt(&r1, &p1.id, "L1 → its worker");
    assert_live_receipt(&r2, &p2.id, "L2 → its worker");
    assert!(wait_until("tokens rendered", SETTLE, || rig.pane_text(&l1.socket, &p1.id).contains("R05-L1-TOKEN")
        && rig.pane_text(&l2.socket, &p2.id).contains("R05-L2-TOKEN")), "both workers receive their own token");
    assert!(!rig.pane_text(&l1.socket, &p1.id).contains("R05-L2-TOKEN"), "L2's message leaked into L1's worker");
    assert!(!rig.pane_text(&l2.socket, &p2.id).contains("R05-L1-TOKEN"), "L1's message leaked into L2's worker");
}

#[test]
fn r06_each_pi_leader_mcp_binding_is_scoped_to_its_own_team() {
    let rig = Rig::new("r06");
    let (l1, l2) = rig.dual_pi();
    ok(&rig.start_fake(&l1, "worker", None), "L1 start");
    ok(&rig.start_fake(&l2, "bravo", None), "L2 start");
    assert_auto_leader_team(&l1, &l2);
    for (leader, peer, own_agent, peer_agent) in [(&l1, &l2, "worker", "bravo"), (&l2, &l1, "bravo", "worker")] {
        let team = assert_own_team_key(leader, "R06");
        let (path, definition) = pi_extension_definition(leader);
        assert!(path.to_string_lossy().contains(&format!("/.team/runtime/pi/{team}/leader/")),
            "K2: {} loads a per-team extension: {}", leader.pane, path.display());
        assert_eq!(definition.pointer("/env/TEAM_AGENT_OWNER_TEAM_ID").and_then(Value::as_str), Some(team.as_str()),
            "K2: the bound MCP server env names the leader's own team: {definition}");
        let pane = rig.wait_worker_ready(&leader.socket, &team, own_agent);
        let mut mcp = Mcp::spawn(&definition, &leader.env, &rig.ws);
        let status = mcp.tool("get_team_status", json!({}));
        let roster: BTreeSet<String> = status["agents"].as_object().map(|m| m.keys().cloned().collect()).unwrap_or_default();
        assert_eq!(roster, BTreeSet::from([own_agent.to_string()]), "D4: MCP get_team_status of {team}: {status}");
        let sent = mcp.tool("send_message", json!({"to": own_agent, "content": format!("R06-{team}-TOKEN")}));
        assert_live_receipt(&sent, &pane.id, &format!("MCP send_message from {team}"));
        let token = format!("R06-LEAK-FROM-{team}");
        let leak = mcp.tool("send_message", json!({"to": peer_agent, "content": token}));
        assert!(leak["ok"] != json!(true), "D4: MCP send to a peer-only name must be refused: {leak}");
        assert!(!inbox_text(&rig.ws, &peer.team(), peer_agent).contains(&token),
            "D4: a refused MCP send must never reach the peer seat's inbox");
    }
}

#[test]
fn r07_poisoned_terminal_and_tmux_global_env_cannot_pull_the_second_leader_into_the_first_team() {
    let rig = Rig::new("r07");
    let l1 = rig.launch("pi", &[]);
    let poison = [
        ("TEAM_AGENT_TEAM_ID", "current"), ("TEAM_AGENT_OWNER_TEAM_ID", "current"),
        ("TEAM_AGENT_LEADER_SESSION_UUID", l1.uuid().leak() as &str),
        ("PI_SESSION_FILE", POISON_SESSION_FILE), ("PI_SESSION_ID", POISON_SESSION_ID), ("PI_MODEL", POISON_MODEL),
    ];
    for (k, v) in poison {
        let out = Command::new(&rig.tmux).args(["-S", &l1.socket, "set-environment", "-g", k, v]).output().unwrap();
        assert!(out.status.success(), "fixture: poison tmux global env: {out:?}");
    }
    let l2 = rig.launch("pi", &poison);
    let t2 = assert_auto_leader_team(&l1, &l2);
    for (k, v) in [("PI_SESSION_FILE", POISON_SESSION_FILE), ("PI_SESSION_ID", POISON_SESSION_ID), ("PI_MODEL", POISON_MODEL)] {
        assert_ne!(l2.env.get(k).map(String::as_str), Some(v), "K2: inherited {k} must be scrubbed from the leader provider env");
    }
    if let Some(owner) = l2.env.get("TEAM_AGENT_OWNER_TEAM_ID") {
        assert_eq!(owner, &t2, "K2: a foreign TEAM_AGENT_OWNER_TEAM_ID must not survive into L2");
    }
    let (_, definition) = pi_extension_definition(&l2);
    assert_eq!(definition.pointer("/env/TEAM_AGENT_OWNER_TEAM_ID").and_then(Value::as_str), Some(t2.as_str()),
        "K2: L2's MCP binding names L2's team despite the poisoned env: {definition}");
}

#[test]
fn r08_external_qualified_leader_address_reaches_exactly_that_leaders_team() {
    let rig = Rig::new("r08");
    let (l1, l2) = rig.dual_pi();
    let t2 = assert_auto_leader_team(&l1, &l2);
    let qualified = format!("{}::{t2}/leader", rig.ws.display());
    ok(&rig.plain(&["send", &qualified, "R08-QUALIFIED-T2", "--json"]), "D4: external qualified send to L2");
    ok(&rig.plain(&["send", "leader", "R08-SCOPED-T2", "--workspace", rig.ws.to_str().unwrap(), "--team", &t2, "--json"]),
        "D4: external --workspace/--team send to L2");
    let current = format!("{}::current/leader", rig.ws.display());
    ok(&rig.plain(&["send", &current, "R08-QUALIFIED-CURRENT", "--json"]), "D4: external qualified send to L1");
    let t2_inbox = inbox_text(&rig.ws, &t2, "leader");
    let current_inbox = inbox_text(&rig.ws, "current", "leader");
    assert!(t2_inbox.contains("R08-QUALIFIED-T2") && t2_inbox.contains("R08-SCOPED-T2"), "L2's leader inbox: {t2_inbox}");
    assert!(!current_inbox.contains("R08-QUALIFIED-T2") && !current_inbox.contains("R08-SCOPED-T2"), "L1 got L2's mail");
    assert!(current_inbox.contains("R08-QUALIFIED-CURRENT") && !t2_inbox.contains("R08-QUALIFIED-CURRENT"), "L1's mail");
}

#[test]
fn r09_qualified_leader_address_and_live_status_survive_a_current_team_in_another_workspace() {
    let rig = Rig::new("r09");
    let l1 = rig.launch("pi", &[]);
    let other = rig.workspace("ws2");
    let _other_leader = rig.launch_in(&other, "pi", &[]);
    let (_, entries) = rig.leaders();
    let entry = entries.iter().find(|e| e["team_key"] == "current")
        .unwrap_or_else(|| panic!("M1.9: this workspace's leader must be listed: {entries:?}"));
    assert_eq!(entry["status"], json!("LIVE"),
        "M1.9: a live leader stays LIVE when another workspace also has `current` (GraphEngineering: AMBIGUOUS): {entry}");
    let qualified = format!("{}::current/leader", rig.ws.display());
    ok(&rig.plain(&["send", &qualified, "R09-QUALIFIED", "--json"]), "M1.9 qualified send");
    assert!(inbox_text(&rig.ws, "current", "leader").contains("R09-QUALIFIED"), "qualified send must land in this workspace");
    assert!(!inbox_text(&other, "current", "leader").contains("R09-QUALIFIED"), "qualified send leaked to the other workspace");
    let _ = l1;
}

#[test]
fn r10_inside_a_leader_bare_commands_stay_on_its_own_team_when_a_peer_team_is_alive() {
    let rig = Rig::new("r10");
    let (l1, l2) = rig.dual_pi();
    ok(&rig.start_fake(&l1, "worker", None), "L1 bare start worker");
    rig.wait_worker_ready(&l1.socket, "current", "worker");
    ok(&rig.start_fake(&l2, "t1", Some("morph")), "L2 start t1 --team morph");
    rig.wait_worker_ready(&l2.socket, "morph", "t1");
    let status = rig.as_leader(&l1, &["status", "--json"]);
    assert_eq!(status.code, 0, "D4: L1's bare status must not fail with multiple alive teams: {status:?}");
    assert_eq!(status_names(&status), BTreeSet::from(["worker".to_string()]), "D4: L1 sees only its own team");
    let stop = rig.as_leader(&l1, &["stop", "worker", "--json"]);
    ok(&stop, "D4: L1's bare stop targets its own team (GraphEngineering: owner gate of the peer team)");
    assert!(wait_until("L1 worker stopped", SETTLE, || rig.agent_pane(&l1.socket, "current", "worker").is_none()), "stopped");
    let start = ok(&rig.start_fake(&l1, "worker", None), "D4: L1's bare start targets its own team");
    assert_eq!(start["scope"]["team"], json!("current"), "{start}");
    rig.wait_worker_ready(&l1.socket, "current", "worker");
    assert!(rig.agent_pane(&l2.socket, "morph", "t1").is_some(), "the peer team is untouched");
}

#[test]
fn g6_dead_provider_shell_is_not_listed_as_a_live_leader() {
    let rig = Rig::new("r11");
    let l1 = rig.launch("pi", &[]);
    let (_, before) = rig.leaders();
    assert!(before.iter().any(|e| e["status"] == "LIVE"), "precondition: live leader listed: {before:?}");
    rig.kill_provider(&l1);
    assert!(rig.pane_alive(&l1.socket, &l1.pane), "fixture: the provider-exited keepalive shell keeps the pane (GraphEngineering %100)");
    let (out, entries) = rig.leaders();
    assert!(entries.iter().all(|e| e["status"] != "LIVE"),
        "K4: a pane whose provider exited is not a live leader: {}", out.stdout);
}

#[test]
fn r12_bare_relaunch_after_a_leader_exit_allocates_a_new_team_and_never_hijacks_the_old_one() {
    let rig = Rig::new("r12");
    let (l1, l2) = rig.dual_pi();
    let t2 = assert_auto_leader_team(&l1, &l2);
    let owner_before = rig.state()["teams"][&t2]["team_owner"].clone();
    rig.kill_provider(&l2);
    let l3 = rig.launch("pi", &[]);
    let t3 = l3.team();
    assert!(t3 != t2 && t3 != "current" && t3.starts_with("leader-"),
        "D5: a bare relaunch is a new leader instance with a new team; got {t3:?} (old {t2:?})");
    assert_eq!(rig.state()["teams"][&t2]["team_owner"], owner_before, "D5: the exited leader's team is not hijacked");
    assert_eq!(rig.state().pointer("/teams/current/team_owner/pane_id").and_then(Value::as_str), Some(l1.pane.as_str()),
        "L1 still owns current");
}

// ───────────────────────────── RED: state integrity (GraphEngineering replay) ─────────────────────────────

#[test]
fn r13_reactivating_a_stopped_seat_survives_a_peer_team_becoming_top_level() {
    let rig = Rig::new("r13");
    let (l1, _l2) = peer_team_on_top(&rig);
    let pane = reactivate(&rig, &l1);
    let settled = wait_until("state row converges", SETTLE, || {
        let row = rig.row("current", "worker");
        row["status"] == "running" && row["pane_id"].as_str() == Some(pane.id.as_str())
    });
    let row = rig.row("current", "worker");
    assert!(settled && row["explicit_stop"] != json!(true),
        "persist authority: the reactivated seat must be running on {} in teams.current (GraphEngineering: reverted to \
         stopped/explicit_stop by the top-level peer team): {row}", pane.id);
    if let Some(seat) = rig.seat_endpoint("current", "worker") {
        assert_eq!(seat, pane.id, "station seat and state.json must agree");
    }
    let receipt = ok(&rig.as_leader(&l1, &["send", "worker", "R13-AFTER-REACTIVATE", "--team", "current", "--json"]), "send");
    assert_live_receipt(&receipt, &pane.id, "send after reactivation (GraphEngineering: recipient_not_running)");
}

#[test]
fn r14_second_start_of_a_running_seat_is_a_no_op_and_never_orphan_kills_it() {
    let rig = Rig::new("r14");
    let (l1, _l2) = peer_team_on_top(&rig);
    let pane = reactivate(&rig, &l1);
    let mark = rig.events().len();
    let again = ok(&rig.as_leader(&l1, &["start", "worker", "--team", "current", "--json"]), "second start");
    std::thread::sleep(Duration::from_secs(2));
    let kills: Vec<Value> = rig.events().into_iter().skip(mark)
        .filter(|e| e["event"] == "start_agent.orphan_stop" && e["agent_id"] == "worker").collect();
    assert!(kills.is_empty(), "a running, seat-bound pane is never an orphan: {kills:?} receipt={again}");
    assert!(rig.pane_alive(&l1.socket, &pane.id), "the live worker pane {} must survive a repeated start", pane.id);
    assert_eq!(rig.agent_pane(&l1.socket, "current", "worker").map(|p| p.id), Some(pane.id.clone()), "no replacement pane");
}

#[test]
fn r15_restart_rebinds_state_seat_and_receipt_to_the_new_pane_while_a_peer_team_is_top_level() {
    let rig = Rig::new("r15");
    let (l1, _l2) = peer_team_on_top(&rig);
    let old = reactivate(&rig, &l1);
    let out = rig.as_leader(&l1, &["restart", "worker", "--team", "current", "--json"]);
    let receipt = ok(&out, "restart worker --team current (GraphEngineering shape: state save conflict)");
    let new = rig.wait_worker_ready(&l1.socket, "current", "worker");
    assert!(wait_until("state row rebinds", SETTLE, || rig.row("current", "worker")["pane_id"].as_str() == Some(new.id.as_str())),
        "state must name the new pane {}: {}", new.id, rig.row("current", "worker"));
    if let Some(seat) = rig.seat_endpoint("current", "worker") {
        assert_eq!(seat, new.id, "station seat must name the new pane");
    }
    let mut refs = BTreeSet::new();
    pane_refs(&receipt, &mut refs);
    refs.remove(&old.id);
    assert!(refs.iter().all(|p| p == &new.id) || refs.is_empty(),
        "every pane the restart receipt reports as current must be the new live pane {}: {refs:?} {receipt}", new.id);
}

#[test]
fn r16_split_brain_projection_is_repaired_by_start_without_replaying_stored_history() {
    let rig = Rig::new("r16");
    let l1 = rig.launch("pi", &[]);
    ok(&rig.start_fake(&l1, "worker", Some("current")), "start");
    let pane = rig.wait_worker_ready(&l1.socket, "current", "worker");
    // GraphEngineering projection: the live seat-bound pane exists, state.json says stopped.
    let path = rig.ws.join(".team/runtime/state.json");
    let mut state = rig.state();
    assert!(state.pointer("/teams/current/agents/worker").is_some(), "fixture: worker row present: {state}");
    for pointer in ["/teams/current/agents/worker", "/agents/worker"] {
        if let Some(row) = state.pointer_mut(pointer).and_then(Value::as_object_mut) {
            row.insert("status".into(), json!("stopped"));
            row.insert("explicit_stop".into(), json!(true));
            row.remove("pane_id");
            row.remove("pane_pid");
        }
    }
    fs::write(&path, serde_json::to_vec_pretty(&state).unwrap()).unwrap();
    let old = rig.as_leader(&l1, &["send", "worker", "R16-OLD-HISTORY", "--team", "current", "--json"]).json();
    let repaired = rig.as_leader(&l1, &["start", "worker", "--team", "current", "--json"]);
    ok(&repaired, "D6: start repairs the split-brain projection");
    assert!(rig.pane_alive(&l1.socket, &pane.id), "D6: repair must not kill the live seat-bound pane {}: {repaired:?}", pane.id);
    assert!(wait_until("row repaired", SETTLE, || {
        let row = rig.row("current", "worker");
        row["status"] == "running" && row["pane_id"].as_str() == Some(pane.id.as_str())
    }), "D6: state.json must re-sync to the live pane {}: {}", pane.id, rig.row("current", "worker"));
    let new = ok(&rig.as_leader(&l1, &["send", "worker", "R16-NEW", "--team", "current", "--json"]), "send after repair");
    assert_live_receipt(&new, &pane.id, "D6: subsequent sends deliver live");
    if old["live"] != json!(true) {
        std::thread::sleep(Duration::from_secs(2));
        assert!(!rig.pane_text(&l1.socket, &pane.id).contains("R16-OLD-HISTORY"),
            "D6: stored-only history must not be auto-replayed into the pane");
    }
}

#[test]
fn r17_stored_only_human_receipt_says_not_delivered_live_with_next_action() {
    let rig = Rig::new("r17");
    let l1 = rig.launch("pi", &[]);
    ok(&rig.start_fake(&l1, "worker", None), "start");
    rig.wait_worker_ready(&l1.socket, "current", "worker");
    ok(&rig.as_leader(&l1, &["stop", "worker", "--json"]), "stop");
    let out = rig.as_leader(&l1, &["send", "worker", "R17-STORED"]);
    assert_eq!(out.code, 0, "K1: exit code stays 0: {out:?}");
    assert!(out.all().contains(NOT_LIVE_PHRASE), "K1: human output must say `{NOT_LIVE_PHRASE}`: {}", out.all());
    assert!(out.all().contains("Next Action"), "K1: human output must carry a Next Action line: {}", out.all());
}

#[test]
fn r18_bypass_seat_spec_source_path_stays_in_its_own_team_while_a_peer_team_is_top_level() {
    let rig = Rig::new("r18");
    let (l1, _l2) = peer_team_on_top(&rig);
    ok(&rig.as_leader(&l1, &["start", "byp", "--team", "current", "--provider", "fake", "--model", "fake", "--bypass", "true", "--json"]),
        "start bypass seat in current");
    rig.wait_worker_ready(&l1.socket, "current", "byp");
    let paths: Vec<String> = rig.events().into_iter()
        .filter(|e| e["event"] == "worker.spawn_dangerously_skip_permissions" && e["agent_id"] == "byp")
        .filter_map(|e| e["spec_source_path"].as_str().map(str::to_string)).collect();
    assert!(!paths.is_empty(), "precondition: bypass audit event present");
    assert!(paths.iter().all(|p| !p.contains("/start-source/morph/") && (p.is_empty() || p.contains("/current/"))),
        "M2.9: a seat of team current must never cite the peer team's start-source: {paths:?}");
}

// ───────────────────────────── RED: provider launch isolation (K2) ─────────────────────────────

fn mcp_config_servers(raw: &str) -> Value {
    let text = if raw.trim_start().starts_with('{') { raw.to_string() } else {
        fs::read_to_string(raw).unwrap_or_else(|e| panic!("--mcp-config {raw}: {e}"))
    };
    serde_json::from_str::<Value>(&text).expect("mcp config JSON")["mcpServers"].clone()
}

#[test]
fn r19_claude_cli_leader_gets_a_per_launch_team_mcp_config_and_no_shared_config_write() {
    let rig = Rig::new("r19");
    let l1 = rig.launch("pi", &[]);
    let l2 = rig.launch("claude", &[]);
    let t2 = assert_auto_leader_team(&l1, &l2);
    let raw = l2.arg_after("--mcp-config")
        .unwrap_or_else(|| panic!("K2: a claude leader needs a per-launch --mcp-config: {:?}", l2.argv));
    let servers = mcp_config_servers(&raw);
    let bound = servers.as_object().into_iter().flatten().any(|(_, s)| {
        s["env"]["TEAM_AGENT_OWNER_TEAM_ID"].as_str() == Some(t2.as_str())
            && s["args"].as_array().is_some_and(|a| a.iter().any(|x| x == "mcp-server"))
    });
    assert!(bound, "K2: the per-launch config binds team {t2} to `team-agent mcp-server`: {servers}");
    assert!(!rig.ws.join(".mcp.json").exists(), "K2: no project-scope MCP file may be written");
    let claude_json = rig.home.join(".claude.json");
    assert!(!fs::read_to_string(&claude_json).unwrap_or_default().contains("TEAM_AGENT_OWNER_TEAM_ID"),
        "K2: ~/.claude.json must not carry a shared team binding");
}

#[test]
fn r20_codex_cli_leader_gets_a_per_launch_team_mcp_override_and_no_global_config_write() {
    let rig = Rig::new("r20");
    let l1 = rig.launch("pi", &[]);
    let l2 = rig.launch("codex", &[]);
    let t2 = assert_auto_leader_team(&l1, &l2);
    let overrides: Vec<&String> = l2.argv.windows(2).filter(|w| w[0] == "-c").map(|w| &w[1]).collect();
    assert!(overrides.iter().any(|o| o.starts_with("mcp_servers.")), "K2: codex needs a per-launch mcp_servers override: {:?}", l2.argv);
    let joined = overrides.iter().map(|s| s.as_str()).collect::<Vec<_>>().join("\n");
    assert!(joined.contains("TEAM_AGENT_OWNER_TEAM_ID") && joined.contains(&t2),
        "K2: the override binds TEAM_AGENT_OWNER_TEAM_ID={t2}: {joined}");
    let config = rig.home.join(".codex/config.toml");
    assert!(!fs::read_to_string(&config).unwrap_or_default().contains("TEAM_AGENT_OWNER_TEAM_ID"),
        "K2: ~/.codex/config.toml must not carry a shared team binding");
}

#[test]
fn r21_worker_spawn_never_inherits_the_leader_pi_session_identity() {
    let rig = Rig::new("r21");
    let l1 = rig.launch("pi", &[]);
    let mut args = vec!["start", "worker"];
    args.extend_from_slice(FAKE);
    args.push("--json");
    // A real Pi tool shell carries the leader's own session identity.
    let out = rig.as_leader_with(&l1, &[("PI_SESSION_FILE", POISON_SESSION_FILE), ("PI_SESSION_ID", POISON_SESSION_ID), ("PI_MODEL", POISON_MODEL)], &args);
    ok(&out, "start from a Pi tool shell");
    let pane = rig.wait_worker_ready(&l1.socket, "current", "worker");
    let overlay: Vec<String> = rig.events().into_iter()
        .filter(|e| e["event"] == "provider.worker.spawn_argv" && e["agent_id"] == "worker")
        .flat_map(|e| e["env_overlay_keys"].as_array().cloned().unwrap_or_default())
        .filter_map(|k| k.as_str().map(str::to_string)).collect();
    for key in ["PI_SESSION_FILE", "PI_SESSION_ID", "PI_MODEL"] {
        assert!(!overlay.contains(&key.to_string()), "M2.10: worker env overlay must not carry the leader's {key}: {overlay:?}");
    }
    for pid in descendants(pane.pid) {
        let env = process_env_text(pid);
        for value in [POISON_SESSION_FILE, POISON_SESSION_ID, POISON_MODEL] {
            assert!(!env.contains(value), "M2.10: worker process {pid} inherited the leader's Pi identity ({value})");
        }
    }
}

// ───────────────────────────── RED: cross-team claim hazard (live run #3 finding) ─────────────────────────────

/// Leader-receiver, claim and leader-registry audit events that name `team`
/// (by `team_id`, `team_key`, `team`, or the registry file path `..__<team>.json`).
fn receiver_events_for(events: &[Value], team: &str) -> Vec<Value> {
    let suffix = format!("__{team}.json");
    events.iter().filter(|e| {
        let name = e["event"].as_str().unwrap_or_default();
        let names_team = ["team_id", "team_key", "team"].iter().any(|k| e[*k].as_str() == Some(team))
            || e["path"].as_str().is_some_and(|p| p.ends_with(&suffix));
        names_team && (name.starts_with("leader_receiver.") || name.starts_with("leader_registry.") || name.contains("claim"))
    }).cloned().collect()
}

fn events_since(rig: &Rig, mark: usize) -> Vec<Value> {
    rig.events().into_iter().skip(mark).collect()
}

fn registry_entries(rig: &Rig, team: &str) -> BTreeMap<String, String> {
    let suffix = format!("__{team}.json");
    fs::read_dir(rig.home.join(".team-agent/leaders")).into_iter().flatten().flatten()
        .map(|e| e.path())
        .filter(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.ends_with(&suffix)))
        .map(|p| (p.display().to_string(), fs::read_to_string(&p).unwrap_or_default()))
        .collect()
}

/// Live run #3 (c59f5f46): every bare `start`/`stop` from one leader evaluated a leader claim on
/// the PEER team (`leader_receiver.claim_refused reason=previous_owner_alive_refused`, team_id =
/// peer, new_pane_id = caller). It was refused only because the peer leader was alive. Phase B
/// repeats it after the peer leader's provider has exited (GraphEngineering `%100` shape) while
/// its team's worker is still up, where a stray claim would no longer be refused.
#[test]
fn r22_lifecycle_commands_never_claim_or_touch_a_peer_team_whose_leader_exited() {
    let rig = Rig::new("r22");
    let (l1, l2) = rig.dual_pi();
    let t2 = assert_auto_leader_team(&l1, &l2);
    ok(&rig.start_fake(&l1, "worker", None), "L1 bare start worker");
    rig.wait_worker_ready(&l1.socket, "current", "worker");

    // Phase 0: L2 (peer of `current`) writes last, so its team becomes the top-level projection.
    let mark = rig.events().len();
    ok(&rig.start_fake(&l2, "w2", None), "L2 bare start w2");
    let w2 = rig.wait_worker_ready(&l2.socket, &t2, "w2");
    let phase0 = receiver_events_for(&events_since(&rig, mark), "current");

    // Phase A: both leaders alive; L1 acts on its own team.
    let mark = rig.events().len();
    let start = ok(&rig.start_fake(&l1, "helper", None), "D7: L1 bare start helper");
    assert_eq!(start["scope"]["team"], json!("current"), "D7: L1's bare start stays on its own team: {start}");
    rig.wait_worker_ready(&l1.socket, "current", "helper");
    let phase_a = receiver_events_for(&events_since(&rig, mark), &t2);

    // Phase B: L2's provider exits while T2's worker stays up; L1 keeps acting on its own team.
    rig.kill_provider(&l2);
    wait_until("L2 observed as exited", SETTLE, || {
        rig.leaders().1.iter().all(|e| e["pane_id"].as_str() != Some(l2.pane.as_str()) || e["status"] != "LIVE")
    });
    assert!(rig.pane_alive(&l2.socket, &w2.id), "M6.3: the exit of L2's provider must not kill its team's worker {}", w2.id);
    let team_before = rig.state()["teams"][&t2].clone();
    let registry_before = registry_entries(&rig, &t2);
    let mark = rig.events().len();
    ok(&rig.as_leader(&l1, &["stop", "helper", "--json"]), "D7: L1 bare stop helper");
    let team_stop = ok(&rig.as_leader(&l1, &["stop", "--json"]), "D7: L1 bare team stop");
    assert!(wait_until("L1 team panes gone", SETTLE, || rig.agent_pane(&l1.socket, "current", "worker").is_none()),
        "D7: L1's bare team stop must stop its own team: {team_stop}");
    std::thread::sleep(Duration::from_secs(2));
    let phase_b = receiver_events_for(&events_since(&rig, mark), &t2);

    assert!(phase0.is_empty() && phase_a.is_empty() && phase_b.is_empty(),
        "D7: a leader's bare lifecycle commands must never evaluate, claim, rebind, register or unregister the PEER \
         team's leader receiver (live run #3: `leader_receiver.claim_refused` team_id=<peer> new_pane_id=<caller> on every \
         start/stop):\n  L2 start -> events naming `current`: {phase0:?}\n  L1 start, peer alive -> events naming {t2}: \
         {phase_a:?}\n  L1 stop + team stop, peer exited -> events naming {t2}: {phase_b:?}");
    let team_after = rig.state()["teams"][&t2].clone();
    assert_eq!(team_after["team_owner"], team_before["team_owner"],
        "D7: the exited peer's team_owner must stay unchanged (no cross-team claim)");
    assert_eq!(team_after["leader_receiver"], team_before["leader_receiver"],
        "D7: the exited peer's leader_receiver must stay unchanged");
    assert_eq!(registry_entries(&rig, &t2), registry_before, "D7: the peer team's leader registry entry must stay unchanged");
    assert!(rig.pane_alive(&l2.socket, &w2.id), "D7: L1's lifecycle commands must not touch the peer team's worker {}", w2.id);
    assert_eq!(rig.row(&t2, "w2")["status"], json!("running"), "D7: the peer team's worker row stays running: {}", rig.row(&t2, "w2"));
}
