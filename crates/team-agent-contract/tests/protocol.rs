//! Connection metadata is not a replacement for physical target/liveness proof.
mod support;
use support::*;
use team_agent_contract::contract::{probe::ProcessIdentity, types::*};
use team_agent_contract::host::process::{ImageStamp, ProcessStamp};
use team_agent_contract::orchestration::{protocol::*, Error};

#[test]
fn unpublished_native_target_cannot_be_upgraded_by_a_server_claim() {
    let sandbox = Sandbox::new();
    let mut store = sandbox.store();
    let seat = start(
        &mut store,
        &mut FakeHost::default(),
        &mut FakeIo::new("unused"),
        "worker",
    )
    .target;
    let process = ProcessStamp {
        identity: ProcessIdentity {
            pid: 100,
            birth_identity: "controlled".into(),
            executable: "/controlled/candidate".into(),
            executable_sha256: HASH,
        },
        parent: 99,
        image: ImageStamp {
            device: 1,
            inode: 1,
            length: 1,
            modified_ns: 0,
        },
    };
    let record = ConnectionRecord {
        identity: seat.identity.clone(),
        connection: InstanceId::new("connection").unwrap(),
        server_key: seat.server_key,
        binding_key: seat.binding_key,
        process: process.clone(),
        native_process: process,
        closed: false,
    };
    assert_eq!(
        store.register_connection(&record),
        Err(Error::Invalid("native target unpublished"))
    );
    assert!(store.connections(&seat.identity).unwrap().is_empty());
}

#[test]
fn identity_proposals_do_not_register_a_generation_or_reuse_an_instance() {
    let sandbox = Sandbox::new();
    let mut store = sandbox.store();
    let id = SeatId::new("worker").unwrap();
    let a = store.propose_identity(&id).unwrap();
    let b = store.propose_identity(&id).unwrap();
    assert_eq!(a.generation, Generation(1));
    assert_eq!(b.generation, Generation(1));
    assert_ne!(a.instance, b.instance);
    assert!(store.seat(&id).unwrap().is_none());
    start(
        &mut store,
        &mut FakeHost::default(),
        &mut FakeIo::new("unused"),
        "worker",
    );
    assert_eq!(
        store.propose_identity(&id).unwrap().generation,
        Generation(2)
    );
    assert_eq!(store.seats().unwrap().len(), 1);
}

#[test]
fn returned_uncertain_registry_probe_blocks_input_but_permits_owned_teardown() {
    use team_agent_contract::orchestration::{
        lifecycle::Lifecycle,
        operator::OperatorSend,
        store::{Outcome, SeatStatus},
    };
    let sandbox = Sandbox::new();
    let mut store = sandbox.store();
    let mut host = FakeHost::default();
    let mut io = FakeIo::new("unused");
    let seat = start(&mut store, &mut host, &mut io, "worker").target;
    let operation = OperationId::new("inspect").unwrap();
    store.begin_observation(&seat.identity, &operation).unwrap();
    assert!(store
        .begin_observation(&seat.identity, &OperationId::new("other").unwrap())
        .is_err());
    assert!(store
        .fail_observation(&seat.identity, &OperationId::new("other").unwrap())
        .is_err());
    store.fail_observation(&seat.identity, &operation).unwrap();
    assert_eq!(
        store.seat(&seat.identity.seat).unwrap().unwrap().status,
        SeatStatus::Unknown
    );
    assert!(store
        .send_from_operator(&OperatorSend {
            recipient: seat.identity.seat.clone(),
            content: "do not replay".into(),
            task: None,
            message: None,
            mailbox: false
        })
        .is_err());
    let stopped = Lifecycle {
        store: &mut store,
        host: &mut host,
        io: &mut io,
    }
    .teardown(&seat.identity, OperationId::new("stop").unwrap())
    .unwrap();
    assert_eq!(stopped.outcome, Outcome::Committed);
    assert_eq!(stopped.target.status, SeatStatus::Stopped);
}

#[test]
fn readonly_status_connection_cannot_enqueue_or_change_a_seat() {
    use team_agent_contract::orchestration::{operator::OperatorSend, store::ContractStore};
    let sandbox = Sandbox::new();
    let mut store = sandbox.store();
    let seat = start(
        &mut store,
        &mut FakeHost::default(),
        &mut FakeIo::new("unused"),
        "worker",
    )
    .target;
    let mut view =
        ContractStore::open_readonly(store.root(), store.scope().clone(), store.endpoint())
            .unwrap();
    assert_eq!(view.seat(&seat.identity.seat).unwrap(), Some(seat.clone()));
    assert!(view
        .send_from_operator(&OperatorSend {
            recipient: seat.identity.seat.clone(),
            content: "must not write".into(),
            task: None,
            message: None,
            mailbox: false
        })
        .is_err());
    assert!(store.deliveries().unwrap().is_empty());
    assert_eq!(store.seat(&seat.identity.seat).unwrap(), Some(seat));
}
