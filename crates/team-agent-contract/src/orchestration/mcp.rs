//! Three logical MCP tools. Transport-injected identity is never read from arguments.
use super::store::{current, unleased, ContractStore, SeatStatus};
use super::Error;
use crate::contract::types::*;
use rusqlite::{params, OptionalExtension, TransactionBehavior};
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
    // Business tools require the host's unified station handler. Never silently
    // fall back to SQLite messages/outbox when a handler was not installed.
    if matches!(tool, "send_message" | "report_result") {
        return Err(Error::Invalid("direct business handler required"));
    }
    validate_arguments(tool, args)?;
    let scope = store.scope().clone();
    let tx = store
        .connection
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    let seat = current(&tx, &scope, &context.identity)?;
    if seat.binding_key != context.binding_key
        || matches!(seat.status, SeatStatus::Stopped | SeatStatus::Unknown)
    {
        return Err(Error::Fence);
    }
    unleased(&tx, &context.identity.seat)?;
    let caller = serde_json::to_string(&context.identity)?;
    let request = json!({"name":tool,"arguments":args}).to_string();
    let previous: Option<(String, String)> = tx
        .query_row(
            "SELECT request,response FROM contract_calls WHERE caller=?1 AND call_key=?2",
            params![caller, key],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    if let Some((original, response)) = previous {
        if serde_json::from_str::<Value>(&request)? != serde_json::from_str::<Value>(&original)? {
            return Err(Error::Conflict);
        }
        return Ok(serde_json::from_str(&response)?);
    }
    tx.execute(
        "INSERT OR IGNORE INTO contract_facts VALUES(?1,?2,'invocation_received')",
        params![caller, key],
    )?;
    let response = match tool {
        "get_team_status" => {
            let mut statement = tx.prepare("SELECT record FROM contract_seats ORDER BY seat")?;
            let rows = statement.query_map([], |r| r.get::<_, String>(0))?;
            let mut seats = vec![];
            for row in rows {
                let record: super::store::SeatRecord = serde_json::from_str(&row?)?;
                seats.push(json!({"identity":record.identity,"provider":record.provider,"status":record.status}));
            }
            let mut statement =
                tx.prepare("SELECT record FROM contract_framework_peers ORDER BY recipient")?;
            let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
            for row in rows {
                let peer: super::forward::FrameworkPeer = serde_json::from_str(&row?)?;
                seats.push(json!({"id":peer.recipient,"provider":peer.provider,"status":"delegated_to_framework"}));
            }
            json!({"ok":true,"scope":scope,"agents":seats})
        }
        _ => return Err(Error::Invalid("unknown tool")),
    };
    tx.execute(
        "INSERT INTO contract_calls VALUES(?1,?2,?3,?4)",
        params![caller, key, request, response.to_string()],
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
    let scope = store.scope().clone();
    let tx = store
        .connection
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    let seat = current(&tx, &scope, &context.identity)?;
    if seat.binding_key != context.binding_key {
        return Err(Error::Fence);
    }
    let fact = match method {
        "initialize" => "initialize_response_written",
        "tools/list" => "tools_list_response_written",
        "tools/call" => "response_written",
        _ => return Ok(()),
    };
    tx.execute(
        "INSERT OR IGNORE INTO contract_facts VALUES(?1,?2,?3)",
        params![
            serde_json::to_string(&context.identity)?,
            format!("{}:{}", context.connection_id.as_str(), id),
            fact
        ],
    )?;
    tx.commit()?;
    Ok(())
}

/// Authenticate a direct business call without requiring a scheduler turn or
/// writing a message/outbox row. The observation records actual transport input.
pub fn authorize_direct_call(store: &mut ContractStore, context: &CallContext, request: &Value) -> Result<(), Error> {
    if request.get("jsonrpc").and_then(Value::as_str) != Some("2.0") { return Err(Error::Invalid("jsonrpc")); }
    request.get("id").filter(|id| id.is_string() || id.is_i64() || id.is_u64()).ok_or(Error::Invalid("rpc id"))?;
    let seat = current(&store.connection, store.scope(), &context.identity)?;
    if seat.binding_key != context.binding_key || matches!(seat.status, SeatStatus::Stopped | SeatStatus::Unknown) {
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
    store: &mut ContractStore, mut resolve_context: F, mut reader: R, mut writer: W,
    mut rejected: D, mut handler: H,
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
            Ok(context) => (handler(store, context, &request), RequestRejectionStage::Handler),
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
            if success && !matches!(request.pointer("/params/name").and_then(Value::as_str),
                Some("send_message" | "report_result")) {
                response_written(store, &context?, &request)?;
            }
        }
    }
}
