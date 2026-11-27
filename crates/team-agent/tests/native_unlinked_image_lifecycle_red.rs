//! Native seat whose provider image was updated and deleted: RED lifecycle suite (physical, isolated).
//!
//! Incident (2026-11-27). A team had six live `codex_tui` native seats. While they ran, the Codex self-updater
//! renamed their release directory to `releases/.retired.<x>` and then deleted it. The processes kept running from
//! the unlinked image. The user asked the leader to "restart the whole team and reset all roles". Every lifecycle
//! verb then refused, and each refusal named another verb that also refused:
//!   - `stop <seat>` and `stop --team`: `native stop refused (NoEffect) ... graceful exit read-only preflight: native
//!     instance not alive or replaced: Unknown; ... process_state=Some(Unknown(HostError { operation: "process
//!     image", .. }))`. The team stop also killed the legacy session with the Pi/Claude seats. Those rows stayed
//!     recorded as running, and status reported them unknown.
//!   - `agent reset <seat> --discard-session`: `agent_activity_unverified`, "stop the agent before resetting".
//!   - `restart <seat>` and `team restart`: refused.
//!   - `start <seat> --session fresh`: "agent ownership/liveness is unverified ... stop the scoped seat ... first".
//! macOS `proc_pidpath()` fails with ENOENT for a live process whose executable file was unlinked. pid, ppid and
//! start time still match. A rename alone keeps the path resolvable. Linux keeps `/proc/<pid>/exe` usable.
//!
//! Reproduction. `codex_tui` needs a real Codex subscription, so the fixture uses the same native lifecycle with
//! `pi_tui` (real Pi 1.6.1, mock model). Pi's npm launcher runs `#!/usr/bin/env node`, so the carrier image is the
//! `node` that the scratch PATH selects. That `node` is a private clone of the runner's node installation (keg) in
//! `<root>/rel/v1`, with its own inodes. A "provider update" does what the Codex and Homebrew updaters do: it clones
//! the next release, renames the running one to `.retired.<n>`, deletes it (Unlink), and repoints `bin/node`.
//! The Rename mode skips the delete.
//!
//! Frozen rules. Each test prints every violated rule together with its observation. Lifecycle verbs run from the
//! leader pane. Limits: stop 30 s; start, reset and restart 120 s.
//!   R1 stop, then fresh start (Unlink): `stop w1` exits 0 with ok:true. The carrier process is terminated within
//!      10 s, and status reports w1 stopped. Then `start w1 --session fresh` exits 0 with ok:true, and a new carrier
//!      runs that status reports running.
//!   R2 reset, in the incident order (Unlink): `agent reset w1 --discard-session` runs on the live seat. If it
//!      succeeds, fine. If it refuses, then the `stop w1` that it asks for must succeed, and a second
//!      `agent reset w1 --discard-session` must succeed. Either way the old carrier is gone, and a new carrier runs
//!      that status reports running.
//!   R3 restart (Unlink): `restart w1` exits 0 with ok:true. The old carrier is gone, and a new carrier runs that
//!      status reports running.
//!   R4 team stop of a mixed team (Unlink): native w1 plus legacy Pi worker w2, both running on the updated image.
//!      `stop` (whole team) exits 0 with ok:true. Both carriers are terminated within 10 s. Status reports both
//!      stopped, or reports the team as no longer alive.
//!   R5 a failed team stop records the stops it made (checked inside R4): if the team stop fails but still
//!      terminates a seat's carrier, status reports that seat stopped, not unknown. On the base, w2 is killed and
//!      then reported unknown, as in the incident.
//!   R6 one seat that must fail closed does not block the team stop (inside G2): `stop` (whole team) returns within
//!      30 s. It still terminates w2's verified carrier within 10 s, and status reports w2 stopped.
//!   G1 GUARD, linked image controls (Rename, green on the base): `stop w1`, then
//!      `agent reset w1 --discard-session`, then `restart w1`, then `stop` (team). Each exits 0 with ok:true and has
//!      the carrier effects that R1 to R4 require.
//!   G2 GUARD, fail closed on a foreign image: w1's carrier keeps its pid and start time but has exec'd into a
//!      different, linked program (`/bin/cat`, which records its terminal input). The following must hold:
//!        - `stop w1` and the team `stop` never signal that process;
//!        - neither command reports ok:true while that process lives;
//!        - status never reports w1 stopped;
//!        - no input byte is delivered to that terminal.
//!      Birth identity alone is not enough to signal or type into a carrier.
//!   Not covered black-box: PID reuse, where the birth identity differs. It must keep failing closed. That belongs
//!   to unit tests on the process sampler.
//!
//! Observables: rc, wall time and the `--json` stdout of each leader-pane command; `status --json` nodes
//! (`runtime_status`); the carrier pid (`state.json` → `teams.current.tmux_socket` plus the w1/w2 pane id, then
//! `tmux display-message '#{pane_pid}'`); and process liveness from `ps`. No other Team Agent internals are read.
//!
//! Run (macOS shows the bug; on Linux the R tests act as guards and are expected green on the base):
//!   cargo test -p team-agent --test native_unlinked_image_lifecycle_red -- --ignored --test-threads=1
//! Inputs: `TEAM_AGENT_PI_RED_PI_1_6_1` points at a `pi` executable. Otherwise Pi 1.6.1 is npm-installed once into
//! `$TEAM_AGENT_PI_RED_CACHE/pi-1.6.1`. `node`, `tmux` and `cp` must be on PATH.
//! Isolation: each test uses a scratch root with its own HOME (scratch `~/.pi/agent`, mock model only),
//! workspace, TMUX_TMPDIR, tmux servers and node clone. Every process starts from `.env_clear()` with an explicit
//! HOME and current directory. The leader is a bash pane with `TEAM_AGENT_LEADER_PROVIDER=pi`.
//! Receipts: `$CARGO_TARGET_TMPDIR/native-unlinked-image-red/<test>.json`. Set `TEAM_AGENT_NUI_RED_KEEP=1` to keep
//! the scratch root.

#![cfg(unix)]
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
const W1: &str = "w1";
const W2: &str = "w2";
const STOP_LIMIT: Duration = Duration::from_secs(30);
const START_LIMIT: Duration = Duration::from_secs(120);
const STATUS_LIMIT: Duration = Duration::from_secs(30);
const GONE_LIMIT: Duration = Duration::from_secs(10);

fn now() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs_f64()
}

fn unique() -> String {
    format!("{}{}", std::process::id(), SystemTime::now().duration_since(UNIX_EPOCH).unwrap().subsec_nanos() % 100_000)
}

fn scratch_base() -> PathBuf {
    let tmp = PathBuf::from("/private/tmp");
    if tmp.is_dir() { tmp } else { std::env::temp_dir() }
}

fn which(tool: &str) -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|paths| std::env::split_paths(&paths).map(|dir| dir.join(tool)).find(|p| p.is_file()))
}

fn pi_bin() -> PathBuf {
    let key = format!("TEAM_AGENT_PI_RED_PI_{}", PI_VERSION.replace('.', "_"));
    if let Some(path) = std::env::var_os(&key) {
        let path = PathBuf::from(path);
        assert!(path.exists(), "PREREQUISITE: {key}={} does not exist", path.display());
        return path;
    }
    let cache = std::env::var_os("TEAM_AGENT_PI_RED_CACHE").map(PathBuf::from)
        .unwrap_or_else(|| scratch_base().join("ta-pi-red-cache")).join(format!("pi-{PI_VERSION}"));
    let bin = cache.join("node_modules/.bin/pi");
    if !bin.exists() {
        std::fs::create_dir_all(&cache).unwrap();
        std::fs::write(cache.join("package.json"), "{\"private\":true}").unwrap();
        let npm = which("npm").expect("PREREQUISITE: `npm` is not on PATH");
        let out = Command::new(npm).env_clear().env("HOME", std::env::var_os("HOME").unwrap_or_default())
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .args(["install", "--no-audit", "--no-fund", "--ignore-scripts", "--silent",
                &format!("@earendil-works/pi-coding-agent@{PI_VERSION}")])
            .current_dir(&cache).output();
        assert!(matches!(&out, Ok(o) if o.status.success()) && bin.exists(),
            "PREREQUISITE: could not install Pi {PI_VERSION} into {} (set {key}): {out:?}", cache.display());
    }
    bin
}

fn text_of(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts.iter().filter_map(|p| p["text"].as_str()).collect::<Vec<_>>().join(""),
        _ => String::new(),
    }
}

/// In-process mock model (OpenAI chat-completions, streaming): every request is answered with `MOCK-ACK`.
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
        log.lock().unwrap().push(json!({"t": now(), "path": path, "last_user": last.chars().take(200).collect::<String>()}));
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
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Update {
    /// Rename the running release to `.retired.<n>`, then delete it (Codex self-update, `brew upgrade`).
    Unlink,
    /// Rename only: the running image stays linked under its new name.
    Rename,
}

/// One leader-pane command.
#[derive(Debug)]
struct Cmd {
    args: String,
    rc: Option<i32>,
    took: f64,
    json: Value,
}

impl Cmd {
    fn ok(&self) -> bool {
        self.rc == Some(0) && self.json["ok"] == true
    }
    fn summary(&self) -> Value {
        json!({"args": self.args, "rc": self.rc, "wall_s": (self.took * 100.0).round() / 100.0, "ok": self.json["ok"],
            "status": self.json["status"], "reason": self.json["reason"], "error": self.json["error"],
            "step_errors": self.json["steps"].as_array().map(|steps| steps.iter().map(|s| json!({
                "action": s["action"], "agent": s["agent"], "ok": s["ok"], "error": s["receipt"]["error"],
                "native": s["receipt"]["native_receipts"].as_array().map(|n| n.iter().map(|r| json!({
                    "agent": r["agent_id"], "effect": r["effect"], "error": r["error"]})).collect::<Vec<_>>())})).collect::<Vec<_>>())})
    }
}

struct Rig {
    name: String,
    root: PathBuf,
    home: PathBuf,
    ws: PathBuf,
    bin: PathBuf,
    rel: PathBuf,
    cmds: PathBuf,
    outer_sock: PathBuf,
    tmux_tmpdir: PathBuf,
    /// Absolute tools: every probe runs with a cleared environment, so PATH lookup is not available.
    tmux: PathBuf,
    cp: PathBuf,
    node_keg: PathBuf,
    /// Path of `node` inside the keg, e.g. `bin/node`.
    node_in_keg: PathBuf,
    release: usize,
    mock: Mock,
    next_cmd: usize,
    wedged: bool,
    carriers: Vec<u32>,
    t0: Instant,
    steps: Vec<Value>,
    violations: Vec<String>,
}

impl Rig {
    fn new(name: &str, foreign_exec_extension: bool) -> Rig {
        let pi = pi_bin();
        let root = scratch_base().join(format!("nui-{}-{}", &name[..2], unique()));
        let (home, ws, bin, rel, cmds, tmux_tmpdir) =
            (root.join("home"), root.join("ws"), root.join("bin"), root.join("rel"), root.join("cmds"), root.join("t"));
        let agent = home.join(".pi/agent");
        for dir in [&agent.join("extensions"), &ws, &bin, &rel, &cmds, &tmux_tmpdir] {
            std::fs::create_dir_all(dir).unwrap();
        }
        let mock = Mock::start();
        std::fs::write(agent.join("models.json"), json!({"providers": {"mock": {
            "baseUrl": format!("http://127.0.0.1:{}/v1", mock.port), "api": "openai-completions", "apiKey": "mock-key",
            "models": [{"id": "mock-1", "name": "Mock 1", "contextWindow": 128000, "maxTokens": 4096}]}}}).to_string()).unwrap();
        std::fs::write(agent.join("settings.json"), json!({"defaultProvider": "mock", "defaultModel": "mock-1",
            "defaultThinkingLevel": "off", "lastChangelogVersion": PI_VERSION}).to_string()).unwrap();
        if foreign_exec_extension {
            // Scratch-only: when `$HOME/exec-foreign` names this process, it execs (via sh) into /bin/cat, which
            // appends every byte its terminal receives to `$HOME/foreign-input.log`. It reopens /dev/tty because
            // node leaves the inherited terminal descriptor non-blocking. The pid and start time stay
            // the same; the image becomes a different, linked program.
            std::fs::write(agent.join("extensions/foreign-exec.js"),
                "import fs from \"node:fs\";\n\
                 const trigger = `${process.env.HOME}/exec-foreign`;\n\
                 if (process.stdout.isTTY && typeof process.execve === \"function\") {\n\
                 \x20 setInterval(() => {\n\
                 \x20   let target = \"\"; try { target = fs.readFileSync(trigger, \"utf8\").trim(); } catch {}\n\
                 \x20   if (target === String(process.pid)) process.execve(\"/bin/sh\", [\"sh\", \"-c\", `exec /bin/cat < /dev/tty >> \"${process.env.HOME}/foreign-input.log\"`], process.env);\n\
                 \x20 }, 200);\n\
                 }\n\
                 export default function foreignExec(pi) {}\n").unwrap();
        }
        let tmux = which("tmux").expect("PREREQUISITE: `tmux` is not on PATH");
        let cp = which("cp").expect("PREREQUISITE: `cp` is not on PATH");
        let node = which("node").expect("PREREQUISITE: `node` is not on PATH").canonicalize().unwrap();
        let parent = node.parent().unwrap();
        let node_keg = (if parent.file_name().is_some_and(|n| n == "bin") { parent.parent().unwrap() } else { parent }).to_path_buf();
        let node_in_keg = node.strip_prefix(&node_keg).unwrap().to_path_buf();
        let candidate = PathBuf::from(env!("CARGO_BIN_EXE_team-agent"));
        std::os::unix::fs::symlink(&pi, bin.join("pi")).unwrap();
        std::os::unix::fs::symlink(&candidate, bin.join("team-agent")).unwrap();
        std::os::unix::fs::symlink(&tmux, bin.join("tmux")).unwrap();
        for tool in ["fd", "rg"] {
            if let Some(path) = which(tool) { std::os::unix::fs::symlink(path, bin.join(tool)).unwrap(); }
        }
        let driver = root.join("driver.sh");
        std::fs::write(&driver, format!(
            "#!/bin/bash\ncd '{ws}' || exit 97\nn=1\nwhile :; do\n  if [ -f '{c}'/$n.sh ]; then\n    \
             bash '{c}'/$n.sh > '{c}'/$n.out 2>&1; echo $? > '{c}'/$n.rc.tmp; mv '{c}'/$n.rc.tmp '{c}'/$n.rc; n=$((n+1))\n  \
             else sleep 0.1; fi\ndone\n", ws = ws.display(), c = cmds.display())).unwrap();
        let mut rig = Rig { name: name.into(), root: root.clone(), home: home.clone(), ws: ws.clone(), bin: bin.clone(), rel,
            cmds, outer_sock: root.join("outer.sock"), tmux_tmpdir: tmux_tmpdir.clone(), tmux: tmux.clone(), cp, node_keg,
            node_in_keg, release: 1, mock, next_cmd: 1, wedged: false, carriers: vec![], t0: Instant::now(), steps: vec![],
            violations: vec![] };
        rig.clone_release(1);
        std::os::unix::fs::symlink(rig.release_node(1), bin.join("node")).unwrap();
        let probe = Command::new(rig.release_node(1)).env_clear().env("HOME", &home).current_dir(&ws).arg("--version").output().unwrap();
        assert!(probe.status.success(), "PREREQUISITE: the cloned node release does not run: {probe:?}");
        let user = std::env::var("USER").unwrap_or_else(|_| "user".into());
        let path_env = format!("{}:/usr/bin:/bin:/usr/sbin:/sbin", bin.display());
        let status = Command::new(&tmux).env_clear()
            .env("HOME", &home).env("USER", &user).env("LOGNAME", &user).env("PATH", &path_env)
            .env("TMUX_TMPDIR", &tmux_tmpdir).env("TERM", "xterm-256color").env("LANG", "en_US.UTF-8")
            .env("LC_ALL", "en_US.UTF-8").env("SHELL", "/bin/bash").env("TEAM_AGENT_LEADER_PROVIDER", "pi")
            .env("PI_OFFLINE", "1").current_dir(&ws)
            .arg("-S").arg(&rig.outer_sock)
            .args(["-f", "/dev/null", "new-session", "-d", "-s", "outer", "-x", "160", "-y", "50"])
            .arg(format!("bash '{}'", driver.display()))
            .status().unwrap();
        assert!(status.success(), "HARNESS: outer tmux failed to start");
        rig.step("rig ready", json!({"pi": pi.display().to_string(), "candidate": candidate.display().to_string(),
            "node_keg": rig.node_keg.display().to_string(), "root": root.display().to_string()}));
        rig
    }

    fn release_dir(&self, n: usize) -> PathBuf {
        self.rel.join(format!("v{n}"))
    }

    fn release_node(&self, n: usize) -> PathBuf {
        self.release_dir(n).join(&self.node_in_keg)
    }

    /// A private copy of the node keg with its own inodes (APFS clone when available).
    fn clone_release(&self, n: usize) {
        let dst = self.release_dir(n);
        let run = |flags: &str| Command::new(&self.cp).env_clear().env("HOME", &self.home).current_dir(&self.rel)
            .arg(flags).arg(&self.node_keg).arg(&dst).status().is_ok_and(|s| s.success());
        if !run("-cR") {
            let _ = std::fs::remove_dir_all(&dst);
            assert!(run("-R"), "PREREQUISITE: could not copy {} to {}", self.node_keg.display(), dst.display());
        }
    }

    /// The provider update: clone the next release, retire the running one, optionally delete it, repoint `bin/node`.
    fn update_provider(&mut self, update: Update) {
        let (old, next) = (self.release, self.release + 1);
        self.clone_release(next);
        let retired = self.rel.join(format!(".retired.{old}"));
        std::fs::rename(self.release_dir(old), &retired).unwrap();
        if update == Update::Unlink {
            std::fs::remove_dir_all(&retired).unwrap();
        }
        let tmp = self.bin.join("node.next");
        std::os::unix::fs::symlink(self.release_node(next), &tmp).unwrap();
        std::fs::rename(&tmp, self.bin.join("node")).unwrap();
        self.release = next;
        let carriers: Vec<Value> = self.carriers.clone().into_iter().map(|pid| json!({"pid": pid, "alive": alive(pid),
            "txt": self.text_image(pid)})).collect();
        self.step("provider update", json!({"mode": format!("{update:?}"), "retired": retired.display().to_string(),
            "running_release_exists": retired.exists(), "carriers": carriers}));
    }

    /// Diagnostic only: the carrier's mapped executable as `lsof` reports it.
    fn text_image(&self, pid: u32) -> Option<String> {
        let lsof = ["/usr/sbin/lsof", "/usr/bin/lsof"].into_iter().map(PathBuf::from).find(|p| p.is_file())?;
        let out = Command::new(lsof).env_clear().env("HOME", &self.home).current_dir(&self.ws)
            .args(["-a", "-p", &pid.to_string(), "-d", "txt", "-Fn"]).output().ok()?;
        String::from_utf8_lossy(&out.stdout).lines().find_map(|l| l.strip_prefix('n').map(str::to_string))
    }

    fn step(&mut self, what: &str, data: Value) {
        self.steps.push(json!({"t": (self.t0.elapsed().as_secs_f64() * 100.0).round() / 100.0, "step": what, "data": data}));
    }

    fn check(&mut self, ok: bool, rule: &str, observed: impl std::fmt::Display) {
        self.step(if ok { "rule held" } else { "RULE VIOLATED" }, json!({"rule": rule, "observed": observed.to_string()}));
        if !ok {
            self.violations.push(format!("{rule} | observed: {observed}"));
        }
    }

    /// A harness precondition: the fixture itself is broken if this fails, so the test stops here.
    fn require(&mut self, ok: bool, what: &str, observed: impl std::fmt::Display) {
        if !ok {
            self.step("HARNESS PRECONDITION FAILED", json!({"what": what, "observed": observed.to_string()}));
            let path = self.write_receipt();
            panic!("[{}] HARNESS PRECONDITION FAILED: {what} | observed: {observed}\n  receipt: {}", self.name, path.display());
        }
    }

    /// Run `team-agent <args> --workspace <ws> --team current --json` from the leader pane.
    fn ta(&mut self, args: &str, limit: Duration) -> Cmd {
        let full = format!("{args} --workspace '{}' --team current --json", self.ws.display());
        if self.wedged {
            self.step("command skipped (leader pane wedged)", json!({"args": full}));
            return Cmd { args: full, rc: None, took: 0.0, json: Value::Null };
        }
        let n = self.next_cmd;
        self.next_cmd += 1;
        let out = self.cmds.join(format!("{n}.json"));
        let tmp = self.cmds.join(format!("{n}.sh.tmp"));
        std::fs::write(&tmp, format!("team-agent {full} > '{}'\n", out.display())).unwrap();
        std::fs::rename(&tmp, self.cmds.join(format!("{n}.sh"))).unwrap();
        let started = Instant::now();
        let rc_path = self.cmds.join(format!("{n}.rc"));
        let mut rc = None;
        while started.elapsed() < limit {
            if let Ok(s) = std::fs::read_to_string(&rc_path) {
                rc = Some(s.trim().parse().unwrap_or(-1));
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let took = started.elapsed().as_secs_f64();
        let stdout = std::fs::read_to_string(&out).unwrap_or_default();
        let parsed = serde_json::from_str(&stdout).ok()
            .or_else(|| stdout.find('{').and_then(|i| serde_json::from_str(&stdout[i..]).ok())).unwrap_or(Value::Null);
        let stderr = std::fs::read_to_string(self.cmds.join(format!("{n}.out"))).unwrap_or_default();
        if rc.is_none() {
            self.wedged = true;
        }
        self.step("leader pane command", json!({"n": n, "args": full, "rc": rc, "wall_s": took, "limit_s": limit.as_secs(),
            "stdout": parsed.clone(), "stdout_raw": if parsed.is_null() { stdout } else { String::new() }, "stderr": stderr}));
        Cmd { args: args.into(), rc, took, json: parsed }
    }

    fn state(&self) -> Value {
        std::fs::read_to_string(self.ws.join(".team/runtime/state.json")).ok()
            .and_then(|s| serde_json::from_str(&s).ok()).unwrap_or(Value::Null)
    }

    fn tmux_cmd(&self) -> Command {
        let mut cmd = Command::new(&self.tmux);
        cmd.env_clear().env("HOME", &self.home).env("TMUX_TMPDIR", &self.tmux_tmpdir).current_dir(&self.ws);
        cmd
    }

    /// The live carrier pid of `agent`: its recorded pane on the team's tmux server.
    fn carrier(&mut self, agent: &str) -> Option<u32> {
        let state = self.state();
        let team = &state["teams"]["current"];
        let row = &team["agents"][agent];
        let sock = row["native_endpoint"].as_str().or(row["tmux_endpoint"].as_str()).or(team["tmux_socket"].as_str())
            .or(state["tmux_socket"].as_str())?.to_string();
        let pane = row["native_pane"].as_str().or(row["pane_id"].as_str())?.to_string();
        let out = self.tmux_cmd().arg("-S").arg(&sock).args(["display-message", "-p", "-t", &pane, "#{pane_pid}"]).output().ok()?;
        let pid = String::from_utf8_lossy(&out.stdout).trim().parse().ok()?;
        if !self.carriers.contains(&pid) {
            self.carriers.push(pid);
        }
        Some(pid)
    }

    /// `runtime_status` of `agent` from `status --json`; `team_not_alive` when status no longer lists the team.
    fn status(&mut self, agent: &str) -> String {
        let out = self.ta("status", STATUS_LIMIT);
        if let Some(node) = out.json["nodes"].as_array().and_then(|nodes| nodes.iter().find(|n| n["name"] == agent)) {
            return node["runtime_status"].as_str().unwrap_or("missing").to_string();
        }
        if out.json["error"].as_str().is_some_and(|e| e.contains("not found among alive teams")) {
            return "team_not_alive".into();
        }
        format!("unreadable(rc={:?})", out.rc)
    }

    fn wait_gone(&self, pid: u32, limit: Duration) -> bool {
        let started = Instant::now();
        while started.elapsed() < limit {
            if !alive(pid) { return true; }
            std::thread::sleep(Duration::from_millis(100));
        }
        !alive(pid)
    }

    fn start_native(&mut self, agent: &str) -> u32 {
        let out = self.ta(&format!("start {agent} --provider pi_tui --model mock/mock-1 --session fresh"), START_LIMIT);
        self.require(out.ok(), &format!("native `start {agent}` succeeds on the unmodified image"), out.summary());
        let pid = self.carrier(agent);
        self.require(pid.is_some_and(alive), &format!("{agent} has a live carrier after start"), format!("{pid:?}"));
        pid.unwrap()
    }

    fn start_legacy(&mut self, agent: &str) -> u32 {
        let out = self.ta(&format!("start {agent} --provider pi --model mock/mock-1 --session fresh"), START_LIMIT);
        self.require(out.ok(), &format!("legacy `start {agent}` succeeds on the unmodified image"), out.summary());
        let pid = self.carrier(agent);
        self.require(pid.is_some_and(alive), &format!("{agent} has a live carrier after start"), format!("{pid:?}"));
        pid.unwrap()
    }

    /// After a verb that must relaunch: the old carrier is gone, a new one runs, status says running.
    fn assert_relaunched(&mut self, rule: &str, agent: &str, old: u32) {
        let gone = self.wait_gone(old, GONE_LIMIT);
        self.check(gone, &format!("{rule} the old carrier {old} is terminated"), format!("alive={}", alive(old)));
        let new = self.carrier(agent);
        self.check(new.is_some_and(|p| p != old && alive(p)), &format!("{rule} a new carrier runs for {agent}"), format!("{new:?} (old {old})"));
        let status = self.status(agent);
        self.check(status == "running", &format!("{rule} status reports {agent} running"), &status);
    }

    fn write_receipt(&self) -> PathBuf {
        let receipt = json!({"test": self.name, "pi_version": PI_VERSION, "steps": self.steps, "violations": self.violations,
            "model_requests": self.mock.log.lock().unwrap().len()});
        let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("native-unlinked-image-red");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("{}.json", self.name));
        std::fs::write(&path, serde_json::to_string_pretty(&receipt).unwrap()).unwrap();
        path
    }

    fn finish(mut self) {
        let path = self.write_receipt();
        let violations = std::mem::take(&mut self.violations);
        if !violations.is_empty() {
            panic!("\n[{}] {} CONTRACT VIOLATION(S):\n  - {}\n  receipt: {}\n", self.name, violations.len(),
                violations.join("\n  - "), path.display());
        }
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        let servers = std::fs::read_dir(&self.tmux_tmpdir).into_iter().flatten().flatten()
            .flat_map(|d| std::fs::read_dir(d.path()).into_iter().flatten().flatten().map(|s| s.path()).collect::<Vec<_>>());
        for sock in servers.chain(std::iter::once(self.outer_sock.clone())) {
            let _ = self.tmux_cmd().arg("-S").arg(&sock).arg("kill-server").output();
        }
        std::thread::sleep(Duration::from_millis(500));
        for pid in self.carriers.clone() {
            if alive(pid) {
                let _ = Command::new("/bin/kill").env_clear().env("HOME", &self.home).current_dir("/")
                    .args(["-9", &pid.to_string()]).output();
            }
        }
        if std::env::var_os("TEAM_AGENT_NUI_RED_KEEP").is_none() {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }
}

/// A live, non-zombie process.
fn alive(pid: u32) -> bool {
    let out = Command::new("/bin/ps").env_clear().env("HOME", "/").current_dir("/")
        .args(["-o", "stat=", "-p", &pid.to_string()]).output();
    out.is_ok_and(|o| { let stat = String::from_utf8_lossy(&o.stdout).trim().to_string(); !stat.is_empty() && !stat.starts_with('Z') })
}

fn command_name(pid: u32) -> String {
    let out = Command::new("/bin/ps").env_clear().env("HOME", "/").current_dir("/")
        .args(["-o", "comm=", "-p", &pid.to_string()]).output();
    out.map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap_or_default()
}

fn require_survives_update(rig: &mut Rig, pids: &[u32]) {
    let survivors: Vec<_> = pids.iter().map(|pid| (*pid, alive(*pid))).collect();
    rig.require(survivors.iter().all(|(_, a)| *a), "the carriers keep running after the provider update", format!("{survivors:?}"));
}

// ---------------------------------------------------------------------------------------------------------------
// Tests

#[test]
#[ignore = "physical: real Pi + tmux; run with --ignored --test-threads=1"]
#[serial(native_unlinked_image_red)]
fn r1_stop_then_fresh_start_after_the_provider_image_is_deleted() {
    let mut rig = Rig::new("r1_stop_then_fresh_start_after_the_provider_image_is_deleted", false);
    let old = rig.start_native(W1);
    rig.update_provider(Update::Unlink);
    require_survives_update(&mut rig, &[old]);
    let stop = rig.ta(&format!("stop {W1}"), STOP_LIMIT);
    rig.check(stop.ok(), "R1 `stop w1` exits 0 with ok:true within 30s", stop.summary());
    let gone = rig.wait_gone(old, GONE_LIMIT);
    rig.check(gone, "R1 the carrier process is terminated within 10s", format!("pid {old} alive={}", alive(old)));
    let status = rig.status(W1);
    rig.check(status == "stopped", "R1 status reports w1 stopped", &status);
    let start = rig.ta(&format!("start {W1} --session fresh"), START_LIMIT);
    rig.check(start.ok(), "R1 `start w1 --session fresh` exits 0 with ok:true within 120s", start.summary());
    let new = rig.carrier(W1);
    rig.check(new.is_some_and(|p| p != old && alive(p)), "R1 a new carrier runs for w1", format!("{new:?} (old {old})"));
    let status = rig.status(W1);
    rig.check(status == "running", "R1 status reports w1 running", &status);
    rig.finish();
}

#[test]
#[ignore = "physical: real Pi + tmux; run with --ignored --test-threads=1"]
#[serial(native_unlinked_image_red)]
fn r2_reset_discard_session_in_the_incident_order_after_the_image_is_deleted() {
    let mut rig = Rig::new("r2_reset_discard_session_in_the_incident_order_after_the_image_is_deleted", false);
    let old = rig.start_native(W1);
    rig.update_provider(Update::Unlink);
    require_survives_update(&mut rig, &[old]);
    let reset = rig.ta(&format!("agent reset {W1} --discard-session"), START_LIMIT);
    if !reset.ok() {
        rig.step("reset refused on the live seat; following its guidance", reset.summary());
        let stop = rig.ta(&format!("stop {W1}"), STOP_LIMIT);
        rig.check(stop.ok(), "R2 the `stop w1` that the refused reset asks for exits 0 with ok:true within 30s", stop.summary());
        let again = rig.ta(&format!("agent reset {W1} --discard-session"), START_LIMIT);
        rig.check(again.ok(), "R2 `agent reset w1 --discard-session` after that stop exits 0 with ok:true within 120s", again.summary());
    }
    rig.assert_relaunched("R2", W1, old);
    rig.finish();
}

#[test]
#[ignore = "physical: real Pi + tmux; run with --ignored --test-threads=1"]
#[serial(native_unlinked_image_red)]
fn r3_restart_after_the_provider_image_is_deleted() {
    let mut rig = Rig::new("r3_restart_after_the_provider_image_is_deleted", false);
    let old = rig.start_native(W1);
    rig.update_provider(Update::Unlink);
    require_survives_update(&mut rig, &[old]);
    let restart = rig.ta(&format!("restart {W1}"), START_LIMIT);
    rig.check(restart.ok(), "R3 `restart w1` exits 0 with ok:true within 120s", restart.summary());
    rig.assert_relaunched("R3", W1, old);
    rig.finish();
}

#[test]
#[ignore = "physical: real Pi + tmux; run with --ignored --test-threads=1"]
#[serial(native_unlinked_image_red)]
fn r4_team_stop_of_a_mixed_team_after_the_provider_image_is_deleted() {
    let mut rig = Rig::new("r4_team_stop_of_a_mixed_team_after_the_provider_image_is_deleted", false);
    let native = rig.start_native(W1);
    let legacy = rig.start_legacy(W2);
    rig.update_provider(Update::Unlink);
    require_survives_update(&mut rig, &[native, legacy]);
    let stop = rig.ta("stop", STOP_LIMIT);
    rig.check(stop.ok(), "R4 team `stop` exits 0 with ok:true within 30s", stop.summary());
    for (agent, pid) in [(W1, native), (W2, legacy)] {
        let gone = rig.wait_gone(pid, GONE_LIMIT);
        rig.check(gone, &format!("R4 the {agent} carrier is terminated within 10s"), format!("pid {pid} alive={}", alive(pid)));
        let status = rig.status(agent);
        let rule = if gone && !stop.ok() {
            format!("R5 the failed team stop terminated {agent}'s carrier, so status reports {agent} stopped")
        } else {
            format!("R4 status reports {agent} stopped (or the team not alive)")
        };
        rig.check(status == "stopped" || status == "team_not_alive", &rule, &status);
    }
    rig.finish();
}

#[test]
#[ignore = "physical: real Pi + tmux; run with --ignored --test-threads=1"]
#[serial(native_unlinked_image_red)]
fn g2_r6_a_foreign_image_in_a_carrier_fails_closed_without_blocking_the_team_stop() {
    let mut rig = Rig::new("g2_r6_a_foreign_image_in_a_carrier_fails_closed_without_blocking_the_team_stop", true);
    let native = rig.start_native(W1);
    let legacy = rig.start_legacy(W2);
    let before = command_name(native);
    std::fs::write(rig.home.join("exec-foreign"), native.to_string()).unwrap();
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(10) && !command_name(native).ends_with("cat") {
        std::thread::sleep(Duration::from_millis(100));
    }
    std::thread::sleep(Duration::from_secs(1));
    let after = command_name(native);
    let log = rig.home.join("foreign-input.log");
    let size = |log: &PathBuf| std::fs::metadata(log).map(|m| m.len()).unwrap_or(0);
    let baseline = size(&log);
    rig.step("w1 carrier exec'd into a foreign image", json!({"pid": native, "before": before, "after": after,
        "txt": rig.text_image(native), "input_bytes_before_stops": baseline}));
    rig.require(alive(native) && after.ends_with("cat") && log.exists(), "w1's carrier (same pid) now runs /bin/cat",
        format!("{before:?} -> {after:?}, log exists={}", log.exists()));
    let foreign_untouched = |rig: &Rig| alive(native) && command_name(native).ends_with("cat");
    let seat = rig.ta(&format!("stop {W1}"), STOP_LIMIT);
    std::thread::sleep(Duration::from_secs(1));
    rig.check(foreign_untouched(&rig), "G2 `stop w1` never signals the foreign process in w1's pane",
        format!("pid {native} alive={} comm={:?}", alive(native), command_name(native)));
    rig.check(!seat.ok(), "G2 `stop w1` does not report ok:true while w1's foreign process lives", seat.summary());
    let team = rig.ta("stop", STOP_LIMIT);
    std::thread::sleep(Duration::from_secs(1));
    rig.check(foreign_untouched(&rig), "G2 team `stop` never signals the foreign process in w1's pane",
        format!("pid {native} alive={} comm={:?}", alive(native), command_name(native)));
    rig.check(!team.ok(), "G2 team `stop` does not report ok:true while w1's foreign process lives", team.summary());
    let delivered = size(&log).saturating_sub(baseline);
    let bytes = std::fs::read(&log).unwrap_or_default();
    rig.check(delivered == 0, "G2 no input is delivered to the terminal of the foreign process",
        format!("{delivered} bytes: {:?}", String::from_utf8_lossy(&bytes[(baseline as usize).min(bytes.len())..]).chars().take(300).collect::<String>()));
    let w1_status = rig.status(W1);
    rig.check(w1_status != "stopped", "G2 status never reports w1 stopped while its foreign process lives", &w1_status);
    rig.check(team.rc.is_some() && team.took <= STOP_LIMIT.as_secs_f64(), "R6 the team stop returns within 30s", team.summary());
    let gone = rig.wait_gone(legacy, GONE_LIMIT);
    rig.check(gone, "R6 the team stop still terminates w2's verified carrier within 10s", format!("pid {legacy} alive={}", alive(legacy)));
    let w2_status = rig.status(W2);
    rig.check(w2_status == "stopped", "R6 status reports w2 stopped", &w2_status);
    rig.finish();
}

#[test]
#[ignore = "physical: real Pi + tmux; run with --ignored --test-threads=1"]
#[serial(native_unlinked_image_red)]
fn g1_linked_image_controls_every_verb_works_after_a_rename_only_update() {
    let mut rig = Rig::new("g1_linked_image_controls_every_verb_works_after_a_rename_only_update", false);
    let first = rig.start_native(W1);
    rig.update_provider(Update::Rename);
    require_survives_update(&mut rig, &[first]);
    let stop = rig.ta(&format!("stop {W1}"), STOP_LIMIT);
    rig.check(stop.ok(), "G1 `stop w1` exits 0 with ok:true within 30s", stop.summary());
    let gone = rig.wait_gone(first, GONE_LIMIT);
    rig.check(gone, "G1 the carrier process is terminated within 10s", format!("pid {first} alive={}", alive(first)));
    let status = rig.status(W1);
    rig.check(status == "stopped", "G1 status reports w1 stopped", &status);
    let reset = rig.ta(&format!("agent reset {W1} --discard-session"), START_LIMIT);
    rig.check(reset.ok(), "G1 `agent reset w1 --discard-session` exits 0 with ok:true within 120s", reset.summary());
    let second = rig.carrier(W1);
    rig.check(second.is_some_and(|p| p != first && alive(p)), "G1 reset leaves a new carrier running", format!("{second:?}"));
    if let Some(second) = second {
        rig.update_provider(Update::Rename);
        let restart = rig.ta(&format!("restart {W1}"), START_LIMIT);
        rig.check(restart.ok(), "G1 `restart w1` exits 0 with ok:true within 120s", restart.summary());
        rig.assert_relaunched("G1", W1, second);
    }
    let third = rig.carrier(W1);
    let team = rig.ta("stop", STOP_LIMIT);
    rig.check(team.ok(), "G1 team `stop` exits 0 with ok:true within 30s", team.summary());
    if let Some(third) = third {
        let gone = rig.wait_gone(third, GONE_LIMIT);
        rig.check(gone, "G1 the team stop terminates the carrier within 10s", format!("pid {third} alive={}", alive(third)));
    }
    rig.finish();
}

/// The suite stays self-contained: physical tests above are serial and ignored, and every process is spawned
/// from a cleared environment with an explicit HOME and current directory.
#[test]
fn static_guard_every_command_is_isolated() {
    let source = include_str!("native_unlinked_image_lifecycle_red.rs");
    let spawns = source.matches(concat!("Command::", "new(")).count();
    let cleared = source.matches(concat!(".env_", "clear()")).count();
    assert!(spawns > 0 && cleared >= spawns, "every Command must start from a cleared environment ({cleared}/{spawns})");
}
