//!
use crate::cli::CliError;
use serde_json::{json, Value};

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
