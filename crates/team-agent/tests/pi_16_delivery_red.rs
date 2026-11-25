//! Pi 1.6-scoped leader delivery: RED acceptance suite (physical, isolated, real Pi 1.6.0 and 1.5.2).
//!
//! Incident: a worker `send_message` to a Pi leader reports `live:true`. Delivery is a tmux paste + Enter into the
//! leader's prompt dock. Reproduced defects:
//!   - Case B: a half-typed human draft is concatenated with the worker text and submitted as one turn.
//!   - Gap B: a message queued while Pi is busy is pulled back into the editor on Escape, merged into the draft and
//!     never reaches the model.
//! A first native implementation (#442) was reverted (#443): it failed closed with "pi native receiver unavailable"
//! for leaders that were already running before the upgrade.
//!
//! Scope of this contract:
//!   - Native delivery is for Pi 1.6.x only. It must use the existing Team MCP extension (`team-mcp.ts`, materialized
//!     from `pi_mcp_extension.js`): no new extension file, no `--tui-mode`, no change to the user's Pi config.
//!   - Every other leader keeps the existing physical `direct_tmux` delivery:
//!       * any non-1.6 Pi;
//!       * a Pi 1.6 leader started by a pre-upgrade `team-agent` (no native receiver handshake);
//!       * a legacy bare `leader` without `leader_receiver` (covered by `space_station_m1_red`).
//!
//! Frozen rules (each test prints every violated rule with its observation):
//!   T0 GUARD (Pi 1.6.0 and 1.5.2): native fullscreen, no UI-altering argv/env, exactly one `-e` extension (the Team
//!      MCP `team-mcp.ts`), no other extension source file created, scratch `~/.pi` byte-identical outside
//!      `agent/sessions` (Pi's own auth/models stores and managed fd/rg excepted), no project `.pi` created.
//!   T1 (1.6.0) idle + human draft with the cursor inside it: the draft and the cursor position are untouched, the
//!      draft is never submitted, the message is its own user turn, and the reply is `accepted:true`.
//!   T2 (1.6.0) busy + draft: the reply is `submission:native_pending`. While busy, nothing is in Pi's
//!      steering/followUp queue, there is no Steering/Follow-up preview, the draft is intact and the message is not
//!      ingested. After `agent_settled{aborted:false}` it is ingested exactly once as its own turn.
//!   T3 (1.6.0) busy + draft + Escape: `native_pending`. After `agent_settled{aborted:true}` the editor holds exactly
//!      the draft (Gap B), nothing is ingested, and the message is in the durable inbox. The next normal settle
//!      releases it as its own turn.
//!   T4 (1.6.0) `/new` and `/reload` before delivery: ingested exactly once in the live session, the draft is intact,
//!      and no stale-handle or extension error is visible.
//!   T5 (1.6.0) honest ACK. Any `accepted:true` must already be in the Pi transcript when the reply returns (1 s
//!      grace); this applies to every test. An idle delivery must be `accepted:true`.
//!   T6 (1.6.0) abort-hold scope: after an aborted run has settled and Pi is idle, a NEW message is delivered
//!      immediately with `accepted:true`.
//!   T7 (1.5.2) non-1.6 fallback: idle and busy deliveries use physical `direct_tmux` (`live:true`,
//!      `delivery:"physical"`, `submission:"physical"`, no `delivery_error`) and are ingested exactly once.
//!   T8 (1.6.0) rolling upgrade: the leader is launched by the pinned pre-upgrade `team-agent`
//!      (`PREUPGRADE_SHA`), then the PATH `team-agent` is replaced by the candidate (in-place upgrade) and the
//!      candidate sends. The send must not fail with "pi native receiver unavailable". It must use physical delivery
//!      (`live:true`, `delivery:"physical"`, `submission:"physical"`). The SAME running Pi process must ingest it
//!      exactly once, without restart.
//!   T9 (1.6.0, amendment) receiver refresh: after the documented `team-agent claim-leader` runs from the leader
//!      pane (same owner/epoch/pane, new `attached_at`), an idle delivery with a human draft is still
//!      `accepted:true`, the draft is untouched and never submitted, and the message is its own turn.
//!
//! Run (macOS/Linux with tmux, node, npm, git):
//!   cargo test -p team-agent --test pi_16_delivery_red -- --ignored --test-threads=1
//! Inputs:
//!   - Pi: `TEAM_AGENT_PI_RED_PI_1_6_0` / `TEAM_AGENT_PI_RED_PI_1_5_2` point at a `pi` executable. Otherwise
//!     `@earendil-works/pi-coding-agent@<v>` is npm-installed once into `$TEAM_AGENT_PI_RED_CACHE/pi-<v>` (default
//!     cache `/private/tmp/ta-pi-red-cache`).
//!   - Pre-upgrade team-agent (T8): `TEAM_AGENT_PI_RED_PREUPGRADE_TEAM_AGENT` points at a `team-agent` binary built
//!     from `PREUPGRADE_SHA`. Otherwise it is built once from `git archive PREUPGRADE_SHA` into
//!     `$TEAM_AGENT_PI_RED_CACHE/team-agent-<sha12>/`.
//! Isolation: scratch HOME (Pi config = scratch `~/.pi/agent`), scratch workspace, own tmux servers, in-process
//! deterministic mock model (prompt with `SLOW` streams for 14 s, otherwise instant `MOCK-ACK`). A scratch-only
//! observer extension reports editor text, idle/pending state, settle events and user message ingestion.
//! Receipts: `$CARGO_TARGET_TMPDIR/pi-16-delivery-red/<test>-pi-<v>.json`. `TEAM_AGENT_PI_RED_KEEP=1` keeps scratch.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use serial_test::serial;

/// The mock model streams one token per second for this long when the last user text contains `SLOW`.
const SLOW_SECONDS: u64 = 14;
const SENDER: &str = "backend-dev";
/// Last origin/main without any native Pi receiver (revert #443 of #442; same extension as 98f55d0b8).
const PREUPGRADE_SHA: &str = "e341a3d1bd9264366af22d9e121fb01f9a309e7e";
const NATIVE_UNAVAILABLE: &str = "native receiver unavailable";
/// Every human draft starts with this marker, so a split draft (text spliced in at the cursor) is still detected.
const DRAFT_MARK: &str = "HUMAN-DRAFT";

fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs_f64()
}

fn unique() -> String {
    format!(
        "{}{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .subsec_nanos()
    )
}

fn scratch_base() -> PathBuf {
    if let Some(root) = std::env::var_os("TEAM_AGENT_PI_RED_ROOT") {
        return PathBuf::from(root);
    }
    let tmp = PathBuf::from("/private/tmp");
    if tmp.is_dir() {
        tmp
    } else {
        std::env::temp_dir()
    }
}

fn cache_root() -> PathBuf {
    std::env::var_os("TEAM_AGENT_PI_RED_CACHE")
        .map(PathBuf::from)
        .unwrap_or_else(|| scratch_base().join("ta-pi-red-cache"))
}

fn candidate() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_team-agent"))
}

fn pi_bin(version: &str) -> PathBuf {
    let key = format!("TEAM_AGENT_PI_RED_PI_{}", version.replace('.', "_"));
    if let Some(path) = std::env::var_os(&key) {
        let path = PathBuf::from(path);
        assert!(
            path.exists(),
            "PREREQUISITE: {key}={} does not exist",
            path.display()
        );
        return path;
    }
    let cache = cache_root().join(format!("pi-{version}"));
    let bin = cache.join("node_modules/.bin/pi");
    if !bin.exists() {
        std::fs::create_dir_all(&cache).unwrap();
        std::fs::write(cache.join("package.json"), "{\"private\":true}").unwrap();
        let out = Command::new("npm")
            .args([
                "install",
                "--no-audit",
                "--no-fund",
                "--ignore-scripts",
                "--silent",
                &format!("@earendil-works/pi-coding-agent@{version}"),
            ])
            .current_dir(&cache)
            .output();
        assert!(matches!(&out, Ok(o) if o.status.success()) && bin.exists(),
            "PREREQUISITE: could not install Pi {version} into {} (set {key} to an existing pi executable): {out:?}",
            cache.display());
    }
    bin
}

/// The `team-agent` that started already-running leaders before the upgrade (T8).
fn preupgrade_team_agent() -> PathBuf {
    const KEY: &str = "TEAM_AGENT_PI_RED_PREUPGRADE_TEAM_AGENT";
    if let Some(path) = std::env::var_os(KEY) {
        let path = PathBuf::from(path);
        assert!(
            path.exists(),
            "PREREQUISITE: {KEY}={} does not exist",
            path.display()
        );
        return path;
    }
    let dir = cache_root().join(format!("team-agent-{}", &PREUPGRADE_SHA[..12]));
    let bin = dir.join("team-agent");
    if bin.exists() {
        return bin;
    }
    let src = dir.join("src");
    let _ = std::fs::remove_dir_all(&src);
    std::fs::create_dir_all(&src).unwrap();
    let mut archive = Command::new("git")
        .arg("-C")
        .arg(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."))
        .args(["archive", "--format=tar", PREUPGRADE_SHA])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let untar = Command::new("tar")
        .arg("-x")
        .arg("-C")
        .arg(&src)
        .stdin(archive.stdout.take().unwrap())
        .status()
        .unwrap();
    assert!(archive.wait().unwrap().success() && untar.success(),
        "PREREQUISITE: `git archive {PREUPGRADE_SHA}` failed (fetch that commit or set {KEY} to its team-agent binary)");
    let target = dir.join("target");
    let built = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()))
        .args(["build", "-p", "team-agent", "--bin", "team-agent"])
        .current_dir(&src)
        .env("CARGO_TARGET_DIR", &target)
        .status()
        .unwrap();
    assert!(
        built.success(),
        "PREREQUISITE: building pre-upgrade team-agent {PREUPGRADE_SHA} failed (set {KEY})"
    );
    let tmp = dir.join("team-agent.tmp");
    std::fs::copy(target.join("debug/team-agent"), &tmp).unwrap();
    std::fs::rename(&tmp, &bin).unwrap();
    bin
}

/// Every regular file under `root` (relative path -> bytes), skipping the `skip` subtrees.
fn tree(root: &Path, skip: &[PathBuf]) -> BTreeMap<String, Vec<u8>> {
    let mut out = BTreeMap::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let p = entry.path();
            if skip.iter().any(|s| p.starts_with(s)) {
                continue;
            }
            let Ok(meta) = std::fs::symlink_metadata(&p) else {
                continue;
            };
            if meta.is_dir() {
                stack.push(p);
            } else {
                // Never open sockets or FIFOs; record them by path only.
                let bytes = if meta.is_file() {
                    std::fs::read(&p).unwrap_or_default()
                } else {
                    vec![]
                };
                let rel = p.strip_prefix(root).unwrap().to_string_lossy().into_owned();
                out.insert(rel, bytes);
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------------------------------------------
// Deterministic OpenAI-compatible streaming mock model.

#[derive(Clone)]
struct Mock {
    port: u16,
    log: Arc<Mutex<Vec<Value>>>,
}

fn text_of(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|p| p["text"].as_str())
            .collect::<Vec<_>>()
            .join(""),
        _ => String::new(),
    }
}

impl Mock {
    fn start() -> Mock {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let mock = Mock {
            port: listener.local_addr().unwrap().port(),
            log: Arc::default(),
        };
        let log = mock.log.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let log = log.clone();
                std::thread::spawn(move || Mock::serve(stream, log));
            }
        });
        mock
    }

    fn serve(stream: TcpStream, log: Arc<Mutex<Vec<Value>>>) {
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut out = stream;
        let mut first = String::new();
        if reader.read_line(&mut first).is_err() {
            return;
        }
        let mut len = 0usize;
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                break;
            }
            if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                len = v.trim().parse().unwrap_or(0);
            }
        }
        let mut body = vec![0u8; len];
        let _ = reader.read_exact(&mut body);
        let path = first.split_whitespace().nth(1).unwrap_or("").to_string();
        let req: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
        let users: Vec<String> = req["messages"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|m| m["role"] == "user")
            .map(|m| text_of(&m["content"]))
            .collect();
        let last = users.last().cloned().unwrap_or_default();
        log.lock()
            .unwrap()
            .push(json!({"t": now(), "path": path, "user_texts": users, "last_user": last}));
        if !path.ends_with("/chat/completions") {
            let _ = out.write_all(
                b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            );
            return;
        }
        let slow = last.contains("SLOW");
        let parts: Vec<String> = if slow {
            (0..SLOW_SECONDS).map(|i| format!("tick {i} ")).collect()
        } else {
            vec![
                "MOCK-ACK ".into(),
                last.chars().take(48).collect::<String>().replace('\n', " "),
            ]
        };
        let id = format!("mock-{}", unique());
        let chunk = |delta: Value, finish: Value| {
            format!(
                "data: {}\n\n",
                json!({"id": id, "object": "chat.completion.chunk", "created": 0, "model": req["model"],
                   "choices": [{"index": 0, "delta": delta, "finish_reason": finish}]})
            )
        };
        let mut send = |s: String| {
            out.write_all(s.as_bytes())
                .and_then(|_| out.flush())
                .is_ok()
        };
        if !send("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: close\r\n\r\n".into())
            || !send(chunk(json!({"role": "assistant", "content": ""}), Value::Null)) {
            return;
        }
        for part in parts {
            if !send(chunk(json!({"content": part}), Value::Null)) {
                return;
            }
            if slow {
                std::thread::sleep(Duration::from_secs(1));
            }
        }
        let done = format!(
            "data: {}\n\n",
            json!({"id": id, "object": "chat.completion.chunk", "created": 0,
            "model": req["model"], "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15}})
        );
        let _ = send(done) && send("data: [DONE]\n\n".into());
    }

    fn requests(&self) -> Vec<Value> {
        self.log
            .lock()
            .unwrap()
            .iter()
            .filter(|r| {
                r["path"]
                    .as_str()
                    .unwrap_or("")
                    .ends_with("/chat/completions")
            })
            .cloned()
            .collect()
    }
}

// ---------------------------------------------------------------------------------------------------------------
// Scratch-only observer extension (instrumentation; never delivers anything).

const OBSERVER_JS: &str = r#"import { appendFileSync, readdirSync, readFileSync, renameSync } from "node:fs";
import { join } from "node:path";
const DIR = __DIR__;
export default function teamAgentRedObserver(pi) {
  const gen = (globalThis.__taRedObserverGen = (globalThis.__taRedObserverGen ?? 0) + 1);
  let ctx;
  const log = (rec) => appendFileSync(join(DIR, "events.jsonl"), JSON.stringify({ t: Date.now() / 1000, pid: process.pid, gen, ...rec }) + "\n");
  const state = () => {
    try { return { idle: ctx?.isIdle?.() ?? null, pending: ctx?.hasPendingMessages?.() ?? null, editor: ctx?.ui?.getEditorText?.() ?? null }; }
    catch (error) { return { state_error: String(error) }; }
  };
  const text = (m) => (typeof m?.content === "string" ? m.content : (m?.content ?? []).map((p) => p?.text ?? "").join(""));
  const env = Object.fromEntries(Object.entries(process.env).filter(([k]) => /^PI_|TUI/i.test(k)));
  log({ event: "factory", argv: process.argv, env });
  for (const name of ["session_start", "session_shutdown", "agent_start", "agent_end"]) {
    pi.on(name, (_event, c) => { if (c) ctx = c; log({ event: name, ...state() }); });
  }
  pi.on("agent_settled", (event, c) => { if (c) ctx = c; log({ event: "agent_settled", aborted: event?.aborted ?? null, ...state() }); });
  pi.on("message_end", (event, c) => { if (c) ctx = c; if (event?.message?.role === "user") log({ event: "user_message_end", text: text(event.message) }); });
  const timer = setInterval(() => {
    if (gen !== globalThis.__taRedObserverGen) return; // only the live instance answers after /new or /reload
    let files = [];
    try { files = readdirSync(join(DIR, "triggers")).filter((f) => f.endsWith(".json")); } catch { return; }
    for (const f of files) {
      const src = join(DIR, "triggers", f);
      let t;
      try { t = JSON.parse(readFileSync(src, "utf8")); renameSync(src, `${src}.done`); } catch { continue; }
      log({ event: "snap", id: t.id, ...state() });
    }
  }, 50);
  timer.unref?.();
}
"#;

// ---------------------------------------------------------------------------------------------------------------
// Rig: one isolated `team-agent pi` leader on one Pi version.

struct Rig {
    name: String,
    version: String,
    root: PathBuf,
    home: PathBuf,
    ws: PathBuf,
    agent: PathBuf,
    obs: PathBuf,
    bin: PathBuf,
    path_env: String,
    outer: String,
    sock: String,
    pane: String,
    mock: Mock,
    t0: Instant,
    pi_tree_before: BTreeMap<String, Vec<u8>>,
    steps: Vec<Value>,
    violations: Vec<String>,
}

impl Rig {
    /// Starts `team-agent pi` in a scratch workspace, with `launcher` as the `team-agent` on PATH.
    fn launch(name: &str, version: &str, launcher: &Path) -> Rig {
        let pi = pi_bin(version);
        let root = scratch_base().join(format!("ta-pi16-red-{}", unique()));
        let home = root.join("home");
        let agent = home.join(".pi/agent");
        let ws = root.join("ws");
        let obs = root.join("observer");
        let bin = root.join("bin");
        for dir in [&agent.join("extensions"), &ws, &obs.join("triggers"), &bin] {
            std::fs::create_dir_all(dir).unwrap();
        }
        std::os::unix::fs::symlink(launcher, bin.join("team-agent")).unwrap();
        let mock = Mock::start();
        std::fs::write(agent.join("models.json"), json!({"providers": {"mock": {
            "baseUrl": format!("http://127.0.0.1:{}/v1", mock.port), "api": "openai-completions", "apiKey": "mock-key",
            "models": [{"id": "mock-1", "name": "Mock 1", "contextWindow": 128000, "maxTokens": 4096}]}}}).to_string()).unwrap();
        std::fs::write(
            agent.join("settings.json"),
            json!({"defaultProvider": "mock", "defaultModel": "mock-1",
            "defaultThinkingLevel": "off", "lastChangelogVersion": version})
            .to_string(),
        )
        .unwrap();
        std::fs::write(
            agent.join("extensions/team-agent-red-observer.js"),
            OBSERVER_JS.replace("__DIR__", &json!(obs.to_string_lossy()).to_string()),
        )
        .unwrap();
        let pi_tree_before = tree(&home.join(".pi"), &[agent.join("sessions")]);
        let path_env = format!(
            "{}:{}:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin",
            pi.parent().unwrap().display(),
            bin.display()
        );
        let outer = format!("tapi16-{}", unique());
        let user = std::env::var("USER").unwrap_or_else(|_| "user".into());
        let shell = format!(
            "env -i HOME='{home}' USER='{user}' LOGNAME='{user}' PATH='{path_env}' TERM=xterm-256color LANG=en_US.UTF-8 \
             LC_ALL=en_US.UTF-8 SHELL=/bin/sh bash -c 'cd \"{ws}\" && team-agent pi; echo LAUNCHER_EXIT=$?; sleep 86400'",
            home = home.display(), ws = ws.display());
        let mut rig = Rig {
            name: name.into(),
            version: version.into(),
            root,
            home,
            ws,
            agent,
            obs,
            bin,
            path_env,
            outer,
            sock: String::new(),
            pane: String::new(),
            mock,
            t0: Instant::now(),
            pi_tree_before,
            steps: vec![],
            violations: vec![],
        };
        let status = Command::new("tmux")
            .args([
                "-L",
                &rig.outer,
                "-f",
                "/dev/null",
                "new-session",
                "-d",
                "-s",
                "outer",
                "-x",
                "56",
                "-y",
                "54",
                "-c",
            ])
            .arg(&rig.ws)
            .arg(&shell)
            .status()
            .unwrap();
        assert!(status.success(), "HARNESS: outer tmux failed to start");
        let started = rig.wait(Duration::from_secs(90), |r| {
            r.events().iter().any(|e| e["event"] == "session_start")
        });
        assert!(started, "HARNESS: leader Pi {version} never started (observer saw no session_start). outer screen:\n{}",
            rig.outer_screen());
        let attached = rig.wait(Duration::from_secs(30), |r| {
            let receiver = r.leader_receiver();
            r.sock = receiver["tmux_socket"].as_str().unwrap_or("").into();
            r.pane = receiver["pane_id"].as_str().unwrap_or("").into();
            !r.sock.is_empty() && !r.pane.is_empty()
        });
        assert!(attached, "HARNESS: no leader_receiver in state.json");
        std::thread::sleep(Duration::from_secs(4)); // leader-side registration settles
        rig.t0 = Instant::now();
        let receiver = rig.leader_receiver();
        rig.step(
            "leader ready",
            json!({"socket": rig.sock, "pane": rig.pane, "pi": version,
                "launcher": launcher.display().to_string(), "leader_receiver": receiver}),
        );
        rig
    }

    fn leader_receiver(&self) -> Value {
        let s: Value = std::fs::read(self.ws.join(".team/runtime/state.json"))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or(Value::Null);
        s["teams"]["current"]["leader_receiver"].clone()
    }

    /// In-place upgrade: the PATH `team-agent` now resolves to the candidate; running processes are untouched.
    fn upgrade_to_candidate(&mut self) {
        let tmp = self.bin.join("team-agent.upgrade");
        std::os::unix::fs::symlink(candidate(), &tmp).unwrap();
        std::fs::rename(&tmp, self.bin.join("team-agent")).unwrap();
        self.step(
            "team-agent upgraded in place",
            json!({"path_team_agent": candidate().display().to_string()}),
        );
    }

    fn wait(&mut self, timeout: Duration, mut done: impl FnMut(&mut Rig) -> bool) -> bool {
        let end = Instant::now() + timeout;
        while Instant::now() < end {
            if done(self) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        false
    }

    fn step(&mut self, what: &str, data: Value) {
        self.steps
            .push(json!({"dt": self.t0.elapsed().as_secs_f64(), "step": what, "data": data}));
    }

    fn check(&mut self, ok: bool, rule: &str, observed: impl std::fmt::Display) {
        if !ok {
            self.violations
                .push(format!("{rule}\n      observed: {observed}"));
        }
    }

    fn tmux(&self, args: &[&str]) -> String {
        let out = Command::new("tmux")
            .arg("-S")
            .arg(&self.sock)
            .args(args)
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    fn outer_screen(&self) -> String {
        let out = Command::new("tmux")
            .args(["-L", &self.outer, "capture-pane", "-p", "-t", "outer"])
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    fn screen(&self) -> String {
        self.tmux(&["capture-pane", "-p", "-t", &self.pane])
    }

    fn keys(&mut self, text: &str) {
        self.tmux(&["send-keys", "-t", &self.pane, "-l", text]);
        self.step("human typed", json!(text));
    }

    fn key(&mut self, key: &str) {
        self.tmux(&["send-keys", "-t", &self.pane, key]);
        self.step("human key", json!(key));
    }

    fn events(&self) -> Vec<Value> {
        std::fs::read_to_string(self.obs.join("events.jsonl"))
            .unwrap_or_default()
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect()
    }

    fn snap(&mut self, label: &str) -> Value {
        let id = format!("snap-{}", unique());
        std::fs::write(
            self.obs.join(format!("triggers/{id}.tmp")),
            json!({"id": id}).to_string(),
        )
        .unwrap();
        std::fs::rename(
            self.obs.join(format!("triggers/{id}.tmp")),
            self.obs.join(format!("triggers/{id}.json")),
        )
        .unwrap();
        let mut found = Value::Null;
        self.wait(Duration::from_secs(10), |r| {
            found = r
                .events()
                .into_iter()
                .rev()
                .find(|e| e["event"] == "snap" && e["id"] == id.as_str())
                .unwrap_or(Value::Null);
            !found.is_null()
        });
        assert!(
            !found.is_null(),
            "HARNESS: observer did not answer snapshot {label}"
        );
        let screen = self.screen();
        self.step(
            &format!("snapshot {label}"),
            json!({"state": found, "screen": screen}),
        );
        found
    }

    fn editor(&mut self, label: &str) -> String {
        self.snap(label)["editor"]
            .as_str()
            .unwrap_or("<no editor>")
            .to_string()
    }

    fn type_draft(&mut self, draft: &str) {
        self.keys(draft);
        std::thread::sleep(Duration::from_millis(600));
        let editor = self.editor("draft typed");
        assert_eq!(
            editor, draft,
            "HARNESS: human draft did not reach the editor"
        );
    }

    fn human_prompt_busy(&mut self, prompt: &str) {
        let before = self.events().len();
        self.keys(prompt);
        std::thread::sleep(Duration::from_millis(300));
        self.key("Enter");
        let busy = self.wait(Duration::from_secs(15), |r| {
            r.events()[before..]
                .iter()
                .any(|e| e["event"] == "agent_start")
        });
        assert!(busy, "HARNESS: human prompt did not start a run");
        std::thread::sleep(Duration::from_secs(2));
    }

    fn settled_after(&mut self, from: usize, aborted: bool, timeout: Duration) -> bool {
        self.wait(timeout, |r| {
            r.events()
                .get(from..)
                .unwrap_or(&[])
                .iter()
                .any(|e| e["event"] == "agent_settled" && e["aborted"] == aborted)
        })
    }

    fn wait_any_settle(&mut self) {
        self.wait(Duration::from_secs(20), |r| {
            r.events()
                .iter()
                .rev()
                .take(20)
                .any(|e| e["event"] == "agent_settled")
        });
        std::thread::sleep(Duration::from_secs(1));
    }

    /// Every user entry in every Pi session transcript of this rig: (session file, text).
    fn transcript_users(&self) -> Vec<(String, String)> {
        let mut files = vec![];
        let mut stack = vec![self.agent.join("sessions")];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
                let p = entry.path();
                if p.is_dir() {
                    stack.push(p)
                } else if p.extension().is_some_and(|e| e == "jsonl") {
                    files.push(p)
                }
            }
        }
        files.sort();
        let mut users = vec![];
        for file in files {
            for line in std::fs::read_to_string(&file).unwrap_or_default().lines() {
                let Ok(o) = serde_json::from_str::<Value>(line) else {
                    continue;
                };
                if o["message"]["role"] == "user" {
                    users.push((
                        file.file_name().unwrap().to_string_lossy().into_owned(),
                        text_of(&o["message"]["content"]),
                    ));
                }
            }
        }
        users
    }

    fn with_tag(&self, tag: &str) -> Vec<(String, String)> {
        self.transcript_users()
            .into_iter()
            .filter(|(_, t)| t.contains(tag))
            .collect()
    }

    fn wait_ingested(&mut self, tag: &str, timeout: Duration) -> bool {
        self.wait(timeout, |r| !r.with_tag(tag).is_empty())
    }

    fn inbox_lists(&self, message_id: &str) -> bool {
        let out = Command::new(candidate())
            .args(["inbox", "leader", "--workspace"])
            .arg(&self.ws)
            .arg("--json")
            .env_clear()
            .env("HOME", &self.home)
            .env("PATH", &self.path_env)
            .output()
            .unwrap();
        !message_id.is_empty() && String::from_utf8_lossy(&out.stdout).contains(message_id)
    }

    /// Worker `send_message(to=leader)` through the candidate's real Team MCP server, exactly as a worker seat calls it.
    fn worker_send(&mut self, content: &str, tag: &str) -> Value {
        let mut child = Command::new(candidate())
            .args(["mcp-server", "--workspace"])
            .arg(&self.ws)
            .env_clear()
            .env("HOME", &self.home)
            .env("PATH", &self.path_env)
            .env("LANG", "en_US.UTF-8")
            .env("USER", std::env::var("USER").unwrap_or_default())
            .env("TEAM_AGENT_WORKSPACE", &self.ws)
            .env("TEAM_AGENT_ID", SENDER)
            .env("TEAM_AGENT_AGENT_ID", SENDER)
            .env("TEAM_AGENT_OWNER_TEAM_ID", "current")
            .env("TEAM_AGENT_AUTH_MODE", "subscription")
            .current_dir(&self.ws)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let (tx, rx) = mpsc::channel::<Value>();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if let Ok(v) = serde_json::from_str::<Value>(&line) {
                    if tx.send(v).is_err() {
                        break;
                    }
                }
            }
        });
        let await_id = |id: i64, rx: &mpsc::Receiver<Value>| -> Value {
            let end = Instant::now() + Duration::from_secs(90);
            while let Ok(v) = rx.recv_timeout(end.saturating_duration_since(Instant::now())) {
                if v["id"] == id {
                    return v;
                }
            }
            Value::Null
        };
        let mut write = |v: Value| {
            writeln!(stdin, "{v}").unwrap();
            stdin.flush().unwrap();
        };
        write(
            json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-06-18",
            "capabilities": {}, "clientInfo": {"name": "pi-16-delivery-red", "version": "1"}}}),
        );
        assert!(
            !await_id(1, &rx).is_null(),
            "HARNESS: team-agent mcp-server did not initialize"
        );
        write(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
        let started = self.t0.elapsed().as_secs_f64();
        write(json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call",
            "params": {"name": "send_message", "arguments": {"to": "leader", "content": content}}}));
        let raw = await_id(2, &rx);
        let returned = self.t0.elapsed().as_secs_f64();
        // Evidence window: the transcript line may trail an in-process ingestion event by a few milliseconds.
        let in_transcript_at_return =
            self.wait(Duration::from_secs(1), |r| !r.with_tag(tag).is_empty());
        drop(stdin);
        let _ = child.kill();
        let _ = child.wait();
        let text = raw["result"]["content"][0]["text"].as_str().unwrap_or("");
        let reply: Value = serde_json::from_str(text).unwrap_or_else(|_| raw["result"].clone());
        self.step(
            "worker send_message",
            json!({"started": started, "returned": returned, "reply": reply,
            "in_transcript_at_return": in_transcript_at_return}),
        );
        assert!(!raw.is_null(), "HARNESS: send_message never returned");
        // T5 (applies to every test): accepted:true is only honest with ingestion evidence at reply time.
        if reply["accepted"] == true {
            self.check(
                in_transcript_at_return,
                "T5 accepted:true was reported before the message was in the Pi session transcript",
                format!("reply={reply}"),
            );
        }
        reply
    }

    fn content(tag: &str) -> String {
        format!("{tag} please review the latest note and reply when done.")
    }

    /// Paths under scratch `~/.pi` (outside `agent/sessions`) that were added, changed or removed.
    fn pi_tree_changes(&self) -> Vec<String> {
        let after = tree(&self.home.join(".pi"), &[self.agent.join("sessions")]);
        let mut changes: Vec<String> = after
            .iter()
            .filter(|(p, b)| self.pi_tree_before.get(*p) != Some(*b))
            .map(|(p, _)| {
                if self.pi_tree_before.contains_key(p) {
                    format!("changed {p}")
                } else {
                    format!("added {p}")
                }
            })
            .collect();
        changes.extend(
            self.pi_tree_before
                .keys()
                .filter(|p| !after.contains_key(*p))
                .map(|p| format!("removed {p}")),
        );
        changes
    }

    fn finish(mut self) {
        let receipt = json!({
            "test": self.name, "pi_version": self.version, "steps": self.steps, "violations": self.violations,
            "transcript_user_entries": self.transcript_users(), "model_requests": self.mock.requests(),
            "observer_events": self.events(), "pi_tree_changes": self.pi_tree_changes(),
            "final_leader_receiver": self.leader_receiver(), "final_screen": self.screen(),
        });
        let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("pi-16-delivery-red");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("{}-pi-{}.json", self.name, self.version));
        std::fs::write(&path, serde_json::to_string_pretty(&receipt).unwrap()).unwrap();
        let violations = std::mem::take(&mut self.violations);
        if !violations.is_empty() {
            panic!(
                "\n[{} | Pi {}] {} CONTRACT VIOLATION(S):\n  - {}\n  receipt: {}\n",
                self.name,
                self.version,
                violations.len(),
                violations.join("\n  - "),
                path.display()
            );
        }
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        if !self.sock.is_empty() {
            let _ = Command::new("tmux")
                .arg("-S")
                .arg(&self.sock)
                .arg("kill-server")
                .output();
        }
        let _ = Command::new("tmux")
            .args(["-L", &self.outer, "kill-server"])
            .output();
        if std::env::var_os("TEAM_AGENT_PI_RED_KEEP").is_none() {
            std::thread::sleep(Duration::from_millis(300));
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }
}

fn show(entries: &[(String, String)]) -> String {
    serde_json::to_string(&entries.iter().map(|(_, t)| t).collect::<Vec<_>>()).unwrap()
}

/// The first model request whose last user text carries `tag`, with its index in the request log.
fn first_model_request(mock: &Mock, tag: &str) -> Option<(usize, Value)> {
    mock.requests()
        .into_iter()
        .enumerate()
        .find(|(_, r)| r["last_user"].as_str().unwrap_or("").contains(tag))
}

fn assert_independent_turn(rig: &mut Rig, test: &str, tag: &str, also_not: Option<&str>) {
    let hits = rig.with_tag(tag);
    rig.check(
        hits.len() == 1,
        &format!("{test} the worker message must be ingested exactly once as a user turn"),
        format!(
            "{} transcript user entries carry the tag: {}",
            hits.len(),
            show(&hits)
        ),
    );
    rig.check(
        hits.iter().all(|(_, t)| !t.contains(DRAFT_MARK)),
        &format!("{test} the worker turn must not contain any part of the human draft (no concatenation or splice)"),
        show(&hits),
    );
    if let Some(other) = also_not {
        rig.check(
            hits.iter().all(|(_, t)| !t.contains(other)),
            &format!("{test} the worker turn must not be merged with another prompt"),
            show(&hits),
        );
    }
    match first_model_request(&rig.mock, tag) {
        Some((_, request)) => {
            let last = request["last_user"].as_str().unwrap_or("").to_string();
            rig.check(!last.contains(DRAFT_MARK), &format!("{test} the model must receive the worker message without the human draft"),
                json!(last));
        }
        None => rig.check(false, &format!("{test} the model must receive the worker message as the last user message of a request"),
            "no model request carried the tag"),
    }
}

/// T7/T8: the reply of the existing physical `direct_tmux` delivery.
fn assert_physical(rig: &mut Rig, rule: &str, reply: &Value) {
    rig.check(
        !reply.to_string().contains(NATIVE_UNAVAILABLE),
        &format!("{rule}: the send must never fail closed with \"pi {NATIVE_UNAVAILABLE}\""),
        format!("reply={reply}"),
    );
    rig.check(
        reply["live"] == true
            && reply["delivery"] == "physical"
            && reply["submission"] == "physical"
            && reply["delivery_error"].is_null(),
        &format!("{rule}: the send must use the existing physical direct_tmux delivery (live:true, delivery:\"physical\", submission:\"physical\", no delivery_error)"),
        format!("live={} delivery={} submission={} delivery_error={} reply={reply}",
            reply["live"], reply["delivery"], reply["submission"], reply["delivery_error"]),
    );
}

fn assert_held_reply(rig: &mut Rig, test: &str, reply: &Value) {
    rig.check(
        reply["submission"] == "native_pending" && reply["accepted"] != true,
        &format!("{test} a delivery held while Pi 1.6 is busy must report submission:\"native_pending\" (stored, not yet accepted)"),
        format!("submission={} accepted={} reply={reply}", reply["submission"], reply["accepted"]),
    );
}

// ---------------------------------------------------------------------------------------------------------------
// Tests

fn run_t0_native_mode_guard(version: &str) {
    let mut rig = Rig::launch("T0_native_mode_guard", version, &candidate());
    let alt = rig.tmux(&["display", "-p", "-t", &rig.pane.clone(), "#{alternate_on}"]);
    rig.check(
        alt.trim() == "1",
        "T0 the leader Pi must run in its native fullscreen (alternate screen) mode",
        format!("alternate_on={alt:?}"),
    );
    let leader_pid = rig
        .events()
        .into_iter()
        .find(|e| e["event"] == "session_start")
        .map(|e| e["pid"].clone());
    let factory = rig
        .events()
        .into_iter()
        .find(|e| e["event"] == "factory" && Some(&e["pid"]) == leader_pid.as_ref())
        .unwrap_or(Value::Null);
    let argv: Vec<String> = factory["argv"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|a| a.as_str().map(str::to_owned))
        .collect();
    let joined = argv.join(" ");
    rig.check(
        !argv.is_empty()
            && !["tui-mode", "tuiMode", "regular", "--mode"]
                .iter()
                .any(|bad| joined.contains(bad)),
        "T0 no UI-altering option (--tui-mode, tuiMode, regular, --mode) may be injected into the leader Pi command line",
        &joined,
    );
    let ui_env: Vec<&String> = factory["env"]
        .as_object()
        .into_iter()
        .flatten()
        .map(|(k, _)| k)
        .filter(|k| k.to_ascii_uppercase().contains("TUI"))
        .collect();
    rig.check(
        ui_env.is_empty(),
        "T0 no UI-altering environment variable may be injected into the leader Pi",
        format!("{ui_env:?} env={}", factory["env"]),
    );
    let extensions: Vec<String> = argv
        .iter()
        .enumerate()
        .filter_map(|(i, a)| match a.as_str() {
            "-e" | "--extension" => argv.get(i + 1).cloned(),
            _ => a.strip_prefix("--extension=").map(str::to_owned),
        })
        .collect();
    rig.check(
        extensions.len() == 1
            && extensions[0].ends_with("/.team/runtime/pi/current/leader/team-mcp.ts"),
        "T0 the leader Pi must load exactly one Team Agent extension: the existing Team MCP extension (.team/runtime/pi/current/leader/team-mcp.ts)",
        format!("{extensions:?}"),
    );
    let tag = format!("T0TAG{}", unique());
    rig.worker_send(&Rig::content(&tag), &tag);
    rig.wait_ingested(&tag, Duration::from_secs(20));
    std::thread::sleep(Duration::from_secs(1));
    let observer = rig.agent.join("extensions/team-agent-red-observer.js");
    let sessions = rig.agent.join("sessions");
    let mut scripts = vec![];
    for root in [&rig.ws, &rig.home] {
        for rel in tree(root, &[sessions.clone(), observer.clone()]).keys() {
            let script = [".js", ".mjs", ".cjs", ".ts", ".mts", ".cts"]
                .iter()
                .any(|e| rel.ends_with(e));
            if script
                && root.join(rel) != rig.ws.join(".team/runtime/pi/current/leader/team-mcp.ts")
            {
                scripts.push(root.join(rel).display().to_string());
            }
        }
    }
    rig.check(
        scripts.is_empty(),
        "T0 Team Agent must not create any extension source file besides the leader's team-mcp.ts",
        format!("{scripts:?}"),
    );
    // Pi itself creates its empty credential/model stores on first start (pi-ai AuthStorage/models-store),
    // and its tools-manager (`ensureTool`) downloads fd/rg into agent/bin when they are not on PATH.
    const PI_OWNED: [&str; 4] = [
        "added agent/auth.json",
        "added agent/models-store.json",
        "added agent/bin/fd",
        "added agent/bin/rg",
    ];
    let changes: Vec<String> = rig
        .pi_tree_changes()
        .into_iter()
        .filter(|c| !PI_OWNED.contains(&c.as_str()))
        .collect();
    rig.check(
        changes.is_empty(),
        "T0 the user's ~/.pi must stay untouched (every existing file byte-identical, settings.json never modified, no new file outside agent/sessions except Pi's own auth.json/models-store.json and its managed agent/bin/fd, agent/bin/rg)",
        format!("{changes:?}"),
    );
    rig.check(
        !rig.ws.join(".pi").exists(),
        "T0 no project-level .pi config may be created in the workspace",
        format!(
            "{:?}",
            tree(&rig.ws.join(".pi"), &[]).keys().collect::<Vec<_>>()
        ),
    );
    rig.finish();
}

fn run_t1_idle_draft_untouched(version: &str) {
    const DRAFT: &str = "HUMAN-DRAFT-T1 half typed note";
    const PROBE: &str = "HUMAN-DRAFT-T1 half typed| note";
    let mut rig = Rig::launch("T1_idle_draft_untouched", version, &candidate());
    let tag = format!("T1TAG{}", unique());
    rig.type_draft(DRAFT);
    for _ in 0..5 {
        rig.key("Left");
    }
    std::thread::sleep(Duration::from_millis(300));
    // Harness self-check: the cursor sits before " note" (insert a marker, then delete it).
    rig.keys("|");
    std::thread::sleep(Duration::from_millis(500));
    let probe = rig.editor("cursor probe");
    assert_eq!(
        probe, PROBE,
        "HARNESS: cursor movement did not reach the editor"
    );
    rig.key("BSpace");
    std::thread::sleep(Duration::from_millis(500));
    let restored = rig.editor("cursor probe removed");
    assert_eq!(restored, DRAFT, "HARNESS: cursor probe was not removed");
    let reply = rig.worker_send(&Rig::content(&tag), &tag);
    let ingested = rig.wait_ingested(&tag, Duration::from_secs(20));
    std::thread::sleep(Duration::from_secs(2));
    rig.check(
        ingested,
        "T1 the worker message must reach the Pi session transcript",
        "not ingested within 20 s",
    );
    rig.check(
        reply["accepted"] == true,
        "T1 an idle delivery on Pi 1.6 must report accepted:true",
        format!("accepted={} reply={reply}", reply["accepted"]),
    );
    let editor = rig.editor("after delivery");
    rig.check(
        editor == DRAFT,
        "T1 the human draft in the prompt dock must stay byte-identical",
        format!("expected {DRAFT:?}, editor={editor:?}"),
    );
    rig.keys("|");
    std::thread::sleep(Duration::from_millis(500));
    let cursor = rig.editor("cursor after delivery");
    rig.check(
        cursor == PROBE,
        "T1 the human's cursor position inside the draft must be preserved (typing continues where it was)",
        format!("typed '|' after delivery: expected {PROBE:?}, editor={cursor:?}"),
    );
    let draft_entries: Vec<_> = rig
        .transcript_users()
        .into_iter()
        .filter(|(_, t)| t.contains(DRAFT_MARK))
        .collect();
    rig.check(
        draft_entries.is_empty(),
        "T1 the human draft (or any part of it) must not be submitted by a delivery",
        show(&draft_entries),
    );
    assert_independent_turn(&mut rig, "T1", &tag, None);
    rig.finish();
}

fn run_t2_busy_hold_then_settled_dispatch(version: &str) {
    let mut rig = Rig::launch("T2_busy_hold_then_settled_dispatch", version, &candidate());
    let tag = format!("T2TAG{}", unique());
    let draft = "HUMAN-DRAFT-T2";
    let prompt = "SLOW T2 long answer";
    let from = rig.events().len();
    rig.human_prompt_busy(prompt);
    rig.type_draft(draft);
    let reply = rig.worker_send(&Rig::content(&tag), &tag);
    assert_held_reply(&mut rig, "T2", &reply);
    let id = reply["message_id"].as_str().unwrap_or("").to_string();
    let durable = rig.inbox_lists(&id);
    rig.check(
        durable,
        "T2 the held message must be in the durable inbox",
        format!("message_id={id:?}"),
    );
    for (i, wait) in [1u64, 2].into_iter().enumerate() {
        std::thread::sleep(Duration::from_secs(wait));
        let s = rig.snap(&format!("busy sample {i}"));
        let screen = rig.screen();
        // The human queued nothing during this run, so any queued-message preview comes from the delivery.
        let preview = screen
            .lines()
            .any(|l| l.contains("Steering:") || l.contains("Follow-up:"));
        if s["idle"] != false {
            rig.step("busy sample skipped: leader already idle", s.clone());
            continue;
        }
        rig.check(s["pending"] == false,
            "T2 while Pi is busy the message must not enter Pi's steering/followUp queue (hasPendingMessages must be false)",
            format!("sample {i}: {s}"));
        rig.check(!preview, "T2 while Pi is busy the dock must not show a Steering/Follow-up preview of the worker message",
            format!("sample {i} screen:\n{screen}"));
        rig.check(
            s["editor"] == draft,
            "T2 while Pi is busy the human draft must stay byte-identical",
            format!("sample {i}: {s}"),
        );
        let early = rig.with_tag(&tag);
        rig.check(
            early.is_empty(),
            "T2 while Pi is busy the message must be held (not ingested into the running turn)",
            show(&early),
        );
    }
    let settled = rig.settled_after(from, false, Duration::from_secs(SLOW_SECONDS + 30));
    rig.check(
        settled,
        "T2 harness expectation: the busy run settles normally",
        "no agent_settled{aborted:false}",
    );
    let ingested = rig.wait_ingested(&tag, Duration::from_secs(25));
    std::thread::sleep(Duration::from_secs(2));
    rig.check(
        ingested,
        "T2 after agent_settled{aborted:false} the held message must be dispatched and ingested",
        "not ingested within 25 s after settle",
    );
    let editor = rig.editor("after dispatch");
    rig.check(
        editor == draft,
        "T2 the human draft must stay byte-identical after dispatch",
        format!("editor={editor:?}"),
    );
    assert_independent_turn(&mut rig, "T2", &tag, Some(prompt));
    rig.finish();
}

fn run_t3_aborted_settle_keeps_message_held(version: &str) {
    let mut rig = Rig::launch(
        "T3_aborted_settle_keeps_message_held",
        version,
        &candidate(),
    );
    let tag = format!("T3TAG{}", unique());
    let draft = "HUMAN-DRAFT-T3";
    let from = rig.events().len();
    rig.human_prompt_busy("SLOW T3 long answer");
    rig.type_draft(draft);
    let reply = rig.worker_send(&Rig::content(&tag), &tag);
    assert_held_reply(&mut rig, "T3", &reply);
    std::thread::sleep(Duration::from_millis(1500));
    rig.key("Escape");
    let aborted = rig.settled_after(from, true, Duration::from_secs(20));
    assert!(
        aborted,
        "HARNESS: Escape did not abort the busy run (no agent_settled{{aborted:true}})"
    );
    std::thread::sleep(Duration::from_millis(1500));
    let editor = rig.editor("after Escape");
    rig.check(editor == draft,
        "T3 after Escape the editor must hold exactly the human draft (worker text must never be merged into it: Gap B)",
        format!("expected {draft:?}, editor={editor:?}"));
    let early = rig.with_tag(&tag);
    rig.check(
        early.is_empty(),
        "T3 an aborted settle must not dispatch the held message",
        show(&early),
    );
    let id = reply["message_id"].as_str().unwrap_or("").to_string();
    let durable = rig.inbox_lists(&id);
    rig.check(
        durable,
        "T3 the held message must remain in the durable inbox",
        format!("message_id={id:?}"),
    );
    // Release policy: the next non-aborted settle dispatches the held message as its own turn.
    let before_release = rig.events().len();
    rig.key("Enter");
    let released = rig.settled_after(before_release, false, Duration::from_secs(30));
    rig.check(
        released,
        "T3 harness expectation: the human's next turn settles normally",
        "no agent_settled{aborted:false}",
    );
    let ingested = rig.wait_ingested(&tag, Duration::from_secs(25));
    std::thread::sleep(Duration::from_secs(2));
    rig.check(
        ingested,
        "T3 after the next agent_settled{aborted:false} the held message must be dispatched",
        "not ingested within 25 s",
    );
    assert_independent_turn(&mut rig, "T3", &tag, None);
    rig.finish();
}

fn run_t4_session_replacement(version: &str, command: &str) {
    let label = if command == "/new" {
        "T4_session_new"
    } else {
        "T4_session_reload"
    };
    let mut rig = Rig::launch(label, version, &candidate());
    let tag = format!("T4TAG{}", unique());
    let draft = "HUMAN-DRAFT-T4";
    let from = rig.events().len();
    rig.keys(command);
    std::thread::sleep(Duration::from_millis(400));
    rig.key("Enter");
    let replaced = rig.wait(Duration::from_secs(20), |r| {
        r.events()[from..]
            .iter()
            .any(|e| e["event"] == "session_start")
    });
    assert!(
        replaced,
        "HARNESS: {command} did not start a replacement session"
    );
    std::thread::sleep(Duration::from_secs(3));
    rig.type_draft(draft);
    rig.worker_send(&Rig::content(&tag), &tag);
    let ingested = rig.wait_ingested(&tag, Duration::from_secs(20));
    std::thread::sleep(Duration::from_secs(2));
    rig.check(
        ingested,
        &format!("T4 after {command} the message must be ingested in the live session (no loss)"),
        "not ingested within 20 s",
    );
    let editor = rig.editor("after delivery");
    rig.check(
        editor == draft,
        &format!("T4 after {command} the human draft must stay byte-identical"),
        format!("editor={editor:?}"),
    );
    let screen = rig.screen().to_lowercase();
    rig.check(
        !screen.contains("stale") && !screen.contains("extension \"<runtime>\" error"),
        &format!("T4 after {command} no stale-handle/extension error may surface to the human"),
        rig.screen(),
    );
    assert_independent_turn(&mut rig, "T4", &tag, None);
    rig.finish();
}

fn run_t5_honest_ack(version: &str) {
    let mut rig = Rig::launch("T5_honest_ack", version, &candidate());
    let idle_tag = format!("T5IDLE{}", unique());
    let reply = rig.worker_send(&Rig::content(&idle_tag), &idle_tag);
    rig.check(
        reply["accepted"] == true,
        "T5 an idle delivery must report accepted:true once its user turn is in the Pi transcript",
        format!("reply.accepted={} reply={reply}", reply["accepted"]),
    );
    rig.wait_ingested(&idle_tag, Duration::from_secs(20));
    rig.wait_any_settle();
    let busy_tag = format!("T5BUSY{}", unique());
    let from = rig.events().len();
    rig.human_prompt_busy("SLOW T5 long answer");
    let reply = rig.worker_send(&Rig::content(&busy_tag), &busy_tag);
    let ingested_now = !rig.with_tag(&busy_tag).is_empty();
    rig.check(
        !(reply["accepted"] == true && !ingested_now),
        "T5 a held (busy) delivery must not report accepted:true",
        format!("reply={reply}"),
    );
    let _ = rig.settled_after(from, false, Duration::from_secs(SLOW_SECONDS + 30));
    rig.wait_ingested(&busy_tag, Duration::from_secs(25));
    rig.finish();
}

fn run_t6_abort_then_later_idle_delivery(version: &str) {
    let mut rig = Rig::launch("T6_abort_then_later_idle_delivery", version, &candidate());
    let from = rig.events().len();
    rig.human_prompt_busy("SLOW T6 long answer");
    rig.key("Escape");
    assert!(
        rig.settled_after(from, true, Duration::from_secs(20)),
        "HARNESS: Escape did not abort the busy run (no agent_settled{{aborted:true}})"
    );
    std::thread::sleep(Duration::from_secs(3));
    let s = rig.snap("idle after abort");
    assert!(
        s["idle"] == true && s["editor"] == "" && s["pending"] == false,
        "HARNESS: leader not idle/empty after abort: {s}"
    );
    let tag = format!("T6TAG{}", unique());
    let reply = rig.worker_send(&Rig::content(&tag), &tag);
    let ingested = rig.wait_ingested(&tag, Duration::from_secs(20));
    std::thread::sleep(Duration::from_secs(2));
    rig.check(
        ingested,
        "T6 a message that arrives while Pi is idle after an earlier aborted run must be delivered (the abort hold only covers messages pending at abort time)",
        format!("not ingested within 20 s; reply={reply}"),
    );
    rig.check(
        reply["accepted"] == true,
        "T6 that idle delivery must report accepted:true",
        format!("reply={reply}"),
    );
    assert_independent_turn(&mut rig, "T6", &tag, None);
    rig.finish();
}

fn run_t7_non_16_physical_fallback(version: &str) {
    let mut rig = Rig::launch("T7_non_16_physical_fallback", version, &candidate());
    let idle_tag = format!("T7IDLE{}", unique());
    let reply = rig.worker_send(&Rig::content(&idle_tag), &idle_tag);
    assert_physical(
        &mut rig,
        &format!("T7 idle delivery to a Pi {version} leader"),
        &reply,
    );
    let ingested = rig.wait_ingested(&idle_tag, Duration::from_secs(20));
    rig.check(
        ingested,
        "T7 the idle physical delivery must be ingested by the Pi leader",
        "not ingested within 20 s",
    );
    rig.wait_any_settle();
    let busy_tag = format!("T7BUSY{}", unique());
    let from = rig.events().len();
    rig.human_prompt_busy("SLOW T7 long answer");
    let reply = rig.worker_send(&Rig::content(&busy_tag), &busy_tag);
    assert_physical(
        &mut rig,
        &format!("T7 busy delivery to a Pi {version} leader (no native hold)"),
        &reply,
    );
    let _ = rig.settled_after(from, false, Duration::from_secs(SLOW_SECONDS + 30));
    let ingested = rig.wait_ingested(&busy_tag, Duration::from_secs(25));
    std::thread::sleep(Duration::from_secs(2));
    rig.check(
        ingested,
        "T7 the busy physical delivery must be ingested by the Pi leader",
        "not ingested within 25 s after the run settled",
    );
    for tag in [&idle_tag, &busy_tag] {
        let hits = rig.with_tag(tag);
        rig.check(
            hits.len() == 1,
            "T7 each physical delivery must be ingested exactly once",
            format!("{tag}: {}", show(&hits)),
        );
    }
    rig.finish();
}

fn run_t8_preupgrade_leader_physical_fallback(version: &str) {
    let preupgrade = preupgrade_team_agent();
    let mut rig = Rig::launch(
        "T8_preupgrade_leader_physical_fallback",
        version,
        &preupgrade,
    );
    let leader_pid = rig
        .events()
        .into_iter()
        .find(|e| e["event"] == "session_start")
        .map(|e| e["pid"].clone())
        .unwrap();
    let pane = rig.pane.clone();
    let events_before = rig.events().len();
    rig.upgrade_to_candidate();
    std::thread::sleep(Duration::from_secs(1));
    let tag = format!("T8TAG{}", unique());
    let reply = rig.worker_send(&Rig::content(&tag), &tag);
    assert_physical(
        &mut rig,
        &format!("T8 delivery to a running Pi {version} leader started before the upgrade (no native handshake)"),
        &reply,
    );
    let ingested = rig.wait_ingested(&tag, Duration::from_secs(20));
    std::thread::sleep(Duration::from_secs(2));
    rig.check(
        ingested,
        "T8 the running pre-upgrade Pi leader must ingest the message",
        format!("not ingested within 20 s; reply={reply}"),
    );
    let hits = rig.with_tag(&tag);
    rig.check(
        hits.len() == 1,
        "T8 the message must be ingested exactly once",
        show(&hits),
    );
    let later = rig.events()[events_before..].to_vec();
    // A short-lived `pi --list-models`/`--version` also loads extensions (factory event); only a new
    // interactive session (session_start from another pid) is a restart.
    let restarted: Vec<&Value> = later
        .iter()
        .filter(|e| e["event"] == "session_start" && e["pid"] != leader_pid)
        .collect();
    let alive = Command::new("kill")
        .arg("-0")
        .arg(leader_pid.to_string())
        .status()
        .is_ok_and(|s| s.success());
    let ingested_by: Vec<&Value> = later
        .iter()
        .filter(|e| {
            e["event"] == "user_message_end"
                && e["text"].as_str().unwrap_or("").contains(tag.as_str())
        })
        .map(|e| &e["pid"])
        .collect();
    rig.check(
        alive
            && restarted.is_empty()
            && !ingested_by.is_empty()
            && ingested_by.iter().all(|pid| **pid == leader_pid),
        "T8 the SAME running Pi process must ingest it, without a restart",
        format!("leader pid={leader_pid} alive={alive} ingested_by={ingested_by:?} new_sessions={restarted:?}"),
    );
    let receiver = rig.leader_receiver();
    rig.check(
        receiver["pane_id"] == pane.as_str(),
        "T8 the leader pane must stay the same",
        format!("before={pane} now={}", receiver["pane_id"]),
    );
    rig.finish();
}

/// T9 (amendment): native delivery must survive a same-owner receiver observation refresh. The documented
/// recovery command `team-agent claim-leader`, run from the leader pane (here: Pi `!!` user bash, a child of the
/// leader Pi), rewrites `leader_receiver.attached_at` while owner, epoch and pane stay the same.
fn run_t9_native_survives_receiver_refresh(version: &str) {
    let mut rig = Rig::launch("T9_native_survives_receiver_refresh", version, &candidate());
    let before_tag = format!("T9PRE{}", unique());
    let reply = rig.worker_send(&Rig::content(&before_tag), &before_tag);
    rig.check(
        reply["accepted"] == true,
        "T9 precondition: an idle delivery before the refresh must report accepted:true",
        format!("accepted={} reply={reply}", reply["accepted"]),
    );
    rig.wait_ingested(&before_tag, Duration::from_secs(20));
    rig.wait_any_settle();
    let before = rig.leader_receiver();
    let out = rig.root.join("claim-leader.out");
    rig.keys(&format!(
        "!!team-agent claim-leader > '{}' 2>&1",
        out.display()
    ));
    std::thread::sleep(Duration::from_millis(400));
    rig.key("Enter");
    let refreshed = rig.wait(Duration::from_secs(20), |r| {
        r.leader_receiver()["attached_at"] != before["attached_at"]
    });
    std::thread::sleep(Duration::from_secs(2));
    let after = rig.leader_receiver();
    let output = std::fs::read_to_string(&out).unwrap_or_default();
    rig.step(
        "operator ran claim-leader from the leader pane",
        json!({"before": before, "after": after, "output": output}),
    );
    assert!(
        refreshed
            && after["owner_epoch"] == before["owner_epoch"]
            && after["leader_session_uuid"] == before["leader_session_uuid"]
            && after["pane_id"] == before["pane_id"],
        "HARNESS: claim-leader did not refresh the same-owner receiver; before={before} after={after} output:\n{output}"
    );
    let s = rig.snap("editor after claim-leader");
    assert!(
        s["editor"] == "",
        "HARNESS: editor not empty after the `!!` command: {s}"
    );
    let draft = "HUMAN-DRAFT-T9 typed after claim-leader";
    rig.type_draft(draft);
    let tag = format!("T9TAG{}", unique());
    let reply = rig.worker_send(&Rig::content(&tag), &tag);
    let ingested = rig.wait_ingested(&tag, Duration::from_secs(20));
    std::thread::sleep(Duration::from_secs(2));
    rig.check(
        ingested,
        "T9 after the refresh the worker message must reach the Pi session transcript",
        "not ingested within 20 s",
    );
    rig.check(
        reply["accepted"] == true,
        "T9 after a same-owner receiver refresh an idle delivery on Pi 1.6 must still report accepted:true (native delivery must not silently degrade)",
        format!("accepted={} delivery={} reply={reply}", reply["accepted"], reply["delivery"]),
    );
    let editor = rig.editor("after delivery");
    rig.check(
        editor == draft,
        "T9 after the refresh the human draft must stay byte-identical",
        format!("expected {draft:?}, editor={editor:?}"),
    );
    let draft_entries: Vec<_> = rig
        .transcript_users()
        .into_iter()
        .filter(|(_, t)| t.contains(DRAFT_MARK))
        .collect();
    rig.check(
        draft_entries.is_empty(),
        "T9 the human draft (or any part of it) must not be submitted by a delivery",
        show(&draft_entries),
    );
    assert_independent_turn(&mut rig, "T9", &tag, None);
    rig.finish();
}

macro_rules! physical {
    ($version:literal, $($name:ident => $body:expr;)*) => {
        $( #[test] #[ignore = "physical: real Pi + tmux; run with --ignored --test-threads=1"] #[serial(pi_16_delivery_red)]
           fn $name() { ($body)($version) } )*
    };
}

mod pi_1_6_0 {
    use super::*;
    physical! { "1.6.0",
        t0_native_mode_guard => run_t0_native_mode_guard;
        t1_idle_draft_untouched => run_t1_idle_draft_untouched;
        t2_busy_hold_then_settled_dispatch => run_t2_busy_hold_then_settled_dispatch;
        t3_aborted_settle_keeps_message_held => run_t3_aborted_settle_keeps_message_held;
        t4_session_new => |v| run_t4_session_replacement(v, "/new");
        t4_session_reload => |v| run_t4_session_replacement(v, "/reload");
        t5_honest_ack => run_t5_honest_ack;
        t6_abort_then_later_idle_delivery => run_t6_abort_then_later_idle_delivery;
        t8_preupgrade_leader_physical_fallback => run_t8_preupgrade_leader_physical_fallback;
        t9_native_survives_receiver_refresh => run_t9_native_survives_receiver_refresh;
    }
}

mod pi_1_5_2 {
    use super::*;
    physical! { "1.5.2",
        t0_native_mode_guard => run_t0_native_mode_guard;
        t7_non_16_physical_fallback => run_t7_non_16_physical_fallback;
    }
}
