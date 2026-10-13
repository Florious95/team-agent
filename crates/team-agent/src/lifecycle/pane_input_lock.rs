//! ---
//! purpose: pane 输入通道互斥锁，让同一 pane 上的写入串行化
//! contract:
//!   provides:
//!     - name: acquire_or_proceed
//!       what: 取到锁则持有到 Drop；超时或锁不可用则告警后放行，不无限等待
//!   depends:
//!     - crate::platform::file_lock
//!     - crate::event_log::EventLog
//! boundary:
//!   - 不锁席位生命周期（那是 lock.rs）
//!   - 不重粘文本、不加自动兜底重投；重试只重按回车由既有 inject 路径负责
//!   - 不在投递层发明临时持有态
//! maturity: wired
//! ---

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::event_log::EventLog;
use crate::model::paths::runtime_dir;

/// Hard upper bound for waiting on the pane input lock. Constant, not a magic number in a loop.
pub(crate) const PANE_INPUT_LOCK_TIMEOUT: Duration = Duration::from_millis(200);

pub(crate) const PANE_INPUT_LOCK_NAME: &str = "pane-input";

/// Event / stderr token. A5 greps this exact string.
pub(crate) const PANE_INPUT_LOCK_TIMEOUT_EVENT: &str = "pane_input_lock.timeout";

pub(crate) struct PaneInputLockRequest<'a> {
    pub workspace: Option<&'a Path>,
    pub target_key: &'a str,
    pub operation: &'static str,
}

pub(crate) struct PaneInputLockGuard {
    file: Option<std::fs::File>,
    guard_gate: Option<std::fs::File>,
}

/// ---
/// purpose: 取 pane 输入锁；拿不到或超时则告警后放行，绝不无限等待
/// params:
///   request: 携带可选 workspace、目标 pane 键与操作名
/// returns: Some 表示已持锁，None 表示无 workspace 可锁或已超时放行
/// ---
/// Acquire the pane lock or proceed after the hard timeout.
///
/// On timeout / lock I/O failure: emit [`PANE_INPUT_LOCK_TIMEOUT_EVENT`] (stderr + events.jsonl)
/// and return `None` so the caller still writes. Relief messages must not wait forever.
pub(crate) fn acquire_or_proceed(request: PaneInputLockRequest<'_>) -> Option<PaneInputLockGuard> {
    let timeout = PANE_INPUT_LOCK_TIMEOUT;
    let Some(workspace) = request.workspace else {
        // No workspace (some unit backends): nothing to lock against. Not an alarm.
        return None;
    };
    acquire_file_lock(workspace, &request, timeout, "proceed")
}

/// Every legacy writer holds shared admission through its whole operation,
/// even when its ordinary 200ms lock times out. Required guards hold exclusive
/// admission, so neither can enter while the other is already writing.
pub(crate) fn acquire_for_write(request: PaneInputLockRequest<'_>) -> Result<Option<PaneInputLockGuard>, ()> {
    if request.workspace.is_none() { return Ok(None); }
    let gate = acquire_guard_gate(&request, false)?;
    let mut guard = acquire_or_proceed(request).unwrap_or(PaneInputLockGuard { file: None, guard_gate: None });
    guard.guard_gate = Some(gate);
    Ok(Some(guard))
}

fn acquire_guard_gate(request: &PaneInputLockRequest<'_>, exclusive: bool) -> Result<std::fs::File, ()> {
    let opened = (|| -> std::io::Result<std::fs::File> {
        let workspace = request.workspace.ok_or_else(|| std::io::Error::other("workspace missing"))?;
        let path = pane_input_lock_path(workspace, request.target_key).with_extension("guard");
        if let Some(parent) = path.parent() { std::fs::create_dir_all(parent)?; }
        let file = std::fs::OpenOptions::new().create(true).read(true).write(true).truncate(false).open(path)?;
        let result = if exclusive { file.try_lock() } else { file.try_lock_shared() };
        result.map_err(|error| match error {
            std::fs::TryLockError::WouldBlock => std::io::Error::from(std::io::ErrorKind::WouldBlock),
            std::fs::TryLockError::Error(error) => error,
        })?;
        Ok(file)
    })();
    opened.map_err(|_| emit_timeout_alarm(request, PANE_INPUT_LOCK_TIMEOUT, Duration::ZERO, "guard_gate", "refuse"))
}

/// Guarded native input must never treat a missing/contended lock as permission.
pub(crate) fn acquire_required(request: PaneInputLockRequest<'_>) -> Option<PaneInputLockGuard> {
    let Some(workspace) = request.workspace else {
        emit_timeout_alarm(&request, PANE_INPUT_LOCK_TIMEOUT, Duration::ZERO, "workspace_missing", "refuse");
        return None;
    };
    let gate = acquire_guard_gate(&request, true).ok()?;
    let mut guard = acquire_file_lock(workspace, &request, PANE_INPUT_LOCK_TIMEOUT, "refuse")?;
    guard.guard_gate = Some(gate);
    Some(guard)
}

fn acquire_file_lock(
    workspace: &Path,
    request: &PaneInputLockRequest<'_>,
    timeout: Duration,
    disposition: &str,
) -> Option<PaneInputLockGuard> {
    let path = pane_input_lock_path(workspace, request.target_key);
    if let Some(parent) = path.parent() {
        if std::fs::create_dir_all(parent).is_err() {
            emit_timeout_alarm(request, timeout, Duration::ZERO, "lock_dir", disposition);
            return None;
        }
    }
    let file = match std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&path)
    {
        Ok(file) => file,
        Err(_) => {
            emit_timeout_alarm(request, timeout, Duration::ZERO, "lock_open", disposition);
            return None;
        }
    };
    let started = Instant::now();
    loop {
        match crate::platform::file_lock::try_lock_once_nonblocking(&file) {
            Ok(true) => {
                return Some(PaneInputLockGuard { file: Some(file), guard_gate: None });
            }
            Ok(false) => {}
            Err(_) => {
                emit_timeout_alarm(request, timeout, started.elapsed(), "lock_io", disposition);
                return None;
            }
        }
        let elapsed = started.elapsed();
        if elapsed >= timeout {
            emit_timeout_alarm(request, timeout, elapsed, "timeout", disposition);
            return None;
        }
        std::thread::sleep(std::cmp::min(
            Duration::from_millis(10),
            timeout.saturating_sub(elapsed),
        ));
    }
}

fn pane_input_lock_path(workspace: &Path, target_key: &str) -> PathBuf {
    let safe: String = target_key
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c
            } else {
                '_'
            }
        })
        .collect();
    runtime_dir(workspace)
        .join(PANE_INPUT_LOCK_NAME)
        .join(format!("{safe}.lock"))
}

fn emit_timeout_alarm(
    request: &PaneInputLockRequest<'_>,
    timeout: Duration,
    waited: Duration,
    reason: &str,
    disposition: &str,
) {
    // A5 watches this exact token on stderr. Commenting this line must turn A5 red.
    eprintln!(
        "{PANE_INPUT_LOCK_TIMEOUT_EVENT} target={} operation={} waited_ms={} timeout_ms={} reason={reason} disposition={disposition}",
        request.target_key,
        request.operation,
        waited.as_millis(),
        timeout.as_millis()
    );
    if let Some(workspace) = request.workspace {
        let _ = EventLog::new(workspace).write(
            PANE_INPUT_LOCK_TIMEOUT_EVENT,
            serde_json::json!({
                "lock_name": PANE_INPUT_LOCK_NAME,
                "target": request.target_key,
                "operation": request.operation,
                "waited_ms": waited.as_millis() as u64,
                "timeout_ms": timeout.as_millis() as u64,
                "reason": reason,
                "disposition": disposition,
            }),
        );
    }
}

impl Drop for PaneInputLockGuard {
    fn drop(&mut self) {
        if let Some(file) = self.file.take() {
            let _ = crate::platform::file_lock::unlock(&file);
        }
        if let Some(gate) = self.guard_gate.take() { let _ = gate.unlock(); }
    }
}

#[cfg(test)]
mod required_tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn required_pane_input_lock_missing_workspace_refuses() {
        assert!(acquire_required(PaneInputLockRequest { workspace: None, target_key: "%1", operation: "fixture" }).is_none());
    }

    #[test]
    fn required_pane_input_lock_contention_refuses_and_drop_releases_same_key() {
        let workspace = std::env::temp_dir().join(format!("ta-required-pane-lock-{}", std::process::id()));
        std::fs::create_dir(&workspace).unwrap();
        let request = || PaneInputLockRequest { workspace: Some(workspace.as_path()), target_key: "%1", operation: "fixture" };
        let held = acquire_required(request()).unwrap();
        assert!(acquire_required(request()).is_none());
        let events = std::fs::read_to_string(workspace.join(".team/logs/events.jsonl")).unwrap();
        assert!(events.lines().filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
            .any(|row| row.to_string().contains("\"disposition\":\"refuse\"")));
        drop(held);
        drop(acquire_required(request()).unwrap());
        // Preserve the old timeout/proceed policy, but hold reader admission
        // through that unheld-main-lock writer so no required guard crosses it.
        let first_writer = acquire_for_write(request()).unwrap().unwrap();
        let timed_out_writer = acquire_for_write(request()).unwrap().unwrap();
        assert!(timed_out_writer.file.is_none());
        drop(first_writer);
        assert!(acquire_required(request()).is_none());
        drop(timed_out_writer);
        drop(acquire_required(request()).unwrap());
        std::fs::remove_dir_all(workspace).unwrap();
    }
}
