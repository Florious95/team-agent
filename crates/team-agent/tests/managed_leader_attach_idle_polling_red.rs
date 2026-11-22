//! RED gate R4: an attached managed leader launcher must be event-driven when idle.
//!
//! `team-agent <provider>` run outside tmux creates the managed leader pane and
//! stays resident while the user's `tmux attach-session` client is open (often
//! for hours). On 0.6.0, `wait_managed_attach_child` sleeps 200 ms and runs a
//! provider-health probe each time; every probe forks `tmux`. Measured in the
//! field: about 5 tmux forks per second and 0.7% CPU continuously (1:38 of CPU
//! in 2h43m) for one idle launcher.
//!
//! Contract: once attached and idle, the launcher may run at most one tmux
//! command in a 10 s window (for example a single blocking `wait-for`). Its own
//! CPU time over that window must be at most 50 ms.
//!
//! Hermetic: a PATH `tmux` shim records every invocation and emulates the
//! managed launch. Its `attach-session` blocks like an attached client. No real
//! tmux server or provider is used, and sockets live in a process-owned short
//! root.

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

const IDLE_WINDOW: Duration = Duration::from_secs(10);
const IDLE_TMUX_INVOCATIONS_MAX: usize = 1;
const IDLE_CPU_MAX_MS: u64 = 50;
const ATTACH_DEADLINE: Duration = Duration::from_secs(30);

const PROVIDER_SHIM: &str = r#"#!/bin/sh
if [ "${1-}" = "--version" ]; then
  printf 'codex 1.0.0\n'
  exit 0
fi
exit 0
"#;

/// Records each invocation, emulates the managed launch, and makes
/// `attach-session` block like a real attached client until it is signalled.
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

fn stop_launcher(child: &mut Child) {
    let pid = child.id() as libc::pid_t;
    unsafe { libc::kill(pid, libc::SIGTERM) };
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(5) {
        if child.try_wait().ok().flatten().is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    // The launcher leads its own session and process group (setsid below), so
    // this reaches the blocking attach client without a host-wide scan.
    unsafe { libc::kill(-pid, libc::SIGKILL) };
    let _ = child.wait();
}

#[test]
#[serial(env)]
fn attached_managed_leader_launcher_is_idle_without_tmux_polling() {
    let env = hermetic_guard::HermeticTestEnv::enter("r4-attach-idle");
    let workspace = env.workspace("r4");
    let fake_bin = env.root().join("fake-bin");
    fs::create_dir_all(&fake_bin).unwrap();
    write_executable(&fake_bin.join("tmux"), TMUX_SHIM);
    write_executable(&fake_bin.join("codex"), PROVIDER_SHIM);
    let tmux_log = env.root().join("tmux.log");

    let base = if cfg!(target_os = "macos") {
        "/private/tmp"
    } else {
        "/tmp"
    };
    let short = ShortRoot(PathBuf::from(base).join(format!("ta-r4-{}", std::process::id())));
    let socket_dir = short.0.join(format!("tmux-{}", unsafe { libc::geteuid() }));
    fs::create_dir_all(&socket_dir).unwrap();
    let endpoint_name = TmuxBackend::for_workspace(&workspace)
        .tmux_endpoint()
        .expect("workspace tmux endpoint name");
    let _endpoint = UnixListener::bind(socket_dir.join(&endpoint_name)).expect("bind endpoint");

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
        .env("R4_SPAWN_SESSION", env.root().join("spawn-session"))
        .env("R4_PANE_TTY", &tty)
        .env("R4_WORKSPACE", &workspace)
        .env_remove("TMUX")
        .env_remove("TMUX_PANE")
        .stdin(Stdio::from(slave))
        .stdout(Stdio::null())
        .stderr(Stdio::null());
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
    let mut child = command
        .spawn()
        .expect("spawn managed leader launcher in pty");

    let started = Instant::now();
    let attached = loop {
        if tmux_invocations(&tmux_log)
            .iter()
            .any(|line| line.split_whitespace().any(|t| t == "attach-session"))
        {
            break true;
        }
        if child.try_wait().ok().flatten().is_some() || started.elapsed() > ATTACH_DEADLINE {
            break false;
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    if !attached {
        stop_launcher(&mut child);
        drop(master);
        panic!(
            "fixture did not reach an attached managed leader within {ATTACH_DEADLINE:?}; tmux_log={:?}",
            tmux_invocations(&tmux_log)
        );
    }

    std::thread::sleep(Duration::from_secs(1)); // let post-attach bookkeeping settle
    let launcher_alive = child.try_wait().ok().flatten().is_none();
    let calls_before = tmux_invocations(&tmux_log).len();
    let cpu_before = cpu_ms(child.id());
    std::thread::sleep(IDLE_WINDOW);
    let still_alive = child.try_wait().ok().flatten().is_none();
    let cpu_after = if still_alive {
        cpu_ms(child.id())
    } else {
        cpu_before
    };
    let calls = tmux_invocations(&tmux_log);
    stop_launcher(&mut child);
    drop(master);

    let idle_calls = &calls[calls_before.min(calls.len())..];
    let cpu_used = cpu_after.saturating_sub(cpu_before);
    let mut failures = Vec::new();
    if !launcher_alive || !still_alive {
        failures.push("launcher exited while its attach client was still open".to_string());
    }
    if idle_calls.len() > IDLE_TMUX_INVOCATIONS_MAX {
        let mut kinds: Vec<String> = idle_calls
            .iter()
            .filter_map(|line| {
                line.split_whitespace()
                    .find(|t| t.chars().all(|c| c.is_ascii_lowercase() || c == '-') && t.len() > 3)
                    .map(str::to_owned)
            })
            .collect();
        kinds.sort();
        kinds.dedup();
        failures.push(format!(
            "{} tmux invocations during a {IDLE_WINDOW:?} idle window (> {IDLE_TMUX_INVOCATIONS_MAX}); commands seen: {kinds:?}",
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
