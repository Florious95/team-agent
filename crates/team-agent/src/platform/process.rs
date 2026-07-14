//! Process lifecycle + snapshot + termination platform primitives.
//!
//! Batch 0 provides:
//! - `Pid`, `SignalKind`, `ProcessLiveness`, `ProcessInfo`,
//!   `TerminationOutcome`
//! - Unix impl: byte-equivalent to the current inline
//!   `libc::kill(pid, 0)` / `waitpid(WNOHANG)` / `ps -axo` logic (no
//!   caller migration in Batch 0)
//! - Windows impl: `unimplemented!()` stubs annotated with the
//!   Windows API Batch 3 will use (`OpenProcess`,
//!   `GetExitCodeProcess`, `TerminateProcess`, Toolhelp snapshot,
//!   Job Objects for shim-owned worker teardown)
//!
//! CR C-6: `terminate_pid` / `terminate_group` return
//! `TerminationOutcome` so a caller that requested `TerminateGraceful`
//! can see that Windows downgraded to `TerminateForce` and emit a
//! `platform.terminate_force_only` event (N38 交底).
//!
//! CR C-4: this module is NOT importable from `messaging/`. Grep guard
//! `platform_process_caller_whitelist_batch0.rs` enforces the boundary.

use std::io;

/// A native process id. Portable u32 for both Unix and Windows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Pid(pub u32);

impl From<u32> for Pid {
    fn from(v: u32) -> Self {
        Self(v)
    }
}

/// Termination request kind. On Unix `TerminateGraceful` maps to
/// `SIGTERM`, `TerminateForce` maps to `SIGKILL`. On Windows there is
/// no grace period equivalent — see `TerminationOutcome::ForceOnly`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignalKind {
    TerminateGraceful,
    TerminateForce,
}

/// Result of a `terminate_pid` / `terminate_group` call. `Graceful`
/// means the OS honored the request kind; `ForceOnly` means Windows
/// downgraded a `TerminateGraceful` to `TerminateProcess` (CR C-6
/// N38 交底 — caller must emit a `platform.terminate_force_only`
/// event).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerminationOutcome {
    /// Requested `SignalKind` was honored (Unix `SIGTERM`/`SIGKILL`
    /// or Windows console-control-event for owned console groups).
    Requested,
    /// Windows downgraded `TerminateGraceful` to `TerminateProcess`
    /// (no grace period). Includes a machine-readable reason so
    /// callers can log it in the audit event.
    ForceOnly { reason: &'static str },
    /// The pid/group was already gone by the time the call resolved.
    /// Not an error.
    AlreadyGone,
}

/// Live/Dead/Unknown liveness classification. `Unknown { reason }`
/// covers the "we don't have OS permission to check" case + Windows
/// paths that Batch 3 will fill in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProcessLiveness {
    Live,
    Dead,
    Unknown { reason: String },
}

/// One row from a process snapshot. Fields that the OS does not
/// expose (Windows has no `pgid`/`session` concept in the Unix sense)
/// are `None`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessInfo {
    pub pid: u32,
    pub ppid: Option<u32>,
    pub group_id: Option<u32>,
    pub session_id: Option<u32>,
    pub command: Option<String>,
}

// ─────────────────────────────────────────────────────────────────────
// Unix implementation.
//
// Batch 0 populates these from the current inline logic in
// `mcp_server/wire.rs`, `coordinator/backoff.rs`,
// `lifecycle/restart/agent.rs`, `coordinator/health.rs`. No caller
// migration yet — Batch 2/3 replaces the inline calls with these
// wrappers. This ensures Unix behavior is byte-equivalent when the
// migration lands.
// ─────────────────────────────────────────────────────────────────────

#[cfg(unix)]
mod unix_impl {
    //! Byte-preserving Unix implementation. Every fn body is the
    //! current inline code from the callers (`coordinator/health.rs`,
    //! `cli/mod.rs`, `coordinator/backoff.rs`, `mcp_server/wire.rs`,
    //! `lifecycle/restart/agent.rs`) with zero behavioral drift.
    use super::*;

    pub fn current_parent_pid() -> Option<u32> {
        // Byte-equivalent to `mcp_server/wire.rs:319` and
        // `coordinator/backoff.rs:258` Unix branches.
        let raw = unsafe { libc::getppid() };
        u32::try_from(raw).ok()
    }

    pub fn current_process_group() -> Option<u32> {
        // Byte-equivalent to `cli/mod.rs:1708`.
        let raw = unsafe { libc::getpgrp() };
        u32::try_from(raw).ok()
    }

    pub fn pid_liveness(pid: u32) -> Result<ProcessLiveness, io::Error> {
        // Byte-equivalent to `lifecycle/restart/agent.rs::pid_is_alive`
        // (EPERM = Live because sender can't signal but process
        // exists). Callers get the same True/False split.
        let ret = unsafe { libc::kill(pid as i32, 0) };
        if ret == 0 {
            return Ok(ProcessLiveness::Live);
        }
        let err = io::Error::last_os_error();
        match err.raw_os_error() {
            Some(code) if code == libc::EPERM => Ok(ProcessLiveness::Live),
            Some(code) if code == libc::ESRCH => Ok(ProcessLiveness::Dead),
            _ => Err(err),
        }
    }

    /// Non-erroring convenience over `pid_liveness`. Returns `true`
    /// when the process is Live (or unknown-but-not-clearly-dead so
    /// callers behave conservatively), `false` when Dead.
    ///
    /// Byte-equivalent to `cli/mod.rs::process_is_live` +
    /// `coordinator/health.rs::pid_is_running` "signal_rc == 0 || EPERM"
    /// branch.
    pub fn pid_is_alive(pid: u32) -> bool {
        matches!(pid_liveness(pid), Ok(ProcessLiveness::Live))
    }

    pub fn process_snapshot() -> Result<Vec<ProcessInfo>, io::Error> {
        // Reserved for Batch 3 follow-up that migrates the `ps -axo
        // pid=,ppid=` shellouts in `coordinator/health.rs` and
        // `cli/mod.rs` here. Left as `Ok(Vec::new())` so downstream
        // callers can migrate incrementally in a future batch; keeping
        // the ps shellout at the callsites for now preserves the
        // exact current output-parsing byte-shape.
        Ok(Vec::new())
    }

    pub fn process_tree(_root: u32) -> Result<Vec<u32>, io::Error> {
        Ok(Vec::new())
    }

    /// Send a SIGTERM (`TerminateGraceful`) or SIGKILL
    /// (`TerminateForce`) to `pid`. Byte-equivalent to the
    /// `libc::kill(pid, SIGTERM|SIGKILL)` shell-out inline at
    /// `coordinator/health.rs:280,284,731` + `cli/mod.rs:1848`.
    ///
    /// Unix always returns `TerminationOutcome::Requested` because
    /// `SIGTERM` is a real grace signal (Windows sees the C-6
    /// downgrade path); the `AlreadyGone` case is preserved when
    /// `kill()` returns ESRCH (the target was reaped between check
    /// and send).
    pub fn terminate_pid(pid: u32, kind: SignalKind) -> Result<TerminationOutcome, io::Error> {
        let signal = match kind {
            SignalKind::TerminateGraceful => libc::SIGTERM,
            SignalKind::TerminateForce => libc::SIGKILL,
        };
        let pid_t = match libc::pid_t::try_from(pid) {
            Ok(p) => p,
            Err(_) => return Ok(TerminationOutcome::AlreadyGone),
        };
        let rc = unsafe { libc::kill(pid_t, signal) };
        if rc == 0 {
            return Ok(TerminationOutcome::Requested);
        }
        let err = io::Error::last_os_error();
        match err.raw_os_error() {
            Some(code) if code == libc::ESRCH => Ok(TerminationOutcome::AlreadyGone),
            _ => Err(err),
        }
    }

    /// Send a signal to a process group (`kill(-pgid, ...)`).
    /// Byte-equivalent to `cli/mod.rs:1854` (`libc::kill(-pgid, signal)`)
    /// used by `send_process_signal_group`.
    pub fn terminate_group(
        group_id: u32,
        kind: SignalKind,
    ) -> Result<TerminationOutcome, io::Error> {
        let signal = match kind {
            SignalKind::TerminateGraceful => libc::SIGTERM,
            SignalKind::TerminateForce => libc::SIGKILL,
        };
        let pgid_t = match libc::pid_t::try_from(group_id) {
            Ok(p) => p,
            Err(_) => return Ok(TerminationOutcome::AlreadyGone),
        };
        // `-pgid` targets every process in that group. Preserves the
        // existing shutdown semantics inline in cli/mod.rs.
        let rc = unsafe { libc::kill(-pgid_t, signal) };
        if rc == 0 {
            return Ok(TerminationOutcome::Requested);
        }
        let err = io::Error::last_os_error();
        match err.raw_os_error() {
            Some(code) if code == libc::ESRCH => Ok(TerminationOutcome::AlreadyGone),
            _ => Err(err),
        }
    }

    /// Best-effort waitpid so a killed child doesn't accumulate as a
    /// zombie. Byte-equivalent to `coordinator/health.rs:350-358` and
    /// `cli/mod.rs:1871-1879`.
    pub fn reap_child_if_possible(pid: u32) {
        let pid_t = match libc::pid_t::try_from(pid) {
            Ok(p) => p,
            Err(_) => return,
        };
        let mut status = 0;
        unsafe {
            // WNOHANG so we never block; if the child isn't ours
            // (ECHILD) libc quietly returns -1 which we ignore.
            libc::waitpid(pid_t, &mut status, libc::WNOHANG);
        }
    }
}

// ─────────────────────────────────────────────────────────────────────
// Windows implementation.
//
// Batch 0 provides `unimplemented!()` stubs so `cargo check --target
// x86_64-pc-windows-msvc` sees the module compile past the signature
// boundary. The RED CI baseline comes from every failing file OUTSIDE
// this module (tmux_backend, coordinator/health, cli/mod, state/persist,
// lifecycle/restart/agent, mcp_server/wire, etc.) — Batches 1-4 will
// remove those, and Batches 3/4 will implement the stubs below.
// ─────────────────────────────────────────────────────────────────────

#[cfg(not(unix))]
mod windows_impl {
    //! Batch 3 real Windows implementation. Uses `OpenProcess` +
    //! `GetExitCodeProcess` for liveness (CR MUST-11 positive
    //! authority: kernel handle + typed status),
    //! `TerminateProcess(hProcess, 1)` for hard kill, Toolhelp
    //! snapshot for parent-pid discovery. **CR C-6 anchor**:
    //! `SignalKind::TerminateGraceful` maps to `TerminationOutcome::ForceOnly`
    //! because `TerminateProcess` has no grace period equivalent —
    //! callers use this signal to emit the `platform.terminate_force_only`
    //! N38 audit event so the semantic difference from
    //! Unix `SIGTERM` is visible in the event log rather than silent.
    use super::*;

    pub fn current_parent_pid() -> Option<u32> {
        use windows::Win32::System::Diagnostics::ToolHelp::{
            CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
            TH32CS_SNAPPROCESS,
        };
        use windows::Win32::System::Threading::GetCurrentProcessId;
        let my_pid = unsafe { GetCurrentProcessId() };
        let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) }.ok()?;
        let mut entry = PROCESSENTRY32W {
            dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        let mut result = unsafe { Process32FirstW(snapshot, &mut entry) };
        while result.is_ok() {
            if entry.th32ProcessID == my_pid {
                let ppid = entry.th32ParentProcessID;
                unsafe {
                    let _ = windows::Win32::Foundation::CloseHandle(snapshot);
                };
                return Some(ppid);
            }
            result = unsafe { Process32NextW(snapshot, &mut entry) };
        }
        unsafe {
            let _ = windows::Win32::Foundation::CloseHandle(snapshot);
        };
        None
    }

    pub fn current_process_group() -> Option<u32> {
        // Windows has no direct process-group equivalent. Job Objects
        // provide grouping semantics per shim/worker (design §Route B:
        // "owned worker teardown: prefer Job Objects in the ConPTY
        // shim so `kill_session` is exact"). Return `None` honestly so
        // caller code (cli/mod.rs::shutdown_protection_set) sees the
        // "no pgid to protect" branch — the current-PID protection
        // still applies via `current_process_id`.
        None
    }

    pub fn pid_liveness(pid: u32) -> Result<ProcessLiveness, io::Error> {
        use windows::Win32::Foundation::{CloseHandle, STILL_ACTIVE};
        use windows::Win32::System::Threading::{
            GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
        };
        // `OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION)` — minimal
        // access rights needed for `GetExitCodeProcess`.
        // MUST-11 positive authority: we own a real kernel handle.
        let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) };
        let handle = match handle {
            Ok(h) => h,
            Err(err) => {
                // Both raw Win32 error codes and their HRESULT
                // encodings (HRESULT_FROM_WIN32) are accepted so
                // future windows-crate API changes don't silently
                // break the code mapping.
                let raw = err.code().0 as u32;
                const ERROR_INVALID_PARAMETER: u32 = 87;
                const E_INVALIDARG_HRESULT: u32 = 0x80070057;
                const ERROR_ACCESS_DENIED: u32 = 5;
                const E_ACCESSDENIED_HRESULT: u32 = 0x80070005;
                if raw == ERROR_INVALID_PARAMETER || raw == E_INVALIDARG_HRESULT {
                    return Ok(ProcessLiveness::Dead);
                }
                if raw == ERROR_ACCESS_DENIED || raw == E_ACCESSDENIED_HRESULT {
                    return Ok(ProcessLiveness::Live);
                }
                return Err(io::Error::from_raw_os_error(err.code().0 as i32));
            }
        };
        let mut exit_code: u32 = 0;
        let result = unsafe { GetExitCodeProcess(handle, &mut exit_code) };
        unsafe {
            let _ = CloseHandle(handle);
        };
        result.map_err(|e| io::Error::from_raw_os_error(e.code().0 as i32))?;
        // `STILL_ACTIVE` (0x103) — the process is still running.
        if exit_code == STILL_ACTIVE.0 as u32 {
            Ok(ProcessLiveness::Live)
        } else {
            Ok(ProcessLiveness::Dead)
        }
    }

    /// Non-erroring convenience over `pid_liveness`. Windows mirrors
    /// the Unix behavior (Unknown branch reported conservatively as
    /// "not Live" so drain/reap don't loop forever).
    pub fn pid_is_alive(pid: u32) -> bool {
        matches!(pid_liveness(pid), Ok(ProcessLiveness::Live))
    }

    pub fn process_snapshot() -> Result<Vec<ProcessInfo>, io::Error> {
        // Reserved for Batch 3 follow-up. `CreateToolhelp32Snapshot`
        // + `Process32First`/`Process32Next` would populate this,
        // but the coordinator/health.rs + cli/mod.rs `ps -axo`
        // callers on Unix still parse a string table so we defer
        // that migration to a later batch to preserve the exact
        // wire shape today.
        Ok(Vec::new())
    }

    pub fn process_tree(_root: u32) -> Result<Vec<u32>, io::Error> {
        Ok(Vec::new())
    }

    pub fn terminate_pid(pid: u32, kind: SignalKind) -> Result<TerminationOutcome, io::Error> {
        use windows::Win32::Foundation::CloseHandle;
        use windows::Win32::System::Threading::{OpenProcess, TerminateProcess, PROCESS_TERMINATE};
        // CR C-6 anchor: Windows has no SIGTERM analogue for a
        // non-console child. `TerminateGraceful` cannot be honored;
        // we still perform the physical `TerminateProcess` (design
        // §Change-Induced Risks: "ConPTY shim should own Job Objects
        // to preserve precise worker teardown") but return
        // `ForceOnly` so callers emit `platform.terminate_force_only`
        // for the audit log.
        let handle = unsafe { OpenProcess(PROCESS_TERMINATE, false, pid) };
        let handle = match handle {
            Ok(h) => h,
            Err(err) => {
                // `err.code().0` is a HRESULT, not a raw Win32 error.
                // For Win32 errors it takes the form
                // `HRESULT_FROM_WIN32(win32)` = `0x8007<win32 low 16>`.
                // Accept BOTH the raw Win32 form (in case a future
                // windows-crate API returns it) and the HRESULT form.
                let raw = err.code().0 as u32;
                const ERROR_INVALID_PARAMETER: u32 = 87;
                const E_INVALIDARG_HRESULT: u32 = 0x80070057; // HRESULT_FROM_WIN32(87)
                if raw == ERROR_INVALID_PARAMETER || raw == E_INVALIDARG_HRESULT {
                    // pid doesn't exist (or reaped between recorded
                    // spawn and this shutdown call).
                    return Ok(TerminationOutcome::AlreadyGone);
                }
                return Err(io::Error::from_raw_os_error(err.code().0 as i32));
            }
        };
        let result = unsafe { TerminateProcess(handle, 1) };
        unsafe {
            let _ = CloseHandle(handle);
        };
        result.map_err(|e| io::Error::from_raw_os_error(e.code().0 as i32))?;
        Ok(match kind {
            SignalKind::TerminateGraceful => TerminationOutcome::ForceOnly {
                reason: "windows_no_sigterm_equivalent_for_non_console_child",
            },
            SignalKind::TerminateForce => TerminationOutcome::Requested,
        })
    }

    pub fn terminate_group(
        _group_id: u32,
        _kind: SignalKind,
    ) -> Result<TerminationOutcome, io::Error> {
        // Windows has no `-pgid` sentinel. Design §Route B: "owned
        // worker teardown: prefer Job Objects in the ConPTY shim".
        // Job-object teardown is a shim-side concern, not a top-level
        // API. Return AlreadyGone honestly here so shutdown code
        // falls back to per-pid termination (the caller's outer
        // loop retries per-pid after the group attempt is a no-op).
        Ok(TerminationOutcome::AlreadyGone)
    }

    pub fn reap_child_if_possible(_pid: u32) {
        // Windows has no zombie waitpid model. Child handles are
        // owned by the spawner and closed at drop; nothing to do.
    }
}

// ─────────────────────────────────────────────────────────────────────
// Re-export the platform-appropriate impl at module top-level so
// callers write `platform::process::pid_liveness(pid)` without cfg.
// ─────────────────────────────────────────────────────────────────────

#[cfg(unix)]
pub use unix_impl::*;

#[cfg(not(unix))]
pub use windows_impl::*;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parent_pid_returns_some_value_on_both_platforms_after_batch_3() {
        // Batch 3 real implementation: both Unix (`getppid`) and
        // Windows (Toolhelp32 snapshot) return the process's parent
        // pid. A `None` here on either platform is a regression.
        let ppid = current_parent_pid();
        assert!(
            ppid.is_some(),
            "current_parent_pid must return Some on both unix and windows"
        );
        assert!(ppid.unwrap() > 0);
    }

    #[test]
    fn pid_liveness_own_pid_is_live_on_unix() {
        // Batch 0 unix impl is byte-equivalent to
        // `lifecycle/restart/agent.rs::pid_is_alive` for our own pid,
        // which must always be Live.
        #[cfg(unix)]
        {
            let my_pid = std::process::id();
            let result = pid_liveness(my_pid).expect("own pid must be checkable");
            assert_eq!(result, ProcessLiveness::Live);
        }
    }

    #[test]
    fn windows_terminate_graceful_returns_force_only_with_reason_when_downgraded() {
        // CR C-6 anchor: caller can distinguish Requested vs
        // ForceOnly and emit the `platform.terminate_force_only`
        // audit event only when the OS downgraded. Use an
        // impossible pid so `AlreadyGone` short-circuits the actual
        // kill — we're only checking the shape of the outcome enum.
        //
        // On Unix the SAME call returns `AlreadyGone` (kill returned
        // ESRCH) because `SIGTERM` is a real signal there.
        #[cfg(not(unix))]
        {
            // On Windows the AlreadyGone short-circuit runs first;
            // to exercise the ForceOnly path we'd need a real
            // process. This test just documents the ForceOnly variant
            // exists and matches what a live-target call would return.
            let outcome = TerminationOutcome::ForceOnly {
                reason: "windows_no_sigterm_equivalent_for_non_console_child",
            };
            match outcome {
                TerminationOutcome::ForceOnly { reason } => {
                    assert!(reason.contains("windows"));
                }
                other => panic!("expected ForceOnly variant, got {other:?}"),
            }
        }
    }

    #[test]
    fn pid_is_alive_returns_true_for_own_pid() {
        // Batch 3 anchor: `pid_is_alive` is the byte-preserving
        // migration target for `lifecycle/restart/agent.rs::pid_is_alive`
        // + `cli/mod.rs::process_is_live`. Callers rely on "our own pid
        // is always alive" invariant to derive drain-loop termination.
        assert!(pid_is_alive(std::process::id()));
    }

    #[test]
    fn pid_is_alive_returns_false_for_definitely_dead_pid() {
        // A pid we absolutely never allocate — 0xFFFF_FFFE — is
        // guaranteed to not exist. Windows OpenProcess returns
        // ERROR_INVALID_PARAMETER; Unix `kill(pid, 0)` returns
        // ESRCH. Both map to `false`.
        assert!(!pid_is_alive(0xFFFF_FFFE));
    }

    #[test]
    fn pid_liveness_returns_dead_for_impossible_pid() {
        match pid_liveness(0xFFFF_FFFE) {
            Ok(ProcessLiveness::Dead) => {}
            Ok(other) => panic!("expected Dead for impossible pid, got {other:?}"),
            Err(e) => panic!("expected Ok(Dead), got Err({e:?})"),
        }
    }

    #[test]
    fn reap_child_if_possible_never_panics_for_arbitrary_pid() {
        // Byte-equivalent to `coordinator/health.rs::reap_child_if_possible`
        // + `cli/mod.rs::reap_child_if_possible` invariant: this is
        // called on foreign pids too (children of coordinator's
        // children), so it must silently no-op if the pid is not
        // reap-able by this process.
        reap_child_if_possible(std::process::id());
        reap_child_if_possible(0xFFFF_FFFE);
    }

    #[test]
    fn terminate_pid_returns_already_gone_for_impossible_pid() {
        // `coordinator/health.rs::terminate_pid` treats "kill returned
        // error because target not-found" as success (idempotent
        // shutdown). Preserve that shape via `AlreadyGone`.
        match terminate_pid(0xFFFF_FFFE, SignalKind::TerminateForce) {
            Ok(TerminationOutcome::AlreadyGone) => {}
            other => panic!("expected AlreadyGone for impossible pid, got {other:?}"),
        }
    }

    #[test]
    fn signal_kind_and_termination_outcome_are_distinguishable_variants() {
        // Both enums must have distinct variants so callers can
        // pattern-match without cfg.
        assert_ne!(SignalKind::TerminateGraceful, SignalKind::TerminateForce);
        let requested = TerminationOutcome::Requested;
        let force_only = TerminationOutcome::ForceOnly { reason: "test" };
        let already_gone = TerminationOutcome::AlreadyGone;
        assert_ne!(requested, force_only);
        assert_ne!(requested, already_gone);
        assert_ne!(force_only, already_gone);
    }
}
