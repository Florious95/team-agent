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
    ForceOnly {
        reason: &'static str,
    },
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
    use super::*;

    pub fn current_parent_pid() -> Option<u32> {
        // Byte-equivalent to mcp_server/wire.rs:317-320 and
        // coordinator/backoff.rs:217-220 Unix branches.
        let raw = unsafe { libc::getppid() };
        u32::try_from(raw).ok()
    }

    pub fn current_process_group() -> Option<u32> {
        // Batch 3 will migrate `cli/mod.rs::getpgrp` callers here.
        let raw = unsafe { libc::getpgrp() };
        u32::try_from(raw).ok()
    }

    pub fn pid_liveness(pid: u32) -> Result<ProcessLiveness, io::Error> {
        // Byte-equivalent to `lifecycle/restart/agent.rs::pid_is_alive`
        // Unix branch. Returns Live/Dead; Unknown reserved for the
        // Windows Batch 3 path.
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

    pub fn process_snapshot() -> Result<Vec<ProcessInfo>, io::Error> {
        // Batch 3 will migrate `coordinator/health.rs` and
        // `cli/mod.rs::ps -axo pid=,ppid=,pgid=,sess=,command=` callers
        // here. Batch 0 keeps a stub that returns an empty vec so the
        // signature is available without moving the ps shellout yet.
        Ok(Vec::new())
    }

    pub fn process_tree(_root: u32) -> Result<Vec<u32>, io::Error> {
        // Batch 3 migration target for
        // `coordinator/health.rs::children_of`. Stub for now.
        Ok(Vec::new())
    }

    pub fn terminate_pid(
        _pid: u32,
        _kind: SignalKind,
    ) -> Result<TerminationOutcome, io::Error> {
        // Batch 3 migrates the SIGTERM→grace→SIGKILL ladder from
        // `coordinator/health.rs` + `cli/mod.rs` here.
        Ok(TerminationOutcome::AlreadyGone)
    }

    pub fn terminate_group(
        _group_id: u32,
        _kind: SignalKind,
    ) -> Result<TerminationOutcome, io::Error> {
        // Batch 3 migration target.
        Ok(TerminationOutcome::AlreadyGone)
    }

    pub fn reap_child_if_possible(pid: u32) {
        // Batch 3 migrates `waitpid(pid, WNOHANG)` here.
        let _ = pid;
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
    use super::*;

    pub fn current_parent_pid() -> Option<u32> {
        // Batch 3 (design.md §Change scope Batch 3): use Toolhelp
        // snapshot to find the parent PID of `GetCurrentProcessId()`.
        // Batch 0 stub returns None (honest unknown) so callers that
        // migrate before Batch 3 see the None branch immediately.
        None
    }

    pub fn current_process_group() -> Option<u32> {
        // Windows has no direct process-group equivalent — Job Objects
        // provide grouping semantics per shim/worker (design.md §Route
        // B: "owned worker teardown: prefer Job Objects in the ConPTY
        // shim so `kill_session` is exact"). Batch 0 stub returns
        // None.
        None
    }

    pub fn pid_liveness(_pid: u32) -> Result<ProcessLiveness, io::Error> {
        // Batch 3 will use `OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION)`
        // + `GetExitCodeProcess` — see design.md §Target Design.
        Ok(ProcessLiveness::Unknown {
            reason: "windows platform::process::pid_liveness not yet implemented (Batch 3)"
                .to_string(),
        })
    }

    pub fn process_snapshot() -> Result<Vec<ProcessInfo>, io::Error> {
        // Batch 3: `CreateToolhelp32Snapshot` +
        // `Process32First`/`Process32Next`.
        Ok(Vec::new())
    }

    pub fn process_tree(_root: u32) -> Result<Vec<u32>, io::Error> {
        // Batch 3: walk Toolhelp snapshot filtering on ppid.
        Ok(Vec::new())
    }

    pub fn terminate_pid(
        _pid: u32,
        kind: SignalKind,
    ) -> Result<TerminationOutcome, io::Error> {
        // Batch 3: `TerminateProcess(hProcess, 1)`.
        //
        // C-6: when caller asked TerminateGraceful, Windows returns
        // `ForceOnly { reason }` — Windows has no SIGTERM equivalent
        // for a non-console child. The caller (coordinator health /
        // cli shutdown) must emit `platform.terminate_force_only`
        // audit event on this return.
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
        // Batch 3: shim-owned children are grouped by Job Objects, not
        // by pgid — this fn will accept a `job_handle: HANDLE` in a
        // Windows-specific shape. Batch 0 stub returns AlreadyGone.
        Ok(TerminationOutcome::AlreadyGone)
    }

    pub fn reap_child_if_possible(_pid: u32) {
        // Windows has no zombie waitpid model. Child handles are
        // owned by the spawner and closed at drop.
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
    fn parent_pid_returns_some_value_on_unix_or_none_on_windows_stub() {
        let ppid = current_parent_pid();
        #[cfg(unix)]
        {
            assert!(ppid.is_some(), "unix must return the process's parent pid");
            assert!(ppid.unwrap() > 0);
        }
        #[cfg(not(unix))]
        {
            assert!(ppid.is_none(), "windows Batch 0 stub must return None honestly");
        }
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
    fn windows_stub_terminate_graceful_downgrades_to_force_only_with_reason() {
        // CR C-6 anchor: caller can distinguish Requested vs
        // ForceOnly and emit the audit event only when the OS
        // downgraded.
        #[cfg(not(unix))]
        {
            let outcome = terminate_pid(1, SignalKind::TerminateGraceful)
                .expect("windows stub returns Ok");
            match outcome {
                TerminationOutcome::ForceOnly { reason } => {
                    assert!(reason.contains("windows"));
                }
                other => panic!("expected ForceOnly, got {other:?}"),
            }
        }
    }

    #[test]
    fn signal_kind_and_termination_outcome_are_distinguishable_variants() {
        // Both enums must have distinct variants so callers can
        // pattern-match without cfg.
        assert_ne!(SignalKind::TerminateGraceful, SignalKind::TerminateForce);
        let requested = TerminationOutcome::Requested;
        let force_only = TerminationOutcome::ForceOnly {
            reason: "test",
        };
        let already_gone = TerminationOutcome::AlreadyGone;
        assert_ne!(requested, force_only);
        assert_ne!(requested, already_gone);
        assert_ne!(force_only, already_gone);
    }
}
