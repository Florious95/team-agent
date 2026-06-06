use serde::{Deserialize, Serialize};

use crate::provider::ApprovalKind;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalPrompt {
    pub agent_id: String,
    pub state: String,
    pub kind: ApprovalKind,
    pub prompt: String,
    pub choices: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
}

impl ApprovalPrompt {
    pub fn to_ordered_value(&self) -> serde_json::Value {
        let mut map = serde_json::Map::new();
        map.insert("agent_id".to_string(), serde_json::json!(self.agent_id));
        map.insert("state".to_string(), serde_json::json!(self.state));
        map.insert("kind".to_string(), serde_json::json!(self.kind));
        if let Some(tool) = &self.tool {
            map.insert("tool".to_string(), serde_json::json!(tool));
        }
        if let Some(command) = &self.command {
            map.insert("command".to_string(), serde_json::json!(command));
        }
        map.insert("prompt".to_string(), serde_json::json!(self.prompt));
        map.insert("choices".to_string(), serde_json::json!(self.choices));
        serde_json::Value::Object(map)
    }
}

pub fn capture_has_approval_prompt(capture: &str) -> bool {
    extract_approval_prompt("agent", capture).is_some()
}

pub fn active_approval_control_index(lines: &[&str]) -> Option<usize> {
    let idx = lines
        .iter()
        .enumerate()
        .rev()
        .find(|(_, line)| is_control_line(&clean_line(line)))
        .map(|(idx, _)| idx)?;
    if lines.iter().skip(idx + 1).any(|line| !clean_line(line).is_empty()) {
        return None;
    }
    Some(idx)
}

pub fn extract_approval_prompt(agent_id: &str, capture: &str) -> Option<ApprovalPrompt> {
    let lines: Vec<&str> = capture.lines().collect();
    let control_idx = active_approval_control_index(&lines)?;

    if let Some((matched_idx, prompt, tool)) = explicit_mcp_prompt(&lines, control_idx) {
        let choices = approval_choices_between(&lines, matched_idx, control_idx);
        return Some(approval(agent_id, ApprovalKind::McpTool, prompt, choices, Some(tool), None));
    }
    if let Some((matched_idx, prompt, tool)) = short_mcp_prompt(&lines, control_idx) {
        let choices = approval_choices_between(&lines, matched_idx, control_idx);
        return Some(approval(agent_id, ApprovalKind::McpTool, prompt, choices, Some(tool), None));
    }
    if let Some((matched_idx, prompt, command)) = command_prompt(&lines, control_idx) {
        let choices = approval_choices_between(&lines, matched_idx, control_idx);
        return Some(approval(agent_id, ApprovalKind::Command, prompt, choices, None, Some(command)));
    }
    let choices = approval_choices_between(&lines, 0, control_idx);
    Some(approval(
        agent_id,
        ApprovalKind::Unknown,
        "approval prompt detected".to_string(),
        choices,
        None,
        None,
    ))
}

pub fn choose_internal_mcp_approval_choice(prompt: &ApprovalPrompt) -> String {
    if prompt.choices.iter().any(|choice| choice == "Allow for this session") {
        return "Allow for this session".to_string();
    }
    for choice in &prompt.choices {
        if choice.starts_with("Yes, and don't ask again") {
            return choice.clone();
        }
    }
    for preferred in ["Allow", "Yes"] {
        if prompt.choices.iter().any(|choice| choice == preferred) {
            return preferred.to_string();
        }
    }
    "Allow for this session".to_string()
}

pub fn approval_choice_keys(prompt: &ApprovalPrompt, capture: &str, target_choice: &str) -> Vec<String> {
    let Some(target_index) = prompt.choices.iter().position(|choice| choice == target_choice) else {
        return vec!["Down".to_string(), "Enter".to_string()];
    };
    let Some(active) = active_choice_index(capture) else {
        return vec![(target_index + 1).to_string(), "Enter".to_string()];
    };
    if active == target_index {
        return vec!["Enter".to_string()];
    }
    let key = if target_index < active { "Up" } else { "Down" };
    let count = active.abs_diff(target_index);
    let mut keys = Vec::with_capacity(count + 1);
    keys.extend(std::iter::repeat_n(key.to_string(), count));
    keys.push("Enter".to_string());
    keys
}

fn approval(
    agent_id: &str,
    kind: ApprovalKind,
    prompt: String,
    choices: Vec<String>,
    tool: Option<String>,
    command: Option<String>,
) -> ApprovalPrompt {
    ApprovalPrompt {
        agent_id: agent_id.to_string(),
        state: "waiting_approval".to_string(),
        kind,
        prompt,
        choices,
        tool,
        command,
    }
}

fn is_control_line(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    lower.contains("enter to submit | esc to cancel")
        || (lower.contains("esc to cancel") && lower.contains("tab to amend"))
}

fn explicit_mcp_prompt(lines: &[&str], control_idx: usize) -> Option<(usize, String, String)> {
    for (idx, raw) in lines.iter().enumerate().take(control_idx).rev() {
        let line = clean_line(raw);
        let prefix = "Allow the team_orchestrator MCP server to run tool \"";
        if let Some(rest) = line.strip_prefix(prefix) {
            if let Some((tool, _)) = rest.split_once('"') {
                if !tool.is_empty() {
                    let tool = tool.to_string();
                    return Some((idx, line, tool));
                }
            }
        }
    }
    None
}

fn short_mcp_prompt(lines: &[&str], control_idx: usize) -> Option<(usize, String, String)> {
    for (idx, raw) in lines.iter().enumerate().take(control_idx).rev() {
        let line = clean_line(raw);
        if parse_choice_line(&line).is_some() {
            continue;
        }
        if let Some(tool) = team_orchestrator_tool(&line, '-') {
            return Some((idx, format!("team_orchestrator - {tool}"), tool));
        }
        if let Some(tool) = team_orchestrator_tool(&line, '.') {
            return Some((idx, format!("team_orchestrator - {tool}"), tool));
        }
    }
    None
}

fn team_orchestrator_tool(line: &str, separator: char) -> Option<String> {
    let marker = "team_orchestrator";
    let start = line.find(marker)?;
    if start > 0 && line[..start].chars().next_back().is_some_and(is_word_char) {
        return None;
    }
    let rest = &line[start + marker.len()..];
    let rest = rest.trim_start().strip_prefix(separator)?.trim_start();
    let tool: String = rest
        .chars()
        .take_while(|ch| ch.is_ascii_alphanumeric() || *ch == '_')
        .collect();
    if tool.is_empty() || !tool.chars().next().is_some_and(|ch| ch.is_ascii_alphabetic() || ch == '_') {
        None
    } else {
        Some(tool)
    }
}

fn is_word_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || ch == '_'
}

fn command_prompt(lines: &[&str], control_idx: usize) -> Option<(usize, String, String)> {
    let prompt_idx = lines
        .iter()
        .enumerate()
        .take(control_idx)
        .rev()
        .find(|(_, line)| clean_line(line) == "Would you like to run the following command?")
        .map(|(idx, _)| idx)?;
    let prompt = clean_line(lines.get(prompt_idx)?);
    let before = lines[..prompt_idx].iter().rev();
    let after = lines.iter().take(control_idx).skip(prompt_idx + 1);
    for line in before.chain(after) {
        let cleaned = clean_line(line);
        if is_command_line(&cleaned) {
            return Some((prompt_idx, prompt, cap_chars(&cleaned, 200)));
        }
    }
    None
}

fn is_command_line(line: &str) -> bool {
    line.starts_with("Bash(") || line.starts_with("Shell(")
}

fn approval_choices_between(lines: &[&str], matched_idx: usize, control_idx: usize) -> Vec<String> {
    let mut choices = Vec::new();
    for line in lines.iter().take(control_idx).skip(matched_idx) {
        if let Some((_, choice)) = parse_choice_line(&clean_line(line)) {
            if !choices.iter().any(|existing| existing == &choice) {
                choices.push(choice);
            }
        }
    }
    choices
}

fn active_choice_index(capture: &str) -> Option<usize> {
    capture.lines().find_map(|line| {
        let cleaned = clean_line(line);
        if !cleaned.starts_with('›') && !cleaned.starts_with('❯') && !cleaned.starts_with('>') {
            return None;
        }
        parse_choice_line(&cleaned).map(|(index, _)| index)
    })
}

fn parse_choice_line(line: &str) -> Option<(usize, String)> {
    let trimmed = line.trim_start_matches(['›', '❯', '>', '*', ' ']);
    let (num, rest) = trimmed.split_once('.')?;
    let raw_index = num.trim().parse::<usize>().ok()?;
    let index = raw_index.checked_sub(1)?;
    let text = trim_choice_columns(rest.trim());
    if text.is_empty() {
        return None;
    }
    Some((index, text.to_string()))
}

fn trim_choice_columns(text: &str) -> &str {
    let bytes = text.as_bytes();
    for idx in 1..bytes.len() {
        if bytes[idx - 1] == b' ' && bytes[idx] == b' ' {
            return text[..idx - 1].trim_end();
        }
    }
    text
}

fn clean_line(line: &str) -> String {
    line.trim()
        .trim_start_matches(['│', '|'])
        .trim()
        .to_string()
}

fn cap_chars(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn explicit_mcp_prompt_extracts_tool() {
        let prompt = extract_approval_prompt(
            "agent-x",
            "noise\nAllow the team_orchestrator MCP server to run tool \"send_message\"?\n  1. Allow for this session\n❯ 2. Deny\nEnter to submit | Esc to cancel\n",
        )
        .unwrap();
        assert_eq!(prompt.agent_id, "agent-x");
        assert_eq!(prompt.state, "waiting_approval");
        assert_eq!(prompt.kind, ApprovalKind::McpTool);
        assert_eq!(prompt.tool.as_deref(), Some("send_message"));
        assert_eq!(prompt.prompt, "Allow the team_orchestrator MCP server to run tool \"send_message\"?");
        assert_eq!(prompt.choices, vec!["Allow for this session", "Deny"]);
    }

    #[test]
    fn short_mcp_prompt_extracts_bare_tool() {
        let prompt = extract_approval_prompt(
            "agent-x",
            "team_orchestrator - assign_task\n  1. Yes\nEnter to submit | Esc to cancel\n",
        )
        .unwrap();
        assert_eq!(prompt.kind, ApprovalKind::McpTool);
        assert_eq!(prompt.tool.as_deref(), Some("assign_task"));
        assert_eq!(prompt.prompt, "team_orchestrator - assign_task");
    }

    #[test]
    fn command_prompt_extracts_command_before_prompt() {
        let prompt = extract_approval_prompt(
            "agent-x",
            "Bash(echo hi)\nWould you like to run the following command?\n  1. Yes\nEnter to submit | Esc to cancel\n",
        )
        .unwrap();
        assert_eq!(prompt.kind, ApprovalKind::Command);
        assert_eq!(prompt.command.as_deref(), Some("Bash(echo hi)"));
        assert_eq!(prompt.prompt, "Would you like to run the following command?");
    }

    #[test]
    fn control_followed_by_non_prompt_text_is_none() {
        assert_eq!(
            active_approval_control_index(&"team_orchestrator - send_message\nEnter to submit | Esc to cancel\nlater output\n".lines().collect::<Vec<_>>()),
            None
        );
        assert_eq!(
            extract_approval_prompt("agent-x", "MCP tool: team_orchestrator - send_message requires approval\n  1. Allow\n"),
            None
        );
    }

    #[test]
    fn unknown_fallback_has_choices_and_state() {
        let prompt = extract_approval_prompt(
            "agent-x",
            "random approval text\n  1. Allow\nEnter to submit | Esc to cancel\n",
        )
        .unwrap();
        assert_eq!(prompt.kind, ApprovalKind::Unknown);
        assert_eq!(prompt.prompt, "approval prompt detected");
        assert_eq!(prompt.choices, vec!["Allow"]);
    }

    #[test]
    fn choose_internal_mcp_approval_choice_matches_golden() {
        let mut prompt = approval(
            "agent-x",
            ApprovalKind::McpTool,
            String::new(),
            vec![
                "Allow for this session".to_string(),
                "Yes, and don't ask again".to_string(),
                "Allow".to_string(),
                "Yes".to_string(),
            ],
            None,
            None,
        );
        assert_eq!(choose_internal_mcp_approval_choice(&prompt), "Allow for this session");
        prompt.choices = vec!["Maybe".to_string()];
        assert_eq!(choose_internal_mcp_approval_choice(&prompt), "Allow for this session");
        prompt.choices = vec!["Maybe".to_string(), "Yes, and don't ask again for this command".to_string(), "Allow".to_string()];
        assert_eq!(
            choose_internal_mcp_approval_choice(&prompt),
            "Yes, and don't ask again for this command"
        );
    }

    #[test]
    fn approval_choice_keys_match_golden() {
        let prompt = approval(
            "agent-x",
            ApprovalKind::McpTool,
            String::new(),
            vec!["Allow".to_string(), "Deny".to_string()],
            None,
            None,
        );
        assert_eq!(approval_choice_keys(&prompt, "❯ 1. Allow\n", "Missing"), vec!["Down", "Enter"]);
        assert_eq!(approval_choice_keys(&prompt, "  1. Allow\n  2. Deny\n", "Deny"), vec!["2", "Enter"]);
        assert_eq!(approval_choice_keys(&prompt, "❯ 1. Allow\n  2. Deny\n", "Deny"), vec!["Down", "Enter"]);
        assert_eq!(approval_choice_keys(&prompt, "  1. Allow\n❯ 2. Deny\n", "Allow"), vec!["Up", "Enter"]);
        assert_eq!(approval_choice_keys(&prompt, "❯ 1. Allow\n  2. Deny\n", "Allow"), vec!["Enter"]);
    }

    #[test]
    fn m5r2_boundary_cases_match_golden_review() {
        let mcp = extract_approval_prompt(
            "agent-x",
            "Allow the team_orchestrator MCP server to run tool \"send_message\"?\nnoise1\nnoise2\nnoise3\nnoise4\nnoise5\nnoise6\nnoise7\n  1. Allow\nenter to submit | esc to cancel\n",
        )
        .unwrap();
        assert_eq!(mcp.tool.as_deref(), Some("send_message"));

        let unknown = extract_approval_prompt(
            "agent-x",
            "xteam_orchestrator - assign_task\n  1. Yes\nEnter to submit | Esc to cancel\n",
        )
        .unwrap();
        assert_eq!(unknown.kind, ApprovalKind::Unknown);

        let command = extract_approval_prompt(
            "agent-x",
            "Bash(echo hi)\nnoise1\nnoise2\nnoise3\nnoise4\nnoise5\nnoise6\nnoise7\nWould you like to run the following command?\n  1. Yes\nEnter to submit | Esc to cancel\n",
        )
        .unwrap();
        assert_eq!(command.command.as_deref(), Some("Bash(echo hi)"));
    }

    #[test]
    fn ordered_value_places_tool_or_command_before_prompt() {
        let mcp = approval(
            "agent-x",
            ApprovalKind::McpTool,
            "p".to_string(),
            vec!["Allow".to_string()],
            Some("send_message".to_string()),
            None,
        )
        .to_ordered_value();
        let keys: Vec<&str> = mcp.as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(keys, vec!["agent_id", "state", "kind", "tool", "prompt", "choices"]);

        let command = approval(
            "agent-x",
            ApprovalKind::Command,
            "p".to_string(),
            vec!["Yes".to_string()],
            None,
            Some("Bash(echo hi)".to_string()),
        )
        .to_ordered_value();
        let keys: Vec<&str> = command.as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(keys, vec!["agent_id", "state", "kind", "command", "prompt", "choices"]);
    }

    #[test]
    fn choices_are_scoped_to_matched_prompt() {
        let mcp = extract_approval_prompt(
            "agent-x",
            "  1. Old Allow\nold text\nteam_orchestrator - send_message\n  1. Yes\nEnter to submit | Esc to cancel\n",
        )
        .unwrap();
        assert_eq!(mcp.choices, vec!["Yes"]);

        let command = extract_approval_prompt(
            "agent-x",
            "  1. Old Allow\nBash(echo hi)\nWould you like to run the following command?\n  1. Yes\nEnter to submit | Esc to cancel\n",
        )
        .unwrap();
        assert_eq!(command.choices, vec!["Yes"]);
    }
}
