//! abnormal track 消费的 fault/approval facts 抽取(`provider_state.read_fault_facts`)。

use super::types::{FactKind, FaultFact, Signature, TurnId};
use super::Provider;

/// `provider_state.read_fault_facts(provider, records)`(`__init__.py:46`)。
/// 从已解析 records 抽取 fault/approval facts(kind ∈ {error, failed, approval}),
/// 携 `(signature, turn_id)` C8 dedup key。旧 `api_error` 无 ids 时 `turn_id == None`。
pub fn read_fault_facts(records: &[serde_json::Value], provider: Provider) -> Vec<FaultFact> {
    records
        .iter()
        .filter_map(|record| fault_fact(provider, record))
        .collect()
}

pub fn explicit_error_fact(record: &serde_json::Value, provider: Provider) -> Option<FaultFact> {
    match provider {
        Provider::Codex => codex_explicit_error_fact(record),
        Provider::Claude | Provider::ClaudeCode => claude_explicit_error_fact(record),
        // copilot 一期不接 jsonl 真相源(C-3-5)。
        Provider::Copilot | Provider::GeminiCli | Provider::Fake => None,
    }
}

pub(crate) fn claude_explicit_error_fact(record: &serde_json::Value) -> Option<FaultFact> {
    if claude_record_is_system_api_error(record) {
        return Some(FaultFact::new(
            Signature::new("api_error"),
            claude_system_api_error_turn_id(record),
            FactKind::Error,
        ));
    }
    if claude_record_is_assistant_api_error(record) {
        return Some(
            FaultFact::new(
                Signature::new("api_error"),
                claude_assistant_api_error_turn_id(record),
                FactKind::Error,
            )
            .with_api_error_details(
                record
                    .get("apiErrorStatus")
                    .and_then(serde_json::Value::as_i64),
                non_empty_string_field(record, "error"),
                non_empty_string_field(record, "requestId"),
                non_empty_string_field(record, "uuid"),
            ),
        );
    }
    None
}

fn codex_explicit_error_fact(record: &serde_json::Value) -> Option<FaultFact> {
    if record.get("method").and_then(serde_json::Value::as_str) != Some("turn/completed") {
        return None;
    }
    let turn = record.get("params").and_then(|p| p.get("turn"))?;
    if turn.get("status").and_then(serde_json::Value::as_str) != Some("failed") {
        return None;
    }
    Some(FaultFact::new(
        Signature::new("turn_failed"),
        turn.get("id")
            .and_then(serde_json::Value::as_str)
            .map(TurnId::new),
        FactKind::Failed,
    ))
}

fn fault_fact(provider: Provider, record: &serde_json::Value) -> Option<FaultFact> {
    match provider {
        Provider::Claude | Provider::ClaudeCode => claude_fault_fact(record),
        Provider::Codex => codex_fault_fact(record),
        Provider::Copilot | Provider::GeminiCli | Provider::Fake => None,
    }
}

fn claude_fault_fact(record: &serde_json::Value) -> Option<FaultFact> {
    if let Some(fact) = claude_explicit_error_fact(record) {
        return Some(fact);
    }
    if record.get("type").and_then(serde_json::Value::as_str) == Some("user")
        && claude_record_has_error_tool_result(record)
    {
        return Some(FaultFact::new(
            Signature::new("tool_result_is_error"),
            record
                .get("parentUuid")
                .or_else(|| record.get("uuid"))
                .and_then(serde_json::Value::as_str)
                .map(TurnId::new),
            FactKind::Error,
        ));
    }
    None
}

pub(crate) fn claude_record_has_error_tool_result(record: &serde_json::Value) -> bool {
    record
        .get("message")
        .and_then(|m| m.get("content"))
        .and_then(serde_json::Value::as_array)
        .is_some_and(|items| {
            items.iter().any(|item| {
                item.get("type").and_then(serde_json::Value::as_str) == Some("tool_result")
                    && item.get("is_error").and_then(serde_json::Value::as_bool) == Some(true)
            })
        })
}

fn claude_record_is_system_api_error(record: &serde_json::Value) -> bool {
    record.get("type").and_then(serde_json::Value::as_str) == Some("system")
        && record.get("subtype").and_then(serde_json::Value::as_str) == Some("api_error")
        && record.get("level").and_then(serde_json::Value::as_str) == Some("error")
}

fn claude_system_api_error_turn_id(record: &serde_json::Value) -> Option<TurnId> {
    record
        .get("sessionId")
        .or_else(|| record.get("parentUuid"))
        .or_else(|| record.get("uuid"))
        .and_then(serde_json::Value::as_str)
        .map(TurnId::new)
}

fn claude_record_is_assistant_api_error(record: &serde_json::Value) -> bool {
    record.get("type").and_then(serde_json::Value::as_str) == Some("assistant")
        && record
            .get("message")
            .and_then(|message| message.get("role"))
            .and_then(serde_json::Value::as_str)
            == Some("assistant")
        && record
            .get("isApiErrorMessage")
            .and_then(serde_json::Value::as_bool)
            == Some(true)
        && (record
            .get("apiErrorStatus")
            .and_then(serde_json::Value::as_i64)
            .is_some()
            || non_empty_string_field(record, "error").is_some()
            || non_empty_string_field(record, "requestId").is_some())
}

fn claude_assistant_api_error_turn_id(record: &serde_json::Value) -> Option<TurnId> {
    record
        .get("uuid")
        .or_else(|| record.get("parentUuid"))
        .or_else(|| record.get("sessionId"))
        .and_then(serde_json::Value::as_str)
        .map(TurnId::new)
}

fn non_empty_string_field(record: &serde_json::Value, key: &str) -> Option<String> {
    record
        .get(key)
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn codex_fault_fact(record: &serde_json::Value) -> Option<FaultFact> {
    let method = record.get("method").and_then(serde_json::Value::as_str);
    if method == Some("turn/completed") {
        let turn = record.get("params").and_then(|p| p.get("turn"))?;
        if turn.get("status").and_then(serde_json::Value::as_str) == Some("failed") {
            return Some(FaultFact::new(
                Signature::new("turn_failed"),
                turn.get("id")
                    .and_then(serde_json::Value::as_str)
                    .map(TurnId::new),
                FactKind::Failed,
            ));
        }
    }
    if method.is_some_and(|m| m.ends_with("requestApproval")) {
        return Some(FaultFact::new(
            Signature::new("approval_required"),
            record
                .get("params")
                .and_then(|p| p.get("turnId"))
                .or_else(|| record.get("params").and_then(|p| p.get("turn_id")))
                .and_then(serde_json::Value::as_str)
                .map(TurnId::new),
            FactKind::Approval,
        ));
    }
    None
}
