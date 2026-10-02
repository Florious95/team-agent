//! G0 governance metrics.
//!
//! References:
//! - `.team/artifacts/tech-debt-audit-and-roadmap.md` §4 Phase G0.
//! - `.team/artifacts/tech-debt-audit-and-roadmap.md` §7 acceptance matrix.
//!
//! The visible command count is a hard gate after C1 acceptance.

#![allow(clippy::expect_used, clippy::panic)]

use std::collections::BTreeSet;
use std::process::Command;

const RESIGN_PLUS_RESULTS: &[&str] = &[
    "quick-start",
    "send",
    "status",
    "results",
    "restart",
    "shutdown",
    "add-agent",
    "start-agent",
    "stop-agent",
    "reset-agent",
    "doctor",
    "claim-leader",
    "takeover",
    "attach-leader",
];

fn exact_visible_contract(help: &str) -> BTreeSet<String> {
    let parsed = parse_visible_commands(help);
    let mut expected: BTreeSet<String> = RESIGN_PLUS_RESULTS
        .iter()
        .map(|command| (*command).to_string())
        .collect();
    if parsed.iter().any(|command| command == "models") {
        expected.insert("models".to_string());
    }
    expected
}

#[test]
fn visible_command_count_stays_within_g0_limit() {
    let output = Command::new(env!("CARGO_BIN_EXE_team-agent"))
        .arg("--help")
        .output()
        .expect("run team-agent --help");
    assert!(
        output.status.success(),
        "team-agent --help must execute before G0 can measure visible commands; status={:?} stderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );

    let help = String::from_utf8_lossy(&output.stdout);
    let commands = parse_visible_commands(&help)
        .into_iter()
        .collect::<BTreeSet<_>>();
    let expected = exact_visible_contract(&help);
    println!(
        "G0_METRIC visible_command_count current={} exact_set={} status=ok",
        commands.len(),
        expected.len()
    );
    println!("G0_VISIBLE_COMMANDS {}", commands.iter().cloned().collect::<Vec<_>>().join(","));
    assert_eq!(
        commands, expected,
        "G0 visible commands must equal the exact published set (d40 ∪ results; models only when advertised), not a slack threshold; commands={commands:?} expected={expected:?}"
    );
}

fn parse_visible_commands(help: &str) -> Vec<String> {
    let mut commands_text = String::new();
    let mut in_commands = false;
    for line in help.lines() {
        if let Some(rest) = line.strip_prefix("Commands:") {
            in_commands = true;
            commands_text.push_str(rest);
            commands_text.push(' ');
            continue;
        }
        if in_commands {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with("Run ") || trimmed.ends_with(':') {
                break;
            }
            commands_text.push_str(trimmed);
            commands_text.push(' ');
        }
    }
    commands_text
        .split(',')
        .map(str::trim)
        .filter(|command| !command.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>()
        .into_iter()
        .chain(parse_sectioned_visible_commands(help))
        .collect()
}

fn parse_sectioned_visible_commands(help: &str) -> Vec<String> {
    let mut commands = Vec::new();
    for line in help.lines() {
        let Some(trimmed) = line.strip_prefix("  ") else {
            continue;
        };
        if trimmed.starts_with("team-agent ") {
            continue;
        }
        let command = trimmed.split_whitespace().next().unwrap_or_default();
        if command
            .chars()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-')
        {
            commands.push(command.to_string());
        }
    }
    commands
}
