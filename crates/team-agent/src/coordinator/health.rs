//! coordinator 健康/身份 & 只读可观测面:metadata 身份原语 + coordinator 路径 + watch 实时流。

use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use serde_json::Value;
use thiserror::Error;

use crate::message_store::MessageStore;

use super::types::{
    CoordinatorHealthStatus, CoordinatorMetadata, HealthReport, MetadataSource, Pid, SchemaError,
    SchemaHealth, StartError, StartOutcome, StartReport, StopError, StopOutcome, StopReport,
    WatchCursor, WorkspacePath, PROTOCOL_VERSION, ROTATION_MARKER,
};

// ===========================================================================
// coordinator daemon lifecycle (lifecycle.py:38-247).
// start_coordinator spawns the `team-agent coordinator --workspace <ws>` daemon subprocess;
// the actual spawn is the #[ignore] real-machine boundary, the idempotent decision is testable.
// ===========================================================================

/// `coordinator_health`(`lifecycle.py:38-46`):`running ∧ metadata_ok ∧ schema_ok` → typed report.
pub fn coordinator_health(workspace: &WorkspacePath) -> HealthReport {
    let schema = message_store_schema_health(workspace);
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
    let metadata_ok = pid.is_some_and(|p| coordinator_metadata_ok(metadata.as_ref(), p));
    let running = matches!(status, CoordinatorHealthStatus::Running);
    HealthReport {
        ok: running && metadata_ok && schema.ok,
        status,
        pid,
        metadata,
        metadata_ok,
        schema,
    }
}

/// `start_coordinator`(`lifecycle.py:49-121`):幂等 — 已健康 no-op(AlreadyRunning);metadata 不兼容
/// 先 stop 再起;schema 不兼容拒启 + hint;否则 spawn `team-agent coordinator --workspace <ws>`。
pub fn start_coordinator(workspace: &WorkspacePath) -> Result<StartReport, StartError> {
    let health = coordinator_health(workspace);
    if health.ok {
        return Ok(StartReport {
            ok: true,
            pid: health.pid,
            status: StartOutcome::AlreadyRunning,
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
            log: None,
            schema_error: health.schema.error,
            action: health.schema.action,
        });
    }
    if health.pid.is_some() && !health.metadata_ok && health.metadata.is_some() {
        match stop_coordinator(workspace) {
            Ok(stop) if stop.ok => {}
            Ok(_) | Err(_) => {
                return Ok(StartReport {
                    ok: false,
                    pid: health.pid,
                    status: StartOutcome::RestartIncompatibleStopFailed,
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
    let child = Command::new(std::env::current_exe()?)
        .args(["coordinator", "--workspace"])
        .arg(workspace.as_path())
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(log_err))
        .spawn()?;
    let pid = Pid::new(child.id());
    std::fs::write(coordinator_pid_path(workspace), pid.to_string())?;
    write_coordinator_metadata(workspace, pid, MetadataSource::Start)?;
    Ok(StartReport {
        ok: true,
        pid: Some(pid),
        status: StartOutcome::Started,
        log: Some(log_path),
        schema_error: None,
        action: None,
    })
}

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
    let Ok(pid_t) = libc::pid_t::try_from(pid.get()) else {
        return Ok(StopReport {
            ok: false,
            status: StopOutcome::KillFailed,
            pid: Some(pid),
        });
    };
    let rc = unsafe { libc::kill(pid_t, libc::SIGTERM) };
    if rc != 0 {
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
    let output = match Command::new("ps")
        .args(["-axo", "pid=,command="])
        .output()
    {
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
    let split = line
        .find(char::is_whitespace)
        .unwrap_or(line.len());
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
        && command.split_whitespace().any(|token| token == "coordinator")
        && command.contains("--workspace")
        && workspaces.iter().any(|workspace| command.contains(workspace))
}

fn terminate_pid(pid: Pid) -> bool {
    if pid_is_running(pid).ok() == Some(false) {
        return true;
    }
    if !send_signal(pid, libc::SIGTERM) {
        return false;
    }
    if wait_until_not_running(pid, Duration::from_millis(750)) {
        return true;
    }
    send_signal(pid, libc::SIGKILL) && wait_until_not_running(pid, Duration::from_millis(750))
}

fn send_signal(pid: Pid, signal: libc::c_int) -> bool {
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
    let out = Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "stat="])
        .output()?;
    if !out.status.success() {
        return Ok(false);
    }
    let stat = String::from_utf8_lossy(&out.stdout).trim().to_string();
    Ok(!stat.is_empty() && !stat.starts_with('Z'))
}

/// `read_coordinator_metadata`(`metadata.py:28-34`)。读 `coordinator.json`;损坏/缺失/非 dict → `None`。
pub fn read_coordinator_metadata(workspace: &WorkspacePath) -> Option<CoordinatorMetadata> {
    let text = std::fs::read_to_string(coordinator_meta_path(workspace)).ok()?;
    serde_json::from_str(&text).ok()
}

/// `coordinator_metadata_ok`(`metadata.py:37-43`):三元全等
/// `meta.pid == pid ∧ meta.protocol_version == PROTOCOL_VERSION ∧
/// meta.message_store_schema_version == SCHEMA_VERSION`。任一不符 → false(不静默继续旧 schema)。
pub fn coordinator_metadata_ok(metadata: Option<&CoordinatorMetadata>, pid: Pid) -> bool {
    metadata.is_some_and(|m| {
        m.pid == pid
            && m.protocol_version == PROTOCOL_VERSION
            && m.message_store_schema_version == crate::db::schema::SCHEMA_VERSION
    })
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
        source,
        updated_at: chrono::Utc::now().to_rfc3339(),
    };
    let text = serde_json::to_string_pretty(&metadata)
        .map_err(|e| std::io::Error::other(e.to_string()))?;
    std::fs::write(path, text)
}

fn message_store_schema_health(workspace: &WorkspacePath) -> SchemaHealth {
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
            action: Some("run team-agent repair-state --schema".to_string()),
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
    let _ = (store, team);
    let logs = crate::model::paths::logs_dir(workspace.as_path());
    let events_path = logs.join("events.jsonl");
    let archive_path = logs.join("events.jsonl.1");
    let archive_signature = file_signature(&archive_path)?;
    let mut lines = Vec::new();

    let size = std::fs::metadata(&events_path).map(|m| m.len()).unwrap_or(0);
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
            if let Some(rendered) = render_event_line(&event) {
                lines.push(rendered);
            }
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
    keys.iter().find_map(|key| event.get(*key).and_then(Value::as_str))
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
