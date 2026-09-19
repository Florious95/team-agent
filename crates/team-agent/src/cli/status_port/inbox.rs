//!
use super::*;

const SUMMARY_LIMIT: usize = 120;

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
        "message_id": message.get("message_id").cloned().unwrap_or(Value::Null),
        "sender": message.get("sender").cloned().unwrap_or(Value::Null),
        "recipient": message.get("recipient").cloned().unwrap_or(Value::Null),
        "status": message.get("status").cloned().unwrap_or(Value::Null),
        "created_at": message.get("created_at").cloned().unwrap_or(Value::Null),
        "summary": compact_summary(message.get("content").and_then(Value::as_str).unwrap_or("")),
    })
}

fn compact_summary(content: &str) -> String {
    let summary = content
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("")
        .to_string();
    let mut chars = summary.chars();
    let truncated = chars.by_ref().take(SUMMARY_LIMIT).collect::<String>();
    if chars.next().is_some() {
        format!("{truncated}…")
    } else {
        truncated
    }
}
