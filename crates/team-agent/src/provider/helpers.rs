//! provider 跨子模块共享 helper:JSONL 解析 / session_id 抽取 / 正则编译。

use super::types::{ProviderError, StatusPatterns};

pub(crate) fn parse_jsonl_records(text: &str) -> Vec<serde_json::Value> {
    if text.trim().is_empty() {
        return Vec::new();
    }
    text.lines()
        .filter_map(|line| {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                None
            } else {
                serde_json::from_str::<serde_json::Value>(trimmed).ok()
            }
        })
        .collect()
}

pub(crate) fn find_session_id(record: &serde_json::Value) -> Option<String> {
    if let Some(s) = record.get("sessionId").and_then(serde_json::Value::as_str) {
        return Some(s.to_string());
    }
    if let Some(s) = record
        .get("session_id")
        .and_then(serde_json::Value::as_str)
    {
        return Some(s.to_string());
    }
    record
        .get("session_meta")
        .and_then(|v| v.get("payload"))
        .or_else(|| record.get("payload"))
        .and_then(|v| v.get("id"))
        .and_then(serde_json::Value::as_str)
        .map(ToString::to_string)
}

pub(crate) fn patterns(idle: &str, processing: &str, error: &str) -> Result<StatusPatterns, ProviderError> {
    Ok(StatusPatterns {
        idle: compile_regex(idle)?,
        processing: compile_regex(processing)?,
        error: compile_regex(error)?,
    })
}

fn compile_regex(pattern: &str) -> Result<regex::Regex, ProviderError> {
    regex::Regex::new(pattern).map_err(|e| ProviderError::Command(format!("regex {pattern}: {e}")))
}
