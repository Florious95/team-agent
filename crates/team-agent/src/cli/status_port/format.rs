use crate::cli::CliError;
use serde_json::{json, Value};

/// `format_status` agent 分支(`queries.py:135-162`)。
pub(super) fn format_agent_status(
    status: &Value,
    agent_id: &str,
    inbox_rows: &[Value],
) -> Result<String, CliError> {
    ensure_agent_known(status, agent_id)?;
    let agents = status.get("agents").and_then(Value::as_object);
    let health = status.get("agent_health").and_then(Value::as_object);
    let empty = json!({});
    let agent = agents.and_then(|map| map.get(agent_id)).unwrap_or(&empty);
    let row = health.and_then(|map| map.get(agent_id)).unwrap_or(&empty);
    let status_text = row
        .get("status")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| {
            agent_health_status_text(agent.get("status").and_then(Value::as_str).unwrap_or(""))
        });
    let tasks = status
        .get("tasks")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let task_id = current_task_for_agent(&tasks, agent_id).unwrap_or_else(|| "-".to_string());
    let mut lines = vec![
        format!("{agent_id}  {status_text}"),
        format!("  provider: {}", py_get(agent, "provider")),
        format!("  model: {}", py_get(agent, "model")),
        format!("  profile: {}", py_get(agent, "profile")),
        format!("  session_id: {}", py_get_or_dash(agent, "session_id")),
        format!("  captured_via: {}", py_get_or_dash(agent, "captured_via")),
        format!(
            "  attribution_confidence: {}",
            py_get_or_dash(agent, "attribution_confidence")
        ),
        format!("  task: {task_id}"),
        format!("  handoff: {}", py_get(agent, "handoff_path")),
        "  recent messages:".to_string(),
    ];
    if inbox_rows.is_empty() {
        lines.push("    none".to_string());
    } else {
        for item in inbox_rows {
            let content = item.get("content").and_then(Value::as_str).unwrap_or("");
            let content: String = content.chars().take(120).collect();
            lines.push(format!(
                "    {} {} -> {} {}: {content}",
                py_get_or_dash(item, "created_at"),
                py_get_or_dash(item, "sender"),
                py_get_or_dash(item, "recipient"),
                py_get_or_dash(item, "status"),
            ));
        }
    }
    Ok(lines.join("\n"))
}

pub(super) fn ensure_agent_known(status: &Value, agent_id: &str) -> Result<(), CliError> {
    let agents = status.get("agents").and_then(Value::as_object);
    let health = status.get("agent_health").and_then(Value::as_object);
    let known = agents.is_some_and(|map| map.contains_key(agent_id))
        || health.is_some_and(|map| map.contains_key(agent_id));
    if !known {
        return Err(CliError::Runtime(format!("unknown agent id: {agent_id}")));
    }
    Ok(())
}

/// `current_task_for_agent`(`approvals/status.py:127-132`)。
pub(super) fn current_task_for_agent(tasks: &[Value], agent_id: &str) -> Option<String> {
    const ACTIVE: [&str; 5] = ["pending", "ready", "running", "blocked", "needs_retry"];
    for task in tasks.iter().rev() {
        let assignee = task.get("assignee").and_then(Value::as_str);
        let status = task
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("pending");
        if assignee == Some(agent_id) && ACTIVE.contains(&status) {
            return task.get("id").and_then(Value::as_str).map(str::to_string);
        }
    }
    None
}

pub(super) fn agent_health_status_text(status: &str) -> String {
    serde_json::to_value(crate::provider::agent_health_status(status))
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_else(|| "-".to_string())
}

/// Python `agent.get(key, '-')`:键缺失 → `-`;键存在但为 null → 打印 `None`。
pub(super) fn py_get(agent: &Value, key: &str) -> String {
    match agent.get(key) {
        None => "-".to_string(),
        Some(Value::Null) => "None".to_string(),
        Some(Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
    }
}

/// Python `agent.get(key) or '-'`:缺失/null/空串都落 `-`。
pub(super) fn py_get_or_dash(agent: &Value, key: &str) -> String {
    match agent.get(key) {
        Some(Value::String(s)) if !s.is_empty() => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        _ => "-".to_string(),
    }
}

/// `status.approvals(workspace, agent_id)`(JSON)/`format_approvals`(人读)。
pub fn format_approvals(value: &Value) -> String {
    let approvals = value
        .get("approvals")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    if approvals.is_empty() {
        return "No pending approvals.".to_string();
    }
    approvals
        .iter()
        .map(|approval| {
            let agent = approval
                .get("agent_id")
                .and_then(Value::as_str)
                .unwrap_or("-");
            let kind = approval
                .get("kind")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            let prompt = approval
                .get("prompt")
                .and_then(Value::as_str)
                .or_else(|| approval.get("subject").and_then(Value::as_str))
                .unwrap_or("-");
            format!("{agent}: {kind} {prompt}")
        })
        .collect::<Vec<_>>()
        .join("\n")
}
