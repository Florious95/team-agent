//! RED gates R4 / R4b: the attached managed leader launcher.
//!
//! `team-agent <provider>` run outside tmux creates the managed leader pane and
//! stays resident while the user's `tmux attach-session` client is open (often
//! for hours).
//!
//! R4: on 0.6.0, `wait_managed_attach_child` sleeps 200 ms and runs a
//! provider-health probe each time; every probe forks `tmux`. Measured in the
//! field: about 5 tmux forks per second and 0.7% CPU continuously (1:38 of CPU
//! in 2h43m) for one idle launcher. Contract: after the bounded startup handoff
//! (the startup-prompt handler stops at a Ready screen and runs for at most
//! 30 x 0.5 s), the attached idle launcher may run at most one tmux command in
//! a 10 s window (for example a single blocking `wait-for`). Its own CPU time
//! over that window must be at most 50 ms.
//!
//! R4b: removing idle polling must not remove the bounded startup handoff. A
//! workspace-trust prompt that appears a few seconds after launch must still be
//! answered automatically while the operator is attached.
//!
//! Hermetic: a PATH `tmux` shim records every invocation and emulates the
//! managed launch and the provider screen. Its `attach-session` blocks like an
//! attached client. No real tmux server or provider is used, and sockets live
//! in a process-owned short root.

#![cfg(unix)]
#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

#[path = "support/hermetic.rs"]
mod hermetic_guard;

use std::ffi::CStr;
use std::fs::{self, File};
use std::io;
use std::os::fd::FromRawFd;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixListener;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serial_test::serial;
use team_agent::tmux_backend::TmuxBackend;
use team_agent::transport::Transport;

/// Upper bound of the startup-prompt handoff (30 checks x 0.5 s) plus margin.
const STARTUP_HANDOFF: Duration = Duration::from_secs(16);
const IDLE_WINDOW: Duration = Duration::from_secs(10);
const IDLE_TMUX_INVOCATIONS_MAX: usize = 1;
const IDLE_CPU_MAX_MS: u64 = 50;
const ATTACH_DEADLINE: Duration = Duration::from_secs(30);
/// The trust prompt appears on the 5th screen capture (about 2 s after launch).
const TRUST_AFTER_CAPTURES: &str = "4";
const TRUST_ANSWER_DEADLINE: Duration = Duration::from_secs(20);

const PROVIDER_SHIM: &str = r#"#!/bin/sh
if [ "${1-}" = "--version" ]; then
  printf 'codex 1.0.0\n'
  exit 0
fi
exit 0
"#;

/// Records each invocation and emulates the managed launch. `capture-pane`
/// renders the provider screen: blank for the first R4_TRUST_AFTER captures,
/// then a workspace-trust menu until a key is sent, then the Ready screen.
/// Without R4_TRUST_AFTER the screen is Ready from the first capture.
/// `attach-session` blocks like a real attached client until it is signalled.
const TMUX_SHIM: &str = r#"#!/bin/sh
printf '%s\n' "$*" >> "$R4_TMUX_LOG"
previous=
target=
last=
spawn_session=
for arg in "$@"; do
  if [ "$previous" = "-t" ]; then target="$arg"; fi
  if [ "$previous" = "-s" ]; then spawn_session="$arg"; fi
  previous="$arg"
  last="$arg"
done
if [ "${1-}" = "-V" ]; then
  printf 'tmux 3.4\n'
  exit 0
fi
case " $* " in
  *" has-session "*) exit 1 ;;
  *" new-session "*|*" new-window "*)
    if [ -n "$spawn_session" ]; then printf '%s\n' "$spawn_session" > "$R4_SPAWN_SESSION"; fi
    printf '%%managed-r4\n'
    exit 0
    ;;
  *" list-panes "*)
    session=managed-leader
    if [ -f "$R4_SPAWN_SESSION" ]; then session=$(cat "$R4_SPAWN_SESSION"); fi
    printf '%%managed-r4\t%s\t0\tcodex\t0\t%s\tcodex\t1\t%s\t1\t0\t4103\t\n' \
      "$session" "$R4_PANE_TTY" "$R4_WORKSPACE"
    exit 0
    ;;
  *" capture-pane "*)
    if [ -z "${R4_TRUST_AFTER-}" ] || [ -f "$R4_STATE.answered" ]; then
      printf 'OpenAI Codex\n\n\342\200\272 \n'
      exit 0
    fi
    printf 'c\n' >> "$R4_STATE.captures"
    if [ "$(wc -l < "$R4_STATE.captures")" -gt "$R4_TRUST_AFTER" ]; then
      : > "$R4_STATE.shown"
      printf 'Do you trust the contents of this directory?\n\n\342\200\272 1. Yes, continue\n  2. No, quit\n'
    fi
    exit 0
    ;;
  *" send-keys "*)
    if [ -f "$R4_STATE.shown" ]; then : > "$R4_STATE.answered"; fi
    exit 0
    ;;
  *" display-message "*)
    case "$last" in
      '#{pane_id}') printf '%s\n' "${target:-%managed-r4}" ;;
      '#{pane_current_command}') printf 'codex\n' ;;
      '#{pane_current_path}') printf '%s\n' "$R4_WORKSPACE" ;;
      '#{pane_tty}') printf '%s\n' "$R4_PANE_TTY" ;;
      '#{pane_dead}') printf '0\n' ;;
      '#{session_name}') printf 'managed-leader\n' ;;
      '#{pane_width}') printf '120\n' ;;
      '#{pane_mode}') printf '0\n' ;;
      *) printf '%s\n' "${target:-%managed-r4}" ;;
    esac
    exit 0
    ;;
  *" attach-session "*) exec sleep 600 ;;
  *) exit 0 ;;
esac
"#;

struct ShortRoot(PathBuf);

impl Drop for ShortRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn write_executable(path: &Path, body: &str) {
    fs::write(path, body).expect("write executable fixture");
    let mut permissions = fs::metadata(path).expect("fixture metadata").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).expect("chmod fixture");
}

fn open_pty() -> io::Result<(File, File, String)> {
    let (mut master, mut slave) = (-1, -1);
    let (name_out, termp, winp) = (
        std::ptr::null_mut(),
        std::ptr::null_mut(),
        std::ptr::null_mut(),
    );
    if unsafe { libc::openpty(&mut master, &mut slave, name_out, termp, winp) } == -1 {
        return Err(io::Error::last_os_error());
    }
    let mut name = [0 as libc::c_char; 256];
    let rc = unsafe { libc::ttyname_r(slave, name.as_mut_ptr(), name.len()) };
    if rc != 0 {
        unsafe {
            libc::close(master);
            libc::close(slave);
        }
        return Err(io::Error::from_raw_os_error(rc));
    }
    let tty = unsafe { CStr::from_ptr(name.as_ptr()) }
        .to_string_lossy()
        .into_owned();
    Ok(unsafe { (File::from_raw_fd(master), File::from_raw_fd(slave), tty) })
}

/// Own CPU time (user + system) of one process in milliseconds.
#[cfg(target_os = "linux")]
fn cpu_ms(pid: u32) -> u64 {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).expect("read /proc stat");
    let fields: Vec<&str> = stat
        .rsplit_once(") ")
        .expect("stat comm")
        .1
        .split(' ')
        .collect();
    let ticks: u64 = fields[11].parse::<u64>().unwrap() + fields[12].parse::<u64>().unwrap();
    let hz = unsafe { libc::sysconf(libc::_SC_CLK_TCK) } as u64;
    ticks * 1000 / hz
}

/// Own CPU time (user + system) of one process in milliseconds, from `ps`
/// (`[[hh:]mm:]ss.cc`, 10 ms resolution).
#[cfg(not(target_os = "linux"))]
fn cpu_ms(pid: u32) -> u64 {
    let out = Command::new("ps")
        .args(["-o", "time=", "-p", &pid.to_string()])
        .output()
        .expect("run ps");
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let mut ms = 0_u64;
    for part in text.split(':') {
        let seconds: f64 = part.parse().unwrap_or_else(|_| panic!("ps time {text:?}"));
        ms = ms * 60 + (seconds * 1000.0).round() as u64;
    }
    ms
}

fn tmux_invocations(log: &Path) -> Vec<String> {
    fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect()
}

/// One managed leader launcher attached through the tmux shim, inside a PTY.
struct Launch {
    child: Child,
    _master: File,
    tmux_log: PathBuf,
    state: PathBuf,
    _endpoint: UnixListener,
    _short: ShortRoot,
    _env: hermetic_guard::HermeticTestEnv,
}

impl Launch {
    fn start(tag: &str, trust_after: Option<&str>) -> Self {
        let env = hermetic_guard::HermeticTestEnv::enter(tag);
        let workspace = env.workspace(tag);
        let fake_bin = env.root().join("fake-bin");
        fs::create_dir_all(&fake_bin).unwrap();
        write_executable(&fake_bin.join("tmux"), TMUX_SHIM);
        write_executable(&fake_bin.join("codex"), PROVIDER_SHIM);
        let tmux_log = env.root().join("tmux.log");
        let state = env.root().join("screen");

        let base = if cfg!(target_os = "macos") {
            "/private/tmp"
        } else {
            "/tmp"
        };
        let short = ShortRoot(PathBuf::from(base).join(format!("ta-{tag}-{}", std::process::id())));
        let socket_dir = short.0.join(format!("tmux-{}", unsafe { libc::geteuid() }));
        fs::create_dir_all(&socket_dir).unwrap();
        let endpoint_name = TmuxBackend::for_workspace(&workspace)
            .tmux_endpoint()
            .expect("workspace tmux endpoint name");
        let endpoint = UnixListener::bind(socket_dir.join(&endpoint_name)).expect("bind endpoint");

        let (master, slave, tty) = open_pty().expect("allocate controlling tty");
        let mut command = Command::new(env!("CARGO_BIN_EXE_team-agent"));
        command
            .args(["codex", "--json"])
            .current_dir(&workspace)
            .env("HOME", env.home())
            .env("TMPDIR", &short.0)
            .env("TMUX_TMPDIR", &short.0)
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    fake_bin.display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            .env("R4_TMUX_LOG", &tmux_log)
            .env("R4_STATE", &state)
            .env("R4_SPAWN_SESSION", env.root().join("spawn-session"))
            .env("R4_PANE_TTY", &tty)
            .env("R4_WORKSPACE", &workspace)
            .env_remove("TMUX")
            .env_remove("TMUX_PANE")
            .env_remove("R4_TRUST_AFTER")
            .stdin(Stdio::from(slave))
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        if let Some(after) = trust_after {
            command.env("R4_TRUST_AFTER", after);
        }
        for key in hermetic_guard::CALLER_IDENTITY_ENVS {
            command.env_remove(key);
        }
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() == -1 || libc::ioctl(0, libc::TIOCSCTTY as _, 0) == -1 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let child = command
            .spawn()
            .expect("spawn managed leader launcher in pty");
        let mut launch = Self {
            child,
            _master: master,
            tmux_log,
            state,
            _endpoint: endpoint,
            _short: short,
            _env: env,
        };
        launch.wait_attached();
        launch
    }

    fn wait_attached(&mut self) {
        let started = Instant::now();
        while started.elapsed() < ATTACH_DEADLINE && self.alive() {
            if tmux_invocations(&self.tmux_log)
                .iter()
                .any(|line| line.split_whitespace().any(|t| t == "attach-session"))
            {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        panic!(
            "fixture did not reach an attached managed leader within {ATTACH_DEADLINE:?}; tmux_log={:?}",
            tmux_invocations(&self.tmux_log)
        );
    }

    fn alive(&mut self) -> bool {
        self.child.try_wait().ok().flatten().is_none()
    }
}

impl Drop for Launch {
    fn drop(&mut self) {
        let pid = self.child.id() as libc::pid_t;
        unsafe { libc::kill(pid, libc::SIGTERM) };
        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(5) && self.alive() {
            std::thread::sleep(Duration::from_millis(50));
        }
        // The launcher leads its own session and process group (setsid above),
        // so this reaches the blocking attach client without a host-wide scan.
        unsafe { libc::kill(-pid, libc::SIGKILL) };
        let _ = self.child.wait();
    }
}

#[test]
#[serial(env)]
fn attached_managed_leader_launcher_is_idle_without_tmux_polling() {
    let mut launch = Launch::start("r4-idle", None);
    std::thread::sleep(STARTUP_HANDOFF);
    let alive_before = launch.alive();
    let calls_before = tmux_invocations(&launch.tmux_log).len();
    let cpu_before = cpu_ms(launch.child.id());
    std::thread::sleep(IDLE_WINDOW);
    let alive_after = launch.alive();
    let cpu_after = if alive_after {
        cpu_ms(launch.child.id())
    } else {
        cpu_before
    };
    let calls = tmux_invocations(&launch.tmux_log);
    drop(launch);

    let idle_calls = &calls[calls_before.min(calls.len())..];
    let cpu_used = cpu_after.saturating_sub(cpu_before);
    let mut failures = Vec::new();
    if !alive_before || !alive_after {
        failures.push("launcher exited while its attach client was still open".to_string());
    }
    if idle_calls.len() > IDLE_TMUX_INVOCATIONS_MAX {
        let mut kinds: Vec<String> = idle_calls
            .iter()
            .filter_map(|line| {
                line.split_whitespace()
                    .find(|t| t.len() > 3 && t.chars().all(|c| c.is_ascii_lowercase() || c == '-'))
                    .map(str::to_owned)
            })
            .collect();
        kinds.sort();
        kinds.dedup();
        failures.push(format!(
            "{} tmux invocations during a {IDLE_WINDOW:?} idle window after the startup handoff (> {IDLE_TMUX_INVOCATIONS_MAX}); commands seen: {kinds:?}",
            idle_calls.len()
        ));
    }
    if cpu_used > IDLE_CPU_MAX_MS {
        failures.push(format!(
            "launcher used {cpu_used} ms CPU during a {IDLE_WINDOW:?} idle window (> {IDLE_CPU_MAX_MS} ms)"
        ));
    }
    assert!(
        failures.is_empty(),
        "attached managed leader launcher polls while idle:\n  {}",
        failures.join("\n  ")
    );
}

#[test]
#[serial(env)]
fn attached_managed_leader_still_answers_a_delayed_workspace_trust_prompt() {
    let mut launch = Launch::start("r4b-trust", Some(TRUST_AFTER_CAPTURES));
    let answered = launch.state.with_extension("answered");
    let shown = launch.state.with_extension("shown");
    let started = Instant::now();
    while started.elapsed() < TRUST_ANSWER_DEADLINE && !answered.exists() && launch.alive() {
        std::thread::sleep(Duration::from_millis(100));
    }
    let alive = launch.alive();
    let captures = fs::read_to_string(launch.state.with_extension("captures"))
        .map(|text| text.lines().count())
        .unwrap_or(0);
    let (was_shown, was_answered) = (shown.exists(), answered.exists());
    drop(launch);

    assert!(
        alive && was_answered,
        "a workspace-trust prompt that appeared after launch was not answered within {TRUST_ANSWER_DEADLINE:?} \
         while attached (screen captures={captures}, prompt shown={was_shown}, launcher alive={alive}); \
         the bounded startup handoff must keep checking until the provider is Ready"
    );
}
