//! RED gate: resident memory of a long-lived `team-agent mcp-server`.
//!
//! A stdio MCP server lives as long as its host agent session (days). It must
//! stay near its idle baseline however large the workspace runtime state grows.
//! Forensics on 0.6.0 with a 1.1 MB state.json: RSS goes from 9.8 MB after
//! `tools/list` to 21.5 MB after one mailbox `send_message`, then to 47.4 MB
//! after one `get_team_status`. Each call re-parses and deep-clones the whole
//! state; macOS malloc keeps the freed pages resident, and in-process heap
//! trimming does not return them.
//!
//! Measured on the spawned process: macOS `proc_pid_rusage` (resident size and
//! phys_footprint, the Activity Monitor "Real Mem" and "Memory" columns); Linux
//! `/proc/<pid>/status` (VmRSS and RssAnon).
//!
//! Delta ceilings are checked on every build profile. Absolute ceilings are
//! checked when `TEAM_AGENT_MEMORY_PROBE_BIN` points at a release binary (the
//! verification pipeline does this).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[path = "support/hermetic.rs"]
mod hermetic_guard;
#[path = "support/memory_fixture.rs"]
mod memory_fixture;

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use hermetic_guard::HermeticTestEnv;

const MIB: u64 = 1024 * 1024;
const STATE_1X: usize = 1024 * 1024;
const STATE_4X: usize = 4 * 1024 * 1024;

/// Growth allowed over the post-`tools/list` baseline after 3 mailbox sends.
const SEND_DELTA_MAX: u64 = 2 * MIB;
/// Growth allowed over the baseline after 3 sends plus 2 `get_team_status`.
const STATUS_DELTA_MAX: u64 = 3 * MIB;
/// Growth allowed across 20 further mixed calls (plateau, anti-leak).
const PLATEAU_DELTA_MAX: u64 = MIB;
/// Final-checkpoint difference allowed between a 1 MiB and a 4 MiB state.
const SCALE_DELTA_MAX: u64 = MIB;
/// Release-binary absolute ceilings.
const IDLE_RSS_MAX: u64 = 11 * MIB;
const IDLE_FOOTPRINT_MAX: u64 = 4 * MIB;
const ACTIVE_RSS_MAX: u64 = 12 * MIB;
const ACTIVE_FOOTPRINT_MAX: u64 = 5 * MIB;
/// stdio servers must die with their host: exit after stdin EOF.
const EOF_EXIT_MAX: Duration = Duration::from_secs(2);

#[derive(Clone, Copy, Debug)]
struct Mem {
    rss: u64,
    footprint: u64,
}

#[cfg(target_os = "macos")]
fn sample(pid: u32) -> Option<Mem> {
    let mut info: libc::rusage_info_v4 = unsafe { std::mem::zeroed() };
    let rc = unsafe {
        libc::proc_pid_rusage(
            pid as libc::c_int,
            libc::RUSAGE_INFO_V4,
            (&mut info as *mut libc::rusage_info_v4).cast::<libc::rusage_info_t>(),
        )
    };
    assert_eq!(rc, 0, "proc_pid_rusage({pid}) failed");
    Some(Mem {
        rss: info.ri_resident_size,
        footprint: info.ri_phys_footprint,
    })
}

#[cfg(target_os = "linux")]
fn sample(pid: u32) -> Option<Mem> {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).expect("read /proc status");
    let kib = |key: &str| {
        status
            .lines()
            .find_map(|line| line.strip_prefix(key))
            .and_then(|rest| rest.split_whitespace().next())
            .and_then(|n| n.parse::<u64>().ok())
            .unwrap_or_else(|| panic!("{key} missing in /proc/{pid}/status"))
            * 1024
    };
    Some(Mem {
        rss: kib("VmRSS:"),
        footprint: kib("RssAnon:"),
    })
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn sample(_pid: u32) -> Option<Mem> {
    None
}

fn probe_binary() -> (PathBuf, bool) {
    match std::env::var_os("TEAM_AGENT_MEMORY_PROBE_BIN") {
        Some(path) => (PathBuf::from(path), true),
        None => (PathBuf::from(env!("CARGO_BIN_EXE_team-agent")), false),
    }
}

struct Server {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
}

impl Server {
    fn spawn(env: &HermeticTestEnv, binary: &Path, ws: &Path) -> Self {
        let mut child = Command::new(binary)
            .arg("mcp-server")
            .arg("--workspace")
            .arg(ws)
            .current_dir(ws)
            .env("HOME", env.home())
            .env("TEAM_AGENT_ID", memory_fixture::SENDER)
            .env("TEAM_AGENT_AGENT_ID", memory_fixture::SENDER)
            .env("TEAM_AGENT_OWNER_TEAM_ID", memory_fixture::ACTIVE_TEAM)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn team-agent mcp-server");
        let stdin = child.stdin.take();
        let stdout = BufReader::new(child.stdout.take().expect("stdout"));
        Self {
            child,
            stdin,
            stdout,
            next_id: 0,
        }
    }

    fn rpc(&mut self, method: &str, params: Value) -> Value {
        self.next_id += 1;
        let request =
            json!({"jsonrpc": "2.0", "id": self.next_id, "method": method, "params": params});
        let stdin = self.stdin.as_mut().expect("stdin open");
        writeln!(stdin, "{request}").expect("write request");
        stdin.flush().expect("flush request");
        let mut line = String::new();
        self.stdout.read_line(&mut line).expect("read response");
        let frame: Value = serde_json::from_str(&line)
            .unwrap_or_else(|err| panic!("{method}: invalid frame ({err}): {line}"));
        assert!(
            frame.get("error").is_none(),
            "{method}: JSON-RPC error frame: {frame}"
        );
        frame["result"].clone()
    }

    fn tool(&mut self, name: &str, arguments: Value) -> Value {
        let result = self.rpc("tools/call", json!({"name": name, "arguments": arguments}));
        let text = result["content"][0]["text"]
            .as_str()
            .unwrap_or_else(|| panic!("{name}: missing text content: {result}"));
        serde_json::from_str(text).unwrap_or_else(|err| panic!("{name}: body is not JSON ({err})"))
    }

    fn send_mailbox(&mut self, seq: usize) {
        let body = self.tool(
            "send_message",
            json!({"to": "leader", "content": format!("memory probe {seq}"), "mailbox": true}),
        );
        assert_eq!(
            body["ok"],
            json!(true),
            "mailbox send must be stored: {body}"
        );
    }

    fn status(&mut self) {
        let _ = self.tool("get_team_status", json!({}));
    }

    fn mem(&self) -> Mem {
        std::thread::sleep(Duration::from_millis(300));
        sample(self.child.id()).expect("platform sampler")
    }

    fn close_and_wait(&mut self) -> Duration {
        drop(self.stdin.take());
        let started = Instant::now();
        while self.child.try_wait().expect("try_wait").is_none() {
            if started.elapsed() > EOF_EXIT_MAX * 5 {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        started.elapsed()
    }
}

/// The test owns and reaps its child directly (no pid-number cleanup that could
/// hit a reused pid): close stdin, then kill only if still running.
impl Drop for Server {
    fn drop(&mut self) {
        drop(self.stdin.take());
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

#[derive(Debug)]
struct Run {
    state_bytes: usize,
    idle: Mem,
    after_send: Mem,
    after_status: Mem,
    after_plateau: Mem,
    eof_exit: Duration,
}

fn run_sequence(env: &HermeticTestEnv, state_target: usize) -> Run {
    let (binary, _) = probe_binary();
    let ws = env.workspace("resident-memory");
    let state_bytes = memory_fixture::seed_runtime_state(&ws, state_target);
    let mut server = Server::spawn(env, &binary, &ws);

    server.rpc("initialize", json!({"protocolVersion": "2024-11-05"}));
    server.rpc("tools/list", json!({}));
    let idle = server.mem();

    for seq in 0..3 {
        server.send_mailbox(seq);
    }
    let after_send = server.mem();

    server.status();
    server.status();
    let after_status = server.mem();

    for seq in 0..10 {
        server.send_mailbox(100 + seq);
        server.status();
    }
    let after_plateau = server.mem();

    let eof_exit = server.close_and_wait();
    let run = Run {
        state_bytes,
        idle,
        after_send,
        after_status,
        after_plateau,
        eof_exit,
    };
    eprintln!("[resident-memory] binary={} {run:?}", binary.display());
    run
}

fn mib(bytes: u64) -> String {
    format!("{:.1} MiB", bytes as f64 / MIB as f64)
}

fn check_delta(failures: &mut Vec<String>, label: &str, from: Mem, to: Mem, max: u64) {
    let rss = to.rss.saturating_sub(from.rss);
    let footprint = to.footprint.saturating_sub(from.footprint);
    if rss > max {
        failures.push(format!("{label}: RSS grew {} (> {})", mib(rss), mib(max)));
    }
    if footprint > max {
        failures.push(format!(
            "{label}: footprint grew {} (> {})",
            mib(footprint),
            mib(max)
        ));
    }
}

fn check_absolute(failures: &mut Vec<String>, label: &str, mem: Mem, rss_max: u64, fp_max: u64) {
    if mem.rss > rss_max {
        failures.push(format!(
            "{label}: RSS {} (> {})",
            mib(mem.rss),
            mib(rss_max)
        ));
    }
    if mem.footprint > fp_max {
        failures.push(format!(
            "{label}: footprint {} (> {})",
            mib(mem.footprint),
            mib(fp_max)
        ));
    }
}

fn platform_supported() -> bool {
    if sample(std::process::id()).is_none() {
        eprintln!("[resident-memory] no sampler on this platform; gate not applicable");
        return false;
    }
    true
}

#[test]
#[serial_test::serial(env)]
fn mcp_server_send_and_status_keep_resident_memory_at_idle_baseline() {
    if !platform_supported() {
        return;
    }
    let env = HermeticTestEnv::enter("mcp-resident-memory");
    let run = run_sequence(&env, STATE_1X);
    let mut failures = Vec::new();

    check_delta(
        &mut failures,
        "3x send_message(mailbox)",
        run.idle,
        run.after_send,
        SEND_DELTA_MAX,
    );
    check_delta(
        &mut failures,
        "+2x get_team_status",
        run.idle,
        run.after_status,
        STATUS_DELTA_MAX,
    );
    check_delta(
        &mut failures,
        "plateau over 20 more calls",
        run.after_status,
        run.after_plateau,
        PLATEAU_DELTA_MAX,
    );
    if probe_binary().1 {
        check_absolute(
            &mut failures,
            "idle after tools/list",
            run.idle,
            IDLE_RSS_MAX,
            IDLE_FOOTPRINT_MAX,
        );
        for (label, mem) in [
            ("after send_message", run.after_send),
            ("after get_team_status", run.after_status),
            ("after plateau", run.after_plateau),
        ] {
            check_absolute(
                &mut failures,
                label,
                mem,
                ACTIVE_RSS_MAX,
                ACTIVE_FOOTPRINT_MAX,
            );
        }
    }
    if run.eof_exit > EOF_EXIT_MAX {
        failures.push(format!(
            "stdin EOF exit took {:?} (> {EOF_EXIT_MAX:?})",
            run.eof_exit
        ));
    }

    assert!(
        failures.is_empty(),
        "resident memory gate failed for a {} byte state.json:\n  {}\n{run:?}",
        run.state_bytes,
        failures.join("\n  ")
    );
}

#[test]
#[serial_test::serial(env)]
fn mcp_server_resident_memory_is_independent_of_runtime_state_size() {
    if !platform_supported() {
        return;
    }
    let env = HermeticTestEnv::enter("mcp-resident-scale");
    let small = run_sequence(&env, STATE_1X);
    let large = run_sequence(&env, STATE_4X);
    let mut failures = Vec::new();
    for (label, a, b) in [
        ("after send_message", small.after_send, large.after_send),
        ("after plateau", small.after_plateau, large.after_plateau),
    ] {
        check_delta(
            &mut failures,
            &format!("{label}: 4x state vs 1x state"),
            a,
            b,
            SCALE_DELTA_MAX,
        );
    }
    assert!(
        failures.is_empty(),
        "resident memory scales with state.json size ({} -> {} bytes):\n  {}\nsmall={small:?}\nlarge={large:?}",
        small.state_bytes,
        large.state_bytes,
        failures.join("\n  ")
    );
}
