//! Failure windows are injected at the actual SQLite commit boundary, not by
//! bypassing lifecycle/handler/supervisor entry points.
#[path = "support/legacy_wire.rs"]
mod legacy_wire;
mod support;
use serde_json::{json, Value};
use support::*;
use team_agent_contract::contract::{plan::*, types::*};
use team_agent_contract::orchestration::{lifecycle::*, mcp::*, store::*, supervisor::*, Error};

fn context(seat: &SeatRecord) -> CallContext {
    CallContext {
        identity: seat.identity.clone(),
        binding_key: seat.binding_key.clone(),
        connection_id: InstanceId::new("connection").unwrap(),
        task_id: "task".into(),
    }
}
fn send() -> Value {
    json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"send_message","arguments":{"to":"worker","content":"once"}}})
}
fn fault_connection(store: &ContractStore, sql: &str) -> rusqlite::Connection {
    let connection = rusqlite::Connection::open(store.root().join("contract.db")).unwrap();
    connection.execute_batch(sql).unwrap();
    connection
}

#[test]
fn wire_snapshot_matches_the_unchanged_legacy_pure_contract() {
    let legacy = include_str!("../../team-agent/src/mcp_server/wire.rs");
    let original = legacy
        .split_once("fn tool_contract(")
        .unwrap()
        .1
        .split_once("pub(crate) fn dispatch_tool")
        .unwrap()
        .0;
    let oracle = include_str!("support/legacy_wire.rs");
    let frozen = oracle
        .split_once("// BEGIN FROZEN EXCERPT\nfn tool_contract(")
        .unwrap()
        .1
        .split_once("// END FROZEN EXCERPT")
        .unwrap()
        .0;
    // Ignore rustfmt whitespace only; the test oracle cannot silently drift from
    // the legacy implementation. Production never imports its runtime/tools.
    assert_eq!(
        original.split_whitespace().collect::<String>(),
        frozen.split_whitespace().collect::<String>()
    );
    assert_eq!(tools_contract(), json!(legacy_wire::expected()));
}

#[test]
fn f2_registration_failure_recovers_staging_without_spawning() {
    let sandbox = Sandbox::new();
    let mut store = sandbox.store();
    let mut host = FakeHost::default();
    let mut io = FakeIo::new("start");
    let fault = fault_connection(&store,"CREATE TRIGGER fault BEFORE INSERT ON contract_seats BEGIN SELECT RAISE(ABORT,'injected F2'); END;");
    let request = request(store.root(), "worker");
    let d = descriptor();
    let h = hooks();
    assert_eq!(
        Lifecycle {
            store: &mut store,
            host: &mut host,
            io: &mut io
        }
        .startup(
            &Adapter {
                descriptor: &d,
                hooks: &h,
                catalog: None
            },
            &request,
            routing("worker"),
            OperationId::new("start").unwrap()
        ),
        Err(Error::Database)
    );
    assert_eq!(host.spawned, 0);
    assert_eq!(io.writes, 1);
    fault.execute_batch("DROP TRIGGER fault").unwrap();
    let recovered = Lifecycle {
        store: &mut store,
        host: &mut host,
        io: &mut io,
    }
    .recover(&OperationId::new("start").unwrap())
    .unwrap();
    assert_eq!(recovered.outcome, Outcome::Compensated);
    assert_eq!(host.removed, 1);
    assert!(store.seats().unwrap().is_empty());
}

#[test]
fn f4_commit_failure_rolls_back_only_its_registration_after_stopping_owned_process() {
    let sandbox = Sandbox::new();
    let mut store = sandbox.store();
    let mut host = FakeHost::default();
    let mut io = FakeIo::new("start");
    let fault = fault_connection(&store,"CREATE TRIGGER fault BEFORE UPDATE ON contract_operations WHEN json_extract(NEW.record,'$.phase')='F4Commit' BEGIN SELECT RAISE(ABORT,'injected F4'); END;");
    let request = request(store.root(), "worker");
    let d = descriptor();
    let h = hooks();
    assert_eq!(
        Lifecycle {
            store: &mut store,
            host: &mut host,
            io: &mut io
        }
        .startup(
            &Adapter {
                descriptor: &d,
                hooks: &h,
                catalog: None
            },
            &request,
            routing("worker"),
            OperationId::new("start").unwrap()
        ),
        Err(Error::Database)
    );
    assert_eq!(host.spawned, 1);
    assert_eq!(host.stopped, 0);
    fault.execute_batch("DROP TRIGGER fault").unwrap();
    let recovered = Lifecycle {
        store: &mut store,
        host: &mut host,
        io: &mut io,
    }
    .recover(&OperationId::new("start").unwrap())
    .unwrap();
    assert_eq!(recovered.outcome, Outcome::Compensated);
    assert_eq!(host.stopped, 1);
    assert_eq!(host.spawned, 1);
    assert!(store.seats().unwrap().is_empty());
}

#[test]
fn f5_receipt_failure_finishes_committed_operation_without_replaying_or_compensating() {
    let sandbox = Sandbox::new();
    let mut store = sandbox.store();
    let mut host = FakeHost::default();
    let mut io = FakeIo::new("start");
    let fault = fault_connection(&store,"CREATE TRIGGER fault BEFORE UPDATE ON contract_operations WHEN json_extract(NEW.record,'$.phase')='F5Receipt' BEGIN SELECT RAISE(ABORT,'injected F5'); END;");
    let request = request(store.root(), "worker");
    let d = descriptor();
    let h = hooks();
    assert_eq!(
        Lifecycle {
            store: &mut store,
            host: &mut host,
            io: &mut io
        }
        .startup(
            &Adapter {
                descriptor: &d,
                hooks: &h,
                catalog: None
            },
            &request,
            routing("worker"),
            OperationId::new("start").unwrap()
        ),
        Err(Error::Database)
    );
    assert_eq!(host.spawned, 1);
    assert_eq!(host.stopped, 0);
    fault.execute_batch("DROP TRIGGER fault").unwrap();
    drop(store);
    let mut store = sandbox.reopen();
    let recovered = Lifecycle {
        store: &mut store,
        host: &mut host,
        io: &mut io,
    }
    .recover(&OperationId::new("start").unwrap())
    .unwrap();
    assert_eq!(recovered.outcome, Outcome::Committed);
    assert_eq!(host.stopped, 0);
    assert_eq!(host.spawned, 1);
    assert!(handle(&mut store, &context(&recovered.target), &send()).is_ok());
}

#[test]
fn result_notification_failure_rolls_back_result_and_dedup_in_one_transaction() {
    let sandbox = Sandbox::new();
    let mut store = sandbox.store();
    let mut host = FakeHost::default();
    let mut io = FakeIo::new("unused");
    let seat = start(&mut store, &mut host, &mut io, "worker").target;
    let _fault=fault_connection(&store,"CREATE TRIGGER fault BEFORE INSERT ON contract_outbox BEGIN SELECT RAISE(ABORT,'notification failure'); END;");
    let request = json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"report_result","arguments":{"summary":"done"}}});
    assert_eq!(
        handle(&mut store, &context(&seat), &request),
        Err(Error::Database)
    );
    assert!(store.results().unwrap().is_empty());
    assert!(store.deliveries().unwrap().is_empty());
    assert!(store.protocol_facts().unwrap().is_empty());
}

#[test]
fn lost_postsubmit_db_commit_never_becomes_noeffect_or_replay() {
    let sandbox = Sandbox::new();
    let mut store = sandbox.store();
    let mut host = FakeHost::default();
    let mut io = FakeIo::new("unused");
    let seat = start(&mut store, &mut host, &mut io, "worker").target;
    handle(&mut store, &context(&seat), &send()).unwrap();
    let fault=fault_connection(&store,"CREATE TRIGGER fault BEFORE UPDATE ON contract_outbox WHEN NEW.state='submitted' BEGIN SELECT RAISE(ABORT,'postsubmit failure'); END;");
    let sample = readiness(&seat, true);
    assert_eq!(
        tick(
            &mut store,
            &seat.identity,
            &sample,
            &authorization(),
            &mut host
        ),
        Err(Error::Database)
    );
    assert_eq!(host.deliveries, 1);
    fault.execute_batch("DROP TRIGGER fault").unwrap();
    drop(store);
    let mut store = sandbox.reopen();
    let delivery = &store.deliveries().unwrap()[0];
    assert_eq!(delivery.state, "in_flight");
    assert_eq!(
        delivery.effect,
        team_agent_contract::contract::delivery::DeliveryEffect::MayHaveSubmitted
    );
    assert!(tick(
        &mut store,
        &seat.identity,
        &sample,
        &authorization(),
        &mut host
    )
    .is_err());
    assert_eq!(host.deliveries, 1);
}

#[test]
fn concurrent_same_call_has_one_durable_message_and_one_response() {
    let sandbox = Sandbox::new();
    let mut store = sandbox.store();
    let mut host = FakeHost::default();
    let mut io = FakeIo::new("unused");
    let seat = start(&mut store, &mut host, &mut io, "worker").target;
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let mut threads = vec![];
    for _ in 0..2 {
        let root = sandbox.root.clone();
        let seat = seat.clone();
        let barrier = barrier.clone();
        threads.push(std::thread::spawn(move || {
            let mut store =
                ContractStore::open(&root, ScopeId::new("scope").unwrap(), "isolated-endpoint")
                    .unwrap();
            barrier.wait();
            handle(&mut store, &context(&seat), &send()).unwrap()
        }));
    }
    let a = threads.remove(0).join().unwrap();
    let b = threads.remove(0).join().unwrap();
    assert_eq!(a, b);
    assert_eq!(store.deliveries().unwrap().len(), 1);
}

#[test]
fn failed_restart_restores_stopped_parent_and_never_reuses_generation() {
    let sandbox = Sandbox::new();
    let mut store = sandbox.store();
    let mut host = FakeHost::default();
    let mut io = FakeIo::new("unused");
    let parent = start(&mut store, &mut host, &mut io, "worker").target;
    let mut request = request(store.root(), "worker");
    request.operation = Operation::Resume;
    request.identity.generation = Generation(2);
    request.identity.instance = InstanceId::new("worker-2").unwrap();
    request.resume = Some(ResumeRequest {
        binding: parent.session.clone().unwrap(),
        expected_source: parent.identity.clone(),
    });
    let d = descriptor();
    let h = hooks();
    host.fail_capture = true;
    io.operation = OperationId::new("restart").unwrap();
    let operation = Lifecycle {
        store: &mut store,
        host: &mut host,
        io: &mut io,
    }
    .startup(
        &Adapter {
            descriptor: &d,
            hooks: &h,
            catalog: None,
        },
        &request,
        routing("worker"),
        OperationId::new("restart").unwrap(),
    )
    .unwrap();
    assert_eq!(operation.outcome, Outcome::Compensated);
    let restored = store.assert_current(&parent.identity).unwrap();
    assert_eq!(restored.status, SeatStatus::Stopped);
    assert_eq!(restored.session, parent.session);
    host.fail_capture = false;
    io.operation = OperationId::new("restart-next").unwrap();
    request.identity.instance = InstanceId::new("worker-3").unwrap();
    assert_eq!(
        Lifecycle {
            store: &mut store,
            host: &mut host,
            io: &mut io
        }
        .startup(
            &Adapter {
                descriptor: &d,
                hooks: &h,
                catalog: None
            },
            &request,
            routing("worker"),
            OperationId::new("restart-next").unwrap()
        ),
        Err(Error::Fence)
    );
    request.identity.generation = Generation(3);
    let successful = Lifecycle {
        store: &mut store,
        host: &mut host,
        io: &mut io,
    }
    .startup(
        &Adapter {
            descriptor: &d,
            hooks: &h,
            catalog: None,
        },
        &request,
        routing("worker"),
        OperationId::new("restart-next").unwrap(),
    )
    .unwrap();
    assert_eq!(successful.outcome, Outcome::Committed);
    assert_eq!(successful.target.identity.generation, Generation(3));
}

#[test]
fn conflicting_pane_or_binding_is_rejected_before_second_materialization() {
    let sandbox = Sandbox::new();
    let mut store = sandbox.store();
    let mut host = FakeHost::default();
    let mut io = FakeIo::new("unused");
    start(&mut store, &mut host, &mut io, "worker");
    let request = request(store.root(), "second");
    let d = descriptor();
    let h = hooks();
    io.operation = OperationId::new("second").unwrap();
    assert_eq!(
        Lifecycle {
            store: &mut store,
            host: &mut host,
            io: &mut io
        }
        .startup(
            &Adapter {
                descriptor: &d,
                hooks: &h,
                catalog: None
            },
            &request,
            routing("worker"),
            OperationId::new("second").unwrap()
        ),
        Err(Error::Fence)
    );
    assert_eq!(host.spawned, 1);
    assert_eq!(io.writes, 1);
    assert_eq!(store.seats().unwrap().len(), 1);
}

#[test]
fn persisted_ids_and_owned_paths_revalidate_on_deserialize() {
    assert!(serde_json::from_value::<OwnedPath>(json!(["/owned", "../escape"])).is_err());
    assert!(serde_json::from_value::<ProviderId>(json!("UPPER")).is_err());
    assert!(serde_json::from_value::<ScopeId>(json!("bad/scope")).is_err());
    assert!(
        serde_json::from_value::<team_agent_contract::contract::session::NativeSessionId>(json!(
            " "
        ))
        .is_err()
    );
}
