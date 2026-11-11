//! Issue #286 shared test oracle: human discovery only, not machine dispatch.
#![allow(dead_code)]

use std::collections::BTreeSet;

/// 34d41fc7 published the canonical root help: these 25 commands, launchers
/// included. Compatibility verbs moved to HIDDEN_COMPAT_COMMANDS.
pub const HUMAN_COMMANDS: &[&str] = &[
    "start",
    "send",
    "schedule",
    "status",
    "models",
    "inbox",
    "stop",
    "team",
    "restart",
    "agent",
    "locate",
    "leaders",
    "doctor",
    "approvals",
    "route",
    "leader-prompt",
    "profile",
    "install-skill",
    "claim-leader",
    "pi",
    "codex",
    "claude",
    "copilot",
    "grok",
    "kiro",
];
/// 34d41fc7: compatibility verbs that still dispatch (each keeps its own
/// help, redirecting to the canonical form) but are absent from root help.
pub const HIDDEN_COMPAT_COMMANDS: &[&str] = &[
    "quick-start",
    "shutdown",
    "add-agent",
    "start-agent",
    "stop-agent",
    "reset-agent",
    "remove-agent",
    "fork-agent",
    "clone-agent",
];
pub const MACHINE_COMMANDS: &[&str] = &["wait", "attach-app-server-leader"];

/// Physically removed or refused-as-retired commands, not hidden Machine
/// commands. e0652b62 retired attach-leader and takeover (refused, naming
/// claim-leader); coordinator is refused as retired (successor: schedule).
pub const RETIRED_COMMANDS: &[&str] = &[
    "cursor",
    "e2e",
    "allow-peer-talk",
    "results",
    "validate",
    "identity",
    "sessions",
    "watch",
    "peek",
    "wait-ready",
    "preflight",
    "attach-leader",
    "takeover",
    "coordinator",
];

pub fn expected_human_commands() -> BTreeSet<String> {
    HUMAN_COMMANDS
        .iter()
        .map(|name| (*name).to_string())
        .collect()
}

pub fn public_commands(help: &str) -> BTreeSet<String> {
    public_command_rows(help).into_iter().collect()
}

pub fn public_command_rows(help: &str) -> Vec<String> {
    help.lines()
        .filter_map(|line| {
            let row = line.strip_prefix("  ")?;
            if row.starts_with("team-agent ") || row.starts_with(char::is_whitespace) {
                return None;
            }
            let name = row.split_whitespace().next()?;
            (name.chars().next()?.is_ascii_lowercase()
                && name
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'))
            .then(|| name.to_string())
        })
        .collect()
}
