//! coordinator 健康/身份 & 只读可观测面:metadata 身份原语 + coordinator 路径 + watch 实时流。

use std::io::{Read, Seek, SeekFrom};
#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use serde_json::Value;
use thiserror::Error;

use crate::message_store::MessageStore;

use super::types::{
    CoordinatorBinaryIdentity, CoordinatorBinaryIdentityRelation, CoordinatorHealthStatus,
    CoordinatorMetadata, CoordinatorMetadataMismatchReason, HealthReport, MetadataSource, Pid,
    SchemaError, SchemaHealth, StartError, StartOutcome, StartReport, StopError, StopOutcome,
    StopReport, WatchCursor, WorkspacePath, PROTOCOL_VERSION, ROTATION_MARKER,
};

// ===========================================================================
// coordinator daemon lifecycle (lifecycle.py:38-247).
// start_coordinator spawns the `team-agent coordinator --workspace <ws>` daemon subprocess;
// the actual spawn is the #[ignore] real-machine boundary, the idempotent decision is testable.
// ===========================================================================

/// `coordinator_health`(`lifecycle.py:38-46`):`running ∧ metadata_ok ∧ schema_ok` → typed report.
pub fn coordinator_health(workspace: &WorkspacePath) -> HealthReport {
    let schema = message_store_schema_health(workspace);
    let current_binary_identity = current_coordinator_binary_identity();
    let pid_path = coordinator_pid_path(workspace);
    let pid = read_pid_file(&pid_path);
    let status = match pid {
        Some(pid) => match pid_is_running(pid) {
            Ok(true) => CoordinatorHealthStatus::Running,
            Ok(false) | Err(_) => CoordinatorHealthStatus::Stale,
        },
        None if pid_path.exists() => CoordinatorHealthStatus::InvalidPid,
        None => CoordinatorHealthStatus::Missing,
    };
    let metadata = read_coordinator_metadata(workspace);
    let wire_metadata_mismatch = pid
        .map(|p| coordinator_wire_metadata_mismatch_reason(metadata.as_ref(), p))
        .unwrap_or(Some(CoordinatorMetadataMismatchReason::MetadataMissing));
    let binary_identity_mismatch =
        coordinator_binary_identity_mismatch_reason(metadata.as_ref(), &current_binary_identity);
    let wire_metadata_ok = wire_metadata_mismatch.is_none();
    let binary_identity_ok = binary_identity_mismatch.is_none();
    let metadata_mismatch = wire_metadata_mismatch.or(binary_identity_mismatch);
    let metadata_ok = wire_metadata_ok && binary_identity_ok;
    let process_running = matches!(status, CoordinatorHealthStatus::Running);
    let binary_identity_relation =
        coordinator_binary_identity_relation(metadata.as_ref(), &current_binary_identity);
    let service_available = process_running && wire_metadata_ok && schema.ok;
    HealthReport {
        ok: process_running && metadata_ok && schema.ok,
        status,
        pid,
        metadata,
        metadata_ok,
        process_running,
        wire_metadata_ok,
        binary_identity_ok,
        binary_identity_relation,
        service_available,
        metadata_mismatch_reason: metadata_mismatch.map(|reason| reason.as_str().to_string()),
        current_binary_identity,
        schema,
    }
}

/// `start_coordinator`(`lifecycle.py:49-121`):幂等 — 已健康 no-op(AlreadyRunning);metadata 不兼容
/// 先 stop 再起;schema 不兼容拒启 + hint;否则 spawn `team-agent coordinator --workspace <ws>`。
pub fn start_coordinator(workspace: &WorkspacePath) -> Result<StartReport, StartError> {
    start_coordinator_with_team(workspace, None)
}

/// 0.5.x Windows portability Batch 9 F8 (leader msg_2a4cc1fa54c0):
/// forward `--team` to the spawned coord daemon so it doesn't have
/// to derive team_key from `state.active_team_key` at boot. The
/// derivation stays as fallback (see `coordinator::backoff::run_daemon`),
/// so Unix daemons and pre-existing test harnesses are byte-preserving.
///
/// Callers that CAN pass team_key (Batch 9 quick-start Windows path)
/// SHOULD — that avoids Batch 8's F8 seed-state trap.
pub fn start_coordinator_with_team(
    workspace: &WorkspacePath,
    team_key: Option<&str>,
) -> Result<StartReport, StartError> {
    let health = coordinator_health(workspace);
    let identity = health.current_binary_identity.clone();
    if health.ok {
        return Ok(StartReport {
            ok: true,
            pid: health.pid,
            status: StartOutcome::AlreadyRunning,
            previous_pid: None,
            binary_path: Some(identity.binary_path),
            binary_version: Some(identity.binary_version),
            rotation_reason: None,
            binary_identity_relation: health.binary_identity_relation,
            log: Some(coordinator_log_path(workspace)),
            schema_error: None,
            action: None,
        });
    }
    if !health.schema.ok {
        return Ok(StartReport {
            ok: false,
            pid: health.pid,
            status: StartOutcome::SchemaIncompatible,
            previous_pid: None,
            binary_path: Some(identity.binary_path),
            binary_version: Some(identity.binary_version),
            rotation_reason: health.metadata_mismatch_reason,
            binary_identity_relation: health.binary_identity_relation,
            log: None,
            schema_error: health.schema.error,
            action: health.schema.action,
        });
    }
    if matches!(health.status, CoordinatorHealthStatus::Running) && !health.wire_metadata_ok {
        return Ok(StartReport {
            ok: false,
            pid: health.pid,
            status: StartOutcome::RestartIncompatibleStopFailed,
            previous_pid: health.pid,
            binary_path: Some(identity.binary_path),
            binary_version: Some(identity.binary_version),
            rotation_reason: health.metadata_mismatch_reason,
            binary_identity_relation: health.binary_identity_relation,
            log: None,
            schema_error: None,
            action: Some(
                "refusing to rotate a coordinator with incompatible protocol or schema metadata"
                    .to_string(),
            ),
        });
    }
    if matches!(health.status, CoordinatorHealthStatus::Running)
        && health.wire_metadata_ok
        && !health.binary_identity_ok
        && matches!(
            health.binary_identity_relation,
            CoordinatorBinaryIdentityRelation::DaemonNewerThanCaller
        )
    {
        crate::event_log::EventLog::new(workspace.as_path()).write(
            "coordinator.newer_daemon_preserved",
            serde_json::json!({
                "pid": health.pid.map(|pid| pid.get()),
                "binary_identity_relation": health.binary_identity_relation.as_str(),
                "reason": "daemon_newer_than_caller",
                "daemon_binary_path": health
                    .metadata
                    .as_ref()
                    .and_then(|metadata| metadata.binary_path.clone()),
                "daemon_binary_version": health
                    .metadata
                    .as_ref()
                    .and_then(|metadata| metadata.binary_version.clone()),
                "caller_binary_path": identity.binary_path.clone(),
                "caller_binary_version": identity.binary_version.clone(),
            }),
        )?;
        return Ok(StartReport {
            ok: true,
            pid: health.pid,
            status: StartOutcome::AlreadyRunning,
            previous_pid: None,
            binary_path: Some(identity.binary_path),
            binary_version: Some(identity.binary_version),
            rotation_reason: Some("daemon_newer_than_caller".to_string()),
            binary_identity_relation: health.binary_identity_relation,
            log: Some(coordinator_log_path(workspace)),
            schema_error: None,
            action: None,
        });
    }
    let rotation_reason =
        if matches!(health.status, CoordinatorHealthStatus::Running) && !health.metadata_ok {
            health.metadata_mismatch_reason.clone()
        } else {
            None
        };
    if matches!(health.status, CoordinatorHealthStatus::Running) && !health.metadata_ok {
        crate::event_log::EventLog::new(workspace.as_path()).write(
            "coordinator.rotation_required",
            serde_json::json!({
                "old_pid": health.pid.map(|pid| pid.get()),
                "reason": rotation_reason.clone(),
                "old_binary_path": health
                    .metadata
                    .as_ref()
                    .and_then(|metadata| metadata.binary_path.clone()),
                "old_binary_version": health
                    .metadata
                    .as_ref()
                    .and_then(|metadata| metadata.binary_version.clone()),
                "current_binary_path": identity.binary_path.clone(),
                "current_binary_version": identity.binary_version.clone(),
            }),
        )?;
        if health.pid.map(|pid| pid.get()) == Some(std::process::id()) {
            return Ok(StartReport {
                ok: false,
                pid: health.pid,
                status: StartOutcome::RestartIncompatibleStopFailed,
                previous_pid: health.pid,
                binary_path: Some(identity.binary_path),
                binary_version: Some(identity.binary_version),
                rotation_reason,
                binary_identity_relation: health.binary_identity_relation,
                log: None,
                schema_error: None,
                action: Some(
                    "refusing to rotate coordinator metadata that points at the caller process"
                        .to_string(),
                ),
            });
        }
        match stop_coordinator(workspace) {
            Ok(stop) if stop.ok => {}
            Ok(_) | Err(_) => {
                return Ok(StartReport {
                    ok: false,
                    pid: health.pid,
                    status: StartOutcome::RestartIncompatibleStopFailed,
                    previous_pid: health.pid,
                    binary_path: Some(identity.binary_path),
                    binary_version: Some(identity.binary_version),
                    rotation_reason,
                    binary_identity_relation: health.binary_identity_relation,
                    log: None,
                    schema_error: None,
                    action: None,
                });
            }
        }
    }

    let runtime_dir = crate::model::paths::runtime_dir(workspace.as_path());
    std::fs::create_dir_all(&runtime_dir)?;
    let log_path = coordinator_log_path(workspace);
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)?;
    let log_err = log.try_clone()?;
    let mut command = Command::new(std::env::current_exe()?);
    command
        .args(["coordinator", "--workspace"])
        .arg(workspace.as_path());
    if let Some(tk) = team_key {
        if !tk.is_empty() {
            command.args(["--team", tk]);
        }
    }
    command
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(log_err));
    detach_daemon_child(&mut command);
    let child = command.spawn()?;
    let pid = Pid::new(child.id());
    std::fs::write(coordinator_pid_path(workspace), pid.to_string())?;
    write_coordinator_metadata(workspace, pid, MetadataSource::Start)?;
    let status = if rotation_reason.is_some() {
        StartOutcome::StartedAfterRotation
    } else {
        StartOutcome::Started
    };
    Ok(StartReport {
        ok: true,
        pid: Some(pid),
        status,
        previous_pid: health.pid,
        binary_path: Some(identity.binary_path),
        binary_version: Some(identity.binary_version),
        rotation_reason,
        binary_identity_relation: health.binary_identity_relation,
        log: Some(log_path),
        schema_error: None,
        action: None,
    })
}

#[cfg(unix)]
fn detach_daemon_child(command: &mut Command) {
    // The coordinator is a daemon: it must not remain in the launcher's process
    // group, otherwise bare SSH command teardown can SIGHUP it after quick-start exits.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                Err(std::io::Error::last_os_error())
            } else {
                Ok(())
            }
        });
    }
}

#[cfg(windows)]
fn detach_daemon_child(command: &mut Command) {
    // 0.5.x Windows portability Batch 8 F7 (leader msg_590b4dce0f68):
    // detach the coordinator daemon on Windows via
    // `DETACHED_PROCESS | CREATE_BREAKAWAY_FROM_JOB` creation flags,
    // matching what `coordinator::conpty_shim::spawn_shim_and_handshake`
    // does for the shim. Without these flags, an SSH-launched
    // quick-start blocks waiting for the coord daemon's process
    // tree to exit (never happens — daemon runs forever), and the
    // quick-start caller sees a hung command.
    use std::os::windows::process::CommandExt;
    const DETACHED_PROCESS: u32 = 0x00000008;
    const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x01000000;
    command.creation_flags(DETACHED_PROCESS | CREATE_BREAKAWAY_FROM_JOB);
}

#[cfg(not(any(unix, windows)))]
fn detach_daemon_child(_command: &mut Command) {}

/// `stop_coordinator`(`lifecycle.py:228-247`):SIGTERM pid + 清 pid/meta → typed report。
pub fn stop_coordinator(workspace: &WorkspacePath) -> Result<StopReport, StopError> {
    let pid_path = coordinator_pid_path(workspace);
    if !pid_path.exists() {
        if let Some(report) = stop_discovered_coordinators(workspace)? {
            return Ok(report);
        }
        return Ok(StopReport {
            ok: true,
            status: StopOutcome::Missing,
            pid: None,
        });
    }
    let Some(pid) = read_pid_file(&pid_path) else {
        remove_file_if_exists(&pid_path)?;
        remove_file_if_exists(&coordinator_meta_path(workspace))?;
        return Ok(StopReport {
            ok: true,
            status: StopOutcome::InvalidPidRemoved,
            pid: None,
        });
    };
    if pid_is_running(pid).ok() == Some(false) {
        remove_file_if_exists(&pid_path)?;
        remove_file_if_exists(&coordinator_meta_path(workspace))?;
        return Ok(StopReport {
            ok: true,
            status: StopOutcome::Missing,
            pid: Some(pid),
        });
    }
    if pid.get() == std::process::id() {
        remove_file_if_exists(&pid_path)?;
        remove_file_if_exists(&coordinator_meta_path(workspace))?;
        return Ok(StopReport {
            ok: true,
            status: StopOutcome::Stopped,
            pid: Some(pid),
        });
    }
    if !terminate_pid(pid) {
        return Ok(StopReport {
            ok: false,
            status: StopOutcome::KillFailed,
            pid: Some(pid),
        });
    }
    remove_file_if_exists(&pid_path)?;
    remove_file_if_exists(&coordinator_meta_path(workspace))?;
    Ok(StopReport {
        ok: true,
        status: StopOutcome::Stopped,
        pid: Some(pid),
    })
}

fn stop_discovered_coordinators(
    workspace: &WorkspacePath,
) -> Result<Option<StopReport>, StopError> {
    let pids = discover_coordinator_pids(workspace);
    if pids.is_empty() {
        return Ok(None);
    }

    let mut stopped = None;
    let mut failed = None;
    for pid in pids {
        if terminate_pid(pid) {
            stopped.get_or_insert(pid);
        } else {
            failed.get_or_insert(pid);
        }
    }
    remove_file_if_exists(&coordinator_meta_path(workspace))?;

    if let Some(pid) = stopped {
        Ok(Some(StopReport {
            ok: true,
            status: StopOutcome::Stopped,
            pid: Some(pid),
        }))
    } else {
        Ok(Some(StopReport {
            ok: false,
            status: StopOutcome::KillFailed,
            pid: failed,
        }))
    }
}

fn discover_coordinator_pids(workspace: &WorkspacePath) -> Vec<Pid> {
    let output = match crate::os_probe::bounded_command_output_with_probe(
        Command::new("ps").args(["-axo", "pid=,command="]),
        "ps_table",
        None,
    ) {
        Ok(output) if output.status.success() => output,
        _ => return Vec::new(),
    };
    let text = String::from_utf8_lossy(&output.stdout);
    let candidates = workspace_match_candidates(workspace.as_path());
    text.lines()
        .filter_map(|line| parse_ps_command_line(line))
        .filter(|(pid, command)| {
            *pid != std::process::id()
                && coordinator_command_matches_workspace(command, &candidates)
        })
        .map(|(pid, _)| Pid::new(pid))
        .collect()
}

fn parse_ps_command_line(line: &str) -> Option<(u32, &str)> {
    let line = line.trim_start();
    let split = line.find(char::is_whitespace).unwrap_or(line.len());
    let pid = line.get(..split)?.trim().parse::<u32>().ok()?;
    let command = line.get(split..)?.trim();
    Some((pid, command))
}

fn workspace_match_candidates(workspace: &Path) -> Vec<String> {
    let mut candidates = vec![workspace.to_string_lossy().to_string()];
    if let Ok(canonical) = workspace.canonicalize() {
        let text = canonical.to_string_lossy().to_string();
        if !candidates.iter().any(|candidate| candidate == &text) {
            candidates.push(text);
        }
    }
    candidates
}

fn coordinator_command_matches_workspace(command: &str, workspaces: &[String]) -> bool {
    command
        .split_whitespace()
        .any(|token| token == "team-agent" || token.ends_with("/team-agent"))
        && command
            .split_whitespace()
            .any(|token| token == "coordinator")
        && command.contains("--workspace")
        && workspaces
            .iter()
            .any(|workspace| command.contains(workspace))
}

fn terminate_pid(pid: Pid) -> bool {
    // 0.5.x Windows portability Batch 3: routes signal delivery through
    // `platform::process::terminate_pid`. Unix keeps
    // SIGTERM → 5s grace → SIGKILL semantics byte-for-byte
    // (`SignalKind::TerminateGraceful` → SIGTERM,
    // `SignalKind::TerminateForce` → SIGKILL). Windows performs
    // `TerminateProcess` for both kinds; the `TerminationOutcome::ForceOnly`
    // return on the graceful call is what a future audit-event
    // emitter (CR C-6) will trigger `platform.terminate_force_only`
    // on. For this batch the return value is discarded, matching the
    // current inline `let _ = send_signal(...)` pattern.
    if pid_is_running(pid).ok() == Some(false) {
        return true;
    }
    let pids = process_tree_pids(pid);
    for child in pids.iter().rev() {
        let _ = crate::platform::process::terminate_pid(
            child.get(),
            crate::platform::process::SignalKind::TerminateGraceful,
        );
    }
    if !wait_until_all_not_running(&pids, Duration::from_secs(5)) {
        for child in pids.iter().rev() {
            let _ = crate::platform::process::terminate_pid(
                child.get(),
                crate::platform::process::SignalKind::TerminateForce,
            );
        }
    }
    wait_until_all_not_running(&pids, Duration::from_secs(5))
}

/// Public wrapper for diagnostic cleanup paths that must reuse coordinator
/// shutdown's SIGTERM-then-SIGKILL semantics.
pub fn terminate_pid_tree(pid: Pid) -> bool {
    terminate_pid(pid)
}

fn process_tree_pids(root: Pid) -> Vec<Pid> {
    let root_pid = root.get();
    let pairs = crate::os_probe::bounded_command_output_with_probe(
        Command::new("ps").args(["-axo", "pid=,ppid="]),
        "ps_parent",
        None,
    )
    .ok()
    .map(|out| String::from_utf8_lossy(&out.stdout).to_string())
    .unwrap_or_default()
    .lines()
    .filter_map(|line| {
        let mut parts = line.split_whitespace();
        let pid = parts.next()?.parse::<u32>().ok()?;
        let ppid = parts.next()?.parse::<u32>().ok()?;
        Some((pid, ppid))
    })
    .collect::<Vec<_>>();
    let mut out = Vec::new();
    collect_child_pids(root_pid, &pairs, &mut out);
    out.push(root_pid);
    out.sort_unstable();
    out.dedup();
    out.into_iter().map(Pid::new).collect()
}

fn collect_child_pids(parent: u32, pairs: &[(u32, u32)], out: &mut Vec<u32>) {
    for (pid, ppid) in pairs {
        if *ppid == parent && !out.contains(pid) {
            out.push(*pid);
            collect_child_pids(*pid, pairs, out);
        }
    }
}

fn wait_until_all_not_running(pids: &[Pid], timeout: Duration) -> bool {
    let start = std::time::Instant::now();
    loop {
        for pid in pids {
            reap_child_if_possible(*pid);
        }
        if pids
            .iter()
            .all(|pid| pid_is_running(*pid).ok() != Some(true))
        {
            return true;
        }
        if start.elapsed() >= timeout {
            return false;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn reap_child_if_possible(pid: Pid) {
    // Batch 3: routed through `platform::process`. Unix `waitpid
    // (WNOHANG)`; Windows no-op (no zombie model).
    crate::platform::process::reap_child_if_possible(pid.get());
}

#[cfg(unix)]
#[allow(dead_code)]
fn send_signal(pid: Pid, signal: libc::c_int) -> bool {
    // Retained (dead code post-Batch-3) as a Unix-only helper for any
    // future non-standard signal delivery. All product paths now use
    // `crate::platform::process::terminate_pid` with `SignalKind`.
    let Ok(pid_t) = libc::pid_t::try_from(pid.get()) else {
        return false;
    };
    unsafe { libc::kill(pid_t, signal) == 0 }
}

fn wait_until_not_running(pid: Pid, timeout: Duration) -> bool {
    let start = std::time::Instant::now();
    loop {
        if pid_is_running(pid).ok() != Some(true) {
            return true;
        }
        if start.elapsed() >= timeout {
            return false;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

// ===========================================================================
// metadata 身份原语(metadata.py)—— 自由函数面
// ===========================================================================

/// `pid_is_running`(`metadata.py:16-25`):`os.kill(pid, 0)` + `ps -o stat=` 查 zombie(Z* → 不算活)。
/// §10 fallible:进程探测 I/O 可失败 → Result。
///
/// 0.5.x Windows portability Batch 4: this function has UNIQUE
/// coordinator-metadata semantics — it treats `EPERM` as "not
/// running" (different from `platform::process::pid_liveness` which
/// treats `EPERM` as Live) because the coordinator only owns
/// processes it can signal, and uses `ps -o stat=` for zombie
/// detection. The `ps` shellout is Unix-only.
///
/// On Windows the coordinator-metadata identity check runs through
/// `platform::process::pid_liveness` instead (Windows has no zombie
/// state — a process is either Live or Dead — so the additional
/// `ps stat=` step has no analogue). This preserves the coordinator's
/// "am I the owner" check without silently reporting stale pids as
/// alive.
#[cfg(unix)]
pub fn pid_is_running(pid: Pid) -> Result<bool, std::io::Error> {
    let Ok(pid_t) = libc::pid_t::try_from(pid.get()) else {
        return Ok(false);
    };
    let signal_rc = unsafe { libc::kill(pid_t, 0) };
    if signal_rc != 0 {
        let err = std::io::Error::last_os_error();
        return match err.raw_os_error() {
            Some(libc::EPERM) | Some(libc::ESRCH) => Ok(false),
            _ => Err(err),
        };
    }
    let out = crate::os_probe::bounded_command_output_with_probe(
        Command::new("ps").args(["-p", &pid.to_string(), "-o", "stat="]),
        "ps_table",
        Some(pid.get()),
    )?;
    if !out.status.success() {
        return Ok(false);
    }
    let stat = String::from_utf8_lossy(&out.stdout).trim().to_string();
    Ok(!stat.is_empty() && !stat.starts_with('Z'))
}

/// Windows shim: no `ps stat=` zombie detection needed (Windows has
/// no zombie state — a process is either Live or Dead). Route through
/// `platform::process::pid_liveness` and map to bool. The EPERM
/// semantic ("we can't signal → treat as not running") maps to
/// Windows `ERROR_ACCESS_DENIED` which the platform layer already
/// treats as `Live`; so on Windows the coordinator sees a process
/// it can't query as still-running (safer than pretending it's gone
/// and losing the ownership handle).
#[cfg(not(unix))]
pub fn pid_is_running(pid: Pid) -> Result<bool, std::io::Error> {
    match crate::platform::process::pid_liveness(pid.get())? {
        crate::platform::process::ProcessLiveness::Live => Ok(true),
        crate::platform::process::ProcessLiveness::Dead => Ok(false),
        crate::platform::process::ProcessLiveness::Unknown { .. } => Ok(false),
    }
}

/// `read_coordinator_metadata`(`metadata.py:28-34`)。读 `coordinator.json`;损坏/缺失/非 dict → `None`。
pub fn read_coordinator_metadata(workspace: &WorkspacePath) -> Option<CoordinatorMetadata> {
    let text = std::fs::read_to_string(coordinator_meta_path(workspace)).ok()?;
    serde_json::from_str(&text).ok()
}

pub fn current_coordinator_binary_identity() -> CoordinatorBinaryIdentity {
    if let Ok(raw) = std::env::var("TEAM_AGENT_TEST_CALLER_BINARY_IDENTITY") {
        if let Ok(identity) = serde_json::from_str::<CoordinatorBinaryIdentity>(&raw) {
            if !identity.binary_path.is_empty() && !identity.binary_version.is_empty() {
                return identity;
            }
        }
    }
    let binary_path = std::env::current_exe()
        .map(|path| path.canonicalize().unwrap_or(path))
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "<unknown>".to_string());
    CoordinatorBinaryIdentity {
        binary_path,
        binary_version: crate::packaging::Version::current().as_str().to_string(),
    }
}

/// `coordinator_metadata_ok` now includes daemon binary identity in addition
/// to the original pid/protocol/schema tuple.
pub fn coordinator_metadata_ok(metadata: Option<&CoordinatorMetadata>, pid: Pid) -> bool {
    coordinator_metadata_mismatch_reason(metadata, pid).is_none()
}

pub fn coordinator_metadata_mismatch_reason(
    metadata: Option<&CoordinatorMetadata>,
    pid: Pid,
) -> Option<CoordinatorMetadataMismatchReason> {
    let identity = current_coordinator_binary_identity();
    coordinator_metadata_mismatch_reason_with_identity(metadata, pid, &identity)
}

fn coordinator_metadata_mismatch_reason_with_identity(
    metadata: Option<&CoordinatorMetadata>,
    pid: Pid,
    identity: &CoordinatorBinaryIdentity,
) -> Option<CoordinatorMetadataMismatchReason> {
    coordinator_wire_metadata_mismatch_reason(metadata, pid)
        .or_else(|| coordinator_binary_identity_mismatch_reason(metadata, identity))
}

fn coordinator_wire_metadata_mismatch_reason(
    metadata: Option<&CoordinatorMetadata>,
    pid: Pid,
) -> Option<CoordinatorMetadataMismatchReason> {
    let Some(metadata) = metadata else {
        return Some(CoordinatorMetadataMismatchReason::MetadataMissing);
    };
    if metadata.pid != pid {
        return Some(CoordinatorMetadataMismatchReason::PidMismatch);
    }
    if metadata.protocol_version != PROTOCOL_VERSION {
        return Some(CoordinatorMetadataMismatchReason::ProtocolVersionMismatch);
    }
    if metadata.message_store_schema_version != crate::db::schema::SCHEMA_VERSION {
        return Some(CoordinatorMetadataMismatchReason::MessageStoreSchemaVersionMismatch);
    }
    None
}

fn coordinator_binary_identity_mismatch_reason(
    metadata: Option<&CoordinatorMetadata>,
    identity: &CoordinatorBinaryIdentity,
) -> Option<CoordinatorMetadataMismatchReason> {
    let Some(metadata) = metadata else {
        return Some(CoordinatorMetadataMismatchReason::MetadataMissing);
    };
    let Some(binary_version) = metadata
        .binary_version
        .as_deref()
        .filter(|value| !value.is_empty())
    else {
        return Some(CoordinatorMetadataMismatchReason::BinaryIdentityMissing);
    };
    let Some(binary_path) = metadata
        .binary_path
        .as_deref()
        .filter(|value| !value.is_empty())
    else {
        return Some(CoordinatorMetadataMismatchReason::BinaryIdentityMissing);
    };
    if binary_version != identity.binary_version {
        return Some(CoordinatorMetadataMismatchReason::BinaryVersionMismatch);
    }
    if !binary_path_matches_current_identity(binary_path, &identity.binary_path) {
        return Some(CoordinatorMetadataMismatchReason::BinaryPathMismatch);
    }
    None
}

fn coordinator_binary_identity_relation(
    metadata: Option<&CoordinatorMetadata>,
    identity: &CoordinatorBinaryIdentity,
) -> CoordinatorBinaryIdentityRelation {
    let Some(metadata) = metadata else {
        return CoordinatorBinaryIdentityRelation::Unknown;
    };
    let Some(daemon_version) = metadata
        .binary_version
        .as_deref()
        .filter(|value| !value.is_empty())
    else {
        return CoordinatorBinaryIdentityRelation::Unknown;
    };
    match compare_version_strings(daemon_version, &identity.binary_version) {
        Some(std::cmp::Ordering::Greater) => {
            return CoordinatorBinaryIdentityRelation::DaemonNewerThanCaller;
        }
        Some(std::cmp::Ordering::Less) => {
            return CoordinatorBinaryIdentityRelation::CallerNewerThanDaemon;
        }
        Some(std::cmp::Ordering::Equal) => {}
        None => return CoordinatorBinaryIdentityRelation::Unknown,
    }
    let Some(daemon_path) = metadata
        .binary_path
        .as_deref()
        .filter(|value| !value.is_empty())
    else {
        return CoordinatorBinaryIdentityRelation::Unknown;
    };
    if binary_path_matches_current_identity(daemon_path, &identity.binary_path) {
        CoordinatorBinaryIdentityRelation::Same
    } else {
        CoordinatorBinaryIdentityRelation::SameVersionPathMismatch
    }
}

fn compare_version_strings(left: &str, right: &str) -> Option<std::cmp::Ordering> {
    let left = parse_numeric_version(left)?;
    let right = parse_numeric_version(right)?;
    Some(left.cmp(&right))
}

fn parse_numeric_version(value: &str) -> Option<Vec<u64>> {
    if value.is_empty() {
        return None;
    }
    value
        .split('.')
        .map(|part| part.parse::<u64>().ok())
        .collect()
}

fn binary_path_matches_current_identity(metadata_path: &str, identity_path: &str) -> bool {
    if metadata_path == identity_path {
        return true;
    }
    test_harness_binary_path_matches(metadata_path)
}

/// Test harness escape hatch for integration tests whose process identity is the
/// test binary while fixture metadata intentionally points at the built CLI.
/// Production must not infer this from path shape; callers must set the
/// `TEAM_AGENT_TEST_HARNESS_BINARY_PATH_MATCH` env explicitly to either the
/// expected binary path or the target directory containing `team-agent`.
fn test_harness_binary_path_matches(metadata_path: &str) -> bool {
    let Ok(path) = std::env::var("TEAM_AGENT_TEST_HARNESS_BINARY_PATH_MATCH") else {
        return false;
    };
    let path = PathBuf::from(path);
    path_matches(metadata_path, &path)
        || path_matches(metadata_path, &path.join("team-agent"))
        || path
            .parent()
            .is_some_and(|parent| path_matches(metadata_path, &parent.join("team-agent")))
}

fn path_matches(metadata_path: &str, path: &Path) -> bool {
    path.to_string_lossy() == metadata_path
        || path
            .canonicalize()
            .ok()
            .is_some_and(|path| path.to_string_lossy() == metadata_path)
}

/// `write_coordinator_metadata`(`metadata.py:46-61`)。写 `coordinator.json`(pretty indent=2),
/// `updated_at = now(utc).isoformat()`。
pub fn write_coordinator_metadata(
    workspace: &WorkspacePath,
    pid: Pid,
    source: MetadataSource,
) -> Result<(), std::io::Error> {
    let path = coordinator_meta_path(workspace);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let metadata = CoordinatorMetadata {
        pid,
        protocol_version: PROTOCOL_VERSION,
        message_store_schema_version: crate::db::schema::SCHEMA_VERSION,
        binary_path: Some(current_coordinator_binary_identity().binary_path),
        binary_version: Some(crate::packaging::Version::current().as_str().to_string()),
        source,
        updated_at: chrono::Utc::now().to_rfc3339(),
    };
    let text = serde_json::to_string_pretty(&metadata)
        .map_err(|e| std::io::Error::other(e.to_string()))?;
    std::fs::write(path, text)
}

pub(crate) fn message_store_schema_health(workspace: &WorkspacePath) -> SchemaHealth {
    match MessageStore::open(workspace.as_path()) {
        Ok(_) => SchemaHealth {
            ok: true,
            schema_version: crate::db::schema::SCHEMA_VERSION,
            error: None,
            action: None,
        },
        Err(e) => SchemaHealth {
            ok: false,
            schema_version: crate::db::schema::SCHEMA_VERSION,
            error: Some(SchemaError::InitFailed {
                message: e.to_string(),
            }),
            action: Some("run team-agent doctor --fix-schema --json".to_string()),
        },
    }
}

fn read_pid_file(path: &Path) -> Option<Pid> {
    let text = std::fs::read_to_string(path).ok()?;
    let raw = text.trim().parse::<u32>().ok()?;
    Some(Pid::new(raw))
}

fn remove_file_if_exists(path: &Path) -> Result<(), std::io::Error> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

// ===========================================================================
// coordinator 路径(paths.py)
// ===========================================================================

/// `coordinator.pid` 路径(`paths.py:8`)= `runtime_dir(workspace)/coordinator.pid`。
pub fn coordinator_pid_path(workspace: &WorkspacePath) -> PathBuf {
    crate::model::paths::runtime_dir(workspace.as_path()).join("coordinator.pid")
}

/// `coordinator.json` 路径(`paths.py:12`)。
pub fn coordinator_meta_path(workspace: &WorkspacePath) -> PathBuf {
    crate::model::paths::runtime_dir(workspace.as_path()).join("coordinator.json")
}

/// `coordinator.log` 路径(`paths.py:16`)。
pub fn coordinator_log_path(workspace: &WorkspacePath) -> PathBuf {
    crate::model::paths::runtime_dir(workspace.as_path()).join("coordinator.log")
}

// ===========================================================================
// watch 实时流(watch/__init__.py)—— `team-agent watch`
// ===========================================================================

/// `collect_watch_lines`(`watch.py:40`)。tail events.jsonl(过滤 team)+ latest_results,
/// 渲染人类可读行;处理 log rotation(ROTATION_MARKER + offset 重置,不重放历史段)。
/// 推进 `cursor`。
pub fn collect_watch_lines(
    workspace: &WorkspacePath,
    cursor: &mut WatchCursor,
    store: &MessageStore,
    team: Option<&str>,
) -> Result<Vec<String>, WatchError> {
    let mut lines = collect_event_lines(workspace, cursor, team)?;
    lines.extend(collect_result_lines(workspace, cursor, store, team)?);
    Ok(lines)
}

/// `_collect_event_lines`(`watch.py:66-97`):tail events.jsonl,按 team 过滤。
fn collect_event_lines(
    workspace: &WorkspacePath,
    cursor: &mut WatchCursor,
    team: Option<&str>,
) -> Result<Vec<String>, WatchError> {
    let logs = crate::model::paths::logs_dir(workspace.as_path());
    let events_path = logs.join("events.jsonl");
    let archive_path = logs.join("events.jsonl.1");
    let archive_signature = file_signature(&archive_path)?;
    let mut lines = Vec::new();

    let size = std::fs::metadata(&events_path)
        .map(|m| m.len())
        .unwrap_or(0);
    let rotated = cursor.initialized
        && (cursor.archive_signature != archive_signature || cursor.event_offset > size);
    if rotated {
        lines.push(ROTATION_MARKER.to_string());
        cursor.event_offset = 0;
    }
    cursor.archive_signature = archive_signature;

    let mut file = match std::fs::File::open(&events_path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            cursor.initialized = true;
            return Ok(lines);
        }
        Err(e) => return Err(WatchError::Io(e)),
    };
    file.seek(SeekFrom::Start(cursor.event_offset))?;
    let mut text = String::new();
    file.read_to_string(&mut text)?;
    cursor.event_offset = file.stream_position()?;
    cursor.initialized = true;
    for line in text.lines() {
        if let Ok(event) = serde_json::from_str::<Value>(line) {
            // watch.py:91 — `if team and _event_team_id(event) != team: continue`.
            if team.is_some() && event_team_id(&event).as_deref() != team {
                continue;
            }
            if let Some(rendered) = render_event_line(&event) {
                lines.push(rendered);
            }
        }
    }
    Ok(lines)
}

/// `_event_team_id`(`watch.py:132-134`)。
fn event_team_id(event: &Value) -> Option<String> {
    ["team_id", "owner_team_id", "team"]
        .iter()
        .find_map(|key| event.get(*key))
        .and_then(|value| match value {
            Value::String(s) if !s.is_empty() => Some(s.clone()),
            Value::Number(n) => Some(n.to_string()),
            _ => None,
        })
}

/// `_collect_result_lines`(`watch.py:100-112`):store.latest_results(owner_team_id=team)
/// 出 `result_received: {agent} -> {summary}` 行;按 cursor.seen_result_ids 去重。
fn collect_result_lines(
    workspace: &WorkspacePath,
    cursor: &mut WatchCursor,
    store: &MessageStore,
    team: Option<&str>,
) -> Result<Vec<String>, WatchError> {
    let db_path = crate::model::paths::runtime_dir(workspace.as_path()).join("team.db");
    if !db_path.exists() {
        return Ok(Vec::new());
    }
    let mut lines = Vec::new();
    for row in store.latest_results(20, team)? {
        let Some(result_id) = row
            .get("result_id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
            .map(str::to_string)
        else {
            continue;
        };
        if !cursor.seen_result_ids.insert(result_id) {
            continue;
        }
        let mut summary = crate::message_store::result_summary_from_row(&row)
            .unwrap_or_else(|| serde_json::json!({}));
        if let Some(obj) = summary.as_object_mut() {
            obj.insert(
                "event".to_string(),
                Value::String("result_received".to_string()),
            );
        }
        if let Some(rendered) = render_event_line(&summary) {
            lines.push(rendered);
        }
    }
    Ok(lines)
}

/// `render_event_line`(`watch.py:46-63`)。把一条 step 4 事件渲染成人类可读行;非可渲染事件 → `None`。
/// 消费的事件类型:`result_received` / `leader_receiver.{injected,submitted}` / `send.failed` /
/// `leader_receiver.rebind_required` / `leader.api_error`(card 表)。
pub fn render_event_line(event: &Value) -> Option<String> {
    let event_name = event.get("event").and_then(Value::as_str)?;
    match event_name {
        "result_received" => Some(format!(
            "result_received: {} -> {}",
            clean_field(event, &["agent_id"], "-"),
            prefix_chars(&clean_field(event, &["summary"], "-"), 80)
        )),
        "leader_receiver.injected" | "leader_receiver.submitted" => {
            let id = first_field(event, &["message_id", "msg_id"]).unwrap_or("-");
            let id = prefix_chars(id, 12);
            Some(format!(
                "leader_receiver.injected: {} -> {}",
                id,
                clean_field(event, &["recipient", "to"], "-")
            ))
        }
        "send.failed" => Some(format!(
            "send.failed: {} reason={}",
            clean_field(event, &["recipient", "to", "target"], "-"),
            clean_field(event, &["reason", "error"], "-")
        )),
        "leader_receiver.rebind_required" => Some(format!(
            "leader_receiver.rebind_required: pane={} reason={}",
            clean_field(event, &["old_pane_id", "pane_id", "target"], "-"),
            clean_field(event, &["reason", "rediscovery_status"], "-")
        )),
        "leader.api_error" => Some(format!(
            "leader.api_error: {} provider={} snippet={}",
            clean_field(event, &["error_class"], "Unknown"),
            clean_field(event, &["provider"], "-"),
            clean_field(event, &["matched_pattern_snippet", "snippet"], "-")
        )),
        _ => None,
    }
}

/// `run_watch`(`watch.py:25`)。`team-agent watch` 主循环:反复 `collect_watch_lines` + 输出 + sleep。
/// `output`/`sleep` 注入便于测试。§10 返 Result。
pub fn run_watch(
    workspace: &WorkspacePath,
    team: Option<&str>,
    interval_sec: f64,
    output: &mut dyn FnMut(&str),
) -> Result<(), WatchError> {
    let store = MessageStore::open(workspace.as_path())?;
    let mut cursor = WatchCursor::default();
    let interval = if interval_sec.is_finite() && interval_sec > 0.0 {
        std::time::Duration::from_secs_f64(interval_sec)
    } else {
        std::time::Duration::from_millis(100)
    };
    loop {
        for line in collect_watch_lines(workspace, &mut cursor, &store, team)? {
            output(&line);
        }
        std::thread::sleep(interval);
    }
}

/// watch 错误(读 events.jsonl / latest_results)。
#[derive(Debug, Error)]
pub enum WatchError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("message store: {0}")]
    MessageStore(#[from] crate::message_store::MessageStoreError),
}

fn file_signature(path: &Path) -> Result<Option<(u64, i128)>, WatchError> {
    let meta = match std::fs::metadata(path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(WatchError::Io(e)),
    };
    let modified = meta.modified().ok();
    let nanos = modified
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .and_then(|d| i128::try_from(d.as_nanos()).ok())
        .unwrap_or(0);
    Ok(Some((meta.len(), nanos)))
}

fn first_field<'a>(event: &'a Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .find_map(|key| event.get(*key).and_then(Value::as_str))
}

fn clean_field(event: &Value, keys: &[&str], default: &str) -> String {
    first_field(event, keys)
        .map(clean_text)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| default.to_string())
}

fn clean_text(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn prefix_chars(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    struct ChildGuard(std::process::Child);

    impl Drop for ChildGuard {
        fn drop(&mut self) {
            unsafe {
                libc::kill(self.0.id() as libc::pid_t, libc::SIGTERM);
            }
            let _ = self.0.wait();
        }
    }

    #[test]
    fn coordinator_daemon_spawn_helper_detaches_session() {
        let mut command = Command::new("/bin/sleep");
        command
            .arg("30")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        detach_daemon_child(&mut command);

        let child = command.spawn().expect("spawn detached child");
        let guard = ChildGuard(child);
        let pid = guard.0.id() as libc::pid_t;
        let sid = unsafe { libc::getsid(pid) };

        assert_ne!(sid, -1, "getsid({pid}) failed");
        assert_eq!(
            sid, pid,
            "detached coordinator children must become session leaders so launcher SIGHUP does not reach them"
        );
    }
}
