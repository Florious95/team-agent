//! Cross-runtime forwarding never creates a second native delivery queue.
mod support;
use serde_json::{json, Value};
use support::*;
use team_agent_contract::contract::types::*;
use team_agent_contract::orchestration::{
    forward::*, mcp::*, operator::*, store::ContractStore, Error,
};

fn fixture() -> (Sandbox, ContractStore, CallContext) {
    let sandbox = Sandbox::new();
    let mut store = sandbox.store();
    let seat = start(
        &mut store,
        &mut FakeHost::default(),
        &mut FakeIo::new("unused"),
        "worker",
    )
    .target;
    let context = CallContext {
        identity: seat.identity,
        binding_key: seat.binding_key,
        connection_id: InstanceId::new("connection").unwrap(),
        task_id: "assigned-task".into(),
    };
    store
        .set_framework_routes(
            &[
                FrameworkPeer {
                    recipient: "legacy".into(),
                    route: "root-scope/legacy".into(),
                    provider: "pi".into(),
                },
                FrameworkPeer {
                    recipient: "leader".into(),
                    route: "root-scope/leader".into(),
                    provider: "codex".into(),
                },
            ],
            Some("root-scope/results"),
        )
        .unwrap();
    (sandbox, store, context)
}
fn rpc(id: u64, name: &str, arguments: Value) -> Value {
    json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{"name":name,"arguments":arguments}})
}

#[test]
fn external_message_is_durable_but_only_destination_port_can_accept_it() {
    let (sandbox, mut store, context) = fixture();
    let call = rpc(
        1,
        "send_message",
        json!({"to":"legacy","content":"Do the real task"}),
    );
    let response = handle(&mut store, &context, &call).unwrap();
    assert!(response.is_some());
    assert!(store.deliveries().unwrap().is_empty());
    let pending = store.pending_forwards(10).unwrap();
    assert_eq!(pending.len(), 1);
    assert!(
        matches!(&pending[0].payload, ForwardPayload::Message { recipient, content, .. }
        if recipient == "legacy" && content == "Do the real task")
    );
    let id = pending[0].id.clone();
    assert_eq!(
        store.claim_forward(&id).unwrap().state,
        ForwardState::InFlight
    );
    assert!(store.claim_forward(&id).is_err());
    drop(store);
    let mut store = sandbox.reopen();
    assert!(store.pending_forwards(10).unwrap().is_empty());
    assert_eq!(store.forward(&id).unwrap().state, ForwardState::InFlight);
    store
        .finish_forward(&id, ForwardState::Unknown, json!({"code":"response_lost"}))
        .unwrap();
    assert!(store.claim_forward(&id).is_err());
    assert_eq!(store.forward(&id).unwrap().state, ForwardState::Unknown);
    assert!(store.deliveries().unwrap().is_empty());
}

#[test]
fn result_forwarding_is_once_and_does_not_fabricate_leader_delivery() {
    let (_sandbox, mut store, context) = fixture();
    let report = rpc(
        1,
        "report_result",
        json!({"summary":"Completed the assigned task"}),
    );
    let response = handle(&mut store, &context, &report).unwrap();
    assert_eq!(handle(&mut store, &context, &report).unwrap(), response);
    handle(
        &mut store,
        &context,
        &rpc(
            2,
            "report_result",
            json!({"summary":"Completed the assigned task"}),
        ),
    )
    .unwrap();
    let pending = store.pending_forwards(10).unwrap();
    assert_eq!(pending.len(), 1);
    assert!(
        matches!(&pending[0].payload, ForwardPayload::Result { result_id, envelope }
        if result_id == &pending[0].id && envelope["task_id"] == "assigned-task")
    );
    assert!(store.deliveries().unwrap().is_empty());
    let id = pending[0].id.clone();
    store.claim_forward(&id).unwrap();
    store
        .finish_forward(
            &id,
            ForwardState::Accepted,
            json!({"persisted":true,"leader_notified":false}),
        )
        .unwrap();
    let finished = store.forward(&id).unwrap();
    assert_eq!(finished.state, ForwardState::Accepted);
    assert_eq!(finished.receipt.unwrap()["leader_notified"], false);
    assert_eq!(
        handle(
            &mut store,
            &context,
            &rpc(3, "report_result", json!({"summary":"Changed result"}))
        ),
        Err(Error::Conflict)
    );
}

#[test]
fn mixed_broadcast_has_one_native_queue_and_external_intents_not_fake_seats() {
    let (_sandbox, mut store, context) = fixture();
    start(
        &mut store,
        &mut FakeHost::default(),
        &mut FakeIo::new("unused"),
        "other-native",
    );
    handle(
        &mut store,
        &context,
        &rpc(1, "send_message", json!({"to":"*","content":"Shared task"})),
    )
    .unwrap();
    assert_eq!(store.seats().unwrap().len(), 2);
    assert_eq!(store.deliveries().unwrap().len(), 1);
    assert_eq!(store.pending_forwards(10).unwrap().len(), 2);
    assert!(store
        .set_framework_routes(
            &[FrameworkPeer {
                recipient: "worker".into(),
                route: "foreign".into(),
                provider: "pi".into()
            }],
            None
        )
        .is_err());
    assert!(handle(
        &mut store,
        &context,
        &rpc(
            2,
            "send_message",
            json!({"to":"legacy","content":"bad","sender":"leader"})
        )
    )
    .is_err());
    assert_eq!(store.pending_forwards(10).unwrap().len(), 2);
}

#[test]
fn captured_routes_are_not_retargeted_and_json_member_order_is_not_a_new_call() {
    let (_sandbox, mut store, context) = fixture();
    let first = rpc(
        1,
        "send_message",
        serde_json::from_str("{\"to\":\"legacy\",\"content\":\"same\"}").unwrap(),
    );
    let reordered = rpc(
        1,
        "send_message",
        serde_json::from_str("{\"content\":\"same\",\"to\":\"legacy\"}").unwrap(),
    );
    assert_eq!(
        handle(&mut store, &context, &first).unwrap(),
        handle(&mut store, &context, &reordered).unwrap()
    );
    store
        .set_framework_routes(
            &[FrameworkPeer {
                recipient: "legacy".into(),
                route: "new-framework-route".into(),
                provider: "pi".into(),
            }],
            None,
        )
        .unwrap();
    let pending = store.pending_forwards(10).unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].route, "root-scope/legacy");
}

#[test]
fn generated_ids_are_unique_across_store_incarnations_and_framework_sender_is_preserved() {
    let (_a, mut a, ca) = fixture();
    let (_b, mut b, cb) = fixture();
    let request = OperatorSend {
        recipient: SeatId::new("worker").unwrap(),
        content: "Task".into(),
        task: None,
        message: None,
        mailbox: false,
    };
    let sender = SeatId::new("legacy-peer").unwrap();
    let ar = a.send_from_framework(&request, &sender).unwrap();
    let br = b.send_from_framework(&request, &sender).unwrap();
    assert_ne!(ar.message, br.message);
    assert_eq!(
        a.inbox(&ca.identity.seat, 10).unwrap()[0]["sender"],
        "legacy-peer"
    );
    assert_eq!(
        b.inbox(&cb.identity.seat, 10).unwrap()[0]["sender"],
        "legacy-peer"
    );
}
