//! Failure windows are injected at the actual SQLite commit boundary, not by
//! bypassing lifecycle/handler/supervisor entry points.
#[path = "support/legacy_wire.rs"]
mod legacy_wire;
mod support;
use support::direct_business::{handle};
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
fn stopped_seat_releases_its_pane_to_a_seat_spawned_on_a_restarted_shared_server() {
    let sandbox = Sandbox::new();
    let mut store = sandbox.store();
    let mut host = FakeHost::default();
    let mut io = FakeIo::new("unused");
    // A shared tmux server that exited with its last seat numbers its first pane %0 again.
    host.route_after_spawn = Some(("/shared/tmux.sock".into(), "%0".into(), "binding-worker".into()));
    let worker = start(&mut store, &mut host, &mut io, "worker").target;
    Lifecycle {
        store: &mut store,
        host: &mut host,
        io: &mut io,
    }
    .teardown(&worker.identity, OperationId::new("stop-worker").unwrap())
    .unwrap();
    host.route_after_spawn = Some(("/shared/tmux.sock".into(), "%0".into(), "binding-second".into()));
    let second = start(&mut store, &mut host, &mut io, "second");
    assert_eq!(second.outcome, Outcome::Committed, "{:?}", second.failure);
    assert_eq!(host.stopped, 1, "only the torn-down worker was stopped");
    // A live seat on that pane still fences the new route; the unpublished
    // process is left to explicit recovery, never stopped on a guess.
    host.route_after_spawn = Some(("/shared/tmux.sock".into(), "%0".into(), "binding-third".into()));
    let request = request(store.root(), "third");
    let d = descriptor();
    let h = hooks();
    io.operation = OperationId::new("third").unwrap();
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
            routing("third"),
            OperationId::new("third").unwrap()
        )
        .map(|record| record.outcome),
        Err(Error::Fence)
    );
    assert_eq!(host.stopped, 1);
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
