//! Command-specific human triage projections for `doctor` and `diagnose`.
//!
//! The JSON report remains the source of truth. This renderer only projects its
//! stable issue and repair entries; it does not perform I/O or recompute health.

use serde_json::Value;

use super::{CmdOutput, CmdResult};

const LINE_LIMIT_BYTES: usize = 160;

pub(crate) fn report(value: Value, as_json: bool, command: &str) -> CmdResult {
    let result = CmdResult::from_json(value, as_json);
    if as_json {
        result
    } else {
        human_result(result, command)
    }
}

pub(crate) fn human_result(result: CmdResult, command: &str) -> CmdResult {
    let text = match &result.output {
        CmdOutput::Json(report) => render(command, report),
        CmdOutput::Human(text) => text.clone(),
        CmdOutput::None => String::new(),
    };
    CmdResult {
        output: CmdOutput::Human(text),
        ..result
    }
}

pub(crate) fn render(command: &str, report: &Value) -> String {
    // Keep the renderer downstream of the external redaction boundary. The
    // generic emitter redacts again for defense in depth.
    let report = crate::redaction::redact_external_value(report);
    let ok = report.get("ok").and_then(Value::as_bool).unwrap_or(false);
    let runtime = report
        .get("runtime")
        .and_then(|runtime| runtime.get("status"))
        .and_then(Value::as_str);
    let issues = array_items(report.get("issues"));
    let repairs = array_items(report.get("suggested_repairs"));
    let mut summary = format!(
        "{command}: {}",
        if ok { "ok" } else { "needs attention" }
    );
    if let Some(runtime) = runtime {
        summary.push_str(&format!("; runtime={runtime}"));
    }
    summary.push_str(&format!("; issues={}; repairs={}", issues.len(), repairs.len()));
    let mut lines = vec![bounded(summary)];
    for issue in issues {
        lines.push(bounded(format!("issue: {}", summarize(issue, false))));
    }
    for repair in repairs {
        lines.push(bounded(format!("repair: {}", summarize(repair, true))));
    }
    // Short triage lines are navigation, not the authoritative refusal. Retain
    // complete causes/actions as JSON, without dumping unrelated runtime or
    // provider diagnostics. Values have already crossed the redaction boundary.
    let details = diagnostic_details(&report);
    if details.as_object().is_some_and(|details| !details.is_empty()) {
        lines.push(format!("details: {details}"));
    }
    lines.join("\n")
}

// Shared by doctor/diagnose and send. Never include message bodies, profiles or
// unrelated runtime/provider dumps in a command's diagnostic output.
pub(crate) fn diagnostic_details(report: &Value) -> Value {
    let report = crate::redaction::redact_external_value(report);
    let mut details = serde_json::Map::new();
    for key in [
        "error", "status", "summary", "reason", "blockers", "action",
        "next_action", "next_actions", "issues", "suggested_repairs",
        "state_path", "log", "schema_error", "stage", "delivery_status",
        "message_status", "verification", "channel", "turn_verification",
    ] {
        if let Some(value) = report.get(key) {
            details.insert(key.to_string(), value.clone());
        }
    }
    Value::Object(details)
}

fn array_items(value: Option<&Value>) -> &[Value] {
    value.and_then(Value::as_array).map(Vec::as_slice).unwrap_or(&[])
}

fn summarize(value: &Value, repair: bool) -> String {
    match value {
        Value::String(value) => value.clone(),
        Value::Null => "null".to_string(),
        Value::Bool(value) => value.to_string(),
        Value::Number(value) => value.to_string(),
        Value::Array(values) => format!("{} entries", values.len()),
        Value::Object(object) => {
            let keys = if repair {
                ["action", "hint_action", "next_action", "issue", "code", "reason"]
                    .as_slice()
            } else {
                ["code", "issue", "id", "type", "status", "reason", "message"]
                    .as_slice()
            };
            let mut parts = Vec::new();
            if let Some(path) = object.get("path").and_then(Value::as_str) {
                let location = match object.get("line").and_then(Value::as_u64) {
                    Some(line) => format!("{path}:{line}"),
                    None => path.to_string(),
                };
                // Keep the target ahead of prose so bounded human lines retain
                // the distinguishing location rather than only the issue id.
                parts.push(format!("at={location}"));
            }
            for key in keys {
                if let Some(value) = object.get(*key).and_then(Value::as_str) {
                    parts.push(format!("{key}={value}"));
                }
            }
            if parts.is_empty() {
                let mut names = object.keys().cloned().collect::<Vec<_>>();
                names.sort_unstable();
                format!("fields={}", names.join(","))
            } else {
                parts.join("; ")
            }
        }
    }
}

fn bounded(line: String) -> String {
    let clean = line
        .chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .collect::<String>();
    if clean.len() <= LINE_LIMIT_BYTES {
        return clean;
    }
    let budget = LINE_LIMIT_BYTES.saturating_sub('…'.len_utf8());
    let mut end = 0;
    for (index, ch) in clean.char_indices() {
        let next = index + ch.len_utf8();
        if next > budget {
            break;
        }
        end = next;
    }
    format!("{}…", &clean[..end.min(budget)])
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use serde_json::json;

    #[test]
    fn renderer_projects_each_issue_and_repair_without_dumping_runtime() {
        let report = json!({
            "ok": false,
            "issues": ["short_issue", {"code": "object_issue", "message": "detail"}],
            "suggested_repairs": [{"issue": "object_issue", "action": "run repair"}],
            "providers": {"claude": {"version": "secret"}},
            "runtime": {"workspace": "/tmp/secret"}
        });
        let output = render("doctor", &report);
        assert!(output.contains("issue: short_issue"));
        assert!(output.contains("issue: code=object_issue"));
        assert!(output.contains("repair: action=run repair"));
        assert!(!output.contains("providers:"));
        assert!(!output.contains("runtime:"));
    }

    #[test]
    fn renderer_sanitizes_and_bounds_utf8_lines() {
        let report = json!({"ok": false, "issues": ["问题\n".to_string() + &"x".repeat(200)]});
        let output = render("doctor", &report);
        for line in output.lines() {
            if let Some(details) = line.strip_prefix("details: ") {
                let parsed: Value = serde_json::from_str(details).unwrap();
                assert_eq!(parsed.get("issues"), report.get("issues"));
            } else {
                assert!(line.len() <= LINE_LIMIT_BYTES);
            }
            assert!(!line.chars().any(char::is_control));
        }
    }

    #[test]
    fn renderer_preserves_typed_failure_details_and_long_actions() {
        let action = "inspect the precise failure ".repeat(20);
        let report = json!({
            "ok": false,
            "status": "preflight_blocked",
            "reason": "exact refusal",
            "blockers": [{"stage": "database", "reason": "permission denied"}],
            "next_actions": [action],
            "state_path": "/tmp/workspace/.team/runtime/state.json"
        });
        let output = render("doctor", &report);
        let details = output.lines().find_map(|line| line.strip_prefix("details: ")).unwrap();
        let parsed: Value = serde_json::from_str(details).unwrap();
        for key in ["status", "reason", "blockers", "next_actions", "state_path"] {
            assert_eq!(parsed.get(key), report.get(key), "lost {key}");
        }
    }

    #[test]
    fn renderer_projects_secret_findings_without_values() {
        let report = json!({
            "ok": false,
            "issues": [{
                "id": "secret_scan_finding",
                "rule": "api_key_assignment",
                "path": "leaky-role.md",
                "line": 7,
            }],
            "suggested_repairs": [{"issue": "secret_scan_finding", "action": "remove secret finding at leaky-role.md:7"}],
            "secret_scan": {
                "findings": [{
                    "rule": "api_key_assignment",
                    "path": "leaky-role.md",
                    "line": 7,
                    "match_excerpt": "OPENAI_API_KEY=secret"
                }]
            }
        });
        let output = render("doctor", &report);
        assert!(output.contains("issue: at=leaky-role.md:7; id=secret_scan_finding"));
        assert!(!output.contains("match_excerpt"));
        assert!(!output.contains("OPENAI_API_KEY=secret"));
    }
}
