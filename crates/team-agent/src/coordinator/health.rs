//! ---
//! purpose: coordinator daemon 的健康判定、幂等启停与只读观测面——pid/metadata/schema 三合一健康、spawn 与终止、runtime 路径、以及 team-agent watch 的事件渲染
//! contract:
//!   provides:
//!     - name: coordinator_health
//!       what: 由 pid 文件、coordinator.json 与 message store schema 合成 HealthReport，ok 与 service_available 分开表达
//!     - name: start_coordinator
//!       what: 幂等启动：已健康则 no-op，metadata 不兼容先停再起，schema 不兼容拒启并给修复 hint
//!     - name: start_coordinator_with_team
//!       what: 同上，并把 team_key 以 --team 传给子进程，免得 daemon 自己从 state 推
//!     - name: stop_coordinator
//!       what: 终止 daemon 并清 pid/meta；pid 文件缺失时用 ps 扫描发现流浪 coordinator
//!     - name: collect_watch_lines
//!       what: 从 events.jsonl 与结果表增量取出可渲染行，并推进 WatchCursor
//!     - name: render_event_line
//!       what: 把一条结构化事件渲染成人类可读行，不认识的事件返回 None
//!     - name: run_watch
//!       what: team-agent watch 主循环：反复 collect 后输出并 sleep
//!     - name: coordinator_pid_path
//!       what: coordinator.pid 的位置
//!     - name: coordinator_meta_path
//!       what: coordinator.json 的位置
//!     - name: coordinator_log_path
//!       what: coordinator.log 的位置
//!   depends:
//!     - super::types
//!     - crate::message_store
//!     - crate::db::schema
//!     - crate::event_log
//!     - crate::model::paths
//!     - crate::packaging
//!     - crate::platform::process
//!     - crate::os_probe
//! boundary:
//!   - 不做 tick 编排，也不投递任何消息
//!   - 不读 provider 凭据、不碰 .env；身份只取自当前可执行文件与已落盘 metadata
//!   - 终止进程限定本 workspace：优先按本次判定拿到的精确 pid；pid 文件缺失时的流浪回收会按 ps 命令行匹配「coordinator --workspace <本 ws>」发现目标，仍不做跨 workspace 的 pkill/killall 泛清
//!   - watch 侧只读：不重放已归档段，rotation 只插一条 marker 并重置 offset
//! maturity: wired
//! ---
//!
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
/// ---
/// purpose: 一次性判定本 workspace 的 coordinator daemon 是否健康
/// params:
///   workspace: workspace 根；pid/metadata 路径与 message store 都由它派生
/// returns: HealthReport。ok = 进程在跑 ∧ metadata 三元全等 ∧ 二进制身份一致 ∧ schema 兼容；service_available 刻意排除二进制身份，表示「这个 daemon 还能处理本队队列」；status 区分 Missing / InvalidPid / Running / Stale
/// ---
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
/// ---
/// purpose: 不带 team_key 的幂等启动入口
/// params:
///   workspace: workspace 根
/// returns: 与 start_coordinator_with_team(workspace, None) 完全一致
/// errors: 同 start_coordinator_with_team
/// ---
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
/// ---
/// purpose: 幂等启动 coordinator daemon 子进程，并把不兼容/需轮换的情形分成互不折叠的结局
/// params:
///   workspace: workspace 根
///   team_key: 传给子进程的 --team；None 或空串时子进程回落到 state.active_team_key
/// returns: StartReport。已健康 → AlreadyRunning（含「daemon 比调用方新，保留不动」这一支，rotation_reason=daemon_newer_than_caller）；schema 不兼容 → SchemaIncompatible 且 ok=false 并带修复 hint；在跑但 wire metadata 不兼容、或 metadata 指向调用方自身、或先停失败 → RestartIncompatibleStopFailed；成功 spawn → Started，因身份轮换而重起则为 StartedAfterRotation
/// errors: 建目录、开日志、spawn、写 pid/metadata 或写事件失败时返回 StartError；「拒启」不是 Err，而是 ok=false 的报告
/// ---
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
/// ---
/// purpose: 停掉本 workspace 的 coordinator daemon 并清掉 pid/meta 文件
/// params:
///   workspace: workspace 根
/// returns: StopReport。pid 文件不存在时先尝试按 ps 发现流浪 coordinator，仍没有则 Missing；pid 文件内容非法 → 清文件并报 InvalidPidRemoved；终止成功 → Stopped；信号发不出去 → KillFailed
/// errors: 删 pid/meta 文件失败时返回 StopError；本函数不写事件（EventLog 变体在此路径无产生点）
/// ---
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
/// ---
/// purpose: 把 coordinator 停机用的「先温和后强制」终止语义暴露给诊断清理路径复用
/// params:
///   pid: 要终止的进程；只终止这棵进程树，不做名字匹配的批量清理
/// returns: 超时窗口内整棵树都不再存活为 true
/// ---
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
/// ---
/// purpose: 判断某 pid 是不是本 coordinator 还能拥有的活进程（Unix 实现）
/// params:
///   pid: 待判定进程号
/// returns: 存活且非僵尸为 true。语义与通用存活探针不同：signal 返回 EPERM 一律判 false，因为 coordinator 只认自己能发信号的进程；另用 ps 的 stat 排掉僵尸
/// errors: 除 EPERM/ESRCH 外的 signal 错误、以及 ps 探测失败时返回 io::Error
/// cfg: unix
/// ---
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
/// ---
/// purpose: 判断某 pid 是不是本 coordinator 还能拥有的活进程（非 Unix 实现）
/// params:
///   pid: 待判定进程号
/// returns: 平台层报 Live 为 true，Dead 或 Unknown 均为 false。Windows 没有僵尸态，故不做 ps stat 那一步；平台层把拒绝访问算作 Live，于是查不动的进程仍被视为在跑
/// errors: 平台层存活查询失败时返回 io::Error
/// cfg: not(unix)
/// ---
#[cfg(not(unix))]
pub fn pid_is_running(pid: Pid) -> Result<bool, std::io::Error> {
    match crate::platform::process::pid_liveness(pid.get())? {
        crate::platform::process::ProcessLiveness::Live => Ok(true),
        crate::platform::process::ProcessLiveness::Dead => Ok(false),
        crate::platform::process::ProcessLiveness::Unknown { .. } => Ok(false),
    }
}

/// `read_coordinator_metadata`(`metadata.py:28-34`)。读 `coordinator.json`;损坏/缺失/非 dict → `None`。
/// ---
/// purpose: 读出已落盘的 coordinator.json
/// params:
///   workspace: workspace 根
/// returns: 解析成功才有值；文件缺失、读不动或 JSON 形状不符一律为 None，绝不返回半份 metadata
/// ---
pub fn read_coordinator_metadata(workspace: &WorkspacePath) -> Option<CoordinatorMetadata> {
    let text = std::fs::read_to_string(coordinator_meta_path(workspace)).ok()?;
    serde_json::from_str(&text).ok()
}

/// ---
/// purpose: 给出「当前这个 CLI 二进制」的身份，用于和 daemon 已记录的身份比对
/// returns: 路径取自当前可执行文件（尽量 canonicalize）而非 PATH 查找，版本取自编译进来的包版本；路径取不到时退化成 <unknown>。测试可用 TEAM_AGENT_TEST_CALLER_BINARY_IDENTITY 覆盖，且只有两字段都非空才采信
/// ---
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
/// ---
/// purpose: 判断已落盘 metadata 是否与当前事实完全一致
/// params:
///   metadata: 已读出的 coordinator.json；None 视为不一致
///   pid: 实际观测到的 daemon pid
/// returns: pid、协议版本、message store schema 版本、以及 daemon 二进制身份四者全对才为 true
/// ---
pub fn coordinator_metadata_ok(metadata: Option<&CoordinatorMetadata>, pid: Pid) -> bool {
    coordinator_metadata_mismatch_reason(metadata, pid).is_none()
}

/// ---
/// purpose: 给出 metadata 不一致的机器可读原因，而不是只给一个布尔
/// params:
///   metadata: 已读出的 coordinator.json；None 时原因为 MetadataMissing
///   pid: 实际观测到的 daemon pid
/// returns: 第一个不匹配项对应的原因；全部一致时为 None。先判 pid/协议/schema 这组线协议字段，再判二进制身份
/// ---
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
/// ---
/// purpose: 落盘 coordinator.json，把当前 daemon 的身份三元与二进制身份记下来
/// params:
///   workspace: workspace 根
///   pid: 本次要记录的 daemon pid
///   source: 这份 metadata 是 daemon 自举时写的还是 CLI start 时写的
/// returns: 写成功返回 ()。协议版本与 schema 版本取自当前构建常量，updated_at 是写入时刻的 UTC
/// errors: 建目录、序列化或写文件失败时返回 io::Error
/// ---
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

/// ---
/// purpose: 用「能不能真的打开本队 message store」来判 schema 兼容门
/// params:
///   workspace: workspace 根
/// returns: 打开成功则 ok=true 且 error/action 为空；失败则 ok=false，带 InitFailed 原文与修复 hint。schema_version 恒为当前构建的版本号
/// ---
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
/// ---
/// purpose: 给出 coordinator.pid 的位置
/// params:
///   workspace: workspace 根
/// returns: runtime 目录下的 coordinator.pid；只算路径，不保证文件存在
/// ---
pub fn coordinator_pid_path(workspace: &WorkspacePath) -> PathBuf {
    crate::model::paths::runtime_dir(workspace.as_path()).join("coordinator.pid")
}

/// `coordinator.json` 路径(`paths.py:12`)。
/// ---
/// purpose: 给出 coordinator.json 的位置
/// params:
///   workspace: workspace 根
/// returns: runtime 目录下的 coordinator.json；只算路径，不保证文件存在
/// ---
pub fn coordinator_meta_path(workspace: &WorkspacePath) -> PathBuf {
    crate::model::paths::runtime_dir(workspace.as_path()).join("coordinator.json")
}

/// `coordinator.log` 路径(`paths.py:16`)。
/// ---
/// purpose: 给出 coordinator.log 的位置
/// params:
///   workspace: workspace 根
/// returns: runtime 目录下的 coordinator.log；daemon 子进程的 stdout/stderr 都追加到这里
/// ---
pub fn coordinator_log_path(workspace: &WorkspacePath) -> PathBuf {
    crate::model::paths::runtime_dir(workspace.as_path()).join("coordinator.log")
}

// ===========================================================================
// watch 实时流(watch/__init__.py)—— `team-agent watch`
// ===========================================================================

/// `collect_watch_lines`(`watch.py:40`)。tail events.jsonl(过滤 team)+ latest_results,
/// 渲染人类可读行;处理 log rotation(ROTATION_MARKER + offset 重置,不重放历史段)。
/// 推进 `cursor`。
/// ---
/// purpose: 增量取出自上次游标以来的可渲染 watch 行（事件 + 结果两路）
/// params:
///   workspace: workspace 根
///   cursor: 可变游标，函数会推进 offset、已见结果 id 集合与归档签名
///   store: 已打开的 message store，用于取结果行
///   team: 只看这个 team 的事件；None 表示不过滤
/// returns: 本次新增的渲染行，事件行在前、结果行在后；无新内容时为空 Vec
/// errors: 读事件文件或查库失败时返回 WatchError
/// ---
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
/// ---
/// purpose: 把一条结构化事件渲染成一行人类可读文本
/// params:
///   event: 事件 JSON 对象；靠其中的 event 字段分派
/// returns: 已知事件类型返回渲染行，其余一律 None（不猜、不打印原始 JSON）。摘要字段做长度截断
/// ---
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
        "result_wake.registered" => Some(format!(
            "result_wake.registered: task={} watcher={}",
            clean_field(event, &["task_id"], "-"),
            clean_field(event, &["watcher_id"], "-")
        )),
        "result_wake.notified" => Some(format!(
            "result_wake.notified: task={} result={} watcher={}",
            clean_field(event, &["task_id"], "-"),
            clean_field(event, &["result_id"], "-"),
            clean_field(event, &["watcher_id"], "-")
        )),
        "result_wake.notify_failed" => Some(format!(
            "result_wake.notify_failed: task={} watcher={} reason={}",
            clean_field(event, &["task_id"], "-"),
            clean_field(event, &["watcher_id"], "-"),
            clean_field(event, &["reason", "error"], "-")
        )),
        _ => None,
    }
}

/// `run_watch`(`watch.py:25`)。`team-agent watch` 主循环:反复 `collect_watch_lines` + 输出 + sleep。
/// `output`/`sleep` 注入便于测试。§10 返 Result。
/// ---
/// purpose: team-agent watch 的主循环：反复增量收集、输出、休眠
/// params:
///   workspace: workspace 根
///   team: 只看这个 team；None 表示不过滤
///   interval_sec: 轮询间隔；非有限值或非正数时回落到内置默认
///   output: 输出回调，注入以便测试；本函数自己不写 stdout
/// returns: 循环结束时为 Ok。这是个长跑循环，正常运行期间不返回
/// errors: 打开 message store 或某轮收集失败时返回 WatchError
/// ---
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
