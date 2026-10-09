//! Issue #286 shared test oracle: human discovery only, not machine dispatch.
#![allow(dead_code)]

use std::collections::BTreeSet;

pub const HUMAN_COMMANDS: &[&str] = &[
    "quick-start",
    "send",
    "status",
    "models",
    "restart",
    "shutdown",
    "add-agent",
    "start-agent",
    "stop-agent",
    "reset-agent",
    "claim-leader",
    "takeover",
    "attach-leader",
    "codex",
    "claude",
    "copilot",
    "grok",
    "cursor",
    "pi",
    "leaders",
    "doctor",
    "remove-agent",
    "fork-agent",
    "clone-agent",
    "approvals",
    "route",
    "leader-prompt",
    "profile",
    "install-skill",
    "inbox",
];
pub const MACHINE_COMMANDS: &[&str] = &[
    "wait",
    "attach-app-server-leader",
    "coordinator",
];

/// Physically removed commands, not hidden Machine commands.
pub const RETIRED_COMMANDS: &[&str] = &[
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
