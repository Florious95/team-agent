//! Test double for the host business plane (M4).
//!
//! Production business tools (`send_message`, `report_result`) are not handled by
//! this crate: `mcp::handle` refuses them with "direct business handler required"
//! and the team-agent host routes them through the station send core. This crate
//! keeps the MCP framing, the identity/binding fences and the lifecycle, which
//! the remaining tests exercise through the production entry points below.
//!
//! The double authorizes a business call through the production fence
//! (`mcp::authorize_direct_call`) and acknowledges it without persisting anything.
//! There is deliberately no message, outbox, result or forward plane to observe:
//! that plane was SQLite-backed and is retired. Assertions about it were excised,
//! not ported (STAGE4-M4-TEST-EXCISION-CLASSIFICATION.md).
use serde_json::{json, Value};
use std::io::{BufRead, Write};
use team_agent_contract::orchestration::{
    mcp::{self, CallContext},
    store::ContractStore,
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
    mcp::serve_with_handler(store, resolve, reader, writer, diagnostic, handle)
}

/// Production handling first; only a business tool that production refuses for
/// lack of a host handler reaches the double, and only after the production fence.
pub fn handle(store: &mut ContractStore, context: &CallContext, request: &Value) -> Result<Option<Value>, Error> {
    match mcp::handle(store, context, request) {
        Err(Error::Invalid("direct business handler required")) => {}
        other => return other,
    }
    mcp::authorize_direct_call(store, context, request)?;
    let ack = json!({"ok": true, "status": "test_double_ack"});
    Ok(Some(json!({"jsonrpc": "2.0", "id": request["id"],
        "result": {"isError": false, "content": [{"type": "text", "text": ack.to_string()}]}})))
}
