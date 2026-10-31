//! Three logical MCP tools. Transport-injected identity is never read from arguments.
use super::projection::Key;
use super::store::{current, unleased, ContractStore, SeatStatus};
use super::Error;
use crate::contract::types::*;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct CallRecord {
    pub caller: InstanceIdentity,
    pub key: String,
    pub request: Value,
    pub response: Value,
}
#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct FactRecord {
    pub caller: InstanceIdentity,
    pub key: String,
    pub facts: BTreeSet<String>,
}
#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct ToolEvidence {
    pub caller: InstanceIdentity,
    pub connection: InstanceId,
    pub calls: std::collections::BTreeMap<String, String>,
    pub facts: BTreeSet<String>,
}
impl ToolEvidence {
    pub(crate) fn key(identity: &InstanceIdentity, connection: &InstanceId) -> Result<Key, Error> {
        Ok(Key::new(
            "tool_evidence",
            call_address(identity, connection.as_str())?,
        ))
    }
}
pub(crate) fn call_address(identity: &InstanceIdentity, key: &str) -> Result<String, Error> {
    Ok(format!("{}\n{key}", serde_json::to_string(identity)?))
}

use serde_json::{json, Value};
use std::io::{BufRead, Write};

pub const MAX_FRAME_BYTES: usize = 1024 * 1024;

/// Created by the scoped server/transport, not by tools/call JSON. Connection ID
/// must be unique per transport connection: JSON-RPC IDs alone are not durable
/// idempotency keys and may legitimately be reused on another connection.
#[derive(Clone)]
pub struct CallContext {
    pub identity: InstanceIdentity,
    pub binding_key: String,
    pub connection_id: InstanceId,
    pub task_id: String,
}

pub fn tools_contract() -> Value {
    serde_json::from_str(include_str!("tools-contract.json")).expect("checked-in tool contract")
}

pub fn handle(
    store: &mut ContractStore,
    context: &CallContext,
    request: &Value,
) -> Result<Option<Value>, Error> {
    if request.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return Err(Error::Invalid("jsonrpc"));
    }
    let method = request
        .get("method")
        .and_then(Value::as_str)
        .ok_or(Error::Invalid("method"))?;
    // Notifications may not invoke a tool and never receive an RPC response.
    if method.starts_with("notifications/") {
        return Ok(None);
    }
    let id = request
        .get("id")
        .filter(|v| v.is_string() || v.is_i64() || v.is_u64())
        .ok_or(Error::Invalid("rpc id"))?;
    let seat = store.assert_current(&context.identity)?;
    if seat.binding_key != context.binding_key
        || matches!(seat.status, SeatStatus::Stopped | SeatStatus::Unknown)
    {
        return Err(Error::Fence);
    }
    let result = match method {
        "initialize" => {
            json!({"protocolVersion": request.pointer("/params/protocolVersion").and_then(Value::as_str).unwrap_or("2024-11-05"),
            "capabilities":{"tools":{}},"serverInfo":{"name":"team_agent_contract","version":"0.1.0"}})
        }
        "tools/list" => json!({"tools":tools_contract()}),
        "tools/call" => {
            let params = request.get("params").ok_or(Error::Invalid("params"))?;
            let name = params
                .get("name")
                .and_then(Value::as_str)
                .ok_or(Error::Invalid("tool name"))?;
            let arguments = params.get("arguments").cloned().unwrap_or(json!({}));
            let key = format!("{}:{}", context.connection_id.as_str(), id);
            let response = call(store, context, &key, name, &arguments)?;
            json!({"isError":false,"content":[{"type":"text","text":response.to_string()}]})
        }
        _ => {
            return Ok(Some(
                json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":"unknown method"}}),
            ))
        }
    };
    Ok(Some(json!({"jsonrpc":"2.0","id":id,"result":result})))
}

fn call(
    store: &mut ContractStore,
    context: &CallContext,
    key: &str,
    tool: &str,
    args: &Value,
) -> Result<Value, Error> {
    if matches!(tool, "send_message" | "report_result") {
        return Err(Error::Invalid("direct business handler required"));
    }
    validate_arguments(tool, args)?;
    let address = call_address(&context.identity, key)?;
    let call_key = Key::new("calls", &address);
    let fact_key = Key::new("facts", &address);
    let mut tx = store.files.write([
        call_key.clone(),
        fact_key.clone(),
        Key::new("seats", context.identity.seat.as_str()),
        Key::new("leases", context.identity.seat.as_str()),
    ])?;
    let seat = current(&tx, store.scope(), &context.identity)?;
    if seat.binding_key != context.binding_key
        || matches!(seat.status, SeatStatus::Stopped | SeatStatus::Unknown)
    {
        return Err(Error::Fence);
    }
    unleased(&tx, &context.identity.seat)?;
    let request = json!({"name":tool,"arguments":args});
    if let Some(previous) = tx.get::<CallRecord>(&call_key)? {
        if previous.caller != context.identity || previous.key != key || previous.request != request
        {
            return Err(Error::Conflict);
        }
        return Ok(previous.response);
    }
    let response = match tool {
        "get_team_status" => {
            let mut seats: Vec<Value> = tx
                .list::<super::store::SeatRecord>("seats")?
                .into_iter()
                .map(|s| json!({"identity":s.identity,"provider":s.provider,"status":s.status}))
                .collect();
            for peer in tx.list::<super::forward::FrameworkPeer>("peers")? {
                seats.push(json!({"id":peer.recipient,"provider":peer.provider,"status":"delegated_to_framework"}));
            }
            json!({"ok":true,"scope":store.scope(),"agents":seats})
        }
        _ => return Err(Error::Invalid("unknown tool")),
    };
    let mut facts = tx.get::<FactRecord>(&fact_key)?.unwrap_or(FactRecord {
        caller: context.identity.clone(),
        key: key.into(),
        facts: BTreeSet::new(),
    });
    facts.facts.insert("invocation_received".into());
    tx.put(fact_key, &facts)?;
    tx.put(
        call_key,
        &CallRecord {
            caller: context.identity.clone(),
            key: key.into(),
            request,
            response: response.clone(),
        },
    )?;
    tx.commit()?;
    Ok(response)
}

fn validate_arguments(tool: &str, args: &Value) -> Result<(), Error> {
    let contract = tools_contract();
    let schema = contract
        .as_array()
        .and_then(|v| v.iter().find(|v| v["name"] == tool))
        .ok_or(Error::Invalid("unknown tool"))?;
    let object = args.as_object().ok_or(Error::Invalid("arguments object"))?;
    let properties = schema["inputSchema"]["properties"]
        .as_object()
        .ok_or(Error::Corrupt)?;
    for (key, value) in object {
        let property = properties
            .get(key)
            .ok_or(Error::Invalid("unexpected argument"))?;
        let valid = match property["type"].as_str() {
            Some("string") => value.is_string(),
            Some("boolean") => value.is_boolean(),
            Some("object") => value.is_object(),
            Some("array") => value
                .as_array()
                .is_some_and(|a| a.iter().all(Value::is_object)),
            _ => false,
        };
        if !valid {
            return Err(Error::Invalid("argument type"));
        }
    }
    for required in schema["inputSchema"]["required"]
        .as_array()
        .ok_or(Error::Corrupt)?
    {
        if !object.contains_key(required.as_str().ok_or(Error::Corrupt)?) {
            return Err(Error::Invalid("required argument"));
        }
    }
    Ok(())
}
fn string<'a>(args: &'a Value, key: &str) -> Result<&'a str, Error> {
    args.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .ok_or(Error::Invalid("nonempty string required"))
}
fn normalize_result(context: &CallContext, args: &Value) -> Result<Value, Error> {
    let mut result = args.get("envelope").cloned().unwrap_or(json!({}));
    let object = result.as_object_mut().ok_or(Error::Invalid("envelope"))?;
    for (key, value) in args.as_object().ok_or(Error::Invalid("arguments"))? {
        if key != "envelope" {
            if object.get(key).is_some_and(|old| old != value) {
                return Err(Error::Conflict);
            }
            object.insert(key.clone(), value.clone());
        }
    }
    for (key, expected) in [
        ("agent_id", context.identity.seat.as_str()),
        ("task_id", context.task_id.as_str()),
    ] {
        if object
            .get(key)
            .is_some_and(|v| v.as_str() != Some(expected))
        {
            return Err(Error::Fence);
        }
        object.insert(key.into(), json!(expected));
    }
    if object.contains_key("sender") || object.contains_key("owner_team_id") {
        return Err(Error::Fence);
    }
    for field in ["summary", "status"] {
        if object.get(field).is_some_and(|v| !v.is_string()) {
            return Err(Error::Invalid("result field type"));
        }
    }
    for field in ["changes", "tests", "risks", "artifacts", "next_actions"] {
        if object
            .get(field)
            .is_some_and(|v| !v.as_array().is_some_and(|a| a.iter().all(Value::is_object)))
        {
            return Err(Error::Invalid("result array type"));
        }
    }
    object.insert("schema_version".into(), json!(1));
    object.entry("status").or_insert(json!("completed"));
    object
        .entry("presentation")
        .or_insert(json!({"sink":"leader","class":"stage_result"}));
    let presentation = object["presentation"]
        .as_object()
        .ok_or(Error::Invalid("presentation"))?;
    if presentation
        .keys()
        .any(|k| !matches!(k.as_str(), "sink" | "class" | "case_id"))
        || !matches!(
            presentation.get("sink").and_then(Value::as_str),
            Some("leader" | "casefile" | "silent")
        )
        || !matches!(
            presentation.get("class").and_then(Value::as_str),
            Some(
                "message"
                    | "progress"
                    | "stage_result"
                    | "stage_pass"
                    | "bounce"
                    | "blocking"
                    | "final_review"
                    | "timeout"
            )
        )
        || presentation.get("case_id").is_some_and(|v| !v.is_string())
    {
        return Err(Error::Invalid("presentation"));
    }
    Ok(result)
}

/// Record only after the transport actually wrote AND flushed the response.
/// No API here fabricates client consumption or caller presentation.
pub fn response_written(
    store: &mut ContractStore,
    context: &CallContext,
    request: &Value,
) -> Result<(), Error> {
    let id = request.get("id").ok_or(Error::Invalid("rpc id"))?;
    let method = request
        .get("method")
        .and_then(Value::as_str)
        .ok_or(Error::Invalid("method"))?;
    let fact = match method {
        "initialize" => "initialize_response_written",
        "tools/list" => "tools_list_response_written",
        "tools/call" => "response_written",
        _ => return Ok(()),
    };
    let key = format!("{}:{}", context.connection_id.as_str(), id);
    let address = call_address(&context.identity, &key)?;
    let fact_key = Key::new("facts", &address);
    let call_key = Key::new("calls", address);
    let evidence_key = ToolEvidence::key(&context.identity, &context.connection_id)?;
    let mut tx = store.files.write([
        fact_key.clone(),
        call_key.clone(),
        evidence_key.clone(),
        Key::new("seats", context.identity.seat.as_str()),
    ])?;
    let seat = current(&tx, store.scope(), &context.identity)?;
    if seat.binding_key != context.binding_key {
        return Err(Error::Fence);
    }
    let mut record = tx.get::<FactRecord>(&fact_key)?.unwrap_or(FactRecord {
        caller: context.identity.clone(),
        key,
        facts: BTreeSet::new(),
    });
    record.facts.insert(fact.into());
    let mut evidence = tx
        .get::<ToolEvidence>(&evidence_key)?
        .unwrap_or(ToolEvidence {
            caller: context.identity.clone(),
            connection: context.connection_id.clone(),
            calls: Default::default(),
            facts: Default::default(),
        });
    if evidence.caller != context.identity || evidence.connection != context.connection_id {
        return Err(Error::Fence);
    }
    evidence.facts.insert(fact.into());
    if fact == "response_written" && record.facts.contains("invocation_received") {
        let call: CallRecord = tx.get(&call_key)?.ok_or(Error::Corrupt)?;
        if call.caller != context.identity || call.key != record.key {
            return Err(Error::Fence);
        }
        let name = call.request["name"].as_str().ok_or(Error::Corrupt)?;
        if !matches!(name, "send_message" | "report_result" | "get_team_status") {
            return Err(Error::Corrupt);
        }
        evidence.calls.insert(name.into(), record.key.clone());
    }
    tx.put(evidence_key, &evidence)?;
    tx.put(fact_key, &record)?;
    tx.commit()
}

/// Authenticate a direct business call without requiring a scheduler turn or
/// writing a message/outbox row. The observation records actual transport input.
pub fn authorize_direct_call(
    store: &mut ContractStore,
    context: &CallContext,
    request: &Value,
) -> Result<(), Error> {
    if request.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return Err(Error::Invalid("jsonrpc"));
    }
    request
        .get("id")
        .filter(|id| id.is_string() || id.is_i64() || id.is_u64())
        .ok_or(Error::Invalid("rpc id"))?;
    let seat = current(&store.files, store.scope(), &context.identity)?;
    if seat.binding_key != context.binding_key
        || matches!(seat.status, SeatStatus::Stopped | SeatStatus::Unknown)
    {
        return Err(Error::Fence);
    }
    Ok(())
}

/// Bounded newline-delimited stdio transport. A failed/partial write preserves
/// durable results but cannot produce ResponseWritten. No implicit retry loop.
pub fn serve<R: BufRead, W: Write>(
    store: &mut ContractStore,
    context: &CallContext,
    reader: R,
    writer: W,
) -> Result<(), Error> {
    serve_with_context(store, |_, _| Ok(context.clone()), reader, writer)
}

/// The diagnostic observer receives only the rejection boundary and typed error,
/// never request, tool-argument, or response bytes. It cannot authorize a call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RequestRejectionStage {
    Context,
    Handler,
}

/// A persistent native transport resolves its fenced identity/task for each
/// frame. The resolver is framework-owned; tool arguments never confer identity.
/// This shares the same bounded framing, handlers and response-written boundary.
pub fn serve_with_context<R: BufRead, W: Write, F>(
    store: &mut ContractStore,
    resolve_context: F,
    reader: R,
    writer: W,
) -> Result<(), Error>
where
    F: FnMut(&mut ContractStore, &Value) -> Result<CallContext, Error>,
{
    serve_with_diagnostics(store, resolve_context, reader, writer, |_, _| {})
}

/// Preserve the wire result and durable boundaries while exposing a rejected
/// request's real error to the host's bounded diagnostic sink.
pub fn serve_with_diagnostics<R: BufRead, W: Write, F, D>(
    store: &mut ContractStore,
    resolve_context: F,
    reader: R,
    writer: W,
    rejected: D,
) -> Result<(), Error>
where
    F: FnMut(&mut ContractStore, &Value) -> Result<CallContext, Error>,
    D: FnMut(RequestRejectionStage, &Error),
{
    serve_with_handler(store, resolve_context, reader, writer, rejected, handle)
}

/// Share framing and identity fences while the host supplies its business plane.
pub fn serve_with_handler<R: BufRead, W: Write, F, D, H>(
    store: &mut ContractStore,
    mut resolve_context: F,
    mut reader: R,
    mut writer: W,
    mut rejected: D,
    mut handler: H,
) -> Result<(), Error>
where
    F: FnMut(&mut ContractStore, &Value) -> Result<CallContext, Error>,
    D: FnMut(RequestRejectionStage, &Error),
    H: FnMut(&mut ContractStore, &CallContext, &Value) -> Result<Option<Value>, Error>,
{
    loop {
        let mut frame = vec![];
        let read = std::io::Read::take(&mut reader, (MAX_FRAME_BYTES + 1) as u64)
            .read_until(b'\n', &mut frame)?;
        if read == 0 {
            return Ok(());
        }
        if read > MAX_FRAME_BYTES {
            return Err(Error::Invalid("MCP frame too large"));
        }
        let request: Value = serde_json::from_slice(&frame)?;
        let context = resolve_context(store, &request);
        let (handled, stage) = match &context {
            Ok(context) => (
                handler(store, context, &request),
                RequestRejectionStage::Handler,
            ),
            Err(error) => (Err(error.clone()), RequestRejectionStage::Context),
        };
        let (response, success) = match handled {
            Ok(response) => (response, true),
            Err(error) => {
                rejected(stage, &error);
                (
                    Some(
                        json!({"jsonrpc":"2.0","id":request.get("id").cloned().unwrap_or(Value::Null),
                    "error":{"code":-32602,"message":"request rejected"}}),
                    ),
                    false,
                )
            }
        };
        if let Some(response) = response {
            serde_json::to_writer(&mut writer, &response)?;
            writer.write_all(b"\n")?;
            writer.flush()?;
            // Direct business tools have durable inbox receipts. A response must
            // not reacquire a SQLite write transaction after physical delivery.
            if success
                && !matches!(
                    request.pointer("/params/name").and_then(Value::as_str),
                    Some("send_message" | "report_result")
                )
            {
                response_written(store, &context?, &request)?;
            }
        }
    }
}
