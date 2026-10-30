//! Explicit test-side business adapter for native lifecycle/outbox fixtures.
//! The production MCP framing, identity checks and lifecycle implementation remain
//! in use. SQL business rows here are controlled inputs to the native supervisor,
//! NOT an implementation or acceptance test of the M1 station inbox.
use rusqlite::{params, OptionalExtension, Transaction, TransactionBehavior};
use serde_json::{json, Value};
use std::io::{BufRead, Write};
use team_agent_contract::contract::types::*;
use team_agent_contract::orchestration::{
    forward::{ForwardIntent, ForwardPayload, ForwardState, FrameworkPeer},
    mcp::{self, CallContext},
    store::{ContractStore, SeatRecord, SeatStatus},
    Error,
};

pub fn serve<R: BufRead, W: Write>(
    store: &mut ContractStore,
    context: &CallContext,
    reader: R,
    writer: W,
) -> Result<(), Error> {
    serve_with_diagnostics(store, |_, _| Ok(context.clone()), reader, writer, |_, _| {})
}
pub fn serve_with_context<R: BufRead, W: Write, F>(
    store: &mut ContractStore,
    resolve: F,
    reader: R,
    writer: W,
) -> Result<(), Error>
where
    F: FnMut(&mut ContractStore, &Value) -> Result<CallContext, Error>,
{
    serve_with_diagnostics(store, resolve, reader, writer, |_, _| {})
}
pub fn serve_with_diagnostics<R: BufRead, W: Write, F, D>(
    store: &mut ContractStore,
    resolve: F,
    reader: R,
    writer: W,
    diagnostic: D,
) -> Result<(), Error>
where
    F: FnMut(&mut ContractStore, &Value) -> Result<CallContext, Error>,
    D: FnMut(mcp::RequestRejectionStage, &Error),
{
    let pending = std::rc::Rc::new(std::cell::RefCell::new(None));
    let observed = ObservedWriter {
        writer,
        pending: pending.clone(),
        root: store.root().to_owned(),
        scope: store.scope().clone(),
        endpoint: store.endpoint().to_owned(),
    };
    mcp::serve_with_handler(
        store,
        resolve,
        reader,
        observed,
        diagnostic,
        |store, context, request| {
            let response = handle(store, context, request)?;
            if response.is_some()
                && matches!(
                    request.pointer("/params/name").and_then(Value::as_str),
                    Some("send_message" | "report_result")
                )
            {
                *pending.borrow_mut() = Some((context.clone(), request.clone()));
            }
            Ok(response)
        },
    )
}

// Native protocol fixtures explicitly observe successful writes. Production M1
// business dispatch intentionally does NOT perform this SQLite write. A failed
// underlying write/flush must never yield a response-written fixture fact.
struct ObservedWriter<W> {
    writer: W,
    pending: std::rc::Rc<std::cell::RefCell<Option<(CallContext, Value)>>>,
    root: std::path::PathBuf,
    scope: ScopeId,
    endpoint: String,
}
impl<W: Write> Write for ObservedWriter<W> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.writer.write(bytes)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.writer.flush()?;
        if let Some((context, request)) = self.pending.borrow_mut().take() {
            let mut store =
                ContractStore::open(&self.root, self.scope.clone(), self.endpoint.clone())
                    .map_err(std::io::Error::other)?;
            mcp::response_written(&mut store, &context, &request).map_err(std::io::Error::other)?;
        }
        Ok(())
    }
}

pub fn handle(
    store: &mut ContractStore,
    context: &CallContext,
    request: &Value,
) -> Result<Option<Value>, Error> {
    match mcp::handle(store, context, request) {
        Err(Error::Invalid("direct business handler required")) => {}
        other => return other,
    }
    let tool = request["params"]["name"]
        .as_str()
        .ok_or(Error::Invalid("tool name"))?;
    mcp::authorize_direct_call(store, context, request)?;
    let args = &request["params"]["arguments"];
    validate_arguments(tool, args)?;
    let mut db = rusqlite::Connection::open(store.root().join("contract.db"))?;
    db.busy_timeout(std::time::Duration::from_secs(5))?;
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let seat = seat(&tx, context.identity.seat.as_str())?.ok_or(Error::Fence)?;
    if seat.identity != context.identity
        || seat.identity.scope != *store.scope()
        || seat.binding_key != context.binding_key
        || matches!(seat.status, SeatStatus::Stopped | SeatStatus::Unknown)
    {
        return Err(Error::Fence);
    }
    if tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM contract_leases WHERE seat=?1)",
        [context.identity.seat.as_str()],
        |r| r.get::<_, bool>(0),
    )? {
        return Err(Error::Conflict);
    }
    let caller = serde_json::to_string(&context.identity)?;
    let key = format!("{}:{}", context.connection_id.as_str(), request["id"]);
    let fingerprint = json!({"name":tool,"arguments":args}).to_string();
    let previous: Option<(String, String)> = tx
        .query_row(
            "SELECT request,response FROM contract_calls WHERE caller=?1 AND call_key=?2",
            params![caller, key],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    if let Some((original, response)) = previous {
        if original != fingerprint {
            return Err(Error::Conflict);
        }
        return Ok(Some(tool_response(
            &request["id"],
            serde_json::from_str::<Value>(&response)?,
        )));
    }
    tx.execute(
        "INSERT OR IGNORE INTO contract_facts VALUES(?1,?2,'invocation_received')",
        params![caller, key],
    )?;
    let payload = if tool == "send_message" {
        let to = string(args, "to")?;
        let content = string(args, "content")?;
        let mailbox = args["mailbox"].as_bool().unwrap_or(false);
        let recipients = if to == "*" {
            let mut query = tx.prepare("SELECT seat FROM contract_seats WHERE seat<>?1 UNION SELECT recipient FROM contract_framework_peers WHERE recipient<>?1 ORDER BY 1")?;
            let rows =
                query.query_map([context.identity.seat.as_str()], |r| r.get::<_, String>(0))?;
            rows.collect::<Result<Vec<_>, _>>()?
        } else {
            vec![to.to_owned()]
        };
        let mut ids = Vec::new();
        let mut forwarded = false;
        for recipient in recipients {
            forwarded |= peer(&tx, &recipient)?.is_some();
            ids.push(message(
                &tx,
                context,
                &recipient,
                content,
                "{\"sink\":\"leader\",\"class\":\"message\"}",
                mailbox,
            )?);
        }
        json!({"ok":true,"status":if forwarded {"forward_pending"} else if mailbox {"stored_only"} else {"queued"},"message_ids":ids})
    } else {
        let envelope = normalize_result(context, args)?;
        let previous: Option<(String, String)> = tx.query_row("SELECT result_id,envelope FROM results WHERE owner_team_id=?1 AND task_id=?2 AND agent_id=?3",
            params![store.scope().as_str(), context.task_id, context.identity.seat.as_str()], |r| Ok((r.get(0)?, r.get(1)?))).optional()?;
        let id = if let Some((id, original)) = previous {
            if serde_json::from_str::<Value>(&original)? != envelope {
                return Err(Error::Conflict);
            }
            id
        } else {
            let id = next_id(&tx, "result")?;
            tx.execute(
                "INSERT INTO results VALUES(?1,?2,?3,?4,?5,?6)",
                params![
                    id,
                    store.scope().as_str(),
                    context.task_id,
                    context.identity.seat.as_str(),
                    envelope.to_string(),
                    envelope["status"].as_str().unwrap_or("completed")
                ],
            )?;
            let route: Option<String> =
                tx.query_row("SELECT result_route FROM contract_meta", [], |r| r.get(0))?;
            if let Some(route) = route {
                forward(
                    &tx,
                    ForwardIntent {
                        id: id.clone(),
                        source: context.identity.clone(),
                        route,
                        payload: ForwardPayload::Result {
                            result_id: id.clone(),
                            envelope: envelope.clone(),
                        },
                        state: ForwardState::Pending,
                        receipt: None,
                    },
                )?;
            } else if envelope
                .pointer("/presentation/sink")
                .and_then(Value::as_str)
                == Some("leader")
            {
                message(
                    &tx,
                    context,
                    "leader",
                    &envelope.to_string(),
                    &envelope["presentation"].to_string(),
                    false,
                )?;
            }
            id
        };
        json!({"ok":true,"status":"persisted","result_id":id,"leader_notified":false})
    };
    tx.execute(
        "INSERT INTO contract_calls VALUES(?1,?2,?3,?4)",
        params![caller, key, fingerprint, payload.to_string()],
    )?;
    tx.commit()?;
    Ok(Some(tool_response(&request["id"], payload)))
}

fn tool_response(id: &Value, payload: Value) -> Value {
    json!({"jsonrpc":"2.0","id":id,"result":{
        "content":[{"type":"text","text":payload.to_string()}],"structuredContent":payload,"isError":false
    }})
}

fn seat(tx: &Transaction<'_>, id: &str) -> Result<Option<SeatRecord>, Error> {
    let raw: Option<String> = tx
        .query_row(
            "SELECT record FROM contract_seats WHERE seat=?1",
            [id],
            |r| r.get(0),
        )
        .optional()?;
    raw.map(|raw| serde_json::from_str(&raw).map_err(Error::from))
        .transpose()
}
fn peer(tx: &Transaction<'_>, id: &str) -> Result<Option<FrameworkPeer>, Error> {
    let raw: Option<String> = tx
        .query_row(
            "SELECT record FROM contract_framework_peers WHERE recipient=?1",
            [id],
            |r| r.get(0),
        )
        .optional()?;
    raw.map(|raw| serde_json::from_str(&raw).map_err(Error::from))
        .transpose()
}
fn next_id(tx: &Transaction<'_>, prefix: &str) -> Result<String, Error> {
    let number: i64 = tx.query_row(
        "UPDATE contract_sequence SET value=value+1 WHERE id=1 RETURNING value",
        [],
        |r| r.get(0),
    )?;
    let incarnation: String =
        tx.query_row("SELECT incarnation FROM contract_meta", [], |r| r.get(0))?;
    if incarnation.len() != 32 || !incarnation.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(Error::Corrupt);
    }
    Ok(format!("{prefix}-{incarnation}-{number}"))
}
fn forward(tx: &Transaction<'_>, intent: ForwardIntent) -> Result<(), Error> {
    tx.execute(
        "INSERT INTO contract_forwards(id,state,record) VALUES(?1,'Pending',?2)",
        params![intent.id, serde_json::to_string(&intent)?],
    )?;
    Ok(())
}
fn message(
    tx: &Transaction<'_>,
    context: &CallContext,
    recipient: &str,
    content: &str,
    presentation: &str,
    mailbox: bool,
) -> Result<String, Error> {
    let external = peer(tx, recipient)?;
    let target = seat(tx, recipient)?;
    if external.is_none() && target.is_none() && recipient != "leader" {
        return Err(Error::Invalid("unknown recipient"));
    }
    if target
        .as_ref()
        .is_some_and(|target| target.identity.scope != context.identity.scope)
    {
        return Err(Error::Fence);
    }
    let id = next_id(tx, "msg")?;
    tx.execute("INSERT INTO messages(message_id,owner_team_id,task_id,sender,recipient,status,content,presentation) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
        params![id, context.identity.scope.as_str(), context.task_id, context.identity.seat.as_str(), recipient,
            if external.is_some() {"forward_pending"} else if mailbox {"stored_only"} else {"accepted"}, content, presentation])?;
    if let Some(peer) = external {
        forward(
            tx,
            ForwardIntent {
                id: id.clone(),
                source: context.identity.clone(),
                route: peer.route,
                payload: ForwardPayload::Message {
                    message: MessageId::new(&id)?,
                    task: Some(context.task_id.clone()),
                    sender: context.identity.seat.as_str().into(),
                    recipient: recipient.into(),
                    content: content.into(),
                    mailbox,
                },
                state: ForwardState::Pending,
                receipt: None,
            },
        )?;
    } else if !mailbox {
        tx.execute(
            "INSERT INTO contract_outbox VALUES(?1,?2,'queued',NULL,'\"NoEffect\"')",
            params![
                id,
                target
                    .map(|target| serde_json::to_string(&target.identity))
                    .transpose()?
                    .unwrap_or_else(|| "null".into())
            ],
        )?;
    }
    Ok(id)
}

// Fixture validation preserves the old native queue assertions. Public business
// validation is tested at the station handler, not by this adapter.
fn validate_arguments(tool: &str, args: &Value) -> Result<(), Error> {
    let contract = mcp::tools_contract();
    let schema = contract
        .as_array()
        .and_then(|tools| tools.iter().find(|entry| entry["name"] == tool))
        .ok_or(Error::Invalid("unknown tool"))?;
    let args = args.as_object().ok_or(Error::Invalid("arguments object"))?;
    for (key, value) in args {
        let property = schema["inputSchema"]["properties"]
            .get(key)
            .ok_or(Error::Invalid("unexpected argument"))?;
        if !match property["type"].as_str() {
            Some("string") => value.is_string(),
            Some("boolean") => value.is_boolean(),
            Some("object") => value.is_object(),
            Some("array") => value
                .as_array()
                .is_some_and(|array| array.iter().all(Value::is_object)),
            _ => false,
        } {
            return Err(Error::Invalid("argument type"));
        }
    }
    for key in schema["inputSchema"]["required"]
        .as_array()
        .ok_or(Error::Corrupt)?
    {
        if !args.contains_key(key.as_str().ok_or(Error::Corrupt)?) {
            return Err(Error::Invalid("required argument"));
        }
    }
    Ok(())
}
fn string<'a>(args: &'a Value, key: &str) -> Result<&'a str, Error> {
    args.get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
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
            .is_some_and(|value| value.as_str() != Some(expected))
        {
            return Err(Error::Fence);
        }
        object.insert(key.into(), json!(expected));
    }
    if object.contains_key("sender") || object.contains_key("owner_team_id") {
        return Err(Error::Fence);
    }
    for field in ["summary", "status"] {
        if object.get(field).is_some_and(|value| !value.is_string()) {
            return Err(Error::Invalid("result field type"));
        }
    }
    for field in ["changes", "tests", "risks", "artifacts", "next_actions"] {
        if object.get(field).is_some_and(|value| {
            !value
                .as_array()
                .is_some_and(|array| array.iter().all(Value::is_object))
        }) {
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
        .any(|key| !matches!(key.as_str(), "sink" | "class" | "case_id"))
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
        || presentation
            .get("case_id")
            .is_some_and(|value| !value.is_string())
    {
        return Err(Error::Invalid("presentation"));
    }
    Ok(result)
}
