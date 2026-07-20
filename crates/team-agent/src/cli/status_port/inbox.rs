use super::*;

pub fn inbox(
    workspace: &Path,
    agent: &str,
    limit: usize,
    since: Option<&str>,
    as_json: bool,
    owner_team_id: Option<&str>,
) -> Result<Value, CliError> {
    let _ = as_json;
    let store = crate::message_store::MessageStore::open(workspace)
        .map_err(|e| CliError::Runtime(e.to_string()))?;
    let mut messages = store
        .inbox(agent, limit, owner_team_id)
        .map_err(|e| CliError::Runtime(e.to_string()))?;
    if let Some(cutoff) = since.and_then(parse_rfc3339) {
        messages.retain(|message| {
            message
                .get("created_at")
                .and_then(Value::as_str)
                .and_then(parse_rfc3339)
                .is_some_and(|created| created >= cutoff)
        });
    }
    Ok(json!({
        "ok": true,
        "agent_id": agent,
        "messages": messages,
        "since": since,
    }))
}

pub(super) fn parse_rfc3339(value: &str) -> Option<chrono::DateTime<chrono::FixedOffset>> {
    chrono::DateTime::parse_from_rfc3339(value).ok()
}
