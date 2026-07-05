//! Cross-platform exclusive file lock primitive.
//!
//! Batch 0 provides:
//! - `FileLockGuard` type with `Drop` unlock semantics
//! - `try_lock_exclusive(path, timeout)` signature
//! - Unix impl: byte-equivalent to `state/persist.rs::RuntimeLock`
//!   `flock(LOCK_EX|LOCK_NB)` polling loop
//! - Windows impl: `unimplemented!()` stub referencing Batch 2's
//!   `LockFileEx` migration + optional `fs2::FileExt` cross-platform
//!   alternative
//!
//! Batch 2 (design.md §Batch 2) migrates `state/persist.rs` +
//! `lifecycle/lock.rs` callers onto this API and removes the
//! current `#[cfg(not(unix))] not_implemented` fallback.
//!
//! CR C-2 anchor: this module + the Batch 2 migration eliminates the
//! third of three known non-Unix fallbacks
//! (`state/persist.rs:183-189` "not yet implemented on non-unix").
//! Grep guard `platform_process_caller_whitelist_batch0.rs` verifies
//! the fallback is not reintroduced.

use std::fs::File;
use std::io;
use std::path::Path;
use std::time::Duration;

/// Owns the underlying `File` handle + platform lock. Drop releases
/// the lock. Batch 0 exposes only the type — Batch 2 wires callers.
#[allow(dead_code)]
pub struct FileLockGuard {
    file: Option<File>,
}

/// Lock acquisition failure. Currently just carries the io::Error;
/// Batch 2 may add a typed `LockedByOther { holder_pid }` variant if
/// the platform exposes holder identity.
#[derive(Debug, thiserror::Error)]
pub enum LockError {
    #[error("file lock timeout after {timeout_secs:.2}s")]
    Timeout { timeout_secs: f64 },
    #[error("file lock io error: {source}")]
    Io {
        #[from]
        source: io::Error,
    },
}

/// Try to acquire an exclusive advisory lock on `path`. Polls with
/// 50ms sleep until acquired or `timeout` expires.
///
/// Batch 0 stub — Batch 2 wires the actual implementation and moves
/// callers off `state/persist.rs::RuntimeLock::try_new`.
#[allow(dead_code)]
pub fn try_lock_exclusive(
    path: &Path,
    timeout: Duration,
) -> Result<FileLockGuard, LockError> {
    #[cfg(unix)]
    {
        try_lock_exclusive_unix(path, timeout)
    }
    #[cfg(not(unix))]
    {
        try_lock_exclusive_windows(path, timeout)
    }
}

#[cfg(unix)]
#[allow(dead_code)]
fn try_lock_exclusive_unix(path: &Path, timeout: Duration) -> Result<FileLockGuard, LockError> {
    // Byte-equivalent to `state/persist.rs::RuntimeLock::try_new`
    // Unix branch. Batch 2 migrates persist.rs + lifecycle/lock.rs
    // onto this fn and deletes the inline code.
    use std::os::unix::io::AsRawFd;
    use std::time::Instant;
    let file = File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)?;
    let start = Instant::now();
    loop {
        // SAFETY: flock with LOCK_EX|LOCK_NB — non-blocking exclusive.
        let ret = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if ret == 0 {
            return Ok(FileLockGuard { file: Some(file) });
        }
        let err = io::Error::last_os_error();
        if err.raw_os_error() != Some(libc::EWOULDBLOCK)
            && err.raw_os_error() != Some(libc::EAGAIN)
        {
            return Err(LockError::Io { source: err });
        }
        if start.elapsed() >= timeout {
            return Err(LockError::Timeout {
                timeout_secs: timeout.as_secs_f64(),
            });
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[cfg(not(unix))]
#[allow(dead_code)]
fn try_lock_exclusive_windows(
    _path: &Path,
    _timeout: Duration,
) -> Result<FileLockGuard, LockError> {
    // Batch 2 will use `LockFileEx` via `windows-sys`, or `fs2::FileExt`
    // if dependency policy allows (design.md §File lock API:241-244).
    // Batch 0 stub returns a typed Io error so `cargo check` compiles
    // but callers cannot silently succeed on Windows.
    Err(LockError::Io {
        source: io::Error::other(
            "windows platform::file_lock::try_lock_exclusive not yet implemented (Batch 2)",
        ),
    })
}

#[cfg(unix)]
impl Drop for FileLockGuard {
    fn drop(&mut self) {
        use std::os::unix::io::AsRawFd;
        if let Some(file) = self.file.take() {
            // SAFETY: release our own advisory lock. Same shape as
            // `state/persist.rs::RuntimeLock::drop`.
            unsafe {
                libc::flock(file.as_raw_fd(), libc::LOCK_UN);
            }
        }
    }
}

#[cfg(not(unix))]
impl Drop for FileLockGuard {
    fn drop(&mut self) {
        // Batch 2 will call `UnlockFileEx`.
        self.file = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_stub_returns_typed_error_not_silent_ok() {
        // CR C-2 anchor: Windows must NOT silently succeed at file
        // locking (the current `state/persist.rs::not_implemented`
        // fallback returns Locked, which is honest; the new stub must
        // also return a typed error, not Ok).
        #[cfg(not(unix))]
        {
            let path = std::env::temp_dir().join("ta-win-portability-lock-stub-test");
            let err = try_lock_exclusive(&path, Duration::from_millis(100))
                .expect_err("windows stub must not silent-ok");
            match err {
                LockError::Io { source } => {
                    let msg = source.to_string();
                    assert!(msg.contains("windows") || msg.contains("Batch 2"));
                }
                LockError::Timeout { .. } => {
                    panic!("windows stub must return Io not Timeout")
                }
            }
        }
    }

    #[test]
    fn unix_can_acquire_and_release_lock_via_guard_drop() {
        // Byte-equivalent to the semantics `state/persist.rs`
        // expects — same file_lock across processes on the same host.
        #[cfg(unix)]
        {
            let dir = std::env::temp_dir().join("ta-win-portability-file-lock-tests");
            std::fs::create_dir_all(&dir).unwrap();
            let path = dir.join("lock-a.tmp");
            let guard = try_lock_exclusive(&path, Duration::from_secs(1))
                .expect("first lock should succeed");
            drop(guard);
            // Drop released the lock; a subsequent acquisition must
            // also succeed.
            let guard2 = try_lock_exclusive(&path, Duration::from_secs(1))
                .expect("post-drop reacquire should succeed");
            drop(guard2);
            let _ = std::fs::remove_file(&path);
        }
    }
}
