//!
use super::*;

const SUMMARY_LIMIT: usize = 120;

fn clean_text(text: &str) -> String {
    text.chars().filter(|ch| !ch.is_control()).collect()
}

fn compact_field(value: Option<&Value>) -> Value {
    value
        .and_then(Value::as_str)
        .map(|text| Value::String(clean_text(text)))
        .unwrap_or(Value::Null)
}

pub fn inbox(
    workspace: &Path,
    agent: &str,
    limit: usize,
    owner_team_id: Option<&str>,
) -> Result<Value, CliError> {
    let store = crate::message_store::MessageStore::open(workspace)
        .map_err(|e| CliError::Runtime(e.to_string()))?;
    let messages = store
        .inbox(agent, limit, owner_team_id)
        .map_err(|e| CliError::Runtime(e.to_string()))?
        .into_iter()
        .map(compact_message)
        .collect::<Vec<_>>();
    Ok(json!({
        "ok": true,
        "agent_id": agent,
        "messages": messages,
    }))
}

fn compact_message(message: Value) -> Value {
    json!({
        "message_id": compact_field(message.get("message_id")),
        "sender": compact_field(message.get("sender")),
        "recipient": compact_field(message.get("recipient")),
        "status": compact_field(message.get("status")),
        "created_at": compact_field(message.get("created_at")),
        "summary": compact_summary(message.get("content").and_then(Value::as_str).unwrap_or("")),
    })
}

fn compact_summary(content: &str) -> String {
    let summary = content
        .lines()
        .map(|line| clean_text(line).trim().to_string())
        .find(|line| !line.is_empty())
        .unwrap_or_default();
    let mut chars = summary.chars();
    let truncated = chars.by_ref().take(SUMMARY_LIMIT).collect::<String>();
    if chars.next().is_some() {
        format!("{truncated}…")
    } else {
        truncated
    }
}
