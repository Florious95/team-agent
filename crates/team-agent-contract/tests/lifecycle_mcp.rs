//! Controlled fake-provider integration tests; no native/Kiro acceptance claims.
mod support;
use serde_json::{json, Value};
use support::*;
use team_agent_contract::contract::{delivery::*, descriptor::*, fork::*, plan::*, types::*};
use team_agent_contract::orchestration::{lifecycle::*, mcp::*, store::*, supervisor::*, Error};

fn context(seat: &SeatRecord) -> CallContext {
    CallContext {
        identity: seat.identity.clone(),
        binding_key: seat.binding_key.clone(),
        connection_id: InstanceId::new("connection-one").unwrap(),
        task_id: "task-one".into(),
    }
}
fn call(id: u32, tool: &str, args: Value) -> Value {
    json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{"name":tool,"arguments":args}})
}
fn result(response: Option<Value>) -> Value {
    serde_json::from_str(
        response.unwrap()["result"]["content"][0]["text"]
            .as_str()
            .unwrap(),
    )
    .unwrap()
}
fn setup() -> (Sandbox, ContractStore, FakeHost, FakeIo, SeatRecord) {
    let sandbox = Sandbox::new();
    let mut store = sandbox.store();
    let mut host = FakeHost::default();
    let mut io = FakeIo::new("unused");
    let operation = start(&mut store, &mut host, &mut io, "worker");
    assert_eq!(operation.outcome, Outcome::Committed);
    assert_eq!(operation.phase, Phase::F5Receipt);
    let seat = operation.target;
    (sandbox, store, host, io, seat)
}

#[test]
fn startup_and_mcp_result_survive_reopen_without_promoting_readiness() {
    let (sandbox, mut store, host, _, seat) = setup();
    assert_eq!(host.spawned, 1);
    assert_eq!(seat.status, SeatStatus::Starting);
    let ctx = context(&seat);
    let report = call(1, "report_result", json!({"summary":"done"}));
    let first = result(handle(&mut store, &ctx, &report).unwrap());
    assert_eq!(first["status"], "persisted");
    assert_eq!(first["leader_notified"], false);
    drop(store);
    let mut reopened = sandbox.reopen();
    assert_eq!(
        reopened.seat(&seat.identity.seat).unwrap(),
        Some(seat.clone())
    );
    assert_eq!(result(handle(&mut reopened, &ctx, &report).unwrap()), first);
    assert_eq!(reopened.results().unwrap().len(), 1);
    assert_eq!(reopened.deliveries().unwrap().len(), 1);
    assert_eq!(
        reopened.protocol_facts().unwrap(),
        vec!["invocation_received"]
    );
}

#[test]
fn scope_endpoint_and_legacy_root_are_not_adopted() {
    let (sandbox, store, _, _, _) = setup();
    drop(store);
    assert!(ContractStore::open(
        &sandbox.root,
        ScopeId::new("other").unwrap(),
        "isolated-endpoint"
    )
    .is_err());
    assert!(ContractStore::open(
        &sandbox.root,
        ScopeId::new("scope").unwrap(),
        "other-endpoint"
    )
    .is_err());
    assert!(ContractStore::create(
        &sandbox.root,
        ScopeId::new("scope").unwrap(),
        "isolated-endpoint"
    )
    .is_err());
    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;
        let linked = sandbox.parent.join("linked");
        symlink(&sandbox.root, &linked).unwrap();
        assert!(
            ContractStore::open(&linked, ScopeId::new("scope").unwrap(), "isolated-endpoint")
                .is_err()
        );
    }
}

#[test]
fn preflight_rejects_before_any_side_effect() {
    let sandbox = Sandbox::new();
    let mut store = sandbox.store();
    let mut host = FakeHost::default();
    let mut io = FakeIo::new("invalid");
    let mut request = request(&sandbox.root, "worker");
    request.role_effort = Some("ultra".into());
    let d = descriptor();
    let h = hooks();
    assert!(Lifecycle {
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
        OperationId::new("invalid").unwrap()
    )
    .is_err());
    assert_eq!(host.spawned, 0);
    assert_eq!(io.writes, 0);
    assert!(store.seats().unwrap().is_empty());
    assert!(store.unfinished().unwrap().is_empty());
}

fn restart_request(store: &ContractStore, seat: &SeatRecord) -> LaunchRequest {
    let mut request = request(store.root(), seat.identity.seat.as_str());
    request.operation = Operation::Resume;
    request.identity.instance = InstanceId::new("worker-2").unwrap();
    request.identity.generation = Generation(2);
    request.resume = Some(ResumeRequest {
        binding: seat.session.clone().unwrap(),
        expected_source: seat.identity.clone(),
    });
    request
}
#[test]
fn restart_preserves_sid_and_fences_old_generation_and_mcp() {
    let (_sandbox, mut store, mut host, mut io, seat) = setup();
    let request = restart_request(&store, &seat);
    let d = descriptor();
    let h = hooks();
    io.operation = OperationId::new("restart").unwrap();
    let op = Lifecycle {
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
        io_id(),
    )
    .unwrap();
    assert_eq!(op.outcome, Outcome::Committed);
    assert_eq!(host.stopped, 1);
    assert_eq!(
        op.target.session.as_ref().unwrap().native_session,
        seat.session.as_ref().unwrap().native_session
    );
    assert_eq!(store.assert_current(&seat.identity), Err(Error::Fence));
    assert_eq!(
        handle(
            &mut store,
            &context(&seat),
            &call(1, "get_team_status", json!({}))
        ),
        Err(Error::Fence)
    );
    assert!(handle(
        &mut store,
        &context(&op.target),
        &call(1, "get_team_status", json!({}))
    )
    .is_ok());
    fn io_id() -> OperationId {
        OperationId::new("restart").unwrap()
    }
}

#[test]
fn failed_postwrite_materialization_preserves_uncertain_receipt() {
    let sandbox = Sandbox::new();
    let mut store = sandbox.store();
    let mut host = FakeHost::default();
    let mut io = FakeIo::new("unused");
    io.fail_after_write = true;
    let op = start(&mut store, &mut host, &mut io, "worker");
    assert_eq!(op.outcome, Outcome::Compensated);
    assert_eq!(op.resources.len(), 1);
    assert_eq!(
        op.resources[0].write_effect,
        ResourceWriteEffect::MayHaveWritten
    );
    assert_eq!(op.preserved.len(), 1);
    assert_eq!(host.removed, 0);
    assert_eq!(host.spawned, 0);
    assert!(store.seats().unwrap().is_empty());
}

#[test]
fn spawn_without_receipt_stays_fenced_across_recovery() {
    let sandbox = Sandbox::new();
    let mut store = sandbox.store();
    let mut host = FakeHost {
        fail_spawn: true,
        ..Default::default()
    };
    let mut io = FakeIo::new("unused");
    let op = start(&mut store, &mut host, &mut io, "worker");
    assert_eq!(op.outcome, Outcome::NeedsRecovery);
    assert_eq!(op.pending.as_deref(), Some("spawn"));
    drop(store);
    let mut store = sandbox.reopen();
    let recovered = Lifecycle {
        store: &mut store,
        host: &mut host,
        io: &mut io,
    }
    .recover(&op.id)
    .unwrap();
    assert_eq!(recovered.outcome, Outcome::NeedsRecovery);
    // A public shutdown after an unreceipted spawn must not turn the retained
    // lease's unique-key conflict into Database or clear it to claim success.
    let seat = store.assert_current(&op.target.identity).unwrap();
    assert_eq!(
        Lifecycle {
            store: &mut store,
            host: &mut host,
            io: &mut io,
        }
        .teardown(&seat.identity, OperationId::new("stop-unobserved").unwrap()),
        Err(Error::NeedsRecovery)
    );
    assert_eq!(store.assert_current(&seat.identity).unwrap(), seat);
    assert_eq!(store.unfinished().unwrap(), vec![recovered]);
    let connection = rusqlite::Connection::open(store.root().join("contract.db")).unwrap();
    let held: String = connection
        .query_row(
            "SELECT operation FROM contract_leases WHERE seat=?1",
            [seat.identity.seat.as_str()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(held, op.id.as_str());
    assert_eq!(host.spawned, 1);
    assert_eq!(host.stopped, 0);
    assert_eq!(host.removed, 0);
    assert!(handle(
        &mut store,
        &context(&op.target),
        &call(1, "send_message", json!({"to":"leader","content":"never"}))
    )
    .is_err());
}

#[test]
fn captured_spawn_failure_compensates_only_known_process_and_owned_resources() {
    let sandbox = Sandbox::new();
    let mut store = sandbox.store();
    let mut host = FakeHost {
        fail_capture: true,
        ..Default::default()
    };
    let mut io = FakeIo::new("unused");
    let op = start(&mut store, &mut host, &mut io, "worker");
    assert_eq!(op.outcome, Outcome::Compensated);
    assert_eq!(host.spawned, 1);
    assert_eq!(host.stopped, 1);
    assert_eq!(host.removed, 1);
    assert_eq!(op.target.status, SeatStatus::Stopped);
}

fn fork_request(store: &ContractStore, parent: &SeatRecord, mode: ForkMode) -> ForkRequest {
    let target = if mode == ForkMode::InWindowBranch {
        parent.identity.clone()
    } else {
        request(store.root(), "child").identity
    };
    ForkRequest {
        mode,
        auth: AuthMode::NativeSubscription,
        source: parent.session.clone().unwrap(),
        expected_source: parent.identity.clone(),
        target,
        target_native_session: if mode == ForkMode::NewSeatFullSnapshot {
            Some(
                team_agent_contract::contract::session::NativeSessionId::new("child-snapshot")
                    .unwrap(),
            )
        } else {
            None
        },
        target_backing: if mode == ForkMode::NewSeatFullSnapshot {
            Some(OwnedPath::new(store.root().into(), "snapshot".into()).unwrap())
        } else {
            None
        },
        channel: Channel::Tmux,
        input_profile: Some("fake".into()),
        cwd: parent.cwd.clone(),
        native: parent.native.clone(),
        evidence_kind: EvidenceKind::Fixture,
        selected_turn: None,
        operation_id: OperationId::new("fork").unwrap(),
    }
}
#[test]
fn all_fork_modes_use_f0_to_f5_and_preserve_parent() {
    for mode in [
        ForkMode::InWindowBranch,
        ForkMode::NewSeatFullSnapshot,
        ForkMode::NativeNewSeat,
    ] {
        let (_sandbox, mut store, mut host, mut io, parent) = setup();
        let request_fork = fork_request(&store, &parent, mode);
        let d = descriptor();
        let h = hooks();
        let mut launch = request(store.root(), "child");
        launch.operation = mode.operation();
        launch.fork = Some(Box::new(resolve_fork(&d, &h, &request_fork).unwrap()));
        io.operation = request_fork.operation_id.clone();
        let op = Lifecycle {
            store: &mut store,
            host: &mut host,
            io: &mut io,
        }
        .fork(
            &Adapter {
                descriptor: &d,
                hooks: &h,
                catalog: None,
            },
            &request_fork,
            if mode == ForkMode::InWindowBranch {
                None
            } else {
                Some((&launch, routing("child")))
            },
        )
        .unwrap();
        assert_eq!(
            op.outcome,
            Outcome::Committed,
            "{:?}: {:?}",
            mode,
            op.failure
        );
        assert_eq!(op.phase, Phase::F5Receipt);
        assert_eq!(op.parent, Some(parent.clone()));
        assert_ne!(
            op.target.session.as_ref().unwrap().native_session,
            parent.session.as_ref().unwrap().native_session
        );
        if mode == ForkMode::InWindowBranch {
            assert_eq!(host.spawned, 1);
            assert_eq!(host.controls, 1);
            assert_eq!(store.seats().unwrap().len(), 1);
        } else {
            assert_eq!(host.spawned, 2);
            assert_eq!(store.assert_current(&parent.identity).unwrap(), parent);
        }
    }
}
#[test]
fn unsupported_fork_has_no_operation_or_host_effects() {
    let (_sandbox, mut store, mut host, mut io, parent) = setup();
    let req = fork_request(&store, &parent, ForkMode::NewSeatFullSnapshot);
    let mut d = descriptor();
    d.fork.full_snapshot = Support::Unsupported(NO);
    let h = hooks();
    assert!(Lifecycle {
        store: &mut store,
        host: &mut host,
        io: &mut io
    }
    .fork(
        &Adapter {
            descriptor: &d,
            hooks: &h,
            catalog: None
        },
        &req,
        None
    )
    .is_err());
    assert_eq!(host.spawned, 1);
    assert_eq!(io.writes, 1);
    assert!(store.unfinished().unwrap().is_empty());
}

#[test]
fn result_and_outbox_are_atomic_idempotent_and_identity_bound() {
    let (_sandbox, mut store, _, _, seat) = setup();
    let ctx = context(&seat);
    for args in [
        json!({"summary":"x","agent_id":"other"}),
        json!({"envelope":{"task_id":"other"}}),
        json!({"sender":"other"}),
    ] {
        assert!(handle(&mut store, &ctx, &call(1, "report_result", args)).is_err());
    }
    assert!(store.results().unwrap().is_empty());
    assert!(store.deliveries().unwrap().is_empty());
    let request = call(1, "report_result", json!({"summary":"done"}));
    let a = result(handle(&mut store, &ctx, &request).unwrap());
    let b = result(
        handle(
            &mut store,
            &ctx,
            &call(2, "report_result", json!({"summary":"done"})),
        )
        .unwrap(),
    );
    assert_eq!(a["result_id"], b["result_id"]);
    assert!(handle(
        &mut store,
        &ctx,
        &call(1, "report_result", json!({"summary":"different"}))
    )
    .is_err());
    assert_eq!(store.results().unwrap().len(), 1);
    assert_eq!(store.deliveries().unwrap().len(), 1);
}

#[test]
fn mailbox_silent_and_schema_keep_only_three_public_tools() {
    let (_sandbox, mut store, _, _, seat) = setup();
    let ctx = context(&seat);
    assert_eq!(tools_contract().as_array().unwrap().len(), 3);
    handle(
        &mut store,
        &ctx,
        &call(
            1,
            "send_message",
            json!({"to":"leader","content":"note","mailbox":true}),
        ),
    )
    .unwrap();
    handle(
        &mut store,
        &ctx,
        &call(
            2,
            "report_result",
            json!({"summary":"done","presentation":{"sink":"silent","class":"stage_result"}}),
        ),
    )
    .unwrap();
    assert!(store.deliveries().unwrap().is_empty());
    assert_eq!(store.results().unwrap().len(), 1);
    assert!(handle(&mut store, &ctx, &call(3, "fork", json!({}))).is_err());
    assert!(handle(
        &mut store,
        &ctx,
        &call(
            4,
            "send_message",
            json!({"to":"leader","content":"x","mailbox":"yes"})
        )
    )
    .is_err());
}

#[test]
fn server_write_failure_does_not_erase_result_or_claim_client_consumption() {
    struct Broken;
    impl std::io::Write for Broken {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::ErrorKind::BrokenPipe.into())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let (_sandbox, mut store, _, _, seat) = setup();
    let ctx = context(&seat);
    let request = call(1, "report_result", json!({"summary":"durable"}));
    assert!(serve(
        &mut store,
        &ctx,
        std::io::Cursor::new(format!("{request}\n")),
        Broken
    )
    .is_err());
    assert_eq!(store.results().unwrap().len(), 1);
    assert_eq!(store.protocol_facts().unwrap(), vec!["invocation_received"]);
    let mut output = vec![];
    serve(
        &mut store,
        &ctx,
        std::io::Cursor::new(format!("{request}\n")),
        &mut output,
    )
    .unwrap();
    assert!(store
        .protocol_facts()
        .unwrap()
        .contains(&"response_written".into()));
    assert!(!store
        .protocol_facts()
        .unwrap()
        .iter()
        .any(|s| s.contains("consumption") || s.contains("presentation")));
}

#[test]
fn shared_supervisor_consumes_bootstrap_once_durably() {
    let (sandbox, mut store, mut host, _, seat) = setup();
    let ctx = context(&seat);
    for id in 1..=2 {
        handle(
            &mut store,
            &ctx,
            &call(id, "send_message", json!({"to":"worker","content":"body"})),
        )
        .unwrap();
    }
    let sample = readiness(&seat, false);
    assert!(matches!(
        tick(
            &mut store,
            &seat.identity,
            &sample,
            &authorization(),
            &mut host
        )
        .unwrap(),
        Tick::Recorded {
            uncertain: false,
            ..
        }
    ));
    drop(store);
    let mut store = sandbox.reopen();
    assert_eq!(
        tick(
            &mut store,
            &seat.identity,
            &sample,
            &authorization(),
            &mut host
        )
        .unwrap(),
        Tick::Blocked
    );
    assert_eq!(host.deliveries, 1);
    let current = store.assert_current(&seat.identity).unwrap();
    let sample = readiness(&current, true);
    assert!(matches!(
        tick(
            &mut store,
            &seat.identity,
            &sample,
            &authorization(),
            &mut host
        )
        .unwrap(),
        Tick::Recorded {
            uncertain: false,
            ..
        }
    ));
    assert_eq!(host.deliveries, 2);
    assert_eq!(store.deliveries().unwrap().len(), 2);
}

#[test]
fn uncertain_effect_is_not_replayed_after_reopen_or_new_tick() {
    let (sandbox, mut store, mut host, _, seat) = setup();
    host.uncertain_delivery = true;
    handle(
        &mut store,
        &context(&seat),
        &call(1, "send_message", json!({"to":"worker","content":"body"})),
    )
    .unwrap();
    let sample = readiness(&seat, true);
    assert!(matches!(
        tick(
            &mut store,
            &seat.identity,
            &sample,
            &authorization(),
            &mut host
        )
        .unwrap(),
        Tick::Recorded {
            effect: DeliveryEffect::MayHaveSubmitted,
            uncertain: true,
            ..
        }
    ));
    drop(store);
    let mut store = sandbox.reopen();
    assert!(tick(
        &mut store,
        &seat.identity,
        &sample,
        &authorization(),
        &mut host
    )
    .is_err());
    assert_eq!(host.deliveries, 1);
    assert_eq!(
        store.deliveries().unwrap()[0].effect,
        DeliveryEffect::MayHaveSubmitted
    );
}

#[test]
fn stale_or_wrong_probe_never_reaches_physical_host() {
    let (_sandbox, mut store, mut host, _, seat) = setup();
    handle(
        &mut store,
        &context(&seat),
        &call(1, "send_message", json!({"to":"worker","content":"body"})),
    )
    .unwrap();
    let mut sample = readiness(&seat, true);
    sample.pane.scope.identity.generation = Generation(0);
    assert_eq!(
        tick(
            &mut store,
            &seat.identity,
            &sample,
            &authorization(),
            &mut host
        )
        .unwrap(),
        Tick::Blocked
    );
    assert_eq!(host.deliveries, 0);
    assert_eq!(store.deliveries().unwrap()[0].state, "queued");
}

#[test]
fn teardown_stops_exact_seat_and_preserves_snapshot_backing() {
    let (_sandbox, mut store, mut host, mut io, parent) = setup();
    let req = fork_request(&store, &parent, ForkMode::NewSeatFullSnapshot);
    let d = descriptor();
    let h = hooks();
    let mut launch = request(store.root(), "child");
    launch.operation = Operation::NewSeatFullSnapshot;
    launch.fork = Some(Box::new(resolve_fork(&d, &h, &req).unwrap()));
    io.operation = req.operation_id.clone();
    let child = Lifecycle {
        store: &mut store,
        host: &mut host,
        io: &mut io,
    }
    .fork(
        &Adapter {
            descriptor: &d,
            hooks: &h,
            catalog: None,
        },
        &req,
        Some((&launch, routing("child"))),
    )
    .unwrap()
    .target;
    let op = Lifecycle {
        store: &mut store,
        host: &mut host,
        io: &mut io,
    }
    .teardown(&child.identity, OperationId::new("stop-child").unwrap())
    .unwrap();
    assert_eq!(op.outcome, Outcome::Committed);
    assert_eq!(host.stopped, 1);
    assert_eq!(host.removed, 1);
    assert_eq!(op.preserved, vec![req.target_backing.unwrap()]);
    assert_eq!(store.assert_current(&parent.identity).unwrap(), parent);
    assert!(handle(
        &mut store,
        &context(&child),
        &call(1, "report_result", json!({"summary":"stale"}))
    )
    .is_err());
}
