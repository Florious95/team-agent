//! Cross-platform exclusive file lock primitive.
//!
//! ## Batch 2 real implementation (leader msg_c833639b61b8)
//!
//! Batch 2 promotes this module from a signature-only scaffold to a
//! **byte-preserving migration** of the file-lock primitives used by
//! `state/persist.rs::RuntimeLock` (state-save lock) and
//! `lifecycle/lock.rs::LifecycleLockGuard` (per-workspace agent
//! lifecycle lock).
//!
//! ## Two-layer API
//!
//! The existing callers wrap their own polling loop + timeout +
//! `held_long` event emission around the raw `flock` syscall. To
//! preserve the exact caller behavior (persist.rs's 50ms poll + typed
//! `StateError::Locked`; lifecycle/lock.rs's waiter file + 5s
//! held-long event + N38 three-line timeout error), platform::file_lock
//! exposes **two primitives**, not one convenience `try_lock_exclusive`:
//!
//! 1. `try_lock_once_nonblocking(&File) -> Result<bool, io::Error>` —
//!    non-blocking exclusive lock attempt. Returns `Ok(true)` on
//!    success, `Ok(false)` on "would block" (both Unix `EWOULDBLOCK`
//!    and Windows `ERROR_LOCK_VIOLATION`), `Err(...)` on real I/O
//!    error. Callers keep their own polling loops so the outer
//!    timeout/event/error shape is byte-preserving.
//! 2. `unlock(&File)` — release the lock. Called from `Drop` in both
//!    caller sites.
//!
//! A high-level `try_lock_exclusive(path, timeout) -> FileLockGuard`
//! wrapper is also provided (uses the same primitives + a bare polling
//! loop) for future callers that don't need the metadata/waiter
//! machinery — but the two existing product callers keep their own
//! loops per this batch's byte-preserving constraint.
//!
//! ## Windows implementation
//!
//! Windows implementation uses `LockFileEx` /
//! `UnlockFileEx` via `windows-sys`. The lock is `LOCKFILE_EXCLUSIVE_LOCK
//! | LOCKFILE_FAIL_IMMEDIATELY` so it maps 1:1 to
//! `flock(LOCK_EX|LOCK_NB)`. Range is `[0, u64::MAX)` (whole-file lock).
//!
//! ## CR anchors
//!
//! - **C-2**: consuming the two callers via these primitives eliminates
//!   the `state/persist.rs::not_yet_implemented` fallback (persist.rs
//!   loop becomes cfg-free) AND the `lifecycle/lock.rs::lock_timeout_error`
//!   non-Unix stub. The `platform_fallback_burndown_batch0.rs` grep
//!   guard is updated to flip its persist.rs assertion from
//!   "fallback present + FIXME marker" to "fallback removed".
//! - **C-6 N38 (held_long event)**: the 5s `write_lock_held_long_event`
//!   in `lifecycle/lock.rs` stays in the caller (this module is
//!   pure OS primitives). Windows sees the SAME event because the
//!   caller's polling loop is now cfg-free.

use std::fs::File;
use std::io;
use std::path::Path;
use std::time::Duration;

/// Try to acquire an exclusive advisory lock on `file` without
/// blocking. Returns `Ok(true)` if the lock was acquired, `Ok(false)`
/// if the file is already locked by someone else, `Err(...)` on real
/// I/O error. Callers own the polling loop + timeout + event emission.
///
/// Unix: `flock(fd, LOCK_EX|LOCK_NB)`. `EWOULDBLOCK` → `Ok(false)`.
/// Windows: `LockFileEx(handle, LOCKFILE_EXCLUSIVE_LOCK|LOCKFILE_FAIL_IMMEDIATELY, 0, u32::MAX, u32::MAX, &mut overlapped)`.
/// `ERROR_LOCK_VIOLATION` / `ERROR_IO_PENDING` → `Ok(false)`.
pub fn try_lock_once_nonblocking(file: &File) -> io::Result<bool> {
    #[cfg(unix)]
    {
        try_lock_once_unix(file)
    }
    #[cfg(windows)]
    {
        try_lock_once_windows(file)
    }
    #[cfg(not(any(unix, windows)))]
    {
        // Neither Unix nor Windows — return io error so callers
        // surface honest failure instead of silent-ok.
        let _ = file;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "platform::file_lock: unsupported host (neither unix nor windows)",
        ))
    }
}

/// Release the exclusive lock held on `file`. Called from `Drop` in
/// caller code so the release runs even on panic.
///
/// Unix: `flock(fd, LOCK_UN)`.
/// Windows: `UnlockFileEx(handle, 0, u32::MAX, u32::MAX, &mut overlapped)`.
///
/// Errors are best-effort: the OS will release the lock when the
/// file handle closes anyway. Returning `io::Result` gives tests a
/// hook.
pub fn unlock(file: &File) -> io::Result<()> {
    #[cfg(unix)]
    {
        unlock_unix(file)
    }
    #[cfg(windows)]
    {
        unlock_windows(file)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = file;
        Ok(())
    }
}

// ─────────────────────────────────────────────────────────────────────
// Unix impl (byte-preserving migration of the current inline flock
// code in `state/persist.rs` and `lifecycle/lock.rs`).
// ─────────────────────────────────────────────────────────────────────

#[cfg(unix)]
fn try_lock_once_unix(file: &File) -> io::Result<bool> {
    use std::os::unix::io::AsRawFd;
    // SAFETY: fd is owned by `file` for the duration of this call.
    // LOCK_EX | LOCK_NB matches the byte-for-byte behavior in
    // `state/persist.rs:172-173` and `lifecycle/lock.rs:108`.
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if rc == 0 {
        return Ok(true);
    }
    let err = io::Error::last_os_error();
    match err.raw_os_error() {
        Some(code) if code == libc::EWOULDBLOCK => Ok(false),
        Some(code) if code == libc::EAGAIN => Ok(false),
        _ => Err(err),
    }
}

#[cfg(unix)]
fn unlock_unix(file: &File) -> io::Result<()> {
    use std::os::unix::io::AsRawFd;
    // SAFETY: same fd as above; LOCK_UN releases our own lock.
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN) };
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

// ─────────────────────────────────────────────────────────────────────
// Windows impl (LockFileEx/UnlockFileEx, semantic 1:1 with flock).
// ─────────────────────────────────────────────────────────────────────

#[cfg(windows)]
fn try_lock_once_windows(file: &File) -> io::Result<bool> {
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::Foundation::{ERROR_IO_PENDING, ERROR_LOCK_VIOLATION, HANDLE};
    use windows::Win32::Storage::FileSystem::{
        LockFileEx, LOCKFILE_EXCLUSIVE_LOCK, LOCKFILE_FAIL_IMMEDIATELY,
    };
    use windows::Win32::System::IO::OVERLAPPED;
    let handle = HANDLE(file.as_raw_handle() as *mut _);
    // LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY:
    // exclusive lock, do not block if already held. Semantic 1:1 with
    // `flock(LOCK_EX | LOCK_NB)`.
    //
    // Range covers `[0, u32::MAX + u32::MAX<<32)` — the entire file
    // extent — the same coverage `flock` gives (whole-file advisory
    // lock).
    let mut overlapped: OVERLAPPED = unsafe { std::mem::zeroed() };
    let result = unsafe {
        LockFileEx(
            handle,
            LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY,
            Some(0),
            u32::MAX,
            u32::MAX,
            &mut overlapped,
        )
    };
    match result {
        Ok(()) => Ok(true),
        Err(err) => {
            let raw = err.code().0 as i32;
            let raw_u32 = raw as u32;
            if raw_u32 == ERROR_LOCK_VIOLATION.0 || raw_u32 == ERROR_IO_PENDING.0 {
                Ok(false)
            } else {
                Err(io::Error::from_raw_os_error(raw))
            }
        }
    }
}

#[cfg(windows)]
fn unlock_windows(file: &File) -> io::Result<()> {
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::Storage::FileSystem::UnlockFileEx;
    use windows::Win32::System::IO::OVERLAPPED;
    let handle = HANDLE(file.as_raw_handle() as *mut _);
    let mut overlapped: OVERLAPPED = unsafe { std::mem::zeroed() };
    let result = unsafe { UnlockFileEx(handle, Some(0), u32::MAX, u32::MAX, &mut overlapped) };
    result.map_err(|e| io::Error::from_raw_os_error(e.code().0 as i32))
}

// ─────────────────────────────────────────────────────────────────────
// High-level convenience wrapper.
// ─────────────────────────────────────────────────────────────────────

/// Owns the underlying `File` handle + platform lock. `Drop` unlocks
/// via `unlock(&file)`.
///
/// Callers that don't need the lifecycle-lock metadata/waiter/held_long
/// event machinery can use `try_lock_exclusive(path, timeout)` directly.
/// The existing product callers (`state/persist.rs`, `lifecycle/lock.rs`)
/// use `try_lock_once_nonblocking` + `unlock` directly and keep their
/// own polling loops for byte-preserving behavior.
pub struct FileLockGuard {
    file: Option<File>,
}

impl std::fmt::Debug for FileLockGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FileLockGuard")
            .field("held", &self.file.is_some())
            .finish()
    }
}

impl FileLockGuard {
    /// Test-only accessor for the wrapped file handle.
    #[cfg(test)]
    fn file(&self) -> &File {
        self.file.as_ref().expect("file present until Drop")
    }
}

impl Drop for FileLockGuard {
    fn drop(&mut self) {
        if let Some(file) = self.file.take() {
            // Best-effort unlock. If it fails the OS still releases on
            // handle close.
            let _ = unlock(&file);
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum LockError {
    #[error("file lock timeout after {timeout_secs:.2}s on {path}")]
    Timeout { timeout_secs: f64, path: String },
    #[error("file lock io error on {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: io::Error,
    },
}

/// High-level: acquire an exclusive lock on `path` within `timeout`,
/// polling every 50ms. Returns a guard that unlocks on `Drop`.
///
/// Existing product callers do NOT use this — they need their own
/// metadata/waiter/held_long event machinery. This wrapper exists for
/// simple future callers.
pub fn try_lock_exclusive(path: &Path, timeout: Duration) -> Result<FileLockGuard, LockError> {
    let file = File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .map_err(|e| LockError::Io {
            path: path.display().to_string(),
            source: e,
        })?;
    let start = std::time::Instant::now();
    loop {
        match try_lock_once_nonblocking(&file) {
            Ok(true) => return Ok(FileLockGuard { file: Some(file) }),
            Ok(false) => {}
            Err(e) => {
                return Err(LockError::Io {
                    path: path.display().to_string(),
                    source: e,
                });
            }
        }
        if start.elapsed() >= timeout {
            return Err(LockError::Timeout {
                timeout_secs: timeout.as_secs_f64(),
                path: path.display().to_string(),
            });
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

// ─────────────────────────────────────────────────────────────────────
// Tests: primitive + convenience wrapper on the host platform.
// Both branches (unix + windows) exercise the same trait shape via
// the top-level `try_lock_once_nonblocking` / `unlock` functions —
// the test source is cfg-free.
// ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn primitives_acquire_and_release_on_host_platform() {
        // Batch 2 anchor: the OS primitive must work on the CURRENT
        // host (macOS/Linux in dev, both macOS/Linux/Windows in CI).
        // This test compiles on both cfg branches and validates the
        // 1:1 shape mapping between `flock` and `LockFileEx`.
        let dir = std::env::temp_dir().join("ta-b2-primitives");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("primitives-lock.tmp");
        let file = File::options()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .unwrap();
        assert!(
            try_lock_once_nonblocking(&file).unwrap(),
            "fresh lock file should acquire on first attempt"
        );
        assert!(
            unlock(&file).is_ok(),
            "unlock must succeed for our own lock"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn convenience_try_lock_exclusive_acquires_and_drop_releases() {
        // The high-level convenience wrapper is what future callers
        // will use. Verify the drop-releases-lock invariant.
        let dir = std::env::temp_dir().join("ta-b2-convenience");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("convenience-lock.tmp");
        {
            let guard = try_lock_exclusive(&path, Duration::from_secs(1))
                .expect("first acquire must succeed");
            let _ = guard.file();
        }
        let guard2 = try_lock_exclusive(&path, Duration::from_secs(1))
            .expect("post-drop reacquire must succeed");
        drop(guard2);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn second_acquire_blocks_until_first_releases() {
        // C-6 N38 anchor: the primitive returns `Ok(false)` for the
        // "already held" case on BOTH unix and windows (mapped from
        // EWOULDBLOCK or ERROR_LOCK_VIOLATION respectively). Callers
        // use this to drive their polling loop; without the mapping
        // they would treat the transient contention as a hard error.
        let dir = std::env::temp_dir().join("ta-b2-contention");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("contention-lock.tmp");
        let file1 = File::options()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .unwrap();
        let file2 = File::options()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .unwrap();
        assert!(try_lock_once_nonblocking(&file1).unwrap());
        // A second attempt from a DIFFERENT file handle must see
        // Ok(false), not an error.
        assert!(
            !try_lock_once_nonblocking(&file2).unwrap(),
            "second-holder attempt must return Ok(false) (would-block), not error"
        );
        unlock(&file1).unwrap();
        // Now the second holder can acquire.
        assert!(
            try_lock_once_nonblocking(&file2).unwrap(),
            "post-unlock reacquire from second holder must succeed"
        );
        unlock(&file2).unwrap();
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn timeout_error_carries_path_and_seconds() {
        // Convenience wrapper's timeout error shape.
        //
        // 0.5.43 debt-sweep (§6.2): the pre-0.5.43 fixed
        // `ta-b2-timeout/timeout-lock.tmp` path let parallel test
        // workers race for the same file across cargo threads. Each
        // run now allocates a per-process + monotonic-atomic dir so
        // `--test-threads=2` cannot false-fail. Timeout shape and
        // `timeout-lock.tmp` basename are preserved so downstream
        // guards keep firing.
        static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "ta-b2-timeout-{}-{}",
            std::process::id(),
            N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("timeout-lock.tmp");
        let _hold =
            try_lock_exclusive(&path, Duration::from_secs(1)).expect("first acquire must succeed");
        let err = try_lock_exclusive(&path, Duration::from_millis(150))
            .expect_err("second acquire must timeout");
        match err {
            LockError::Timeout {
                timeout_secs,
                path: p,
            } => {
                assert!(timeout_secs > 0.0);
                assert!(p.ends_with("timeout-lock.tmp"), "path suffix: {p}");
            }
            other => panic!("expected Timeout, got {other:?}"),
        }
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
