use std::path::Path;

use crate::lifecycle::{DangerousApproval, DangerousApprovalSource, LifecycleError};

/// `detect_inherited_dangerous_permissions`(`launch/config.py`):扫进程祖先链找
/// `--dangerously-*` flag,产出危险审批继承态。launch 在 inherited=false 且无 --yes 时拒。
pub fn detect_dangerous_approval() -> Result<DangerousApproval, LifecycleError> {
    if let Ok(raw) = std::env::var("TEAM_AGENT_TEST_PROCESS_ANCESTRY_ARGV_JSON") {
        let argv_tokens = serde_json::from_str::<Vec<String>>(&raw).map_err(|e| {
            LifecycleError::StatePersist(format!("invalid test ancestry argv: {e}"))
        })?;
        return Ok(detect_dangerous_approval_in_argv(&argv_tokens)
            .unwrap_or_else(disabled_dangerous_approval));
    }
    for argv_tokens in process_ancestry_argv(std::process::id()) {
        if let Some(detected) = detect_dangerous_approval_in_argv(&argv_tokens) {
            return Ok(detected);
        }
    }
    Ok(disabled_dangerous_approval())
}

pub(super) fn disabled_dangerous_approval() -> DangerousApproval {
    DangerousApproval {
        enabled: false,
        source: DangerousApprovalSource::Disabled,
        inherited: false,
        provider: None,
        flag: None,
        worker_capability_above_leader: false,
        ancestry_binary_name: None,
        unexpected_binary: false,
    }
}

pub(super) fn detect_dangerous_approval_in_argv(
    argv_tokens: &[String],
) -> Option<DangerousApproval> {
    let argv0 = argv_tokens.first().map(String::as_str).unwrap_or("");
    let ancestry_binary_name = binary_name(argv0);
    for token in argv_tokens {
        for (provider, flag) in dangerous_leader_flags() {
            if token == flag {
                let unexpected_binary =
                    !binary_matches_provider(provider, ancestry_binary_name.as_deref());
                return Some(DangerousApproval {
                    enabled: true,
                    source: DangerousApprovalSource::LeaderProcess,
                    inherited: true,
                    provider: Some((*provider).to_string()),
                    flag: Some((*flag).to_string()),
                    worker_capability_above_leader: false,
                    ancestry_binary_name,
                    unexpected_binary,
                });
            }
        }
    }
    None
}

pub(super) fn dangerous_leader_flags() -> &'static [(&'static str, &'static str)] {
    // Process argv command grammar, not persisted provider identity parsing.
    &[
        ("claude", "--dangerously-skip-permissions"),
        ("claude", "--dangerously-skip-permission"),
        ("codex", "--dangerously-bypass-approvals-and-sandbox"),
        // 0.3.27 P1 (E54 symptom 2): copilot's full-permission flags. Without
        // these entries, a copilot leader running with --allow-all / --yolo would
        // NOT propagate bypass to claude/codex workers because
        // detect_dangerous_approval scans the coordinator's ancestor argv and
        // only matches entries in THIS table.
        ("copilot", "--allow-all"),
        ("copilot", "--yolo"),
    ]
}

pub(super) fn binary_matches_provider(provider: &str, binary: Option<&str>) -> bool {
    // Binary-name command grammar, not provider identity parsing.
    match (provider, binary) {
        ("codex", Some("codex")) => true,
        ("claude", Some("claude" | "claude-code" | "claude_code")) => true,
        // 0.3.27 P1 (E54): copilot binary detection for dangerous-approval scan.
        ("copilot", Some("copilot" | "github-copilot" | "gh-copilot")) => true,
        _ => false,
    }
}

pub(super) fn binary_name(argv0: &str) -> Option<String> {
    Path::new(argv0)
        .file_name()
        .and_then(|v| v.to_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

pub(super) fn process_ancestry_argv(pid: u32) -> Vec<Vec<String>> {
    let mut out = Vec::new();
    let mut current = pid;
    let mut seen = std::collections::BTreeSet::new();
    for _ in 0..12 {
        if current == 0 || !seen.insert(current) {
            break;
        }
        if let Some(argv_tokens) = process_argv_tokens(current) {
            out.push(argv_tokens);
        }
        let Some(parent) = process_parent_pid(current) else {
            break;
        };
        if parent <= 1 || parent == current {
            break;
        }
        current = parent;
    }
    out
}

// 0.5.x Windows portability Batch 4: `process_argv_tokens` and
// `process_parent_pid` route through `crate::platform::argv`. Unix
// impls are byte-preserving (Linux `/proc/<pid>/cmdline`, macOS
// `sysctl(KERN_PROCARGS2)`, other Unix `ps -p ... -o command=`).
// Windows returns `None` per design §Batch 4 conservative anchor:
// unknown argv must never infer elevated approval; callers already
// treat `None` as "no elevation inherited" via
// `disabled_dangerous_approval()`.

pub(super) fn process_argv_tokens(pid: u32) -> Option<Vec<String>> {
    crate::platform::argv::argv_tokens(pid)
}

pub(super) fn process_parent_pid(pid: u32) -> Option<u32> {
    crate::platform::argv::parent_pid(pid)
}
