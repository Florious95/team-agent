//! Pi leader native delivery: RED acceptance suite (physical, isolated, real Pi 1.5.2 and 1.6.0).
//!
//! Incident: a worker `send_message` to a Pi leader reported `live:true`, but delivery is a tmux paste + Enter into
//! the leader's prompt dock. Reproduced defects (both Pi versions):
//!   - Case 3: a half-typed human draft is concatenated with the worker text and submitted as one turn.
//!   - Gap B: a message queued while Pi is busy (steer/followUp) is pulled back into the editor on Escape, merged into
//!     the human draft, and never reaches the model.
//!
//! Frozen contract (each test prints every violated rule with observations):
//!   T1 idle + human draft: the draft is not mutated, and the worker message is its own user turn.
//!   T2 busy (streaming) + human draft: the message is held outside Pi's steering/followUp queue (nothing pending,
//!      no Steering/Follow-up preview, not ingested, draft intact). After `agent_settled{aborted:false}` it is
//!      dispatched as an independent turn, exactly once, never merged with the draft or the running prompt.
//!   T3 busy + human draft + Escape (`agent_settled{aborted:true}`): the message stays held, the editor still holds
//!      exactly the human draft, nothing is ingested, and the message stays in the durable inbox. Release policy: the
//!      next non-aborted settle (here: the human submits the draft) dispatches it as its own turn.
//!   T4 session replacement (`/new`, `/reload`) before delivery: no stale-handle error surfaces, the message is
//!      ingested exactly once in the live session as its own turn, and the human draft is intact.
//!   T5 honest ACK:
//!      - every `accepted:true` reply must already have its message in the Pi transcript when the reply returns
//!        (1 s persistence grace);
//!      - an idle delivery must report `accepted:true`;
//!      - a held (busy) delivery must not report `accepted:true`.
//!   T0 GUARD (expected green on baseline): the leader Pi runs in its native fullscreen mode with no injected
//!      UI options, and the user's Pi settings.json is never modified.
//!
//! Run (macOS/Linux with tmux, node, npm):
//!   cargo test -p team-agent --test pi_native_delivery_red -- --ignored --test-threads=1
//! Pi binaries:
//!   - `TEAM_AGENT_PI_RED_PI_1_5_2` / `TEAM_AGENT_PI_RED_PI_1_6_0` point at a `pi` executable (node_modules/.bin/pi).
//!   - Otherwise the suite npm-installs `@earendil-works/pi-coding-agent@<v>` into
//!     `$TEAM_AGENT_PI_RED_CACHE` (default `/private/tmp/ta-pi-red-cache` or the system temp dir).
//! Isolation:
//!   - Each test uses a scratch HOME (Pi config = scratch `~/.pi/agent`), a scratch workspace, its own tmux servers
//!     and an in-process deterministic mock model (no credentials, no network).
//!   - A scratch-only observer extension (auto-discovered from the scratch Pi config) reports the editor text,
//!     idle/pending state, settle events and user message ingestion. The real `~/.pi` is never read or written.
//!   - Receipts are written to `$CARGO_TARGET_TMPDIR/pi-native-delivery-red/<test>.json`.
//!     `TEAM_AGENT_PI_RED_KEEP=1` keeps scratch dirs.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use serial_test::serial;

/// The mock model streams one token per second for this long when the last user text contains `SLOW`.
const SLOW_SECONDS: u64 = 14;
const SENDER: &str = "backend-dev";

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
    let cache = std::env::var_os("TEAM_AGENT_PI_RED_CACHE")
        .map(PathBuf::from)
        .unwrap_or_else(|| scratch_base().join("ta-pi-red-cache"))
        .join(format!("pi-{version}"));
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
  log({ event: "factory", argv: process.argv });
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
    path_env: String,
    outer: String,
    sock: String,
    pane: String,
    mock: Mock,
    t0: Instant,
    settings_before: Vec<u8>,
    steps: Vec<Value>,
    violations: Vec<String>,
}

impl Rig {
    fn launch(name: &str, version: &str) -> Rig {
        let pi = pi_bin(version);
        let root = scratch_base().join(format!("ta-pi-red-{}", unique()));
        let home = root.join("home");
        let agent = home.join(".pi/agent");
        let ws = root.join("ws");
        let obs = root.join("observer");
        let bin = root.join("bin");
        for dir in [&agent.join("extensions"), &ws, &obs.join("triggers"), &bin] {
            std::fs::create_dir_all(dir).unwrap();
        }
        std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_team-agent"), bin.join("team-agent"))
            .unwrap();
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
        let settings_before = std::fs::read(agent.join("settings.json")).unwrap();
        let path_env = format!(
            "{}:{}:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin",
            pi.parent().unwrap().display(),
            bin.display()
        );
        let outer = format!("tapired-{}", unique());
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
            path_env,
            outer,
            sock: String::new(),
            pane: String::new(),
            mock,
            t0: Instant::now(),
            settings_before,
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
        let state_path = rig.ws.join(".team/runtime/state.json");
        let attached = rig.wait(Duration::from_secs(30), |r| {
            let s: Value = std::fs::read(&state_path)
                .ok()
                .and_then(|b| serde_json::from_slice(&b).ok())
                .unwrap_or(Value::Null);
            let receiver = &s["teams"]["current"]["leader_receiver"];
            r.sock = receiver["tmux_socket"].as_str().unwrap_or("").into();
            r.pane = receiver["pane_id"].as_str().unwrap_or("").into();
            !r.sock.is_empty() && !r.pane.is_empty()
        });
        assert!(
            attached,
            "HARNESS: no leader_receiver in {}",
            state_path.display()
        );
        std::thread::sleep(Duration::from_secs(4)); // leader-side registration settles
        rig.t0 = Instant::now();
        rig.step(
            "leader ready",
            json!({"socket": rig.sock, "pane": rig.pane, "pi": version}),
        );
        rig
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

    /// Worker `send_message(to=leader)` through the real Team MCP server, exactly as a worker seat calls it.
    fn worker_send(&mut self, content: &str, tag: &str) -> Value {
        let mut child = Command::new(env!("CARGO_BIN_EXE_team-agent"))
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
            "capabilities": {}, "clientInfo": {"name": "pi-native-delivery-red", "version": "1"}}}),
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

    fn finish(mut self) {
        let settings_after = std::fs::read(self.agent.join("settings.json")).unwrap_or_default();
        let receipt = json!({
            "test": self.name, "pi_version": self.version, "steps": self.steps, "violations": self.violations,
            "transcript_user_entries": self.transcript_users(), "model_requests": self.mock.requests(),
            "observer_events": self.events(), "settings_unchanged": settings_after == self.settings_before,
            "final_screen": self.screen(),
        });
        let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("pi-native-delivery-red");
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

fn assert_independent_turn(
    rig: &mut Rig,
    test: &str,
    tag: &str,
    draft: &str,
    also_not: Option<&str>,
) {
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
        hits.iter().all(|(_, t)| !t.contains(draft)),
        &format!("{test} the worker turn must not contain the human draft (no concatenation)"),
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
            rig.check(!last.contains(draft), &format!("{test} the model must receive the worker message without the human draft"),
                json!(last));
        }
        None => rig.check(false, &format!("{test} the model must receive the worker message as the last user message of a request"),
            "no model request carried the tag"),
    }
}

// ---------------------------------------------------------------------------------------------------------------
// Tests

fn run_t1_idle_draft_untouched(version: &str) {
    let mut rig = Rig::launch("T1_idle_draft_untouched", version);
    let tag = format!("T1TAG{}", unique());
    let draft = "HUMAN-DRAFT-T1 half typed note";
    rig.type_draft(draft);
    rig.worker_send(&Rig::content(&tag), &tag);
    let ingested = rig.wait_ingested(&tag, Duration::from_secs(20));
    std::thread::sleep(Duration::from_secs(2));
    rig.check(
        ingested,
        "T1 the worker message must reach the Pi session transcript",
        "not ingested within 20 s",
    );
    let editor = rig.editor("after delivery");
    rig.check(
        editor == draft,
        "T1 the human draft in the prompt dock must stay byte-identical",
        format!("expected {draft:?}, editor={editor:?}"),
    );
    let draft_entries: Vec<_> = rig
        .transcript_users()
        .into_iter()
        .filter(|(_, t)| t.contains(draft))
        .collect();
    rig.check(
        draft_entries.is_empty(),
        "T1 the human draft must not be submitted by a delivery",
        show(&draft_entries),
    );
    assert_independent_turn(&mut rig, "T1", &tag, draft, None);
    rig.finish();
}

fn run_t2_busy_hold_then_settled_dispatch(version: &str) {
    let mut rig = Rig::launch("T2_busy_hold_then_settled_dispatch", version);
    let tag = format!("T2TAG{}", unique());
    let draft = "HUMAN-DRAFT-T2";
    let prompt = "SLOW T2 long answer";
    let from = rig.events().len();
    rig.human_prompt_busy(prompt);
    rig.type_draft(draft);
    rig.worker_send(&Rig::content(&tag), &tag);
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
    assert_independent_turn(&mut rig, "T2", &tag, draft, Some(prompt));
    rig.finish();
}

fn run_t3_aborted_settle_keeps_message_held(version: &str) {
    let mut rig = Rig::launch("T3_aborted_settle_keeps_message_held", version);
    let tag = format!("T3TAG{}", unique());
    let draft = "HUMAN-DRAFT-T3";
    let from = rig.events().len();
    rig.human_prompt_busy("SLOW T3 long answer");
    rig.type_draft(draft);
    let reply = rig.worker_send(&Rig::content(&tag), &tag);
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
        "T3 after Escape the editor must hold exactly the human draft (worker text must never be merged into it)",
        format!("expected {draft:?}, editor={editor:?}"));
    let early = rig.with_tag(&tag);
    rig.check(
        early.is_empty(),
        "T3 an aborted settle must not dispatch the held message",
        show(&early),
    );
    let id = reply["message_id"].as_str().unwrap_or("").to_string();
    let inbox = Command::new(env!("CARGO_BIN_EXE_team-agent"))
        .args(["inbox", "leader", "--workspace"])
        .arg(&rig.ws)
        .arg("--json")
        .env_clear()
        .env("HOME", &rig.home)
        .env("PATH", &rig.path_env)
        .output()
        .unwrap();
    let inbox = String::from_utf8_lossy(&inbox.stdout).into_owned();
    rig.check(
        !id.is_empty() && inbox.contains(&id),
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
    assert_independent_turn(&mut rig, "T3", &tag, draft, None);
    rig.finish();
}

fn run_t4_session_replacement(version: &str, command: &str) {
    let label = if command == "/new" {
        "T4_session_new"
    } else {
        "T4_session_reload"
    };
    let mut rig = Rig::launch(label, version);
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
    assert_independent_turn(&mut rig, "T4", &tag, draft, None);
    rig.finish();
}

fn run_t5_honest_ack(version: &str) {
    let mut rig = Rig::launch("T5_honest_ack", version);
    let idle_tag = format!("T5IDLE{}", unique());
    let reply = rig.worker_send(&Rig::content(&idle_tag), &idle_tag);
    rig.check(
        reply["accepted"] == true,
        "T5 an idle delivery must report accepted:true once its user turn is in the Pi transcript",
        format!("reply.accepted={} reply={reply}", reply["accepted"]),
    );
    rig.wait_ingested(&idle_tag, Duration::from_secs(20));
    rig.wait(Duration::from_secs(20), |r| {
        r.events()
            .iter()
            .rev()
            .take(20)
            .any(|e| e["event"] == "agent_settled")
    });
    std::thread::sleep(Duration::from_secs(1));
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

fn run_t0_native_mode_guard(version: &str) {
    let mut rig = Rig::launch("T0_native_mode_guard", version);
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
    let argv = rig
        .events()
        .into_iter()
        .find(|e| e["event"] == "factory" && Some(&e["pid"]) == leader_pid.as_ref())
        .map(|e| e["argv"].to_string())
        .unwrap_or_default();
    rig.check(
        !argv.contains("tui-mode") && !argv.contains("tuiMode") && !argv.contains("regular"),
        "T0 no UI-altering option may be injected into the leader Pi command line",
        argv,
    );
    let tag = format!("T0TAG{}", unique());
    rig.worker_send(&Rig::content(&tag), &tag);
    rig.wait_ingested(&tag, Duration::from_secs(20));
    let after = std::fs::read(rig.agent.join("settings.json")).unwrap_or_default();
    let unchanged = after == rig.settings_before;
    rig.check(
        unchanged,
        "T0 the user's Pi settings.json must never be modified",
        String::from_utf8_lossy(&after).into_owned(),
    );
    rig.finish();
}

macro_rules! per_version {
    ($($name:ident => $body:expr;)*) => {
        mod pi_1_5_2 {
            use super::*;
            $( #[test] #[ignore = "physical: real Pi + tmux; run with --ignored --test-threads=1"] #[serial(pi_native_delivery_red)]
               fn $name() { ($body)("1.5.2") } )*
        }
        mod pi_1_6_0 {
            use super::*;
            $( #[test] #[ignore = "physical: real Pi + tmux; run with --ignored --test-threads=1"] #[serial(pi_native_delivery_red)]
               fn $name() { ($body)("1.6.0") } )*
        }
    };
}

per_version! {
    t0_native_mode_guard => run_t0_native_mode_guard;
    t1_idle_draft_untouched => run_t1_idle_draft_untouched;
    t2_busy_hold_then_settled_dispatch => run_t2_busy_hold_then_settled_dispatch;
    t3_aborted_settle_keeps_message_held => run_t3_aborted_settle_keeps_message_held;
    t4_session_new => |v| run_t4_session_replacement(v, "/new");
    t4_session_reload => |v| run_t4_session_replacement(v, "/reload");
    t5_honest_ack => run_t5_honest_ack;
}
