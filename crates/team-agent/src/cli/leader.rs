//! cli · leader — `codex`/`claude`/`copilot` passthrough(`cmd_leader_passthrough` + `_provider_args` /
//! `_leader_launcher_args`)+ leader fallback inbox 摘要(`consume_leader_inbox_summary` 及
//! `_leader_inbox_entries` / `_leader_inbox_summary` / `_leader_inbox_entry_title`)。

use super::*;

// =============================================================================
// `codex`/`claude` passthrough 解析(helpers.py `_provider_args`/`_leader_launcher_args`)
// =============================================================================

/// `_provider_args`(`helpers.py:190-193`):剥掉前导 `--`(REMAINDER 第一个 token)。
pub fn provider_args(values: &[String]) -> Vec<String> {
    match values.first().map(String::as_str) {
        Some("--") => values.iter().skip(1).cloned().collect(),
        _ => values.to_vec(),
    }
}

/// `_leader_launcher_args`(`helpers.py:196-226`):解析 attach 旗标;`--attach-session` 缺值 → Err。
pub fn leader_launcher_args(values: &[String]) -> Result<LeaderLauncherArgs, CliError> {
    let mut out = LeaderLauncherArgs::default();
    let mut idx = 0;
    while idx < values.len() {
        let token = &values[idx];
        if token == "--" {
            for value in values.iter().skip(idx + 1) {
                if is_leader_launcher_flag(value) {
                    return Err(CliError::Runtime(format!(
                        "Team Agent launcher flag {value} must appear before --"
                    )));
                }
            }
            out.provider_args.extend(values.iter().skip(idx + 1).cloned());
            break;
        } else if token == "--attach" || token == "--attach-existing" {
            out.attach_existing = true;
        } else if token == "--confirm" {
            out.confirm_attach = true;
        } else if token == "--external-leader" {
            out.external_leader = true;
        } else if token == "--attach-session" {
            let Some(value) = values.get(idx + 1) else {
                return Err(CliError::Runtime(
                    "--attach-session requires a tmux session name".to_string(),
                ));
            };
            out.attach_session = Some(value.clone());
            idx += 1;
        } else if let Some(value) = token.strip_prefix("--attach-session=") {
            out.attach_session = Some(value.to_string());
        } else {
            out.provider_args.push(token.clone());
        }
        idx += 1;
    }
    Ok(out)
}

fn is_leader_launcher_flag(value: &str) -> bool {
    matches!(
        value,
        "--external-leader" | "--attach" | "--attach-existing" | "--confirm" | "--attach-session"
    ) || value.starts_with("--attach-session=")
}

fn leader_launcher_json(values: &[String]) -> bool {
    values
        .iter()
        .take_while(|arg| arg.as_str() != "--")
        .any(|arg| arg == "--json")
}

fn without_leader_json(values: &[String]) -> Vec<String> {
    let mut out = Vec::with_capacity(values.len());
    let mut provider_remainder = false;
    for value in values {
        if provider_remainder || value != "--json" {
            out.push(value.clone());
        }
        if value == "--" {
            provider_remainder = true;
        }
    }
    out
}

/// `codex`/`claude`/`copilot` passthrough(`parser.py:86`/`_run_leader_passthrough`):leader 早返回,
/// **不**进 subparser。`-h`/`--help` 打 usage 直接返回 [`CmdResult::none`]。否则解析 attach
/// 旗标 + `lifecycle_port::start_leader`。`command` ∈ {codex, claude, copilot}。
pub fn cmd_leader_passthrough(
    command: &str,
    provider_args: &[String],
    cwd: &Path,
) -> Result<CmdResult, CliError> {
    if provider_args == ["-h"] || provider_args == ["--help"] {
        return Ok(CmdResult::none());
    }
    let as_json = leader_launcher_json(provider_args);
    let launcher_args = without_leader_json(provider_args);
    let attach = leader_launcher_args(&launcher_args)?;
    let provider = leader_passthrough_provider(command);
    let value = lifecycle_port::start_leader(provider, &attach.provider_args, cwd, &attach)?;
    Ok(CmdResult::from_json(value, as_json))
}

pub(crate) fn leader_passthrough_provider(command: &str) -> crate::model::enums::Provider {
    crate::leader::attribute_command_provider(command)
        .unwrap_or(crate::model::enums::Provider::ClaudeCode)
}

// =============================================================================
// leader fallback inbox 摘要(helpers.py `consume_leader_inbox_summary`)
// =============================================================================

/// `consume_leader_inbox_summary`(`helpers.py:26-55`):每条命令后读
/// `.team/runtime/leader-inbox.log`,从游标 `.cursor` 起读新增字节,渲染 N 条 fallback 条目摘要
/// (`_leader_inbox_summary`,字节预算 budget=500 截断 + `Hint: team-agent inbox leader`),
/// 推进游标。**bug-084 韧性**:OSError/ValueError 一律降级(游标读 0、写失败 pass、读失败 None),
/// **绝不**因坏游标/IO 让命令崩。`None` = 无新条目。
pub fn consume_leader_inbox_summary(workspace: &Path, budget: usize) -> Option<String> {
    let runtime = workspace.join(".team").join("runtime");
    let inbox = runtime.join("leader-inbox.log");
    let cursor = runtime.join("leader-inbox.cursor");
    let bytes = std::fs::read(&inbox).ok()?;
    let offset = match std::fs::read_to_string(&cursor) {
        Ok(raw) => match raw.trim().parse::<isize>() {
            Ok(parsed) if parsed >= 0 => {
                let offset = usize::try_from(parsed).ok()?;
                if offset > bytes.len() {
                    0
                } else {
                    offset
                }
            }
            Ok(_) => 0,
            Err(_) => return None,
        },
        Err(_) => 0,
    };
    if offset == bytes.len() {
        return None;
    }
    let new_text = String::from_utf8_lossy(&bytes[offset..]);
    let _ = std::fs::write(&cursor, bytes.len().to_string());
    let entries = parse_inbox_entries(new_text);
    if entries.is_empty() {
        return None;
    }
    Some(render_inbox_summary(&entries, budget))
}

fn parse_inbox_entries(text: impl AsRef<str>) -> Vec<String> {
    let lines: Vec<String> = text
        .as_ref()
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(ToString::to_string)
        .collect();
    if lines.is_empty() {
        return Vec::new();
    }

    let has_fallback = lines.iter().any(|line| is_fallback_header(line));
    let mut blocks: Vec<Vec<String>> = Vec::new();
    if has_fallback {
        let mut current: Vec<String> = Vec::new();
        for line in lines {
            if is_fallback_header(&line) && !current.is_empty() {
                blocks.push(current);
                current = Vec::new();
            }
            current.push(line);
        }
        if !current.is_empty() {
            blocks.push(current);
        }
    } else {
        blocks.push(lines);
    }

    blocks
        .iter()
        .map(|block| entry_title(block))
        .filter(|title| !title.is_empty())
        .collect()
}

fn render_inbox_summary(entries: &[String], budget: usize) -> String {
    let noun = if entries.len() == 1 {
        "entry"
    } else {
        "entries"
    };
    let header = format!("Leader inbox: {} new fallback {noun}", entries.len());
    let hint = "Hint: team-agent inbox leader";
    let footer = "Truncated: more fallback entries available; run team-agent inbox leader";
    let mut lines = vec![header];
    let mut truncated = false;
    for entry in entries {
        let line = format!("- {entry}");
        let tail_line = hint.to_string();
        let candidate = lines
            .iter()
            .chain(std::iter::once(&line))
            .chain(std::iter::once(&tail_line))
            .cloned()
            .collect::<Vec<_>>()
            .join("\n");
        if char_len(&candidate) <= budget {
            lines.push(line);
            continue;
        }
        truncated = true;
        break;
    }
    let mut summary = if truncated {
        format!("{}\n{footer}", lines.join("\n"))
    } else {
        format!("{}\n{hint}", lines.join("\n"))
    };
    if char_len(&summary) > budget {
        let keep = budget.saturating_sub(char_len(footer)).saturating_sub(6);
        let body = prefix_chars(&lines.join("\n"), keep).trim_end().to_string();
        summary = format!("{body} ...\n{footer}");
    }
    summary
}

fn is_fallback_header(line: &str) -> bool {
    line.starts_with('[') && line.contains("fallback")
}

fn entry_title(lines: &[String]) -> String {
    let mut content = Vec::new();
    for line in lines {
        let trimmed = line.trim();
        if is_fallback_header(trimmed)
            || trimmed.starts_with("Team Agent")
            || trimmed.starts_with("Message id:")
            || trimmed.starts_with("Task id:")
            || trimmed.starts_with("From:")
            || trimmed.starts_with("To:")
            || trimmed.starts_with("Requires ack:")
            || trimmed.starts_with("Artifacts:")
        {
            continue;
        }
        content.push(trimmed);
    }
    let source = if content.is_empty() {
        lines.iter().map(String::as_str).collect::<Vec<_>>()
    } else {
        content
    };
    let collapsed = source
        .join(" ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    prefix_chars(&collapsed, 80)
}
