//!
//! Batch 0 signature only. Batch 2 migrates the raw
//! `libc::{EACCES, EPERM, EBUSY, ENOSPC}` matching in
//! `state/persist.rs::retryable_replace_error` onto these helpers so
//! Windows sees the same classification via
//! `io::ErrorKind` + raw-code mapping.

use std::io;

///
/// **本函数是单节点**:`#[cfg]` 在函数体内部分叉,只存在一个 `fn` 定义,
/// 故不适用 SPEC §4「两份实现＝两个节点」——那条针对的是**多个独立 `fn` 定义**
/// (如 `process.rs` 里 Unix/Windows 各定义一次)。
/// `impl_notes` 只是实现说明,**不是节点身份**,不参与签名全等校验。
///
/// True when the OS error is a transient replace failure worth
/// retrying (EACCES on Windows during antivirus scan, EBUSY on
/// mounted filesystems, etc.). Batch 2 wires the callsite.
#[allow(dead_code)]
pub fn retryable_replace_error(error: &io::Error) -> bool {
    // Batch 2 will migrate `state/persist.rs::retryable_replace_error`
    // here. Batch 0 provides the signature only.
    #[cfg(unix)]
    {
        matches!(
            error.raw_os_error(),
            Some(c)
                if c == libc::EACCES
                    || c == libc::EPERM
                    || c == libc::EBUSY
                    || c == libc::ENOSPC
        )
    }
    #[cfg(not(unix))]
    {
        // Windows: retryable on sharing violation / access denied
        // during antivirus scans. Map by `io::ErrorKind` so we don't
        // depend on `windows-sys` raw codes at this layer.
        matches!(
            error.kind(),
            io::ErrorKind::PermissionDenied | io::ErrorKind::WouldBlock
        )
    }
}

///
/// 返回值是**对外稳定契约**:它会出现在诊断输出里被人和脚本读取,
/// 因此已发布的名字不可改写,只可新增。
///
/// Human-readable name for a raw OS error code — used in diagnostics
/// so operators see `EACCES` instead of `13`.
#[allow(dead_code)]
pub fn os_error_name(error: &io::Error) -> Option<&'static str> {
    #[cfg(unix)]
    {
        match error.raw_os_error()? {
            c if c == libc::EACCES => Some("EACCES"),
            c if c == libc::EPERM => Some("EPERM"),
            c if c == libc::EBUSY => Some("EBUSY"),
            c if c == libc::ENOSPC => Some("ENOSPC"),
            c if c == libc::EWOULDBLOCK => Some("EWOULDBLOCK"),
            c if c == libc::ESRCH => Some("ESRCH"),
            _ => None,
        }
    }
    #[cfg(not(unix))]
    {
        // Batch 2 will map ERROR_ACCESS_DENIED / ERROR_SHARING_VIOLATION
        // / ERROR_LOCK_VIOLATION here. Batch 0 leaves the mapping to
        // `io::Error::kind()`.
        let _ = error;
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retryable_replace_error_matches_eacces_on_unix() {
        #[cfg(unix)]
        {
            let err = io::Error::from_raw_os_error(libc::EACCES);
            assert!(retryable_replace_error(&err));
        }
    }

    #[test]
    fn retryable_replace_error_rejects_random_error() {
        // A generic "not found" is not a replace-race condition.
        let err = io::Error::from(io::ErrorKind::NotFound);
        assert!(!retryable_replace_error(&err));
    }

    #[test]
    fn os_error_name_returns_stable_wire_string_on_unix() {
        #[cfg(unix)]
        {
            let err = io::Error::from_raw_os_error(libc::EACCES);
            assert_eq!(os_error_name(&err), Some("EACCES"));
        }
    }
}
