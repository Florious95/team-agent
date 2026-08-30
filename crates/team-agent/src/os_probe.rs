//! ---
//! purpose: 为运行时提供有界的进程输出读取与前台进程组探测
//! contract:
//!   provides:
//!     - name: bounded_command_output_with_probe
//!       what: 在固定超时内执行命令并读取 stdout，超时后终止子进程
//!     - name: pane_foreground_and_root_pgrp
//!       what: 读取 pane PID 的前台与根进程组 ID，无法判定时返回 None
//!   depends:
//!     - std
//! boundary:
//!   - 只负责有界探测、临时 stdout 文件的独占创建与清理
//!   - 不决定 shutdown 状态，不重试探测，不修改调用方行为
//! arch:
//!   allowed_dependencies:
//!     - std
//! maturity: wired
//! ---

use std::cell::RefCell;
use std::fs::OpenOptions;
use std::io::{self, Read};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const DEFAULT_TIMEOUT: Duration = Duration::from_millis(900);
static NEXT_TEMP_OUTPUT_ID: AtomicU64 = AtomicU64::new(0);

thread_local! {
    static PROBE_TIMEOUT: RefCell<Option<ProbeTimeout>> = const { RefCell::new(None) };
}

#[derive(Clone, Debug)]
pub(crate) struct ProbeTimeout {
    pub(crate) probe: &'static str,
    pub(crate) pid: Option<u32>,
    pub(crate) timeout_ms: u64,
}

#[derive(Debug)]
pub(crate) struct BoundedCommandOutput {
    pub(crate) status: ExitStatus,
    pub(crate) stdout: Vec<u8>,
}

/// ---
/// purpose: 清除当前线程记录的探测超时
/// ---
pub(crate) fn clear_probe_timeout() {
    PROBE_TIMEOUT.with(|timeout| *timeout.borrow_mut() = None);
}

/// ---
/// purpose: 判断当前线程是否记录了探测超时
/// ---
pub(crate) fn probe_timed_out() -> bool {
    PROBE_TIMEOUT.with(|timeout| timeout.borrow().is_some())
}

/// ---
/// purpose: 获取当前线程记录的探测超时详情
/// ---
pub(crate) fn probe_timeout() -> Option<ProbeTimeout> {
    PROBE_TIMEOUT.with(|timeout| timeout.borrow().clone())
}

#[cfg(test)]
/// ---
/// purpose: 为单元测试设置探测超时详情
/// ---
pub(crate) fn set_probe_timeout_for_test(probe: &'static str, pid: Option<u32>, timeout_ms: u64) {
    PROBE_TIMEOUT.with(|current| {
        *current.borrow_mut() = Some(ProbeTimeout {
            probe,
            pid,
            timeout_ms,
        });
    });
}

/// ---
/// purpose: 执行一个带默认超时与探测标识的命令并读取 stdout
/// ---
pub(crate) fn bounded_command_output_with_probe(
    command: &mut Command,
    probe: &'static str,
    pid: Option<u32>,
) -> io::Result<BoundedCommandOutput> {
    bounded_command_output_with_timeout(command, DEFAULT_TIMEOUT, probe, pid)
}

fn bounded_command_output_with_timeout(
    command: &mut Command,
    timeout: Duration,
    probe: &'static str,
    pid: Option<u32>,
) -> io::Result<BoundedCommandOutput> {
    let stdout_path = temp_output_path("stdout");
    let stdout_file = OpenOptions::new()
        .create_new(true)
        .read(true)
        .write(true)
        .open(&stdout_path)?;
    let child = command
        .stdout(Stdio::from(stdout_file.try_clone()?))
        .stderr(Stdio::null())
        .spawn()?;
    wait_for_bounded_child(child, stdout_file, stdout_path, timeout, probe, pid)
}

fn wait_for_bounded_child(
    mut child: std::process::Child,
    stdout_file: std::fs::File,
    stdout_path: std::path::PathBuf,
    timeout: Duration,
    probe: &'static str,
    pid: Option<u32>,
) -> io::Result<BoundedCommandOutput> {
    let start = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            drop(stdout_file);
            let stdout = read_and_remove(&stdout_path);
            return Ok(BoundedCommandOutput { status, stdout });
        }
        if start.elapsed() >= timeout {
            PROBE_TIMEOUT.with(|current| {
                let mut current = current.borrow_mut();
                if current.is_none() {
                    *current = Some(ProbeTimeout {
                        probe,
                        pid,
                        timeout_ms: timeout.as_millis() as u64,
                    });
                }
            });
            let _ = child.kill();
            let status = child.wait()?;
            drop(stdout_file);
            let stdout = read_and_remove(&stdout_path);
            return Ok(BoundedCommandOutput { status, stdout });
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn temp_output_path(kind: &str) -> std::path::PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    temp_output_path_with_nanos(kind, nanos)
}

fn temp_output_path_with_nanos(kind: &str, nanos: u128) -> std::path::PathBuf {
    let unique_id = NEXT_TEMP_OUTPUT_ID.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "team-agent-os-probe-{}-{nanos}-{unique_id}.{kind}",
        std::process::id(),
    ))
}

fn read_and_remove(path: &std::path::Path) -> Vec<u8> {
    let mut stdout = Vec::new();
    if let Ok(mut file) = std::fs::File::open(path) {
        let _ = file.read_to_end(&mut stdout);
    }
    let _ = std::fs::remove_file(path);
    stdout
}

/// 0.4.x Phase 1 (fg-pgrp): terminal foreground process-group probe.
///
/// macOS probe confirmed (2026-06-29, .team/artifacts/fg-pgrp-probe.md):
/// `TIOCGPGRP` ioctl returns Errno 25 on tmux slave PTYs opened from a
/// non-controlling process. We use `ps -o tpgid,pgid -p <pid>` instead —
/// reliable on macOS AND Linux, no platform-specific cfg needed.
///
/// Returns `Ok(Some((tpgid, pgid)))` on success, `Ok(None)` when the PID
/// is gone / ps returned no row / values unparseable. The caller maps
/// `None` to `WorkerRuntimeState::Unknown` (never to idle — Iron Law).
///
/// `tpgid` = terminal foreground process group ID.
/// `pgid`  = the agent root's own process group ID.
/// `tpgid != pgid` means a child process owns the foreground (BUSY signal).
///
/// Bounded via [`run_bounded_command`] so a wedged ps cannot stall the
/// coordinator tick.
/// ---
/// purpose: 读取 pane PID 的前台与根进程组 ID，无法判定时返回 None
/// ---
pub(crate) fn pane_foreground_and_root_pgrp(pane_pid: u32) -> io::Result<Option<(u32, u32)>> {
    let mut command = Command::new("ps");
    command.args(["-o", "tpgid=,pgid=", "-p", &pane_pid.to_string()]);
    let output = bounded_command_output_with_probe(
        &mut command,
        "pane_foreground_and_root_pgrp",
        Some(pane_pid),
    )?;
    if probe_timed_out() {
        return Ok(None);
    }
    if !output.status.success() {
        return Ok(None);
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let first_line = text.lines().next().unwrap_or("").trim();
    if first_line.is_empty() {
        return Ok(None);
    }
    let parts: Vec<&str> = first_line.split_whitespace().collect();
    if parts.len() < 2 {
        return Ok(None);
    }
    let tpgid = parts[0].parse::<i64>().ok();
    let pgid = parts[1].parse::<i64>().ok();
    match (tpgid, pgid) {
        // ps prints `-1` for tpgid when there is no controlling terminal —
        // treat as unknown rather than fabricating a u32.
        (Some(t), Some(p)) if t > 0 && p > 0 => Ok(Some((t as u32, p as u32))),
        _ => Ok(None),
    }
}

#[cfg(test)]
mod fg_pgrp_tests {
    use super::*;

    #[test]
    fn pane_foreground_and_root_pgrp_self_returns_some_or_none() {
        // The current test process is not necessarily attached to a
        // controlling terminal (CI runs detached), so we accept Some OR
        // None. The contract is: never panic, return io::Result.
        let pid = std::process::id();
        match pane_foreground_and_root_pgrp(pid) {
            Ok(Some((tpgid, pgid))) => {
                assert!(tpgid > 0 && pgid > 0, "positive pgid/tpgid");
            }
            Ok(None) => {} // headless / no tty — acceptable
            Err(e) => panic!("probe must not error in normal env: {e}"),
        }
    }

    #[test]
    fn pane_foreground_and_root_pgrp_missing_pid_returns_none() {
        // PID 1 is init/launchd — exists but we read fields; non-existent
        // PIDs return Ok(None) (ps exits non-zero).
        let result =
            pane_foreground_and_root_pgrp(0xFFFF_FFFE).expect("must not error on missing pid");
        assert!(result.is_none(), "missing pid → None");
    }

    #[test]
    fn temp_output_create_new_requires_atomic_discriminator() {
        let fixed_nanos = 0x5eed_u128;
        let first_path = temp_output_path_with_nanos("stdout", fixed_nanos);
        let second_path = temp_output_path_with_nanos("stdout", fixed_nanos);
        let _ = std::fs::remove_file(&first_path);
        let _ = std::fs::remove_file(&second_path);

        let first = OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .open(&first_path)
            .expect("first atomic path create");
        let second = OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .open(&second_path)
            .expect("second atomic path create");
        assert_ne!(first_path, second_path, "atomic discriminator must advance");
        drop(first);
        drop(second);
        let _ = std::fs::remove_file(first_path);
        let _ = std::fs::remove_file(second_path);
    }
}
