//! remove-agent / reset-agent end a seat identity explicitly; no native provider runs.
mod support;
use support::*;
use team_agent_contract::contract::types::*;
use team_agent_contract::orchestration::{lifecycle::*, store::*, Error};

#[test]
fn only_a_stopped_unleased_seat_retires_and_its_id_then_starts_on_a_fresh_generation() {
    let sandbox = Sandbox::new();
    let mut store = sandbox.store();
    let mut host = FakeHost::default();
    let mut io = FakeIo::new("unused");
    let worker = start(&mut store, &mut host, &mut io, "worker").target;
    let original = store.seat(&worker.identity.seat).unwrap().unwrap().session;
    assert!(original.is_some());
    // A store created before the first retirement has no retirement table yet.
    assert!(!store.retired(&worker.identity).unwrap());
    assert_eq!(
        store.retire(&worker.identity),
        Err(Error::Invalid("only a stopped seat can retire"))
    );
    Lifecycle {
        store: &mut store,
        host: &mut host,
        io: &mut io,
    }
    .teardown(&worker.identity, OperationId::new("stop").unwrap())
    .unwrap();
    let observe = OperationId::new("observe").unwrap();
    store.begin_observation(&worker.identity, &observe).unwrap();
    assert!(store.leased(&worker.identity.seat).unwrap());
    assert!(store.retire(&worker.identity).is_err());
    assert!(store.seat(&worker.identity.seat).unwrap().is_some());
    store.finish_observation(&worker.identity, &observe).unwrap();
    assert!(!store.leased(&worker.identity.seat).unwrap());

    let retired = store.retire(&worker.identity).unwrap();
    assert_eq!(retired.identity, worker.identity);
    assert_eq!(retired.status, SeatStatus::Stopped);
    assert_eq!(store.seat(&worker.identity.seat).unwrap(), None);
    assert!(store.retired(&worker.identity).unwrap());
    // History stays: the last creation receipt still names the retired identity.
    assert_eq!(
        store
            .creation_receipt(&worker.identity.seat, None)
            .unwrap()
            .unwrap()
            .target
            .identity,
        worker.identity
    );
    assert_eq!(store.retire(&worker.identity), Err(Error::Fence));
    drop(store);
    assert!(ContractStore::open_readonly(
        &sandbox.root,
        ScopeId::new("scope").unwrap(),
        "isolated-endpoint"
    )
    .unwrap()
    .retired(&worker.identity)
    .unwrap());

    let mut store = sandbox.reopen();
    let next = store.propose_identity(&worker.identity.seat).unwrap();
    assert_eq!(
        next.generation,
        Generation(worker.identity.generation.0 + 1)
    );
    let mut fresh = request(store.root(), "worker");
    fresh.identity = next.clone();
    let operation = OperationId::new("start-worker-fresh").unwrap();
    let mut io = FakeIo::new("start-worker-fresh");
    let d = descriptor();
    let h = hooks();
    let record = Lifecycle {
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
        &fresh,
        routing("worker"),
        operation,
    )
    .unwrap();
    assert_eq!(record.kind, TransactionKind::Startup);
    assert_eq!(record.outcome, Outcome::Committed);
    assert!(record.parent.is_none());
    let current = store.seat(&worker.identity.seat).unwrap().unwrap();
    assert_eq!(current.identity, next);
    assert!(!store.retired(&next).unwrap());
    assert_ne!(
        current.session.map(|session| session.native_session),
        original.map(|session| session.native_session)
    );
}
