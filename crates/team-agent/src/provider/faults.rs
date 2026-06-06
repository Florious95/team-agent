//! abnormal track 消费的 fault/approval facts 抽取(`provider_state.read_fault_facts`)。

use super::types::{FactKind, FaultFact, Signature, TurnId};
use super::Provider;

/// `provider_state.read_fault_facts(provider, records)`(`__init__.py:46`)。
/// 从已解析 records 抽取 fault/approval facts(kind ∈ {error, failed, approval}),
/// 携 `(signature, turn_id)` C8 dedup key。`api_error` 无 ids 时 `turn_id == None`。
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
        Provider::GeminiCli | Provider::Fake => None,
    }
}

fn claude_explicit_error_fact(record: &serde_json::Value) -> Option<FaultFact> {
    if record.get("type").and_then(serde_json::Value::as_str) != Some("system")
        || record.get("subtype").and_then(serde_json::Value::as_str) != Some("api_error")
        || record.get("level").and_then(serde_json::Value::as_str) != Some("error")
    {
        return None;
    }
    Some(FaultFact {
        signature: Signature::new("api_error"),
        turn_id: record
            .get("sessionId")
            .or_else(|| record.get("parentUuid"))
            .or_else(|| record.get("uuid"))
            .and_then(serde_json::Value::as_str)
            .map(TurnId::new),
        kind: FactKind::Error,
    })
}

fn codex_explicit_error_fact(record: &serde_json::Value) -> Option<FaultFact> {
    if record.get("method").and_then(serde_json::Value::as_str) != Some("turn/completed") {
        return None;
    }
    let turn = record.get("params").and_then(|p| p.get("turn"))?;
    if turn.get("status").and_then(serde_json::Value::as_str) != Some("failed") {
        return None;
    }
    Some(FaultFact {
        signature: Signature::new("turn_failed"),
        turn_id: turn.get("id").and_then(serde_json::Value::as_str).map(TurnId::new),
        kind: FactKind::Failed,
    })
}

fn fault_fact(provider: Provider, record: &serde_json::Value) -> Option<FaultFact> {
    match provider {
        Provider::Claude | Provider::ClaudeCode => claude_fault_fact(record),
        Provider::Codex => codex_fault_fact(record),
        Provider::GeminiCli | Provider::Fake => None,
    }
}

fn claude_fault_fact(record: &serde_json::Value) -> Option<FaultFact> {
    if record.get("type").and_then(serde_json::Value::as_str) == Some("system")
        && record.get("subtype").and_then(serde_json::Value::as_str) == Some("api_error")
        && record.get("level").and_then(serde_json::Value::as_str) == Some("error")
    {
        return Some(FaultFact {
            signature: Signature::new("api_error"),
            turn_id: record
                .get("sessionId")
                .or_else(|| record.get("parentUuid"))
                .or_else(|| record.get("uuid"))
                .and_then(serde_json::Value::as_str)
                .map(TurnId::new),
            kind: FactKind::Error,
        });
    }
    if record.get("type").and_then(serde_json::Value::as_str) == Some("user")
        && claude_has_error_tool_result(record)
    {
        return Some(FaultFact {
            signature: Signature::new("tool_result_is_error"),
            turn_id: record
                .get("parentUuid")
                .or_else(|| record.get("uuid"))
                .and_then(serde_json::Value::as_str)
                .map(TurnId::new),
            kind: FactKind::Error,
        });
    }
    None
}

fn claude_has_error_tool_result(record: &serde_json::Value) -> bool {
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

fn codex_fault_fact(record: &serde_json::Value) -> Option<FaultFact> {
    let method = record.get("method").and_then(serde_json::Value::as_str);
    if method == Some("turn/completed") {
        let turn = record.get("params").and_then(|p| p.get("turn"))?;
        if turn.get("status").and_then(serde_json::Value::as_str) == Some("failed") {
            return Some(FaultFact {
                signature: Signature::new("turn_failed"),
                turn_id: turn.get("id").and_then(serde_json::Value::as_str).map(TurnId::new),
                kind: FactKind::Failed,
            });
        }
    }
    if method.is_some_and(|m| m.ends_with("requestApproval")) {
        return Some(FaultFact {
            signature: Signature::new("approval_required"),
            turn_id: record
                .get("params")
                .and_then(|p| p.get("turnId"))
                .or_else(|| record.get("params").and_then(|p| p.get("turn_id")))
                .and_then(serde_json::Value::as_str)
                .map(TurnId::new),
            kind: FactKind::Approval,
        });
    }
    None
}
