use std::io::{Seek, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::lifecycle::types::LifecycleError;
use crate::model::ids::AgentId;

pub(crate) const AGENT_LIFECYCLE_LOCK_NAME: &str = "agent-lifecycle";
pub(crate) const LIFECYCLE_LOCK_TIMEOUT: Duration = Duration::from_secs(30);
pub(crate) const LIFECYCLE_LOCK_HELD_LONG: Duration = Duration::from_secs(5);

pub(crate) struct LifecycleLockRequest<'a> {
    pub workspace: &'a Path,
    pub operation: &'static str,
    pub team: Option<&'a str>,
    pub agent_id: Option<&'a AgentId>,
}

pub(crate) struct LifecycleLockGuard {
    #[allow(dead_code)]
    file: std::fs::File,
}

pub(crate) fn acquire_agent_lifecycle_lock(
    request: LifecycleLockRequest<'_>,
) -> Result<LifecycleLockGuard, LifecycleError> {
    #[cfg(test)]
    if let Some((timeout, held_long)) = test_lifecycle_lock_deadline_override() {
        return acquire_agent_lifecycle_lock_with_deadlines(request, timeout, held_long);
    }
    acquire_agent_lifecycle_lock_with_deadlines(
        request,
        LIFECYCLE_LOCK_TIMEOUT,
        LIFECYCLE_LOCK_HELD_LONG,
    )
}

#[cfg(test)]
pub(crate) fn acquire_agent_lifecycle_lock_for_test(
    request: LifecycleLockRequest<'_>,
    timeout: Duration,
    held_long: Duration,
) -> Result<LifecycleLockGuard, LifecycleError> {
    acquire_agent_lifecycle_lock_with_deadlines(request, timeout, held_long)
}

#[cfg(test)]
thread_local! {
    static TEST_DEADLINE_OVERRIDE: std::cell::Cell<Option<(Duration, Duration)>> =
        const { std::cell::Cell::new(None) };
}

#[cfg(test)]
pub(crate) struct LifecycleLockDeadlineOverrideGuard {
    previous: Option<(Duration, Duration)>,
}

#[cfg(test)]
pub(crate) fn override_agent_lifecycle_lock_deadlines_for_test(
    timeout: Duration,
    held_long: Duration,
) -> LifecycleLockDeadlineOverrideGuard {
    let previous = TEST_DEADLINE_OVERRIDE.with(|override_cell| {
        let previous = override_cell.get();
        override_cell.set(Some((timeout, held_long)));
        previous
    });
    LifecycleLockDeadlineOverrideGuard { previous }
}

#[cfg(test)]
fn test_lifecycle_lock_deadline_override() -> Option<(Duration, Duration)> {
    TEST_DEADLINE_OVERRIDE.with(std::cell::Cell::get)
}

#[cfg(test)]
impl Drop for LifecycleLockDeadlineOverrideGuard {
    fn drop(&mut self) {
        TEST_DEADLINE_OVERRIDE.with(|override_cell| override_cell.set(self.previous));
    }
}

fn acquire_agent_lifecycle_lock_with_deadlines(
    request: LifecycleLockRequest<'_>,
    timeout: Duration,
    held_long: Duration,
) -> Result<LifecycleLockGuard, LifecycleError> {
    // 0.5.x Windows portability Batch 2: migrated to
    // `crate::platform::file_lock::{try_lock_once_nonblocking, unlock}`
    // so the same polling loop + waiter file + 5s `lock_held_long`
    // event + 30s N38 timeout error shape works on both Unix (`flock`)
    // and Windows (`LockFileEx`). The Batch 0 non-Unix stub that
    // returned `lock_timeout_error` unconditionally is now gone —
    // Windows callers get real lock behavior. Byte-preserving on Unix:
    // the polling cadence, waiter file writes, `lock_held_long_event`
    // emission, and error shape are all unchanged relative to the
    // pre-Batch-2 unix branch.
    let lock_path = agent_lifecycle_lock_path(request.workspace);
    if let Some(parent) = lock_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| LifecycleError::StatePersist(format!("create lifecycle lock dir: {e}")))?;
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .map_err(|e| LifecycleError::StatePersist(format!("open lifecycle lock: {e}")))?;
    let started = Instant::now();
    let mut long_event_written = false;
    let mut waiter = None;
    loop {
        match crate::platform::file_lock::try_lock_once_nonblocking(&file) {
            Ok(true) => {
                write_lock_metadata(&mut file, &request, &lock_path)?;
                return Ok(LifecycleLockGuard { file });
            }
            Ok(false) => {}
            Err(e) => {
                return Err(LifecycleError::StatePersist(format!(
                    "lifecycle lock acquire io error at {}: {e}",
                    lock_path.display()
                )));
            }
        }
        let elapsed = started.elapsed();
        if waiter.is_none() {
            waiter = WaiterFile::create(&request, &lock_path).ok();
        }
        if let Some(waiter) = waiter.as_ref() {
            let _ = waiter.write(&request, elapsed);
        }
        if !long_event_written && elapsed >= held_long {
            let _ = write_lock_held_long_event(&request, &lock_path, elapsed, held_long, timeout);
            long_event_written = true;
        }
        if elapsed >= timeout {
            return Err(lock_timeout_error(&request, &lock_path, elapsed));
        }
        std::thread::sleep(std::cmp::min(
            Duration::from_millis(50),
            timeout.saturating_sub(elapsed),
        ));
    }
}

pub(crate) fn agent_lifecycle_lock_path(workspace: &Path) -> PathBuf {
    crate::model::paths::runtime_dir(workspace).join(format!("{AGENT_LIFECYCLE_LOCK_NAME}.lock"))
}

fn write_lock_metadata(
    file: &mut std::fs::File,
    request: &LifecycleLockRequest<'_>,
    lock_path: &Path,
) -> Result<(), LifecycleError> {
    let metadata = serde_json::json!({
        "lock_name": AGENT_LIFECYCLE_LOCK_NAME,
        "pid": std::process::id(),
        "operation": request.operation,
        "team": request.team,
        "agent_id": request.agent_id.map(AgentId::as_str),
        "workspace": request.workspace.display().to_string(),
        "lock_path": lock_path.display().to_string(),
        "acquired_at": chrono::Utc::now().to_rfc3339(),
    });
    let mut bytes = serde_json::to_vec(&metadata).map_err(|e| {
        LifecycleError::StatePersist(format!("encode lifecycle lock metadata: {e}"))
    })?;
    bytes.push(b'\n');
    file.set_len(0)
        .map_err(|e| LifecycleError::StatePersist(format!("truncate lifecycle lock: {e}")))?;
    file.seek(std::io::SeekFrom::Start(0))
        .map_err(|e| LifecycleError::StatePersist(format!("seek lifecycle lock: {e}")))?;
    file.write_all(&bytes)
        .map_err(|e| LifecycleError::StatePersist(format!("write lifecycle lock metadata: {e}")))?;
    Ok(())
}

fn lock_timeout_error(
    request: &LifecycleLockRequest<'_>,
    lock_path: &Path,
    elapsed: Duration,
) -> LifecycleError {
    LifecycleError::LifecycleLockTimeout {
        lock_path: lock_path.to_path_buf(),
        log_path: crate::model::paths::logs_dir(request.workspace).join("events.jsonl"),
        operation: request.operation.to_string(),
        waited_ms: elapsed.as_millis(),
    }
}

fn write_lock_held_long_event(
    request: &LifecycleLockRequest<'_>,
    lock_path: &Path,
    elapsed: Duration,
    held_long: Duration,
    timeout: Duration,
) -> Result<(), crate::event_log::EventLogError> {
    let holder = read_holder(lock_path);
    let holder_duration_ms = holder_duration_ms(holder.as_ref());
    crate::event_log::EventLog::new(request.workspace).write(
        "lifecycle.lock_held_long",
        serde_json::json!({
            "lock_name": AGENT_LIFECYCLE_LOCK_NAME,
            "lock_path": lock_path.display().to_string(),
            "workspace": request.workspace.display().to_string(),
            "operation": request.operation,
            "team": request.team,
            "agent_id": request.agent_id.map(AgentId::as_str),
            "waited_ms": elapsed.as_millis(),
            "threshold_ms": held_long.as_millis(),
            "timeout_ms": timeout.as_millis(),
            "holder": holder,
            "holder_duration_ms": holder_duration_ms,
            "blocked_queue_len": blocked_queue_len(request.workspace),
        }),
    )?;
    Ok(())
}

fn read_holder(lock_path: &Path) -> Option<serde_json::Value> {
    let text = std::fs::read_to_string(lock_path).ok()?;
    let value = serde_json::from_str::<serde_json::Value>(&text).ok()?;
    Some(serde_json::json!({
        "pid": value.get("pid").cloned().unwrap_or(serde_json::Value::Null),
        "operation": value.get("operation").cloned().unwrap_or(serde_json::Value::Null),
        "team": value.get("team").cloned().unwrap_or(serde_json::Value::Null),
        "agent_id": value.get("agent_id").cloned().unwrap_or(serde_json::Value::Null),
        "acquired_at": value.get("acquired_at").cloned().unwrap_or(serde_json::Value::Null),
    }))
}

fn holder_duration_ms(holder: Option<&serde_json::Value>) -> Option<u128> {
    let acquired_at = holder?
        .get("acquired_at")
        .and_then(serde_json::Value::as_str)?;
    let acquired_at = chrono::DateTime::parse_from_rfc3339(acquired_at).ok()?;
    let elapsed = chrono::Utc::now().signed_duration_since(acquired_at.with_timezone(&chrono::Utc));
    elapsed.to_std().ok().map(|d| d.as_millis())
}

fn waiter_dir(workspace: &Path) -> PathBuf {
    crate::model::paths::runtime_dir(workspace).join(format!("{AGENT_LIFECYCLE_LOCK_NAME}.waiters"))
}

fn blocked_queue_len(workspace: &Path) -> Option<usize> {
    std::fs::read_dir(waiter_dir(workspace))
        .ok()
        .map(|entries| entries.filter(|entry| entry.is_ok()).count())
}

struct WaiterFile {
    path: PathBuf,
}

impl WaiterFile {
    fn create(request: &LifecycleLockRequest<'_>, lock_path: &Path) -> std::io::Result<Self> {
        let dir = waiter_dir(request.workspace);
        std::fs::create_dir_all(&dir)?;
        let path = dir.join(format!("{}.json", std::process::id()));
        let file = Self { path };
        file.write_with_lock(request, lock_path, Duration::from_millis(0))?;
        Ok(file)
    }

    fn write(&self, request: &LifecycleLockRequest<'_>, elapsed: Duration) -> std::io::Result<()> {
        self.write_with_lock(
            request,
            &agent_lifecycle_lock_path(request.workspace),
            elapsed,
        )
    }

    fn write_with_lock(
        &self,
        request: &LifecycleLockRequest<'_>,
        lock_path: &Path,
        elapsed: Duration,
    ) -> std::io::Result<()> {
        let payload = serde_json::json!({
            "pid": std::process::id(),
            "operation": request.operation,
            "team": request.team,
            "agent_id": request.agent_id.map(AgentId::as_str),
            "workspace": request.workspace.display().to_string(),
            "lock_path": lock_path.display().to_string(),
            "waited_ms": elapsed.as_millis(),
            "updated_at": chrono::Utc::now().to_rfc3339(),
        });
        let bytes = serde_json::to_vec(&payload).map_err(std::io::Error::other)?;
        std::fs::write(&self.path, bytes)
    }
}

impl Drop for WaiterFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

impl Drop for LifecycleLockGuard {
    fn drop(&mut self) {
        // Batch 2: unlock via platform primitive. Best-effort — OS
        // releases when the file handle closes anyway. Uniform on
        // both `flock(LOCK_UN)` (unix) and `UnlockFileEx` (windows).
        let _ = crate::platform::file_lock::unlock(&self.file);
    }
}
