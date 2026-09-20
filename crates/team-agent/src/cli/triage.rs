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
    let mut lines = vec![bounded(format!(
        "{command}: {}",
        if ok { "ok" } else { "needs attention" }
    ))];

    if command == "doctor" {
        for finding in array_items(
            report
                .get("secret_scan")
                .and_then(|scan| scan.get("findings")),
        ) {
            let Some(object) = finding.as_object() else {
                continue;
            };
            let (Some(rule), Some(path), Some(line)) = (
                object.get("rule").and_then(Value::as_str),
                object.get("path").and_then(Value::as_str),
                object.get("line").and_then(Value::as_u64),
            ) else {
                continue;
            };
            lines.push(bounded(format!("warn: {rule} in {path}:{line}")));
        }
    }

    for issue in array_items(report.get("issues")) {
        lines.push(bounded(format!("issue: {}", summarize(issue, false))));
    }
    for repair in array_items(report.get("suggested_repairs")) {
        lines.push(bounded(format!("repair: {}", summarize(repair, true))));
    }
    if let Some(error) = report.get("error").filter(|value| !value.is_null()) {
        lines.push(bounded(format!("detail: {}", summarize(error, false))));
    }
    if lines.len() == 1 {
        lines.push(bounded("detail: no issues detected".to_string()));
    }
    lines.join("\n")
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
        let output = render("diagnose", &report);
        assert!(output.contains("issue: short_issue"));
        assert!(output.contains("issue: code=object_issue"));
        assert!(output.contains("repair: action=run repair"));
        assert!(!output.contains("providers:"));
        assert!(!output.contains("runtime:"));
    }

    #[test]
    fn renderer_sanitizes_and_bounds_utf8_lines() {
        let report = json!({"ok": false, "issues": ["问题\n".to_string() + &"x".repeat(200)]});
        for line in render("doctor", &report).lines() {
            assert!(line.len() <= LINE_LIMIT_BYTES);
            assert!(!line.chars().any(char::is_control));
        }
    }

    #[test]
    fn renderer_projects_secret_findings_without_values() {
        let report = json!({
            "ok": false,
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
        assert!(output.contains("warn: api_key_assignment in leaky-role.md:7"));
        assert!(!output.contains("match_excerpt"));
        assert!(!output.contains("OPENAI_API_KEY=secret"));
    }
}
