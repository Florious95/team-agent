//! Cold Pi worker, first send: RED acceptance suite (physical, isolated, real Pi 1.6.1).
//!
//! Incident (2026-11-26). A Pi leader ran, in one shell command:
//!   `team-agent start <worker> --provider pi --session fresh --json && team-agent send <worker> "<task>"`
//! `send` returned `ok:true, live:true, delivery:physical, submission:physical` about 0.5 s after the spawn,
//! while the new Pi was still booting (about 3 s on that host). The whole task text was left in Pi's editor:
//! the footer showed `0%`, the model got no request, and nothing ran until a human pressed Enter, 29.8 s and
//! 70.4 s later in two separate occurrences. Later sends to the same running worker were ingested in 0.2 s.
//! The same scenario passes with 0.6.0 (52cfc9b3) and fails since 0.6.1 (9c0c9e66d).
//!
//! Pi 1.6.x accepts keys before it can submit them. Input that arrives before its TUI starts, or after the TUI
//! starts but before the real submit handler is installed, ends up back in the editor (status "Startup is still
//! in progress"). A paste plus one Enter during that time is never submitted.
//!
//! Frozen rules. Each test prints every violated rule together with its observation.
//!   S0 GUARD, fast boot (green on the base): `start && send` to a Pi worker that boots without delay. The message
//!      becomes a user turn exactly once, the model receives it exactly once, and the editor is empty afterwards.
//!   S1 slow boot before the TUI (the incident): every Pi boot of the worker first spends 3 s loading extensions,
//!      before its TUI starts. With `start && send` run back to back, the following must hold:
//!        a. `start` reports `ok:true` and `send` reports `ok:true` and `stored:true`. The message is never refused
//!           or silently dropped.
//!        b. Within 60 s of the command, the message is a Pi transcript user turn exactly once, and the mock
//!           model receives it exactly once, with no human key press.
//!        c. Afterwards the Pi editor holds no part of the message.
//!        d. The command finishes within 60 s. No unbounded wait.
//!   S2 slow startup after the TUI is drawn: the worker has no fd/rg, so Pi tries to download them through a
//!      proxy that holds every connection for 4 s. Pi's editor is fully drawn, but for more than 10 s an Enter
//!      cannot submit. `start && send` must satisfy S1.a to S1.d, with a 120 s limit. A readiness check based on
//!      the screen alone is not enough.
//!   S3 warm follow-up (in the S1 rig, after S1): a second send to the now-running worker returns within 10 s and
//!      is ingested exactly once within 10 s after it returns. The first message is not ingested again.
//!   S4 restart incarnation: the worker is started, booted and fed one message (a warm send). Then
//!      `stop && start --session fresh && send` runs as one command, and the new Pi boots slowly as in S1. The
//!      new message must satisfy S1.a to S1.d. Readiness seen in the previous incarnation does not count for the
//!      new one.
//!
//! Observables used: the receipts of `start --json` and `send --json` (`ok`, `stored`), the Pi transcript files
//! (`$HOME/.pi/agent/sessions/**/*.jsonl`, user entries), the requests the in-process mock model receives, and the
//! worker pane screen (`tmux capture-pane`; the editor is the region between the last two `─` rules). The suite
//! does not read Team Agent internals.
//!
//! Run (macOS or Linux, with tmux, node and npm):
//!   cargo test -p team-agent --test pi_worker_startup_ready_red -- --ignored --test-threads=1
//! Inputs: `TEAM_AGENT_PI_RED_PI_1_6_1` points at a `pi` executable. Otherwise
//! `@earendil-works/pi-coding-agent@1.6.1` is npm-installed once into `$TEAM_AGENT_PI_RED_CACHE/pi-1.6.1`
//! (default cache `/private/tmp/ta-pi-red-cache`).
//! Isolation: each test uses a scratch root with its own HOME (Pi config = scratch `~/.pi/agent`, mock model
//! only), workspace, TMUX_TMPDIR and tmux servers. Every process starts from `.env_clear()` with an explicit HOME
//! and a current directory. The leader is a bash pane with `TEAM_AGENT_LEADER_PROVIDER=pi`. A driver loop in that
//! pane runs the scripted commands, so all commands come from the same leader pane, as in the incident.
//! Receipts: `$CARGO_TARGET_TMPDIR/pi-worker-startup-ready-red/<test>.json`. Set `TEAM_AGENT_PI_RED_KEEP=1` to keep
//! the scratch root.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use serial_test::serial;

const PI_VERSION: &str = "1.6.1";
const WORKER: &str = "w1";
/// Live-host emulation: the incident host spent about 3.0 s between Pi session creation and model selection.
const SLOW_BOOT_MS: u64 = 3000;
/// The holding proxy keeps each connection this long, then answers 502.
const PROXY_HOLD: Duration = Duration::from_secs(4);

fn now() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs_f64()
}

fn unique() -> String {
    format!("{}{}", std::process::id(), SystemTime::now().duration_since(UNIX_EPOCH).unwrap().subsec_nanos() % 100_000)
}

fn scratch_base() -> PathBuf {
    if let Some(root) = std::env::var_os("TEAM_AGENT_PI_RED_ROOT") {
        return PathBuf::from(root);
    }
    let tmp = PathBuf::from("/private/tmp");
    if tmp.is_dir() { tmp } else { std::env::temp_dir() }
}

fn cache_root() -> PathBuf {
    std::env::var_os("TEAM_AGENT_PI_RED_CACHE").map(PathBuf::from).unwrap_or_else(|| scratch_base().join("ta-pi-red-cache"))
}

fn pi_bin(version: &str) -> PathBuf {
    let key = format!("TEAM_AGENT_PI_RED_PI_{}", version.replace('.', "_"));
    if let Some(path) = std::env::var_os(&key) {
        let path = PathBuf::from(path);
        assert!(path.exists(), "PREREQUISITE: {key}={} does not exist", path.display());
        return path;
    }
    let cache = cache_root().join(format!("pi-{version}"));
    let bin = cache.join("node_modules/.bin/pi");
    if !bin.exists() {
        std::fs::create_dir_all(&cache).unwrap();
        std::fs::write(cache.join("package.json"), "{\"private\":true}").unwrap();
        let out = Command::new("npm")
            .args(["install", "--no-audit", "--no-fund", "--ignore-scripts", "--silent",
                &format!("@earendil-works/pi-coding-agent@{version}")])
            .current_dir(&cache)
            .output();
        assert!(matches!(&out, Ok(o) if o.status.success()) && bin.exists(),
            "PREREQUISITE: could not install Pi {version} into {} (set {key} to an existing pi executable): {out:?}",
            cache.display());
    }
    bin
}

/// Resolve a tool on the runner's PATH (used only to build the scratch PATH).
fn which(tool: &str) -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|paths| std::env::split_paths(&paths).map(|dir| dir.join(tool)).find(|p| p.is_file()))
}

fn text_of(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts.iter().filter_map(|p| p["text"].as_str()).collect::<Vec<_>>().join(""),
        _ => String::new(),
    }
}

// ---------------------------------------------------------------------------------------------------------------
// In-process mock model (OpenAI chat-completions, streaming): every request is answered with `MOCK-ACK`.

struct Mock {
    port: u16,
    log: Arc<Mutex<Vec<Value>>>,
}

impl Mock {
    fn start() -> Mock {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let mock = Mock { port: listener.local_addr().unwrap().port(), log: Arc::default() };
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
        let last = req["messages"].as_array().into_iter().flatten().filter(|m| m["role"] == "user")
            .map(|m| text_of(&m["content"])).last().unwrap_or_default();
        log.lock().unwrap().push(json!({"t": now(), "path": path, "last_user": last}));
        if !path.ends_with("/chat/completions") {
            let _ = out.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
            return;
        }
        let chunk = |delta: Value, finish: Value, usage: Value| {
            let mut c = json!({"id": "mock", "object": "chat.completion.chunk", "created": 0, "model": req["model"],
                "choices": [{"index": 0, "delta": delta, "finish_reason": finish}]});
            if !usage.is_null() { c["usage"] = usage; }
            format!("data: {c}\n\n")
        };
        let mut body = String::from("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: close\r\n\r\n");
        body += &chunk(json!({"role": "assistant", "content": ""}), Value::Null, Value::Null);
        body += &chunk(json!({"content": "MOCK-ACK"}), Value::Null, Value::Null);
        body += &chunk(json!({}), json!("stop"), json!({"prompt_tokens": 10, "completion_tokens": 2, "total_tokens": 12}));
        body += "data: [DONE]\n\n";
        let _ = out.write_all(body.as_bytes()).and_then(|_| out.flush());
    }

    fn requests_with(&self, tag: &str) -> usize {
        self.log.lock().unwrap().iter()
            .filter(|r| r["path"].as_str().unwrap_or("").ends_with("/chat/completions"))
            .filter(|r| r["last_user"].as_str().unwrap_or("").contains(tag)).count()
    }
}

/// HTTP(S) proxy that never forwards: it logs the request line, holds the connection, then answers 502.
fn holding_proxy() -> (u16, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let log: Arc<Mutex<Vec<String>>> = Arc::default();
    let shared = log.clone();
    std::thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            let log = shared.clone();
            std::thread::spawn(move || {
                let mut buf = [0u8; 4096];
                let n = stream.read(&mut buf).unwrap_or(0);
                let line = String::from_utf8_lossy(&buf[..n]).lines().next().unwrap_or("").to_string();
                log.lock().unwrap().push(format!("{:.3} {line}", now()));
                std::thread::sleep(PROXY_HOLD);
                let _ = stream.write_all(b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
            });
        }
    });
    (port, log)
}

// ---------------------------------------------------------------------------------------------------------------
// Rig

#[derive(Clone, Copy, Debug, PartialEq)]
enum Boot {
    /// fd/rg on PATH, offline mode: Pi boots as fast as it can.
    Fast,
    /// S1/S4: a scratch-only extension busy-waits at module load, before Pi's TUI starts.
    SlowBeforeTui,
    /// S2: no fd/rg; Pi's managed download goes through the holding proxy after the TUI is drawn.
    SlowAfterTui,
}

struct Rig {
    name: String,
    root: PathBuf,
    home: PathBuf,
    ws: PathBuf,
    cmds: PathBuf,
    outer_sock: PathBuf,
    tmux_tmpdir: PathBuf,
    /// Absolute tmux path: every probe runs with a cleared environment, so PATH lookup is not available.
    tmux: PathBuf,
    mock: Mock,
    proxy_log: Option<Arc<Mutex<Vec<String>>>>,
    next_cmd: usize,
    t0: Instant,
    steps: Vec<Value>,
    violations: Vec<String>,
}

impl Rig {
    fn new(name: &str, boot: Boot) -> Rig {
        let pi = pi_bin(PI_VERSION);
        let root = scratch_base().join(format!("pwsr-{}-{}", &name[..2], unique()));
        let (home, ws, bin, cmds, tmux_tmpdir) =
            (root.join("home"), root.join("ws"), root.join("bin"), root.join("cmds"), root.join("t"));
        let agent = home.join(".pi/agent");
        for dir in [&agent.join("extensions"), &ws, &bin, &cmds, &tmux_tmpdir] {
            std::fs::create_dir_all(dir).unwrap();
        }
        let mock = Mock::start();
        std::fs::write(agent.join("models.json"), json!({"providers": {"mock": {
            "baseUrl": format!("http://127.0.0.1:{}/v1", mock.port), "api": "openai-completions", "apiKey": "mock-key",
            "models": [{"id": "mock-1", "name": "Mock 1", "contextWindow": 128000, "maxTokens": 4096}]}}}).to_string()).unwrap();
        std::fs::write(agent.join("settings.json"), json!({"defaultProvider": "mock", "defaultModel": "mock-1",
            "defaultThinkingLevel": "off", "lastChangelogVersion": PI_VERSION}).to_string()).unwrap();
        if boot == Boot::SlowBeforeTui {
            std::fs::write(agent.join("extensions/slow-boot.js"), format!(
                "// Scratch-only live-host emulation: extension loading takes {SLOW_BOOT_MS} ms before Pi's TUI starts.\n\
                 const end = Date.now() + {SLOW_BOOT_MS}; while (Date.now() < end) {{}}\n\
                 export default function slowBoot(pi) {{}}\n")).unwrap();
        }
        let candidate = PathBuf::from(env!("CARGO_BIN_EXE_team-agent"));
        std::os::unix::fs::symlink(&pi, bin.join("pi")).unwrap();
        std::os::unix::fs::symlink(&candidate, bin.join("team-agent")).unwrap();
        let mut tools = vec!["node", "tmux"];
        if boot != Boot::SlowAfterTui { tools.extend(["fd", "rg"]); }
        for tool in tools {
            match which(tool) {
                Some(path) => std::os::unix::fs::symlink(path, bin.join(tool)).unwrap(),
                None => assert!(tool == "fd" || tool == "rg", "PREREQUISITE: `{tool}` is not on PATH"),
            }
        }
        let path_env = format!("{}:/usr/bin:/bin:/usr/sbin:/sbin", bin.display());
        let driver = root.join("driver.sh");
        std::fs::write(&driver, format!(
            "#!/bin/bash\ncd '{ws}' || exit 97\nn=1\nwhile :; do\n  if [ -f '{c}'/$n.sh ]; then\n    \
             bash '{c}'/$n.sh > '{c}'/$n.out 2>&1; echo $? > '{c}'/$n.rc.tmp; mv '{c}'/$n.rc.tmp '{c}'/$n.rc; n=$((n+1))\n  \
             else sleep 0.1; fi\ndone\n", ws = ws.display(), c = cmds.display())).unwrap();
        let outer_sock = root.join("outer.sock");
        let user = std::env::var("USER").unwrap_or_else(|_| "user".into());
        let tmux = which("tmux").expect("PREREQUISITE: `tmux` is not on PATH");
        let mut outer = Command::new(&tmux);
        outer.env_clear()
            .env("HOME", &home)
            .env("USER", &user)
            .env("LOGNAME", &user)
            .env("PATH", &path_env)
            .env("TMUX_TMPDIR", &tmux_tmpdir)
            .env("TERM", "xterm-256color")
            .env("LANG", "en_US.UTF-8")
            .env("LC_ALL", "en_US.UTF-8")
            .env("SHELL", "/bin/bash")
            .env("TEAM_AGENT_LEADER_PROVIDER", "pi")
            .current_dir(&ws);
        let mut proxy_log = None;
        if boot == Boot::SlowAfterTui {
            let (port, log) = holding_proxy();
            let proxy = format!("http://127.0.0.1:{port}");
            outer.env("HTTPS_PROXY", &proxy).env("HTTP_PROXY", &proxy).env("NO_PROXY", "127.0.0.1,localhost");
            proxy_log = Some(log);
        } else {
            outer.env("PI_OFFLINE", "1");
        }
        let status = outer.arg("-S").arg(&outer_sock)
            .args(["-f", "/dev/null", "new-session", "-d", "-s", "outer", "-x", "160", "-y", "50"])
            .arg(format!("bash '{}'", driver.display()))
            .status().unwrap();
        assert!(status.success(), "HARNESS: outer tmux failed to start");
        let mut rig = Rig { name: name.into(), root, home, ws, cmds, outer_sock, tmux_tmpdir, tmux, mock, proxy_log,
            next_cmd: 1, t0: Instant::now(), steps: vec![], violations: vec![] };
        rig.step("rig ready", json!({"boot": format!("{boot:?}"), "pi": pi.display().to_string(),
            "candidate": candidate.display().to_string(), "root": rig.root.display().to_string()}));
        rig
    }

    fn step(&mut self, what: &str, data: Value) {
        self.steps.push(json!({"t": self.t0.elapsed().as_secs_f64(), "step": what, "data": data}));
    }

    fn check(&mut self, ok: bool, rule: &str, observed: impl std::fmt::Display) {
        if !ok {
            self.violations.push(format!("{rule} | observed: {observed}"));
        }
    }

    fn message(tag: &str) -> String {
        format!("{tag} Read /tmp/nonexistent/SKILL.md and follow it strictly. Your background requirement is in \
            /tmp/nonexistent/a.ts and /tmp/nonexistent/b.md. Work inside /tmp/nonexistent in read-only mode (do not \
            edit any files). Run Step 1 following the CLI playbook, apply the anti-rework admission gate and scannable \
            format strictly, and send ONLY your compliant frontstage output back to leader via send_message.")
    }

    /// Queue one shell script for the leader-pane driver. Returns its index.
    fn run(&mut self, script: &str) -> usize {
        let n = self.next_cmd;
        self.next_cmd += 1;
        let tmp = self.cmds.join(format!("{n}.sh.tmp"));
        std::fs::write(&tmp, script).unwrap();
        std::fs::rename(&tmp, self.cmds.join(format!("{n}.sh"))).unwrap();
        self.step("leader pane command", json!({"n": n, "script": script}));
        n
    }

    fn send_script(&self, n: usize, tag: &str) -> String {
        let msg = self.cmds.join(format!("{n}.msg"));
        std::fs::write(&msg, Self::message(tag)).unwrap();
        format!("team-agent send {WORKER} \"$(cat '{}')\" --json > '{}'", msg.display(), self.cmds.join(format!("{n}.send.json")).display())
    }

    fn start_script(&self, n: usize) -> String {
        format!("team-agent start {WORKER} --provider pi --model mock/mock-1 --session fresh --json > '{}'",
            self.cmds.join(format!("{n}.start.json")).display())
    }

    fn wait_cmd(&mut self, n: usize, timeout: Duration) -> Option<(i32, f64)> {
        let started = Instant::now();
        let rc = self.cmds.join(format!("{n}.rc"));
        while started.elapsed() < timeout {
            if let Ok(s) = std::fs::read_to_string(&rc) {
                let code = s.trim().parse().unwrap_or(-1);
                let took = started.elapsed().as_secs_f64();
                let out = std::fs::read_to_string(self.cmds.join(format!("{n}.out"))).unwrap_or_default();
                self.step("command finished", json!({"n": n, "rc": code, "waited_s": took, "output": out}));
                return Some((code, took));
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        self.step("command timeout", json!({"n": n, "timeout_s": timeout.as_secs_f64()}));
        None
    }

    fn receipt(&self, n: usize, kind: &str) -> Value {
        std::fs::read_to_string(self.cmds.join(format!("{n}.{kind}.json"))).ok()
            .and_then(|s| serde_json::from_str(&s).ok()).unwrap_or(Value::Null)
    }

    fn state(&self) -> Value {
        std::fs::read_to_string(self.ws.join(".team/runtime/state.json")).ok()
            .and_then(|s| serde_json::from_str(&s).ok()).unwrap_or(Value::Null)
    }

    fn worker_endpoint(&self) -> Option<(String, String)> {
        let state = self.state();
        let team = state["teams"]["current"].as_object().map(|_| &state["teams"]["current"]).unwrap_or(&state);
        let agent = &team["agents"][WORKER];
        let sock = team["tmux_socket"].as_str().or(state["tmux_socket"].as_str())?.to_string();
        let pane = agent["pane_id"].as_str()?.to_string();
        Some((sock, pane))
    }

    fn worker_screen(&self) -> String {
        let Some((sock, pane)) = self.worker_endpoint() else { return String::new() };
        let out = Command::new(&self.tmux).env_clear().env("HOME", &self.home).current_dir(&self.ws)
            .arg("-S").arg(sock).args(["capture-pane", "-p", "-t", &pane]).output().unwrap();
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    /// The editor is the region between the last two horizontal rules of Pi's screen.
    fn editor(screen: &str) -> Option<String> {
        let lines: Vec<&str> = screen.lines().collect();
        let rules: Vec<usize> = lines.iter().enumerate()
            .filter(|(_, l)| l.chars().filter(|c| *c == '─').count() >= 20).map(|(i, _)| i).collect();
        let (&a, &b) = (rules.get(rules.len().checked_sub(2)?)?, rules.last()?);
        Some(lines[a + 1..b].join("\n"))
    }

    fn transcript_users(&self) -> Vec<(String, String)> {
        let mut users = vec![];
        let Ok(dirs) = std::fs::read_dir(self.home.join(".pi/agent/sessions")) else { return users };
        for dir in dirs.flatten() {
            let Ok(files) = std::fs::read_dir(dir.path()) else { continue };
            for file in files.flatten().filter(|f| f.path().extension().is_some_and(|e| e == "jsonl")) {
                for line in std::fs::read_to_string(file.path()).unwrap_or_default().lines() {
                    let Ok(entry) = serde_json::from_str::<Value>(line) else { continue };
                    if entry["type"] == "message" && entry["message"]["role"] == "user" {
                        users.push((entry["timestamp"].as_str().unwrap_or("").to_string(), text_of(&entry["message"]["content"])));
                    }
                }
            }
        }
        users.sort();
        users
    }

    fn turns_with(&self, tag: &str) -> usize {
        self.transcript_users().iter().filter(|(_, t)| t.contains(tag)).count()
    }

    fn wait_ingested(&mut self, tag: &str, timeout: Duration) -> Option<f64> {
        let started = Instant::now();
        while started.elapsed() < timeout {
            if self.turns_with(tag) > 0 {
                let took = started.elapsed().as_secs_f64();
                self.step("ingested", json!({"tag": tag, "after_s": took}));
                return Some(took);
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        None
    }

    /// Exactly-once ingestion, model receipt and a clean editor (S*.b and S*.c).
    fn assert_delivered_once(&mut self, rule: &str, tag: &str, ingested: Option<f64>, limit: Duration) {
        let screen = self.worker_screen();
        self.check(ingested.is_some(), &format!("{rule}.b the message must become a Pi user turn within {}s, with no key press",
            limit.as_secs()), format!("no transcript user turn carries {tag}; model requests with it: {}; worker editor: {:?}",
            self.mock.requests_with(tag), Self::editor(&screen)));
        if ingested.is_none() {
            return;
        }
        std::thread::sleep(Duration::from_secs(2)); // duplicate window and model round trip
        let (turns, requests) = (self.turns_with(tag), self.mock.requests_with(tag));
        self.check(turns == 1, &format!("{rule}.b exactly one user turn carries the message"), format!("{turns} turns"));
        self.check(requests == 1, &format!("{rule}.b the model receives the message exactly once"), format!("{requests} requests"));
        let mut editor = None;
        for _ in 0..15 {
            editor = Self::editor(&self.worker_screen());
            if editor.as_deref().is_some_and(|e| !e.contains(tag)) { break; }
            std::thread::sleep(Duration::from_millis(200));
        }
        self.check(editor.as_deref().is_some_and(|e| !e.contains(tag) && !e.contains("Read /tmp/nonexistent")),
            &format!("{rule}.c afterwards the Pi editor holds no part of the message"), format!("editor: {editor:?}"));
    }

    fn assert_accepted(&mut self, rule: &str, n: usize, with_start: bool) {
        if with_start {
            let start = self.receipt(n, "start");
            self.check(start["ok"] == true, &format!("{rule}.a `start` reports ok:true"), &start);
        }
        let send = self.receipt(n, "send");
        self.check(send["ok"] == true && send["stored"] == true, &format!("{rule}.a `send` reports ok:true and stored:true"),
            json!({"ok": send["ok"], "stored": send["stored"], "status": send["status"], "error": send["error"],
                "delivery_error": send["delivery_error"]}));
        self.step("send receipt", send);
    }

    /// `start && send` as one leader-pane command, then S*.a to S*.d.
    fn cold_start_and_send(&mut self, rule: &str, tag: &str, limit: Duration, prefix: &str) {
        let n = self.next_cmd;
        let script = format!("{prefix}{} && {}", self.start_script(n), self.send_script(n, tag));
        let n = self.run(&script);
        let issued = Instant::now();
        let done = self.wait_cmd(n, limit);
        self.check(done.is_some_and(|(rc, _)| rc == 0), &format!("{rule}.d the command finishes (rc 0) within {}s", limit.as_secs()),
            format!("{done:?}; output: {}", std::fs::read_to_string(self.cmds.join(format!("{n}.out"))).unwrap_or_default()));
        self.assert_accepted(rule, n, true);
        let remaining = limit.saturating_sub(issued.elapsed());
        let ingested = self.wait_ingested(tag, remaining);
        self.assert_delivered_once(rule, tag, ingested, limit);
    }

    fn finish(mut self) {
        let receipt = json!({
            "test": self.name, "pi_version": PI_VERSION, "steps": self.steps, "violations": self.violations,
            "transcript_user_entries": self.transcript_users(), "model_requests": *self.mock.log.lock().unwrap(),
            "proxy_requests": self.proxy_log.as_ref().map(|l| l.lock().unwrap().clone()),
            "final_worker_screen": self.worker_screen(),
        });
        let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("pi-worker-startup-ready-red");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("{}.json", self.name));
        std::fs::write(&path, serde_json::to_string_pretty(&receipt).unwrap()).unwrap();
        let violations = std::mem::take(&mut self.violations);
        if !violations.is_empty() {
            panic!("\n[{} | Pi {PI_VERSION}] {} CONTRACT VIOLATION(S):\n  - {}\n  receipt: {}\n",
                self.name, violations.len(), violations.join("\n  - "), path.display());
        }
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        if let Some((sock, _)) = self.worker_endpoint() {
            let _ = Command::new(&self.tmux).env_clear().env("HOME", &self.home).current_dir(&self.ws)
                .arg("-S").arg(sock).arg("kill-server").output();
        }
        let _ = Command::new(&self.tmux).env_clear().env("HOME", &self.home).env("TMUX_TMPDIR", &self.tmux_tmpdir)
            .current_dir(&self.ws).arg("-S").arg(&self.outer_sock).arg("kill-server").output();
        if std::env::var_os("TEAM_AGENT_PI_RED_KEEP").is_none() {
            std::thread::sleep(Duration::from_millis(500));
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }
}

fn tag(test: &str) -> String {
    format!("COLDSEND-{test}-{}", unique())
}

fn wait_screen(rig: &Rig, what: impl Fn(&str) -> bool, timeout: Duration) -> bool {
    let started = Instant::now();
    while started.elapsed() < timeout {
        if what(&rig.worker_screen()) { return true; }
        std::thread::sleep(Duration::from_millis(200));
    }
    false
}

// ---------------------------------------------------------------------------------------------------------------
// Tests

#[test]
#[ignore = "physical: real Pi + tmux; run with --ignored --test-threads=1"]
#[serial(pi_worker_startup_ready_red)]
fn s0_fast_boot_guard() {
    let mut rig = Rig::new("s0_fast_boot_guard", Boot::Fast);
    let tag = tag("s0");
    rig.cold_start_and_send("S0", &tag, Duration::from_secs(60), "");
    rig.finish();
}

#[test]
#[ignore = "physical: real Pi + tmux; run with --ignored --test-threads=1"]
#[serial(pi_worker_startup_ready_red)]
fn s1_slow_boot_before_tui_then_s3_warm_followup() {
    let mut rig = Rig::new("s1_slow_boot_before_tui_then_s3_warm_followup", Boot::SlowBeforeTui);
    let first = tag("s1");
    rig.cold_start_and_send("S1", &first, Duration::from_secs(60), "");
    if rig.turns_with(&first) == 0 {
        rig.step("S3 skipped", json!("S1 never ingested; S3 needs a running worker"));
        rig.finish();
        return;
    }
    let second = tag("s3");
    let n = rig.next_cmd;
    let script = rig.send_script(n, &second);
    let n = rig.run(&script);
    let done = rig.wait_cmd(n, Duration::from_secs(10));
    rig.check(done.is_some_and(|(rc, _)| rc == 0), "S3 a warm send returns (rc 0) within 10s", format!("{done:?}"));
    rig.assert_accepted("S3", n, false);
    let ingested = rig.wait_ingested(&second, Duration::from_secs(10));
    rig.assert_delivered_once("S3", &second, ingested, Duration::from_secs(10));
    let again = rig.turns_with(&first);
    rig.check(again == 1, "S3 the first message is not ingested again", format!("{again} turns carry the S1 message"));
    rig.finish();
}

#[test]
#[ignore = "physical: real Pi + tmux; run with --ignored --test-threads=1"]
#[serial(pi_worker_startup_ready_red)]
fn s2_slow_startup_after_tui_drawn() {
    let mut rig = Rig::new("s2_slow_startup_after_tui_drawn", Boot::SlowAfterTui);
    let tag = tag("s2");
    rig.cold_start_and_send("S2", &tag, Duration::from_secs(120), "");
    let proxied = rig.proxy_log.as_ref().map(|l| l.lock().unwrap().len()).unwrap_or(0);
    assert!(proxied > 0, "HARNESS: Pi never reached the holding proxy, so the post-TUI startup gap was not exercised");
    rig.finish();
}

#[test]
#[ignore = "physical: real Pi + tmux; run with --ignored --test-threads=1"]
#[serial(pi_worker_startup_ready_red)]
fn s4_restart_incarnation_does_not_reuse_readiness() {
    let mut rig = Rig::new("s4_restart_incarnation_does_not_reuse_readiness", Boot::SlowBeforeTui);
    // Incarnation 1: start, let it boot completely, then a warm send (independent of the fix under test).
    let n = rig.next_cmd;
    let script = rig.start_script(n);
    let n = rig.run(&script);
    let started = rig.wait_cmd(n, Duration::from_secs(60));
    assert!(started.is_some_and(|(rc, _)| rc == 0) && rig.receipt(n, "start")["ok"] == true,
        "HARNESS: incarnation 1 did not start: {:?}", rig.receipt(n, "start"));
    assert!(wait_screen(&rig, |s| s.contains("mock-1") && Rig::editor(s).is_some(), Duration::from_secs(60)),
        "HARNESS: incarnation 1 never drew its editor: {}", rig.worker_screen());
    std::thread::sleep(Duration::from_secs(8)); // well past the 3 s slow boot and Pi's startup phase
    let warm = tag("s4w");
    let n = rig.next_cmd;
    let script = rig.send_script(n, &warm);
    let n = rig.run(&script);
    rig.wait_cmd(n, Duration::from_secs(30));
    assert!(rig.wait_ingested(&warm, Duration::from_secs(20)).is_some(),
        "HARNESS: the warm send to incarnation 1 was not ingested; screen: {}", rig.worker_screen());
    // Incarnation 2: one command, exactly like an operator restart followed by an immediate task.
    let tag = tag("s4");
    let stop = format!("team-agent stop {WORKER} --json > '{}' && ", rig.cmds.join("stop.json").display());
    rig.cold_start_and_send("S4", &tag, Duration::from_secs(60), &stop);
    rig.finish();
}
