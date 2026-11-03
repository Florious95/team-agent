use super::*;
use crate::orchestration::forward::FrameworkPeer;

struct Fixture {
    store: ProjectionStore,
    root: PathBuf,
    operation: OperationRecord,
    peer: FrameworkPeer,
    sibling: FrameworkPeer,
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("peer-transfer-{}", projection::nonce()));
        let scope = ScopeId::new("peer-transfer").unwrap();
        let mut store = ProjectionStore::create(&root, scope, "test-endpoint").unwrap();
        let peer = FrameworkPeer {
            recipient: "reviewer".into(),
            route: "framework-route".into(),
            provider: "pi".into(),
        };
        let sibling = FrameworkPeer {
            recipient: "sibling".into(),
            ..peer.clone()
        };
        store
            .set_framework_routes(&[peer.clone(), sibling.clone()], Some("framework-route"))
            .unwrap();
        let target = SeatRecord {
            identity: store
                .propose_identity(&SeatId::new("reviewer").unwrap())
                .unwrap(),
            provider: ProviderId::new("cursor").unwrap(),
            native: NativeIdentity {
                version: "fixture".into(),
                harness: "fixture".into(),
                ui: "fixture".into(),
                platform: Platform::Linux,
                executable_sha256: Digest([1; 32]),
            },
            cwd: CwdIdentity {
                path: root.clone(),
                identity: Digest([2; 32]),
            },
            endpoint: "test-endpoint".into(),
            pane: "unpublished".into(),
            binding_key: "binding-reviewer".into(),
            profile: "fixture".into(),
            server_key: "fixture".into(),
            evidence_kind: EvidenceKind::Fixture,
            status: SeatStatus::Starting,
            process: None,
            physical: None,
            session: None,
            resources: vec![],
            workspace_resources: vec![],
            bootstrap_used: false,
        };
        let operation = OperationRecord {
            id: OperationId::new("convert-reviewer").unwrap(),
            kind: TransactionKind::Startup,
            target,
            parent: None,
            phase: Phase::F0Preflight,
            pending: None,
            spawn_observation: None,
            native_launch: Some(Operation::Fresh),
            resources: vec![],
            effect: DeliveryEffect::NoEffect,
            outcome: Outcome::Running,
            preserved: vec![],
            failure: None,
            recovery_failure: None,
            stop_receipt: None,
        };
        Self {
            store,
            root,
            operation,
            peer,
            sibling,
        }
    }
    fn assert_peer(&self, peer: &FrameworkPeer) {
        assert_eq!(
            self.store
                .framework_peer(&SeatId::new(&peer.recipient).unwrap())
                .unwrap()
                .as_ref(),
            Some(peer)
        );
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[test]
fn ordinary_start_and_wrong_transfer_keep_peer_conflict_fences() {
    let mut f = Fixture::new();
    assert!(f.store.begin(&f.operation).is_err());
    let changed = FrameworkPeer {
        provider: "foreign".into(),
        ..f.peer.clone()
    };
    assert!(f
        .store
        .begin_replacing_peer(&f.operation, &changed)
        .is_err());
    f.assert_peer(&f.peer);
    f.assert_peer(&f.sibling);
    assert!(!f.store.leased(&f.operation.target.identity.seat).unwrap());
    assert_eq!(
        f.store
            .propose_identity(&f.operation.target.identity.seat)
            .unwrap()
            .generation,
        Generation(1)
    );
}

#[test]
fn peer_transfer_is_durable_and_restored_only_after_compensation() {
    let mut f = Fixture::new();
    f.store.begin_replacing_peer(&f.operation, &f.peer).unwrap();
    assert!(f
        .store
        .framework_peer(&f.operation.target.identity.seat)
        .unwrap()
        .is_none());
    f.store = ProjectionStore::open(
        &f.root,
        f.operation.target.identity.scope.clone(),
        "test-endpoint",
    )
    .unwrap();
    let mut operation = f.store.operation(&f.operation.id).unwrap();
    operation.outcome = Outcome::NeedsRecovery;
    f.store.save(&operation, false).unwrap();
    assert!(f
        .store
        .framework_peer(&operation.target.identity.seat)
        .unwrap()
        .is_none());
    assert!(f.store.leased(&operation.target.identity.seat).unwrap());
    operation.outcome = Outcome::Compensated;
    operation.target.status = SeatStatus::Stopped;
    f.store.save(&operation, false).unwrap();
    f.assert_peer(&f.peer);
    f.assert_peer(&f.sibling);
    assert!(f
        .store
        .seat(&operation.target.identity.seat)
        .unwrap()
        .is_none());
    assert!(!f.store.leased(&operation.target.identity.seat).unwrap());
}

#[test]
fn startup_handshake_requires_both_flushed_responses_and_survives_helper_exit() {
    use crate::host::process::{ImageStamp, ProcessStamp};
    use crate::orchestration::{
        mcp::{response_written, CallContext},
        protocol::ConnectionRecord,
    };
    let mut f = Fixture::new();
    f.store.begin_replacing_peer(&f.operation, &f.peer).unwrap();
    f.operation.phase = Phase::F5Receipt;
    f.operation.outcome = Outcome::Committed;
    f.operation.target.status = SeatStatus::Ready;
    f.store.save(&f.operation, true).unwrap();
    let identity = f.operation.target.identity.clone();
    assert!(!f.store.startup_handshake_observed(&identity).unwrap());
    let connection = InstanceId::new("startup-connection").unwrap();
    let process = ProcessStamp {
        identity: crate::contract::probe::ProcessIdentity {
            pid: 123,
            birth_identity: "fixture".into(),
            executable: "fixture".into(),
            executable_sha256: Digest([1; 32]),
        },
        parent: 1,
        image: ImageStamp {
            device: 1,
            inode: 1,
            length: 1,
            modified_ns: 1,
        },
        inputs: vec![],
    };
    let record = ConnectionRecord {
        identity: identity.clone(),
        connection: connection.clone(),
        server_key: f.operation.target.server_key.clone(),
        binding_key: f.operation.target.binding_key.clone(),
        process: process.clone(),
        native_process: process,
        closed: false,
    };
    // Install controlled connection evidence; no process liveness is fabricated by production code.
    let key = Key::new("connections", connection.as_str());
    let index = Key::new("connection_index", identity.instance.as_str());
    let mut tx = f.store.files.write([key.clone(), index.clone()]).unwrap();
    tx.put(key, &record).unwrap();
    tx.put(index, &vec![connection.clone()]).unwrap();
    tx.commit().unwrap();
    let context = CallContext {
        identity: identity.clone(),
        binding_key: record.binding_key,
        connection_id: connection.clone(),
        task_id: "startup".into(),
    };
    response_written(
        &mut f.store,
        &context,
        &serde_json::json!({"id":1,"method":"initialize"}),
    )
    .unwrap();
    assert!(!f.store.startup_handshake_observed(&identity).unwrap());
    response_written(
        &mut f.store,
        &context,
        &serde_json::json!({"id":2,"method":"tools/list"}),
    )
    .unwrap();
    assert!(f.store.startup_handshake_observed(&identity).unwrap());
    f.store.close_connection(&identity, &connection).unwrap();
    assert!(f.store.startup_handshake_observed(&identity).unwrap());
}

#[test]
fn committed_transfer_keeps_native_authority_and_sibling_peer() {
    let mut f = Fixture::new();
    f.store.begin_replacing_peer(&f.operation, &f.peer).unwrap();
    f.operation.phase = Phase::F5Receipt;
    f.operation.outcome = Outcome::Committed;
    f.operation.target.status = SeatStatus::Ready;
    f.store.save(&f.operation, true).unwrap();
    assert!(f
        .store
        .framework_peer(&f.operation.target.identity.seat)
        .unwrap()
        .is_none());
    assert_eq!(
        f.store
            .seat(&f.operation.target.identity.seat)
            .unwrap()
            .as_ref(),
        Some(&f.operation.target)
    );
    f.assert_peer(&f.sibling);
}

#[test]
fn native_collision_does_not_partially_remove_the_legacy_peer() {
    let mut f = Fixture::new();
    f.store.fixture_put_seat(&f.operation.target).unwrap();
    assert!(f.store.begin_replacing_peer(&f.operation, &f.peer).is_err());
    f.assert_peer(&f.peer);
    f.assert_peer(&f.sibling);
}
