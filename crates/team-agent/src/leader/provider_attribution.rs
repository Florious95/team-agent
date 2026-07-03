//! Positive provider attribution for leader panes.

use std::collections::BTreeMap;
use std::process::Command;

use crate::provider::Provider;
use crate::transport::PaneInfo;

use super::helpers::{parse_provider, provider_wire};

// Command grammar, not provider identity parsing: these are process argv/current-command
// needles observed in tmux panes.
const COMMAND_GRAMMAR_PROVIDER_COMMANDS: &[(&str, Provider)] = &[
    ("claude", Provider::ClaudeCode),
    ("claude.exe", Provider::ClaudeCode),
    ("codex", Provider::Codex),
    ("copilot", Provider::Copilot),
    ("gemini", Provider::GeminiCli),
    ("fake", Provider::Fake),
];

pub(crate) fn attribute_pane_provider(pane: &PaneInfo) -> Option<Provider> {
    attribute_pane_provider_with_process(pane, provider_from_pid_env, provider_from_pid_argv)
}

fn attribute_pane_provider_with_process(
    pane: &PaneInfo,
    provider_from_env_pid: impl Fn(u32) -> Option<Provider>,
    provider_from_argv_pid: impl Fn(u32) -> Option<Provider>,
) -> Option<Provider> {
    provider_from_env(&pane.leader_env)
        .or_else(|| pane.pane_pid.and_then(provider_from_env_pid))
        .or_else(|| {
            pane.current_command
                .as_deref()
                .and_then(attribute_command_provider)
        })
        .or_else(|| pane.pane_pid.and_then(provider_from_argv_pid))
}

pub(crate) fn attribute_command_provider(command: &str) -> Option<Provider> {
    provider_from_command_text(command)
}

pub(crate) fn command_matches_provider(provider: Provider, command: &str) -> bool {
    attribute_command_provider(command)
        .is_some_and(|candidate| provider_matches(candidate, provider))
}

pub(crate) fn provider_wire_from_command(command: &str) -> Option<&'static str> {
    attribute_command_provider(command).map(provider_wire_for_command_attribution)
}

fn provider_from_env(env: &BTreeMap<String, String>) -> Option<Provider> {
    env.get("TEAM_AGENT_LEADER_PROVIDER")
        .filter(|raw| !raw.is_empty())
        .and_then(|raw| parse_provider(raw))
}

fn provider_from_pid_argv(pid: u32) -> Option<Provider> {
    process_command_line(pid)
        .as_deref()
        .and_then(provider_from_command_text)
}

fn provider_from_pid_env(pid: u32) -> Option<Provider> {
    process_environment(pid)
        .as_deref()
        .and_then(provider_from_env_text)
}

fn provider_from_command_text(text: &str) -> Option<Provider> {
    let lower = text.to_ascii_lowercase();
    COMMAND_GRAMMAR_PROVIDER_COMMANDS
        .iter()
        .find_map(|(needle, provider)| lower.contains(needle).then_some(*provider))
}

fn provider_from_env_text(text: &str) -> Option<Provider> {
    text.split(|ch: char| ch == '\0' || ch.is_ascii_whitespace())
        .find_map(|part| part.strip_prefix("TEAM_AGENT_LEADER_PROVIDER="))
        .filter(|raw| !raw.is_empty())
        .and_then(parse_provider)
}

pub(crate) fn provider_matches(candidate: Provider, expected: Provider) -> bool {
    candidate == expected
        || matches!(
            (candidate, expected),
            (Provider::Claude, Provider::ClaudeCode) | (Provider::ClaudeCode, Provider::Claude)
        )
}

fn provider_wire_for_command_attribution(provider: Provider) -> &'static str {
    match provider {
        Provider::Claude | Provider::ClaudeCode => "claude",
        other => provider_wire(other),
    }
}

#[cfg(target_os = "linux")]
fn process_command_line(pid: u32) -> Option<String> {
    let bytes = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
    let parts = bytes
        .split(|byte| *byte == 0)
        .filter(|part| !part.is_empty())
        .map(|part| String::from_utf8_lossy(part).to_string())
        .collect::<Vec<_>>();
    (!parts.is_empty()).then(|| parts.join(" "))
}

#[cfg(target_os = "linux")]
fn process_environment(pid: u32) -> Option<String> {
    String::from_utf8(std::fs::read(format!("/proc/{pid}/environ")).ok()?).ok()
}

#[cfg(not(target_os = "linux"))]
fn process_command_line(pid: u32) -> Option<String> {
    let output = crate::os_probe::bounded_command_output_with_probe(
        Command::new("ps")
            .arg("-p")
            .arg(pid.to_string())
            .args(["-o", "command="]),
        "provider_argv",
        Some(pid),
    )
    .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .filter(|text| !text.is_empty())
}

#[cfg(not(target_os = "linux"))]
fn process_environment(pid: u32) -> Option<String> {
    let _ = pid;
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::{PaneId, SessionName};
    fn pane(command: &str, pid: Option<u32>) -> PaneInfo {
        PaneInfo {
            pane_id: PaneId::new("%1"),
            session: SessionName::new("team-test"),
            window_index: None,
            window_name: None,
            pane_index: None,
            tty: None,
            current_command: Some(command.to_string()),
            current_path: None,
            active: true,
            pane_pid: pid,
            leader_env: BTreeMap::new(),
        }
    }

    #[test]
    fn direct_command_form_recognizes_copilot() {
        assert_eq!(
            attribute_pane_provider(&pane("copilot --allow-all-tools", None)),
            Some(Provider::Copilot)
        );
    }

    #[test]
    fn node_running_non_provider_stays_none() {
        assert_eq!(attribute_pane_provider(&pane("node", None)), None);
        assert_eq!(
            attribute_command_provider("node /tmp/not-a-provider.js"),
            None
        );
    }

    #[test]
    fn pane_env_provider_wins_over_command() {
        let mut pane = pane("codex", None);
        pane.leader_env.insert(
            "TEAM_AGENT_LEADER_PROVIDER".to_string(),
            "copilot".to_string(),
        );

        assert_eq!(attribute_pane_provider(&pane), Some(Provider::Copilot));
    }

    #[test]
    fn node_form_copilot_is_attributed_from_process_argv() {
        let pane = pane("node", Some(4242));

        assert_eq!(
            attribute_pane_provider_with_process(
                &pane,
                |_| None,
                |pid| (pid == 4242).then_some(Provider::Copilot),
            ),
            Some(Provider::Copilot)
        );
    }

    #[test]
    fn process_env_provider_wins_for_node_form() {
        let pane = pane("node /opt/homebrew/bin/copilot", Some(4242));

        assert_eq!(
            attribute_pane_provider_with_process(
                &pane,
                |pid| (pid == 4242).then_some(Provider::Copilot),
                |_| Some(Provider::Codex),
            ),
            Some(Provider::Copilot)
        );
    }
}
