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
}

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
    acquire_file_lock(workspace, &request, timeout)
}

fn acquire_file_lock(
    workspace: &Path,
    request: &PaneInputLockRequest<'_>,
    timeout: Duration,
) -> Option<PaneInputLockGuard> {
    let path = pane_input_lock_path(workspace, request.target_key);
    if let Some(parent) = path.parent() {
        if std::fs::create_dir_all(parent).is_err() {
            emit_timeout_alarm(request, timeout, Duration::ZERO, "lock_dir");
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
            emit_timeout_alarm(request, timeout, Duration::ZERO, "lock_open");
            return None;
        }
    };
    let started = Instant::now();
    loop {
        match crate::platform::file_lock::try_lock_once_nonblocking(&file) {
            Ok(true) => {
                return Some(PaneInputLockGuard { file: Some(file) });
            }
            Ok(false) => {}
            Err(_) => {
                emit_timeout_alarm(request, timeout, started.elapsed(), "lock_io");
                return None;
            }
        }
        let elapsed = started.elapsed();
        if elapsed >= timeout {
            emit_timeout_alarm(request, timeout, elapsed, "timeout");
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
) {
    // A5 watches this exact token on stderr. Commenting this line must turn A5 red.
    eprintln!(
        "{PANE_INPUT_LOCK_TIMEOUT_EVENT} target={} operation={} waited_ms={} timeout_ms={} reason={reason} proceeding",
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
                "disposition": "proceed",
            }),
        );
    }
}

impl Drop for PaneInputLockGuard {
    fn drop(&mut self) {
        if let Some(file) = self.file.take() {
            let _ = crate::platform::file_lock::unlock(&file);
        }
    }
}
