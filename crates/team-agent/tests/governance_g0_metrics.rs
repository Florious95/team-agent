//! G0 governance metrics.
//!
//! References:
//! - `.team/artifacts/tech-debt-audit-and-roadmap.md` §4 Phase G0.
//! - `.team/artifacts/tech-debt-audit-and-roadmap.md` §7 acceptance matrix.
//!
//! The visible command count is a hard gate after C1 acceptance. Direct state
//! save enumeration remains diagnostic beside the S1a repository contract.

#![allow(clippy::expect_used, clippy::panic)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const VISIBLE_COMMAND_TARGET: usize = 15;

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
    let commands = parse_visible_commands(&help);
    println!(
        "G0_METRIC visible_command_count current={} target_threshold={} status=ok",
        commands.len(),
        VISIBLE_COMMAND_TARGET
    );
    println!("G0_VISIBLE_COMMANDS {}", commands.join(","));
    assert!(
        commands.len() <= VISIBLE_COMMAND_TARGET,
        "G0 visible command count exceeded target: current={} target={} commands={:?}",
        commands.len(),
        VISIBLE_COMMAND_TARGET,
        commands
    );
}

#[test]
fn direct_state_save_calls_are_diagnostic_g0_metric() {
    let repo = repo_root();
    let mut callsites = Vec::new();
    collect_state_save_calls(&repo.join("crates/team-agent/src"), &mut callsites)
        .expect("scan product source state-save callsites");
    callsites.sort();

    println!(
        "G0_METRIC direct_state_save_callsite_count current={} diagnostic=true",
        callsites.len()
    );
    for callsite in &callsites {
        println!(
            "G0_STATE_SAVE_CALLSITE {}:{} {}",
            callsite.path.display(),
            callsite.line,
            callsite.snippet
        );
    }
}

#[derive(Debug, Eq, PartialEq, Ord, PartialOrd)]
struct Callsite {
    path: PathBuf,
    line: usize,
    snippet: String,
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

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("team-agent crate should live under crates/team-agent")
        .to_path_buf()
}

fn collect_state_save_calls(dir: &Path, out: &mut Vec<Callsite>) -> std::io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            if path.components().any(|part| part.as_os_str() == "tests") {
                continue;
            }
            collect_state_save_calls(&path, out)?;
            continue;
        }
        if path.extension().and_then(|ext| ext.to_str()) != Some("rs") {
            continue;
        }
        scan_file(&path, out)?;
    }
    Ok(())
}

fn scan_file(path: &Path, out: &mut Vec<Callsite>) -> std::io::Result<()> {
    let text = fs::read_to_string(path)?;
    let repo = repo_root();
    let relative = path.strip_prefix(&repo).unwrap_or(path).to_path_buf();
    let mut pending_cfg_test = false;
    let mut skip_test_depth: Option<i32> = None;
    for (index, line) in text.lines().enumerate() {
        let code = line.split("//").next().unwrap_or("").trim();
        if let Some(depth) = skip_test_depth.as_mut() {
            *depth += brace_delta(code);
            if *depth <= 0 {
                skip_test_depth = None;
            }
            continue;
        }
        if code.contains("#[cfg(test)]") {
            pending_cfg_test = true;
            continue;
        }
        if pending_cfg_test {
            if code.contains('{') {
                let depth = brace_delta(code);
                if depth > 0 {
                    skip_test_depth = Some(depth);
                }
                pending_cfg_test = false;
                continue;
            }
            if !code.is_empty() && !code.starts_with("#[") {
                pending_cfg_test = false;
            }
        }
        if code.is_empty()
            || code.contains("fn save_runtime_state")
            || code.contains("fn save_team_scoped_state")
        {
            continue;
        }
        if has_state_save_call(code) {
            out.push(Callsite {
                path: relative.clone(),
                line: index + 1,
                snippet: code.split_whitespace().collect::<Vec<_>>().join(" "),
            });
        }
    }
    Ok(())
}

fn brace_delta(code: &str) -> i32 {
    let opens = code.chars().filter(|ch| *ch == '{').count() as i32;
    let closes = code.chars().filter(|ch| *ch == '}').count() as i32;
    opens - closes
}

fn has_state_save_call(code: &str) -> bool {
    contains_call_token(code, "save_runtime_state")
        || contains_call_token(code, "save_team_scoped_state")
}

fn contains_call_token(code: &str, prefix: &str) -> bool {
    let mut offset = 0;
    while let Some(found) = code[offset..].find(prefix) {
        let start = offset + found;
        let before_ok = start == 0 || !code[..start].chars().next_back().is_some_and(is_ident_char);
        let after_prefix = &code[start + prefix.len()..];
        let token_tail_len = after_prefix
            .chars()
            .take_while(|ch| is_ident_char(*ch))
            .map(char::len_utf8)
            .sum::<usize>();
        let after_token = &after_prefix[token_tail_len..];
        let call_ok = after_token.trim_start().starts_with('(');
        if before_ok && call_ok {
            return true;
        }
        offset = start + prefix.len();
    }
    false
}

fn is_ident_char(ch: char) -> bool {
    ch == '_' || ch.is_ascii_alphanumeric()
}
