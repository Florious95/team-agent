//! Controlled server-journal fixtures, not native liveness or consumption proof.
//!
//! M4: re-expressed with get_team_status. Business tools (send_message,
//! report_result) are handled by the team-agent host through the station send core,
//! and the SQL business adapter that used to write their invocation facts is retired.
//! The pairing, flush and generation properties of the file-backed fact journal are
//! kept on the one tool this crate still handles.
mod support;
use support::direct_business::{serve, handle};
use std::io::{Cursor, Write};
use serde_json::{json, Value};
use support::*;
use team_agent_contract::contract::{plan::*, types::*};
use team_agent_contract::orchestration::{lifecycle::*, mcp::*, store::*};

fn setup() -> (Sandbox, ContractStore, SeatRecord) {
    let sandbox = Sandbox::new();
    let mut store = sandbox.store();
    let seat = start(&mut store, &mut FakeHost::default(), &mut FakeIo::new("start"), "worker").target;
    (sandbox, store, seat)
}
fn context(seat: &SeatRecord, connection: &str) -> CallContext {
    CallContext {
        identity: seat.identity.clone(), binding_key: seat.binding_key.clone(),
        connection_id: InstanceId::new(connection).unwrap(), task_id: "controlled-task".into(),
    }
}
fn call(id: u32, name: &str, arguments: Value) -> Value {
    json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{"name":name,"arguments":arguments}})
}
fn status(id: u32) -> Value {
    call(id, "get_team_status", json!({}))
}
fn exchange(store: &mut ContractStore, context: &CallContext, request: &Value) -> Value {
    let mut output = vec![];
    serve(store, context, Cursor::new(format!("{request}\n").into_bytes()), &mut output).unwrap();
    serde_json::from_slice(&output).unwrap()
}
fn tools(store: &mut ContractStore, context: &CallContext) -> Vec<LogicalTool> {
    store.completed_connection_tools(&context.identity, &context.connection_id).unwrap()
}

#[test]
fn handshake_rejections_and_unpaired_ids_never_promote_a_tool() {
    let (_sandbox, mut store, seat) = setup();
    let context = context(&seat, "connection");
    for method in ["initialize", "tools/list"] {
        assert!(exchange(&mut store, &context, &json!({"jsonrpc":"2.0","id":method,"method":method})).get("result").is_some());
    }
    assert!(tools(&mut store, &context).is_empty());
    assert_eq!(exchange(&mut store, &context, &call(1, "get_team_status", json!({"unexpected":5})))["error"]["code"], -32602);
    assert!(tools(&mut store, &context).is_empty());

    // Invocation A is committed, but no response has yet been flushed for A.
    handle(&mut store, &context, &status(2)).unwrap();
    // Synthetic response-only B cannot be paired with invocation A.
    response_written(&mut store, &context, &status(3)).unwrap();
    assert!(tools(&mut store, &context).is_empty());
    assert!(exchange(&mut store, &context, &status(2)).get("result").is_some());
    assert_eq!(tools(&mut store, &context), vec![LogicalTool::GetTeamStatus]);
    // No client-consumed facts.
    assert!(!store.protocol_facts().unwrap().iter().any(|fact| fact.contains("consum")));
}

#[test]
fn failed_flush_is_not_a_completed_tool_roundtrip() {
    struct RejectFlush;
    impl Write for RejectFlush {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> { Ok(bytes.len()) }
        fn flush(&mut self) -> std::io::Result<()> { Err(std::io::Error::other("controlled flush failure")) }
    }
    let (_sandbox, mut store, seat) = setup();
    let context = context(&seat, "connection");
    let request = format!("{}\n", status(1));
    assert!(serve(&mut store, &context, Cursor::new(request.into_bytes()), RejectFlush).is_err());
    assert!(store.protocol_facts().unwrap().contains(&"invocation_received".to_string()));
    assert!(!store.protocol_facts().unwrap().contains(&"response_written".to_string()));
    assert!(tools(&mut store, &context).is_empty());
}

#[test]
fn tool_pairs_are_not_combined_across_connections_prefixes_or_generations() {
    let (_sandbox, mut store, seat) = setup();
    assert_eq!(
        InstanceId::new("connection:child"),
        Err(ContractError::Invalid("InstanceId"))
    );
    let first = context(&seat, "connection");
    let other = context(&seat, "connection-child");
    assert_ne!(first.connection_id, other.connection_id);
    assert!(other.connection_id.as_str().starts_with(first.connection_id.as_str()));
    // A prefixed connection is a separate ledger: the same rpc id there is a distinct call.
    assert!(exchange(&mut store, &first, &status(1)).get("result").is_some());
    assert_eq!(tools(&mut store, &first), vec![LogicalTool::GetTeamStatus]);
    assert!(tools(&mut store, &other).is_empty());
    assert!(exchange(&mut store, &other, &status(1)).get("result").is_some());
    assert_eq!(tools(&mut store, &other), vec![LogicalTool::GetTeamStatus]);
    assert_eq!(tools(&mut store, &first), vec![LogicalTool::GetTeamStatus]);

    let mut request = request(store.root(), "worker");
    request.operation = Operation::Resume;
    request.identity.instance = InstanceId::new("worker-2").unwrap();
    request.identity.generation = Generation(2);
    request.resume = Some(ResumeRequest { binding: seat.session.clone().unwrap(), expected_source: seat.identity.clone() });
    let descriptor = descriptor();
    let hooks = hooks();
    let next = Lifecycle { store: &mut store, host: &mut FakeHost::default(), io: &mut FakeIo::new("restart") }
        .startup(&Adapter { descriptor: &descriptor, hooks: &hooks, catalog: None }, &request,
            routing("worker"), OperationId::new("restart").unwrap()).unwrap();
    assert_eq!(next.outcome, Outcome::Committed);
    assert_fenced(store.completed_connection_tools(&first.identity, &first.connection_id), "current_instance_or_generation_replaced");
    assert!(tools(&mut store, &context(&next.target, "connection")).is_empty());
}
