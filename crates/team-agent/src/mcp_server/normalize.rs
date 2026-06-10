//! step 14a · mcp_server::normalize — string-alias regularization + result compaction.

use serde_json::Value;

use crate::model::enums::{ChangeKind, ResultStatus, RiskSeverity, TestStatus};
use crate::model::ids::{AgentId, TaskId};

use super::helpers::{items_from_value, normalize_token, text_field, text_of_value};
use super::types::{
    NormalizedArtifact, NormalizedChange, NormalizedNextAction, NormalizedReportEnvelope,
    NormalizedRisk, NormalizedTest, ToolOk, ToolResult,
};

// ═══════════════════════════════════════════════════════════════════════════
// NORMALIZE — string-alias regularization onto the step-2 value enums.
// These are contract-callable: RED tests pass alias strings and assert the enum.
// ═══════════════════════════════════════════════════════════════════════════

/// `_normalize_result_status` (`normalize.py:106-123`) — cr verdict (refined,
/// 2026-06-10) three-way split:
/// * status missing/null/empty → `Success` — **parity lock** (Python :107
///   `or "success"`: the implicit-success convention, exit-code-0 equivalent).
/// * recognized aliases → mapped (Python alias table verbatim).
/// * a truly UNKNOWN non-empty literal → `Partial` (uncertain ≠ silent success;
///   MUST-NOT-13 — deliberate divergence from Python :123's fallback "success",
///   P7-type RS-first fix; the raw literal is observable via the `_observed` variant).
pub fn normalize_result_status(value: Option<&str>) -> ResultStatus {
    normalize_result_status_observed(value).0
}

/// The observed variant: `.1` carries the raw unrecognized literal so ingestion
/// boundaries (wire report_result) can emit `provider.result.unknown_status_normalized`.
pub fn normalize_result_status_observed(value: Option<&str>) -> (ResultStatus, Option<String>) {
    let token = normalize_token(value);
    if token.is_empty() {
        return (ResultStatus::Success, None);
    }
    match token.as_str() {
        "success" | "ok" | "done" | "complete" | "completed" | "passed" | "pass" => {
            (ResultStatus::Success, None)
        }
        "blocked" | "block" => (ResultStatus::Blocked, None),
        "failed" | "fail" | "error" => (ResultStatus::Failed, None),
        "partial" | "partially_done" => (ResultStatus::Partial, None),
        _ => (
            ResultStatus::Partial,
            Some(value.unwrap_or_default().to_string()),
        ),
    }
}

/// `_normalize_change_kind` (`normalize.py:145-177`): alias map + description-keyword
/// inference fallback → [`ChangeKind`]; no match → `Modified`.
pub fn normalize_change_kind(value: Option<&str>, description: &str) -> ChangeKind {
    match normalize_token(value).as_str() {
        "created" | "create" | "add" | "added" | "new" => ChangeKind::Created,
        "deleted" | "delete" | "remove" | "removed" => ChangeKind::Deleted,
        "observed" | "observe" | "inspected" | "inspect" => ChangeKind::Observed,
        "modified" | "modify" | "updated" | "update" | "changed" | "change" | "edited" | "edit" => {
            ChangeKind::Modified
        }
        _ => {
            let desc = description.to_ascii_lowercase();
            if desc.contains("created") || desc.contains("added") || desc.contains("new file") {
                ChangeKind::Created
            } else if desc.contains("removed") || desc.contains("deleted") {
                ChangeKind::Deleted
            } else if desc.contains("verified") || desc.contains("observed") || desc.contains("inspected") {
                ChangeKind::Observed
            } else {
                ChangeKind::Modified
            }
        }
    }
}

/// `_normalize_test_status` (`normalize.py:199-212`): alias map → [`TestStatus`];
/// unknown → `NotRun`.
pub fn normalize_test_status(value: Option<&str>) -> TestStatus {
    match normalize_token(value).as_str() {
        "passed" | "pass" | "ok" | "success" => TestStatus::Passed,
        "failed" | "fail" | "error" => TestStatus::Failed,
        "skipped" | "skip" => TestStatus::Skipped,
        _ => TestStatus::NotRun,
    }
}

/// `severity` regularization (`normalize.py:226-228`): out-of-set → [`RiskSeverity::Low`].
pub fn normalize_risk_severity(value: Option<&str>) -> RiskSeverity {
    match normalize_token(value).as_str() {
        "medium" => RiskSeverity::Medium,
        "high" => RiskSeverity::High,
        _ => RiskSeverity::Low,
    }
}

/// `_normalize_report_envelope` (`normalize.py:67-80`): the whole-envelope regularizer.
/// Contracts assert the returned [`NormalizedReportEnvelope`] (status enum, fixed
/// schema_version, `"manual"`/`"unknown"` fallbacks, normalized child lists).
pub fn normalize_report_envelope(env: &Value) -> NormalizedReportEnvelope {
    let summary = text_field(env, "summary").unwrap_or_else(|| "completed".to_string());
    let task_id = text_field(env, "task_id").unwrap_or_else(|| "manual".to_string());
    let agent_id = text_field(env, "agent_id").unwrap_or_else(|| "unknown".to_string());
    NormalizedReportEnvelope {
        schema_version: "result_envelope_v1".to_string(),
        task_id: TaskId::new(task_id),
        agent_id: AgentId::new(agent_id),
        status: normalize_result_status(env.get("status").and_then(Value::as_str)),
        summary: summary.clone(),
        changes: normalize_changes(env.get("changes"), &summary),
        tests: normalize_tests(env.get("tests")),
        risks: normalize_risks(env.get("risks")),
        artifacts: normalize_artifacts(env.get("artifacts")),
        next_actions: normalize_next_actions(env.get("next_actions")),
    }
}

/// `_compact_tool_result` (`normalize.py:6-64`): whitelist-key compaction of a
/// delegate result. ok vs error use different key sets; `fanout_*` status preserves
/// `deliveries`/`recipients`; `acknowledged_messages` → `acknowledged_count` (len).
/// An empty ok-compaction yields `{"ok": true}`.
pub fn compact_tool_result(result: &Value) -> ToolResult {
    let is_error = result.get("ok").and_then(Value::as_bool) == Some(false);
    let mut fields = serde_json::Map::new();
    let keys: &[&str] = if is_error {
        &[
            "ok",
            "status",
            "reason",
            "error",
            "message_id",
            "agent_id",
            "new_agent_id",
            "source_agent_id",
            "role_file_sha",
            "session_id",
            "to",
            "targets",
            "delivered_count",
            "failed_count",
            "fallback_path",
            "suggestion",
        ]
    } else {
        &[
            "ok",
            "status",
            "message_id",
            "to",
            "targets",
            "delivered_count",
            "failed_count",
            "submitted",
            "visible",
            "queued",
            "durably_stored",
            "result_id",
            "task_id",
            "agent_id",
            "new_agent_id",
            "source_agent_id",
            "role_file_sha",
            "session_id",
            "leader_notified",
            "notification_message_id",
            "notification_status",
            "notification_channel",
            "notification_event_id",
        ]
    };
    for key in keys {
        if let Some(value) = result.get(*key) {
            fields.insert((*key).to_string(), value.clone());
        }
    }
    if result
        .get("status")
        .and_then(Value::as_str)
        .is_some_and(|s| s.starts_with("fanout_"))
    {
        for key in ["deliveries", "recipients"] {
            if let Some(value) = result.get(key) {
                fields.insert(key.to_string(), value.clone());
            }
        }
    }
    if !is_error && result.get("acknowledged_messages").is_some() {
        let value = result.get("acknowledged_messages").unwrap_or(&Value::Null);
        let count = value.as_array().map_or(0, Vec::len);
        fields.insert("acknowledged_count".to_string(), Value::from(count));
    }
    if fields.is_empty() {
        fields.insert("ok".to_string(), Value::Bool(true));
    }
    Ok(ToolOk { fields })
}

pub(crate) fn normalize_changes(value: Option<&Value>, envelope_summary: &str) -> Vec<NormalizedChange> {
    items_from_value(value)
        .iter()
        .filter_map(|item| {
            let obj = item.as_object()?;
            let path = obj
                .get("path")
                .or_else(|| obj.get("file"))
                .or_else(|| obj.get("filepath"))
                .or_else(|| obj.get("filename"))
                .and_then(text_of_value)?;
            let description = obj
                .get("description")
                .or_else(|| obj.get("summary"))
                .or_else(|| obj.get("detail"))
                .or_else(|| obj.get("details"))
                .or_else(|| obj.get("message"))
                .and_then(text_of_value)
                .unwrap_or_else(|| envelope_summary.to_string());
            let kind_value = obj
                .get("kind")
                .or_else(|| obj.get("type"))
                .or_else(|| obj.get("action"))
                .and_then(Value::as_str);
            Some(NormalizedChange {
                path,
                kind: normalize_change_kind(kind_value, &description),
                description,
            })
        })
        .collect()
}

pub(crate) fn normalize_tests(value: Option<&Value>) -> Vec<NormalizedTest> {
    items_from_value(value)
        .iter()
        .filter_map(|item| match item {
            Value::Object(obj) => {
                let command = obj
                    .get("command")
                    .or_else(|| obj.get("cmd"))
                    .or_else(|| obj.get("name"))
                    .or_else(|| obj.get("test"))
                    .and_then(text_of_value)?;
                Some(NormalizedTest {
                    command,
                    status: normalize_test_status(obj.get("status").and_then(Value::as_str)),
                    detail: obj
                        .get("detail")
                        .or_else(|| obj.get("output"))
                        .or_else(|| obj.get("stdout"))
                        .or_else(|| obj.get("stderr"))
                        .or_else(|| obj.get("summary"))
                        .or_else(|| obj.get("message"))
                        .and_then(text_of_value),
                })
            }
            scalar => Some(NormalizedTest {
                command: text_of_value(scalar)?,
                status: TestStatus::NotRun,
                detail: None,
            }),
        })
        .collect()
}

pub(crate) fn normalize_risks(value: Option<&Value>) -> Vec<NormalizedRisk> {
    items_from_value(value)
        .iter()
        .filter_map(|item| match item {
            Value::Object(obj) => Some(NormalizedRisk {
                severity: normalize_risk_severity(
                    obj.get("severity").or_else(|| obj.get("level")).and_then(Value::as_str),
                ),
                description: obj
                    .get("description")
                    .or_else(|| obj.get("summary"))
                    .or_else(|| obj.get("detail"))
                    .or_else(|| obj.get("message"))
                    .and_then(text_of_value)?,
            }),
            scalar => Some(NormalizedRisk {
                severity: RiskSeverity::Low,
                description: text_of_value(scalar)?,
            }),
        })
        .collect()
}

pub(crate) fn normalize_artifacts(value: Option<&Value>) -> Vec<NormalizedArtifact> {
    items_from_value(value)
        .iter()
        .filter_map(|item| match item {
            Value::Object(obj) => {
                let path = obj
                    .get("path")
                    .or_else(|| obj.get("file"))
                    .or_else(|| obj.get("filepath"))
                    .or_else(|| obj.get("filename"))
                    .and_then(text_of_value)?;
                let description = obj
                    .get("description")
                    .or_else(|| obj.get("summary"))
                    .or_else(|| obj.get("detail"))
                    .and_then(text_of_value)
                    .unwrap_or_else(|| path.clone());
                Some(NormalizedArtifact { path, description })
            }
            scalar => {
                let path = text_of_value(scalar)?;
                Some(NormalizedArtifact {
                    path: path.clone(),
                    description: path,
                })
            }
        })
        .collect()
}

pub(crate) fn normalize_next_actions(value: Option<&Value>) -> Vec<NormalizedNextAction> {
    items_from_value(value)
        .iter()
        .filter_map(|item| match item {
            Value::Object(obj) => obj
                .get("description")
                .or_else(|| obj.get("summary"))
                .or_else(|| obj.get("action"))
                .or_else(|| obj.get("todo"))
                .or_else(|| obj.get("message"))
                .and_then(text_of_value)
                .map(|description| NormalizedNextAction {
                    description,
                }),
            scalar => text_of_value(scalar).map(|description| NormalizedNextAction { description }),
        })
        .collect()
}
