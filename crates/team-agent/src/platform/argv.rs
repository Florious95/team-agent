//! Process argv / environ probe.
//!
//! Batch 4 (design.md §Batch 4) migrates
//! `lifecycle/launch.rs:3523-3605` (leader ancestry argv tokens for
//! approval inheritance) + `leader/provider_attribution.rs:101-125`
//! onto these helpers. Batch 0 provides the signatures only.
//!
//! Conservative Windows fallback (design §Batch 4 Verification):
//! "unknown argv must never infer elevated approval; it should keep
//! worker permission at provider default or require explicit user
//! consent". Batch 4 implementation will honor this — the Batch 0
//! stub returns `None` so callers see the honest unknown branch.

/// Return the argv tokens for `pid`, or `None` if unavailable on this
/// platform.
///
/// Unix Batch 4: `/proc/<pid>/cmdline` on Linux, `sysctl(KERN_PROCARGS2)`
/// on macOS, `ps -p <pid>` fallback.
/// Windows Batch 4: WMI/CIM or `NtQueryInformationProcess`+PEB read;
/// see design.md §Full Unix-Only Surface — Process Management row for
/// `lifecycle/launch.rs:3523-3605`.
#[allow(dead_code)]
pub fn argv_tokens(pid: u32) -> Option<Vec<String>> {
    let _ = pid;
    None
}

/// Return the environ text (NUL-joined) for `pid`, or `None`.
/// Same migration boundary as `argv_tokens`.
#[allow(dead_code)]
pub fn environ_text(pid: u32) -> Option<String> {
    let _ = pid;
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn batch0_stub_returns_none_for_argv_and_environ() {
        // Callers pre-Batch-4 must see None so the "unknown argv"
        // approval fallback path is exercised. This test is the
        // signature pin — Batch 4 will replace the impl and this test
        // will move to "returns Some(...) on unix / None on windows".
        assert!(argv_tokens(std::process::id()).is_none());
        assert!(environ_text(std::process::id()).is_none());
    }
}
