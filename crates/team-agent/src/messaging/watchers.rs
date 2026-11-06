//! Formatting and identification of result notifications.

pub fn result_id_from_text(content: &str) -> Option<String> {
    for line in content.lines() {
        if let Some(rest) = line.strip_prefix("Result id: ") {
            let id = rest.trim();
            if id.is_empty() {
                return None;
            }
            return Some(id.to_string());
        }
    }
    None
}

pub fn format_result_watcher_notification(result: &serde_json::Value) -> String {
    let task_id = result
        .get("task_id")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown task");
    let agent_id = result
        .get("agent_id")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown agent");
    let status = result
        .get("status")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    let summary = result
        .get("summary")
        .and_then(|v| v.as_str())
        .unwrap_or("completed");
    let mut lines = vec![format!(
        "Task {task_id} reported {status} from {agent_id}: {summary}"
    )];
    if let Some(tests) = result.get("tests").and_then(|v| v.as_array()) {
        let rendered: Vec<String> = tests
            .iter()
            .take(3)
            .filter_map(|test| {
                let command = test.get("command").and_then(|v| v.as_str())?;
                let status = test.get("status").and_then(|v| v.as_str())?;
                Some(format!("{command}={status}"))
            })
            .collect();
        if !rendered.is_empty() {
            lines.push(format!("Tests: {}", rendered.join("; ")));
        }
    }
    if let Some(result_id) = result.get("result_id").and_then(|v| v.as_str()) {
        if !result_id.is_empty() {
            lines.push(format!("Result id: {result_id}"));
        }
    }
    lines.push(
        "Team Agent has collected this result and updated team_state.md. No manual polling is needed."
            .to_string(),
    );
    lines.join("\n")
}
