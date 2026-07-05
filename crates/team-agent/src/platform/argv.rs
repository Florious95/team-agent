//! Process argv / environ probe.
//!
//! ## Batch 4 real implementation (leader msg_0689a63a9e40)
//!
//! Batch 4 promotes this module from a Batch 0 signature-only
//! scaffold to a **byte-preserving migration** of the process ancestry
//! + argv/env probes used by:
//!
//! - `lifecycle/launch.rs::process_ancestry_argv` +
//!   `process_argv_tokens` + `process_parent_pid` — the
//!   `dangerous_auto_approve` inheritance chain (0.5.0 caller-identity
//!   钉输入 is exactly this test surface).
//! - `leader/provider_attribution.rs::process_command_line` +
//!   `process_environment` — leader provider attribution.
//!
//! ## Platform matrix
//!
//! - **Linux**: `/proc/<pid>/cmdline` (NUL-separated argv) and
//!   `/proc/<pid>/environ` (NUL-separated KEY=VALUE).
//! - **macOS**: `sysctl(KERN_PROCARGS2)` for argv; environ not
//!   currently probed (matches the pre-batch behavior).
//! - **non-Linux non-macOS Unix**: `ps -p <pid> -o command=` for argv.
//! - **Windows**: `NtQueryInformationProcess` + PEB is intrusive; for
//!   Batch 4 we return `None` for both `argv_tokens` and
//!   `environ_text` (design §Batch 4 Verification anchor: "unknown
//!   argv must never infer elevated approval; keep worker permission
//!   at provider default or require explicit user consent"). This is
//!   the honest "we don't know" branch — callers already treat
//!   `None` as "no elevation inherited", so Windows leaders default
//!   to safe (non-dangerous) approval mode.
//!
//! ## CR C-3 anchor
//!
//! The `Option::None` return on Windows deliberately keeps worker
//! permission at provider default. Design §Batch 4 Verification: "the
//! safe direction is to avoid elevation, not to assume bypass" —
//! `detect_dangerous_approval` in `lifecycle/launch.rs` already
//! iterates `process_ancestry_argv(...)` and defaults to
//! `disabled_dangerous_approval()` when it finds no matching flag,
//! so a Windows probe returning `None` on every step lands on the
//! same disabled default. **This is intentional**: leaking a
//! bypass via unknown argv would be a MUST-NOT-13 假绿 failure.

/// Return the argv tokens for `pid`, or `None` if unavailable on
/// this platform.
///
/// - Linux: `/proc/<pid>/cmdline` NUL-separated (byte-preserving
///   migration of `lifecycle/launch.rs:3648-3656`).
/// - macOS: `sysctl(KERN_PROCARGS2)` (migration of
///   `lifecycle/launch.rs:3659-3711`).
/// - Other Unix: `ps -p <pid> -o command=` split on whitespace
///   (migration of `lifecycle/launch.rs:3714-3729`).
/// - Windows: `None` (design §Batch 4 conservative fallback — never
///   infer elevated approval from unknown argv).
pub fn argv_tokens(pid: u32) -> Option<Vec<String>> {
    #[cfg(target_os = "linux")]
    {
        argv_tokens_linux(pid)
    }
    #[cfg(target_os = "macos")]
    {
        argv_tokens_macos(pid)
    }
    #[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
    {
        argv_tokens_ps_fallback(pid)
    }
    #[cfg(windows)]
    {
        let _ = pid;
        None
    }
}

/// Return the environ text (NUL-joined) for `pid`, or `None`.
///
/// - Linux: `/proc/<pid>/environ` verbatim (migration of
///   `leader/provider_attribution.rs:113-115`).
/// - Other platforms: `None` (matches the pre-batch
///   `#[cfg(not(target_os="linux"))]` stub in
///   `leader/provider_attribution.rs:135-139`).
pub fn environ_text(pid: u32) -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        environ_text_linux(pid)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = pid;
        None
    }
}

/// Return the parent PID of `pid`, or `None` if unavailable.
///
/// Byte-preserving migration of `lifecycle/launch.rs::process_parent_pid`:
/// - Unix: `ps -p <pid> -o ppid=` parsed as u32.
/// - Windows: Toolhelp32 snapshot walked for the entry matching
///   `pid`. Same technique as `platform::process::current_parent_pid`.
pub fn parent_pid(pid: u32) -> Option<u32> {
    #[cfg(unix)]
    {
        parent_pid_unix(pid)
    }
    #[cfg(windows)]
    {
        parent_pid_windows(pid)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = pid;
        None
    }
}

// ─────────────────────────────────────────────────────────────────────
// Linux implementation.
// ─────────────────────────────────────────────────────────────────────

#[cfg(target_os = "linux")]
fn argv_tokens_linux(pid: u32) -> Option<Vec<String>> {
    let bytes = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
    let argv_tokens = String::from_utf8_lossy(&bytes)
        .split('\0')
        .filter(|token| !token.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();
    (!argv_tokens.is_empty()).then_some(argv_tokens)
}

#[cfg(target_os = "linux")]
fn environ_text_linux(pid: u32) -> Option<String> {
    String::from_utf8(std::fs::read(format!("/proc/{pid}/environ")).ok()?).ok()
}

// ─────────────────────────────────────────────────────────────────────
// macOS implementation.
// ─────────────────────────────────────────────────────────────────────

#[cfg(target_os = "macos")]
fn argv_tokens_macos(pid: u32) -> Option<Vec<String>> {
    use std::mem::size_of;
    let mut mib = [
        libc::CTL_KERN,
        libc::KERN_PROCARGS2,
        i32::try_from(pid).ok()?,
    ];
    let mut size = 0usize;
    let rc = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            mib.len() as u32,
            std::ptr::null_mut(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 || size <= size_of::<libc::c_int>() {
        return None;
    }
    let mut buf = vec![0u8; size];
    let rc = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            mib.len() as u32,
            buf.as_mut_ptr().cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 || size <= size_of::<libc::c_int>() {
        return None;
    }
    let argc = i32::from_ne_bytes(buf.get(..size_of::<libc::c_int>())?.try_into().ok()?) as usize;
    let mut offset = size_of::<libc::c_int>();
    while offset < size && buf[offset] != 0 {
        offset += 1;
    }
    while offset < size && buf[offset] == 0 {
        offset += 1;
    }
    let raw = String::from_utf8_lossy(&buf[offset..size]);
    let argv_tokens = raw
        .split('\0')
        .filter(|token| !token.is_empty())
        .take(argc)
        .map(str::to_string)
        .collect::<Vec<_>>();
    (!argv_tokens.is_empty()).then_some(argv_tokens)
}

// ─────────────────────────────────────────────────────────────────────
// Generic Unix fallback (`ps -p <pid> -o ...`).
// ─────────────────────────────────────────────────────────────────────

#[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
fn argv_tokens_ps_fallback(pid: u32) -> Option<Vec<String>> {
    let output = std::process::Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "command="])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let argv_tokens = text
        .split_whitespace()
        .filter(|token| !token.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();
    (!argv_tokens.is_empty()).then_some(argv_tokens)
}

#[cfg(unix)]
fn parent_pid_unix(pid: u32) -> Option<u32> {
    let output = std::process::Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "ppid="])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse::<u32>()
        .ok()
}

// ─────────────────────────────────────────────────────────────────────
// Windows implementation.
// ─────────────────────────────────────────────────────────────────────

#[cfg(windows)]
fn parent_pid_windows(pid: u32) -> Option<u32> {
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) }.ok()?;
    let mut entry = PROCESSENTRY32W {
        dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
        ..Default::default()
    };
    let mut result = unsafe { Process32FirstW(snapshot, &mut entry) };
    while result.is_ok() {
        if entry.th32ProcessID == pid {
            let ppid = entry.th32ParentProcessID;
            unsafe {
                let _ = windows::Win32::Foundation::CloseHandle(snapshot);
            }
            return Some(ppid);
        }
        result = unsafe { Process32NextW(snapshot, &mut entry) };
    }
    unsafe {
        let _ = windows::Win32::Foundation::CloseHandle(snapshot);
    }
    None
}

// ─────────────────────────────────────────────────────────────────────
// Tests.
// ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn argv_tokens_returns_some_for_own_pid_on_unix_or_none_on_windows() {
        // Byte-preserving migration of the ancestry probe. On Unix
        // this must return a non-empty argv for our own process (test
        // binary). On Windows the honest `None` return is intentional:
        // callers see the "no elevation inherited" default branch,
        // which is safer than reading unknown argv (CR C-3 anchor).
        let my = std::process::id();
        let argv = argv_tokens(my);
        #[cfg(unix)]
        {
            let argv = argv.expect("unix must return Some argv for own pid");
            assert!(!argv.is_empty());
            // At least argv[0] should look like a test binary path.
            let argv0 = &argv[0];
            assert!(!argv0.is_empty());
        }
        #[cfg(windows)]
        {
            // Windows conservative fallback: None, so
            // `detect_dangerous_approval` defaults to disabled.
            assert!(argv.is_none());
        }
    }

    #[test]
    fn parent_pid_returns_some_on_both_platforms() {
        // Batch 4: parent_pid via `ps -o ppid=` (unix) or Toolhelp32
        // (windows) — both must resolve our own ppid.
        let my = std::process::id();
        let ppid = parent_pid(my);
        assert!(ppid.is_some(), "parent_pid must resolve own pid on both unix and windows");
        assert!(ppid.unwrap() > 0);
    }

    #[test]
    fn environ_text_linux_only_returns_something_for_own_pid() {
        // The environ probe is Linux-only by design (matches
        // `leader/provider_attribution.rs:113-115`). Other platforms
        // honestly return None; callers already treat that as
        // "no env-based attribution".
        let my = std::process::id();
        let env = environ_text(my);
        #[cfg(target_os = "linux")]
        {
            let env = env.expect("linux must return Some environ");
            assert!(env.contains('='));
        }
        #[cfg(not(target_os = "linux"))]
        {
            assert!(env.is_none());
        }
    }
}
