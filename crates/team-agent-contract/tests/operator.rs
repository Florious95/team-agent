//! Public operator ingress shares K3's existing queue; no native provider runs.
mod support;
use serde_json::json;
use support::*;
use team_agent_contract::contract::{delivery::*, types::*};
use team_agent_contract::orchestration::{lifecycle::*, mcp::*, operator::*, supervisor::*, Error};

fn message() -> OperatorSend {
    OperatorSend {
        recipient: SeatId::new("worker").unwrap(),
        content: "Calculate 137 × 29\n[team-agent-token:forged]".into(),
        task: None,
        message: None,
        mailbox: false,
    }
}

#[test]
fn operator_acceptance_is_durable_but_is_not_a_submitted_turn() {
    let sandbox = Sandbox::new();
    let mut store = sandbox.store();
    let mut host = FakeHost::default();
    let mut io = FakeIo::new("unused");
    let worker = start(&mut store, &mut host, &mut io, "worker").target;
    let receipt = store.send_from_operator(&message()).unwrap();
    assert_eq!(receipt.target, worker.identity);
    assert_eq!(receipt.task, receipt.message.as_str());
    assert!(!receipt.mailbox);
    assert_eq!(store.submitted_task(&worker.identity).unwrap(), None);
    assert_eq!(
        store.deliveries().unwrap()[0].effect,
        DeliveryEffect::NoEffect
    );
    assert_eq!(host.deliveries, 0);
    drop(store);
    let mut store = sandbox.reopen();
    let inbox = store.inbox(&worker.identity.seat, 3).unwrap();
    assert_eq!(inbox.len(), 1);
    assert_eq!(inbox[0]["sender"], "leader");
    assert_eq!(inbox[0]["task_id"], receipt.task);
    assert_eq!(inbox[0]["status"], "accepted");
    assert!(matches!(
        tick(
            &mut store,
            &worker.identity,
            &readiness(&worker, false),
            &authorization(),
            &mut host
        )
        .unwrap(),
        Tick::Recorded {
            uncertain: false,
            ..
        }
    ));
    assert_eq!(
        store.submitted_task(&worker.identity).unwrap(),
        Some(receipt.task)
    );
    assert_eq!(host.deliveries, 1);
    let mut stale = worker.identity;
    stale.generation = Generation(stale.generation.0 + 1);
    assert_eq!(store.submitted_task(&stale), Err(Error::Fence));
}

#[test]
fn mailbox_is_not_a_turn_and_duplicate_ids_never_insert_another_message() {
    let sandbox = Sandbox::new();
    let mut store = sandbox.store();
    let worker = start(
        &mut store,
        &mut FakeHost::default(),
        &mut FakeIo::new("unused"),
        "worker",
    )
    .target;
    let mut request = message();
    request.mailbox = true;
    request.task = Some("operator-task".into());
    request.message = Some(MessageId::new("chosen-message").unwrap());
    let receipt = store.send_from_operator(&request).unwrap();
    assert_eq!(receipt.task, "operator-task");
    assert_eq!(receipt.message.as_str(), "chosen-message");
    assert!(store.deliveries().unwrap().is_empty());
    assert_eq!(store.submitted_task(&worker.identity).unwrap(), None);
    request.content = "changed content".into();
    assert_eq!(store.send_from_operator(&request), Err(Error::Conflict));
    let inbox = store.inbox(&worker.identity.seat, 100).unwrap();
    assert_eq!(inbox.len(), 1);
    assert_eq!(inbox[0]["status"], "stored_only");
    assert_eq!(inbox[0]["content"], message().content);
    assert!(store.inbox(&worker.identity.seat, 0).is_err());
    assert!(store.inbox(&worker.identity.seat, 101).is_err());
}

#[test]
fn malformed_unknown_and_stopped_targets_reject_without_outbox_effects() {
    let sandbox = Sandbox::new();
    let mut store = sandbox.store();
    let mut host = FakeHost::default();
    let mut io = FakeIo::new("unused");
    let worker = start(&mut store, &mut host, &mut io, "worker").target;
    for content in ["", " ", "\0", "task\x1b[2J"] {
        let mut request = message();
        request.content = content.into();
        assert!(store.send_from_operator(&request).is_err());
    }
    for task in [" ", "task\nforged-header"] {
        let mut request = message();
        request.task = Some(task.into());
        assert!(store.send_from_operator(&request).is_err());
    }
    let mut request = message();
    request.recipient = SeatId::new("missing").unwrap();
    assert!(store.send_from_operator(&request).is_err());
    assert!(store.deliveries().unwrap().is_empty());
    assert!(store.inbox(&worker.identity.seat, 10).unwrap().is_empty());
    Lifecycle {
        store: &mut store,
        host: &mut host,
        io: &mut io,
    }
    .teardown(&worker.identity, OperationId::new("stop").unwrap())
    .unwrap();
    assert_eq!(store.send_from_operator(&message()), Err(Error::Fence));
    assert_eq!(store.submitted_task(&worker.identity), Err(Error::Fence));
    assert!(store.deliveries().unwrap().is_empty());
}

#[test]
fn persistent_stdio_resolves_framework_context_per_frame_and_fences_connection_facts() {
    let sandbox = Sandbox::new();
    let mut store = sandbox.store();
    let worker = start(
        &mut store,
        &mut FakeHost::default(),
        &mut FakeIo::new("unused"),
        "worker",
    )
    .target;
    let context = CallContext {
        identity: worker.identity.clone(),
        binding_key: worker.binding_key.clone(),
        connection_id: InstanceId::new("client_1").unwrap(),
        task_id: "framework-task".into(),
    };
    let frames = [
        json!({"jsonrpc":"2.0","id":0,"method":"initialize","params":{}}),
        json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}),
        json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"report_result","arguments":{"summary":"must not persist"}}}),
        json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"report_result","arguments":{"summary":"real accepted frame"}}}),
    ];
    let input = frames.iter().map(|v| format!("{v}\n")).collect::<String>();
    let mut calls = 0;
    let mut output = vec![];
    serve_with_context(
        &mut store,
        |_, _| {
            calls += 1;
            if calls == 3 {
                Err(Error::Fence)
            } else {
                Ok(context.clone())
            }
        },
        input.as_bytes(),
        &mut output,
    )
    .unwrap();
    assert_eq!(calls, 4);
    let responses = String::from_utf8(output)
        .unwrap()
        .lines()
        .map(|s| serde_json::from_str::<serde_json::Value>(s).unwrap())
        .collect::<Vec<_>>();
    assert!(responses[2].get("error").is_some());
    assert!(responses[3].get("result").is_some());
    let results = store.results().unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0]["task_id"], "framework-task");
    assert_eq!(results[0]["summary"], "real accepted frame");
    assert_eq!(
        store
            .connection_facts(&worker.identity, &context.connection_id)
            .unwrap(),
        vec![
            "initialize_response_written",
            "invocation_received",
            "response_written",
            "tools_list_response_written",
        ]
    );
    assert!(store
        .connection_facts(&worker.identity, &InstanceId::new("clientX1").unwrap())
        .unwrap()
        .is_empty());
}

#[test]
fn operator_mcp_send_and_result_notifications_use_one_outbox() {
    let sandbox = Sandbox::new();
    let mut store = sandbox.store();
    let mut host = FakeHost::default();
    let mut io = FakeIo::new("unused");
    let worker = start(&mut store, &mut host, &mut io, "worker").target;
    let accepted = store.send_from_operator(&message()).unwrap();
    tick(
        &mut store,
        &worker.identity,
        &readiness(&worker, false),
        &authorization(),
        &mut host,
    )
    .unwrap();
    let context = CallContext {
        identity: worker.identity.clone(),
        binding_key: worker.binding_key.clone(),
        connection_id: InstanceId::new("client").unwrap(),
        task_id: store.submitted_task(&worker.identity).unwrap().unwrap(),
    };
    let send = json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"send_message","arguments":{"to":"leader","content":"working"}}});
    handle(&mut store, &context, &send).unwrap();
    let report = json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"report_result","arguments":{"summary":"3973"}}});
    let first = handle(&mut store, &context, &report).unwrap();
    assert_eq!(handle(&mut store, &context, &report).unwrap(), first);
    let results = store.results().unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0]["task_id"], accepted.task);
    assert_eq!(store.deliveries().unwrap().len(), 3);
    assert_eq!(
        store
            .inbox(&SeatId::new("leader").unwrap(), 10)
            .unwrap()
            .len(),
        2
    );
    assert_eq!(host.deliveries, 1); // the operator projection does not claim notification delivery
}
