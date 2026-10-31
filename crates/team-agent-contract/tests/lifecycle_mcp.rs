//! Controlled fake-provider integration tests; no native/Kiro acceptance claims.
mod support;
use support::direct_business::{handle, serve, serve_with_diagnostics};
use serde_json::{json, Value};
use support::*;
use team_agent_contract::contract::{delivery::*, descriptor::*, fork::*, plan::*, types::*};
use team_agent_contract::orchestration::{lifecycle::*, mcp::*, store::*, Error};

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
fn production_business_tools_still_require_an_explicit_host_handler() {
    let (_sandbox, mut store, _, _, seat) = setup();
    let ctx = context(&seat);
    for request in [call(1, "report_result", json!({"summary":"done"})),
        call(2, "send_message", json!({"to":"leader","content":"hello"}))] {
        assert_eq!(team_agent_contract::orchestration::mcp::handle(&mut store, &ctx, &request),
            Err(Error::Invalid("direct business handler required")));
    }
    assert!(store.results().unwrap().is_empty());
    assert!(store.deliveries().unwrap().is_empty());
    assert!(store.protocol_facts().unwrap().is_empty());
}

#[test]
// M4: re-expressed. The original used report_result and the retired SQL result
// plane; the property kept is that a tool call survives reopen through the
// file-backed call ledger (same response, one invocation fact) and never promotes
// a Starting seat. Business results belong to the station inbox (M1 suites).
fn startup_and_mcp_call_survive_reopen_without_promoting_readiness() {
    let (sandbox, mut store, host, _, seat) = setup();
    assert_eq!(host.spawned, 1);
    assert_eq!(seat.status, SeatStatus::Starting);
    let ctx = context(&seat);
    let status = call(1, "get_team_status", json!({}));
    let first = handle(&mut store, &ctx, &status).unwrap();
    assert!(first.as_ref().is_some_and(|response| response.get("result").is_some()));
    drop(store);
    let mut reopened = sandbox.reopen();
    assert_eq!(
        reopened.seat(&seat.identity.seat).unwrap(),
        Some(seat.clone())
    );
    assert_eq!(handle(&mut reopened, &ctx, &status).unwrap(), first);
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
    assert_fenced(store.assert_current(&seat.identity), "current_instance_or_generation_replaced");
    assert_fenced(
        handle(
            &mut store,
            &context(&seat),
            &call(1, "get_team_status", json!({}))
        ),
        "current_instance_or_generation_replaced"
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
fn positive_no_spawn_receipt_compensates_without_stopping_or_replaying() {
    let sandbox = Sandbox::new();
    let mut store = sandbox.store();
    let mut host = FakeHost { spawn_error: Some(Error::SpawnNotStarted(team_agent_contract::host::HostError::new(
        "materialization receipt", team_agent_contract::host::HostErrorKind::Ownership))), ..Default::default() };
    let mut io = FakeIo::new("unused");
    let op = start(&mut store, &mut host, &mut io, "worker");
    assert_eq!(op.outcome, Outcome::Compensated);
    assert!(op.pending.is_none());
    assert_eq!(host.spawned, 0);
    assert_eq!(host.stopped, 0);
    assert_eq!(host.removed, 1);
    assert!(store.seat(&op.target.identity.seat).unwrap().is_none());
    assert!(sandbox.reopen().unfinished().unwrap().is_empty());
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
    // M4: the lease is read through the store, not a contract_leases SQL row. Which
    // operation holds it is pinned by `unfinished()` above.
    assert!(store.leased(&seat.identity.seat).unwrap());
    assert_eq!(host.spawned, 1);
    assert_eq!(host.stopped, 0);
    assert_eq!(host.removed, 0);
    // The original also asserted that send_message is refused while the lease is held.
    // Business admission now belongs to the team-agent host handler (station send); it
    // is listed as a carried-over invariant, not ported into this crate's test double.
}

fn session_observation_timeout(identity: InstanceIdentity) -> Error {
    use team_agent_contract::runtime::{delivery::*, journal::*};
    Error::NativeControl(Box::new(InjectionReport {
        metadata: AttemptMetadata {
            owner: identity, attempt: AttemptId::new("inspect").unwrap(),
            correlation: Correlation::NativeControl(OperationId::new("inspect").unwrap()),
            operation: Operation::SessionInspect, policy_sha256: HASH,
            payload_sha256: HASH, payload_bytes: 8,
        },
        disposition: InjectionDisposition::Unresolved,
        counts: PhysicalCounts { initial: ActionCounts { issued: 1, confirmed: 1, uncertain: 0 }, ..Default::default() },
        effect_floor: DeliveryEffect::Submitted, events: vec![],
        problems: vec![ExecutionProblem { code: "observation-deadline", host: None }],
        persistence: PersistenceState::Durable, bootstrap_consumed: false,
        unreleased_buffer: None, control_paste: None,
    }))
}

#[test]
fn all_unresolved_session_inspections_retain_spawn_and_its_actual_pane() {
    use team_agent_contract::host::{HostError, HostErrorKind};
    use team_agent_contract::runtime::delivery::ExecutionProblem;
    for case in 0..7 {
        let sandbox = Sandbox::new();
        let mut store = sandbox.store();
        let mut error = session_observation_timeout(request(store.root(), "worker").identity);
        let Error::NativeControl(report) = &mut error else { unreachable!() };
        match case {
            0 => {}
            1 | 2 => report.problems = vec![ExecutionProblem {
                code: "capture-failed",
                host: Some(HostError::new("tmux capture", if case == 1 { HostErrorKind::Deadline }
                    else { HostErrorKind::Io(std::io::ErrorKind::UnexpectedEof) })),
            }],
            3 => report.problems = vec![ExecutionProblem {
                code: "future-observer-diagnostic", host: Some(HostError::new("capture", HostErrorKind::Unknown)),
            }],
            4 => {
                report.persistence = PersistenceState::Failed;
                report.problems.push(ExecutionProblem { code: "journal-write-failed", host: None });
            }
            5 => { report.persistence = PersistenceState::Pending; report.problems.clear(); }
            _ => report.problems.clear(),
        }
        let diagnostic = error.to_string();
        let mut host = FakeHost {
            capture_error: Some(error),
            route_after_spawn: Some(("isolated-endpoint".into(), "%17".into(), "native-binding".into())),
            ..Default::default()
        };
        let mut io = FakeIo::new("unused");
        let op = start(&mut store, &mut host, &mut io, "worker");
        assert_eq!(op.outcome, Outcome::Committed, "case {case}: {diagnostic}");
        assert_eq!(op.phase, Phase::F5Receipt);
        assert_eq!((host.spawned, host.stopped, host.removed), (1, 0, 0));
        assert_eq!(op.target.pane, "%17");
        assert!(op.target.process.is_some());
        assert!(op.target.session.is_none());
        assert_eq!(op.target.status, SeatStatus::Starting);
        assert!(op.failure.as_ref().unwrap().contains(&diagnostic));
        assert_eq!(store.seat(&op.target.identity.seat).unwrap(), Some(op.target.clone()));
        assert!(store.unfinished().unwrap().is_empty());
        // M4: the spawned seat stays addressable. The original asserted a SQL-persisted
        // report_result; the business plane is the station inbox now.
        assert!(handle(&mut store, &context(&op.target), &call(1, "get_team_status", json!({}))).is_ok());
        drop(store);
        assert_eq!(sandbox.reopen().operation(&op.id).unwrap(), op);
    }
}

#[test]
fn unresolved_capture_deadline_on_restart_does_not_stop_the_new_generation() {
    let (_sandbox, mut store, mut host, mut io, parent) = setup();
    let request = restart_request(&store, &parent);
    let mut error = session_observation_timeout(request.identity.clone());
    let Error::NativeControl(report) = &mut error else { unreachable!() };
    report.problems[0].code = "capture-failed";
    report.problems[0].host = Some(team_agent_contract::host::HostError::new(
        "tmux capture", team_agent_contract::host::HostErrorKind::Deadline));
    host.capture_error = Some(error);
    let d = descriptor();
    let h = hooks();
    io.operation = OperationId::new("restart").unwrap();
    let op = Lifecycle { store: &mut store, host: &mut host, io: &mut io }
        .startup(&Adapter { descriptor: &d, hooks: &h, catalog: None },
            &request, routing("worker"), OperationId::new("restart").unwrap()).unwrap();
    assert_eq!(op.outcome, Outcome::Committed);
    assert_eq!((host.spawned, host.stopped), (2, 1));
    assert_eq!(op.target.identity, request.identity);
    assert!(op.target.session.is_none());
    assert!(op.failure.as_ref().unwrap().contains("capture-failed"));
    assert!(op.failure.as_ref().unwrap().contains("Deadline"));
    assert!(handle(&mut store, &context(&parent), &call(2, "report_result", json!({"summary":"old"}))).is_err());
}

#[test]
fn foreign_other_operation_and_non_unresolved_reports_remain_fatal() {
    use team_agent_contract::runtime::delivery::InjectionDisposition;
    for case in 0..5 {
        let sandbox = Sandbox::new();
        let mut store = sandbox.store();
        let mut error = session_observation_timeout(request(store.root(), "worker").identity);
        let Error::NativeControl(report) = &mut error else { unreachable!() };
        match case {
            0 => report.metadata.operation = Operation::OrdinarySend,
            1 => report.metadata.owner.instance = InstanceId::new("foreign").unwrap(),
            2 => report.disposition = InjectionDisposition::Refused,
            3 => report.disposition = InjectionDisposition::Deferred,
            _ => { report.disposition = InjectionDisposition::NativeAccepted; report.persistence = PersistenceState::Failed; }
        }
        let mut host = FakeHost { capture_error: Some(error), ..Default::default() };
        let op = start(&mut store, &mut host, &mut FakeIo::new("unused"), "worker");
        assert_eq!(op.outcome, Outcome::Compensated, "case {case}");
        assert_eq!(host.stopped, 1, "case {case}");
    }
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
// M4: trimmed. The mailbox/silent result assertions read the retired SQL message
// and result plane and were excised; mailbox semantics are owned by the station
// send core (M1 suites). The public tool surface assertions are kept.
fn schema_keeps_only_three_public_tools_and_refuses_unknown_tools() {
    let (_sandbox, mut store, _, _, seat) = setup();
    let ctx = context(&seat);
    assert_eq!(tools_contract().as_array().unwrap().len(), 3);
    assert!(handle(&mut store, &ctx, &call(3, "fork", json!({}))).is_err());
}

#[test]
// M4: re-expressed with get_team_status. The original also asserted that a SQL result
// row survives the write failure; that plane is retired. The kept property: a failed
// server write never records response_written or any client-consumption fact, and a
// later successful write does.
fn server_write_failure_never_claims_response_written_or_client_consumption() {
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
    let request = call(1, "get_team_status", json!({}));
    assert!(serve(
        &mut store,
        &ctx,
        std::io::Cursor::new(format!("{request}\n")),
        Broken
    )
    .is_err());
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
// M4: re-expressed with get_team_status. Business-tool argument validation moved to
// the team-agent host handler, and the SQL result plane is retired. The kept
// property: context and handler rejections are reported with their real stage and
// error, never with request bytes, and the wire response is the same masked error.
fn request_diagnostics_distinguish_fences_from_bad_arguments_without_changing_rpc() {
    let (_sandbox, mut store, _, _, seat) = setup();
    let ctx = context(&seat);
    let requests = [
        call(1, "get_team_status", json!({})),
        call(2, "get_team_status", json!({"secret":"do-not-log-request"})),
    ];
    let input: String = requests
        .iter()
        .map(|request| format!("{request}\n"))
        .collect();
    let mut resolved = 0;
    let mut rejected = Vec::new();
    let mut output = Vec::new();
    serve_with_diagnostics(
        &mut store,
        |_, _| {
            resolved += 1;
            if resolved == 1 {
                Err(Error::Conflict)
            } else {
                Ok(ctx.clone())
            }
        },
        std::io::Cursor::new(input),
        &mut output,
        |stage, error| rejected.push((stage, error.clone())),
    )
    .unwrap();
    assert_eq!(
        rejected,
        vec![
            (RequestRejectionStage::Context, Error::Conflict),
            (
                RequestRejectionStage::Handler,
                Error::Invalid("unexpected argument")
            ),
        ]
    );
    assert!(!format!("{rejected:?}").contains("do-not-log-request"));
    let output = String::from_utf8(output).unwrap();
    let responses: Vec<Value> = output
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(responses.len(), 2);
    for (response, request) in responses.iter().zip(&requests) {
        assert_eq!(
            response,
            &json!({"jsonrpc":"2.0","id":request["id"],
                "error":{"code":-32602,"message":"request rejected"}})
        );
    }
    assert!(store.protocol_facts().unwrap().is_empty());
    let request = call(3, "get_team_status", json!({}));
    let mut output = Vec::new();
    serve_with_diagnostics(
        &mut store,
        |_, _| Ok(ctx.clone()),
        std::io::Cursor::new(format!("{request}\n")),
        &mut output,
        |_, _| panic!("a valid request must not emit rejection diagnostics"),
    )
    .unwrap();
    assert!(serde_json::from_slice::<Value>(&output).unwrap().get("result").is_some());
    assert!(store
        .protocol_facts()
        .unwrap()
        .contains(&"response_written".into()));
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
