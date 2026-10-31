//! Developer-owned importer/authority probes; no native process is simulated as live.
#![cfg(unix)]
use rusqlite::{params, Connection};
use serde_json::json;
use std::{
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};
use team_agent_contract::{
    contract::{delivery::DeliveryEffect, session::CwdIdentity, types::*},
    orchestration::{
        mcp::{self, CallContext},
        store::*,
    },
};

const M3_SCHEMA: &str = include_str!("fixtures/m4_contract_v4_schema.sql");
static NEXT: AtomicU64 = AtomicU64::new(0);
struct Fixture {
    root: PathBuf,
    seat: SeatRecord,
}
impl Fixture {
    fn new() -> Self {
        use std::os::unix::fs::DirBuilderExt;
        let root = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "m4-import-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&root)
            .unwrap();
        let identity = InstanceIdentity {
            scope: ScopeId::new("migration-test").unwrap(),
            seat: SeatId::new("worker").unwrap(),
            instance: InstanceId::new("original").unwrap(),
            generation: Generation(7),
        };
        let seat = SeatRecord {
            identity,
            provider: ProviderId::new("pi").unwrap(),
            native: NativeIdentity {
                version: "fixture".into(),
                harness: "fixture".into(),
                ui: "fixture".into(),
                platform: Platform::MacOs,
                executable_sha256: Digest([1; 32]),
            },
            cwd: CwdIdentity {
                path: root.clone(),
                identity: Digest([2; 32]),
            },
            endpoint: "migration-endpoint".into(),
            pane: "%fixture".into(),
            binding_key: "original-binding".into(),
            profile: "fixture".into(),
            server_key: "fixture".into(),
            evidence_kind: EvidenceKind::Fixture,
            status: SeatStatus::Ready,
            process: None,
            physical: None,
            session: None,
            resources: vec![],
            workspace_resources: vec![],
            bootstrap_used: false,
        };
        let op = operation(&seat, "original-op", TransactionKind::Startup);
        let db = Connection::open(root.join("contract.db")).unwrap();
        db.execute_batch(M3_SCHEMA).unwrap();
        db.execute(
            "INSERT INTO contract_meta VALUES(?1,?2,?3,'legacy-incarnation',NULL)",
            params![
                seat.identity.scope.as_str(),
                seat.endpoint,
                root.to_str().unwrap()
            ],
        )
        .unwrap();
        db.execute(
            "INSERT INTO contract_seats VALUES(?1,?2)",
            params![
                seat.identity.seat.as_str(),
                serde_json::to_string(&seat).unwrap()
            ],
        )
        .unwrap();
        db.execute(
            "INSERT INTO contract_generations VALUES(?1,?2)",
            params![seat.identity.seat.as_str(), seat.identity.generation.0],
        )
        .unwrap();
        db.execute(
            "INSERT INTO contract_instances VALUES(?1,?2,?3)",
            params![
                seat.identity.instance.as_str(),
                seat.identity.seat.as_str(),
                seat.identity.generation.0
            ],
        )
        .unwrap();
        db.execute(
            "INSERT INTO contract_operations VALUES(?1,?2)",
            params![op.id.as_str(), serde_json::to_string(&op).unwrap()],
        )
        .unwrap();
        Self { root, seat }
    }
    fn migrate(&self) -> serde_json::Value {
        team_agent::importer::contract::migrate(
            &self.root,
            self.seat.identity.scope.clone(),
            &self.seat.endpoint,
        )
        .unwrap()
    }
    fn store(&self) -> ProjectionStore {
        ProjectionStore::open(
            &self.root,
            self.seat.identity.scope.clone(),
            &self.seat.endpoint,
        )
        .unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
fn operation(seat: &SeatRecord, id: &str, kind: TransactionKind) -> OperationRecord {
    OperationRecord {
        id: OperationId::new(id).unwrap(),
        kind,
        target: seat.clone(),
        parent: None,
        phase: Phase::F5Receipt,
        pending: None,
        spawn_observation: None,
        native_launch: None,
        resources: vec![],
        effect: DeliveryEffect::NoEffect,
        outcome: Outcome::Committed,
        preserved: vec![],
        failure: None,
        recovery_failure: None,
        stop_receipt: None,
    }
}
fn cycle(store: &mut ProjectionStore, current: &SeatRecord) -> SeatRecord {
    let mut stop = operation(current, "stop-op", TransactionKind::Teardown);
    stop.phase = Phase::F0Preflight;
    stop.outcome = Outcome::Running;
    SeatAuthority::begin(store, &stop).unwrap();
    stop.target.status = SeatStatus::Stopped;
    stop.phase = Phase::F5Receipt;
    stop.outcome = Outcome::Committed;
    SeatAuthority::save(store, &stop, true).unwrap();
    let parent = store.seat(&current.identity.seat).unwrap().unwrap();
    assert_eq!(parent.status, SeatStatus::Stopped);
    let mut target = parent.clone();
    target.identity = store.propose_identity(&parent.identity.seat).unwrap();
    target.status = SeatStatus::Starting;
    let mut start = operation(&target, "restart-op", TransactionKind::Restart);
    start.parent = Some(parent);
    start.phase = Phase::F0Preflight;
    start.outcome = Outcome::Running;
    SeatAuthority::begin(store, &start).unwrap();
    start.phase = Phase::F5Receipt;
    start.outcome = Outcome::Committed;
    SeatAuthority::save(store, &start, true).unwrap();
    store.seat(&current.identity.seat).unwrap().unwrap()
}
#[test]
fn legacy_authority_import_is_scoped_idempotent_and_preserves_stop_start_generation() {
    let fixture = Fixture::new();
    let bytes = std::fs::read(fixture.root.join("contract.db")).unwrap();
    assert!(team_agent::importer::contract::migrate(
        &fixture.root,
        ScopeId::new("foreign").unwrap(),
        &fixture.seat.endpoint
    )
    .is_err());
    assert!(!fixture.root.join("projection").exists());
    assert_eq!(fixture.migrate()["status"], "migrated");
    let mut store = fixture.store();
    assert_eq!(
        store.seat(&fixture.seat.identity.seat).unwrap(),
        Some(fixture.seat.clone())
    );
    assert_eq!(
        store
            .creation_receipt(&fixture.seat.identity.seat, Some(&fixture.seat.identity))
            .unwrap()
            .unwrap()
            .id
            .as_str(),
        "original-op"
    );
    let restarted = cycle(&mut store, &fixture.seat);
    assert_eq!(restarted.identity.generation.0, 8);
    assert_ne!(restarted.identity.instance, fixture.seat.identity.instance);
    assert_eq!(fixture.migrate()["status"], "already_migrated");
    assert_eq!(
        fixture
            .store()
            .seat(&fixture.seat.identity.seat)
            .unwrap()
            .unwrap(),
        restarted
    );
    assert_eq!(
        std::fs::read(fixture.root.join("contract.db")).unwrap(),
        bytes
    );
}
#[test]
fn importer_reads_committed_wal_without_mutating_the_legacy_database() {
    let fixture = Fixture::new();
    let db = Connection::open(fixture.root.join("contract.db")).unwrap();
    db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0; INSERT INTO contract_generations VALUES('history',3); INSERT INTO contract_instances VALUES('historical','history',3);").unwrap();
    let before = std::fs::read(fixture.root.join("contract.db")).unwrap();
    fixture.migrate();
    assert_eq!(
        fixture
            .store()
            .propose_identity(&SeatId::new("history").unwrap())
            .unwrap()
            .generation
            .0,
        4
    );
    assert_eq!(
        std::fs::read(fixture.root.join("contract.db")).unwrap(),
        before
    );
}
#[test]
fn stale_nonrunning_addresses_are_rejected_before_any_tmux_input() {
    let fixture = Fixture::new();
    for status in ["stopped", "unknown", "removed", "foreign", "paused"] {
        let directory = team_agent::direct_delivery::Directory { workspace: fixture.root.clone(), team: Some("test".into()),
            state: json!({"agents":{"worker":{"provider":"legacy-fixture","status":status,"pane_id":"%0","native_endpoint":"/nonexistent-p7-socket"}}}) };
        assert!(directory.send("worker", "MUST NOT TYPE").unwrap_err().contains("recipient_not_running"));
    }
    assert!(!fixture.root.join(".team").exists());
}

#[test]
fn native_addresses_never_fall_back_to_stale_snapshot_without_authority() {
    let fixture = Fixture::new();
    let directory = team_agent::direct_delivery::Directory { workspace: fixture.root.clone(), team: Some("test".into()),
        state: json!({"agents":{"worker":{"provider":"pi_tui","status":"running","pane_id":"%0","native_endpoint":"/nonexistent-p7-socket"}}}) };
    assert!(directory.endpoint("worker").is_err());
    assert!(!fixture.root.join(".team").exists());
}

#[test]
fn public_provider_aliases_cannot_bypass_native_authority() {
    let fixture = Fixture::new();
    for marker in [json!({"runtime_family":"contract"}), json!({"contract_scope":"native-scope"})] {
        let mut worker = json!({"provider":"grok","status":"running","pane_id":"%0","native_endpoint":"/nonexistent-p7-socket"});
        worker.as_object_mut().unwrap().extend(marker.as_object().unwrap().clone());
        let directory = team_agent::direct_delivery::Directory { workspace: fixture.root.clone(), team: Some("test".into()),
            state: json!({"agents":{"worker":worker}}) };
        assert!(directory.endpoint("worker").is_err());
    }
    assert!(!fixture.root.join(".team").exists());
}

#[test]
fn an_existing_published_pane_cannot_be_claimed_by_another_seat() {
    let fixture = Fixture::new(); fixture.migrate(); let mut store = fixture.store();
    let mut target = fixture.seat.clone();
    target.identity = store.propose_identity(&SeatId::new("intruder").unwrap()).unwrap();
    target.binding_key = "different-binding".into(); target.status = SeatStatus::Starting;
    let mut op = operation(&target, "conflicting-start", TransactionKind::Startup);
    op.phase = Phase::F0Preflight; op.outcome = Outcome::Running;
    let error = SeatAuthority::begin(&mut store, &op).unwrap_err();
    assert!(error.is_fence());
    assert!(error.to_string().contains("physical_route_owned_by_another_seat"));
    assert!(store.seat(&target.identity.seat).unwrap().is_none());
}

#[test]
fn two_concurrent_authority_starts_share_routes_without_sharing_seat_locks() {
    use std::sync::{Arc, Barrier};
    use team_agent_contract::orchestration::forward::FrameworkPeer;
    let fixture = Fixture::new();
    fixture.migrate();
    for round in 0..4 {
        let barrier = Arc::new(Barrier::new(2));
        let children: Vec<_> = ["alpha", "beta"]
            .into_iter()
            .map(|name| {
                let root = fixture.root.clone();
                let mut target = fixture.seat.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    let mut store = ProjectionStore::open(
                        &root,
                        target.identity.scope.clone(),
                        &target.endpoint,
                    )
                    .unwrap();
                    target.identity = store
                        .propose_identity(&SeatId::new(format!("{name}-{round}")).unwrap())
                        .unwrap();
                    target.pane = "unpublished".into();
                    target.binding_key = format!("binding-{name}-{round}");
                    target.status = SeatStatus::Starting;
                    let peer = FrameworkPeer {
                        recipient: "leader".into(),
                        route: "leader-route".into(),
                        provider: "framework".into(),
                    };
                    barrier.wait();
                    store
                        .set_framework_routes(&[peer], Some("leader-route"))
                        .unwrap();
                    let mut op = operation(
                        &target,
                        &format!("start-{name}-{round}"),
                        TransactionKind::Startup,
                    );
                    op.phase = Phase::F0Preflight;
                    op.outcome = Outcome::Running;
                    let began = SeatAuthority::begin(&mut store, &op);
                    barrier.wait(); // Both F0 reservations precede either simulated F3 publication.
                    began.unwrap();
                    target.pane = format!("%{name}-{round}");
                    op.target = target.clone();
                    op.phase = Phase::F3Spawn;
                    SeatAuthority::save(&mut store, &op, true).unwrap();
                    op.phase = Phase::F5Receipt;
                    op.outcome = Outcome::Committed;
                    SeatAuthority::save(&mut store, &op, true).unwrap();
                    assert_eq!(store.assert_current(&target.identity).unwrap(), target);
                })
            })
            .collect();
        for child in children {
            child.join().unwrap();
        }
    }
}

#[test]
fn concurrent_route_publications_leave_one_complete_configuration() {
    use std::sync::{Arc, Barrier};
    use team_agent_contract::orchestration::forward::FrameworkPeer;
    let fixture = Fixture::new();
    fixture.migrate();
    for _ in 0..8 {
        fixture.store().set_framework_routes(&[], None).unwrap();
        let barrier = Arc::new(Barrier::new(2));
        let children: Vec<_> = ["a", "b"]
            .into_iter()
            .map(|name| {
                let root = fixture.root.clone();
                let scope = fixture.seat.identity.scope.clone();
                let endpoint = fixture.seat.endpoint.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    let mut store = ProjectionStore::open(&root, scope, &endpoint).unwrap();
                    let peer = FrameworkPeer {
                        recipient: format!("external-{name}"),
                        route: format!("route-{name}"),
                        provider: "fixture".into(),
                    };
                    barrier.wait();
                    store
                        .set_framework_routes(&[peer.clone()], Some(&peer.route))
                        .unwrap();
                })
            })
            .collect();
        for child in children {
            child.join().unwrap();
        }
        let values: Vec<serde_json::Value> =
            std::fs::read_dir(fixture.root.join("projection/cells/peers"))
                .unwrap()
                .map(|entry| {
                    serde_json::from_slice::<serde_json::Value>(
                        &std::fs::read(entry.unwrap().path()).unwrap(),
                    )
                    .unwrap()
                })
                .filter(|cell| cell["value"]["state"] == "present")
                .collect();
        assert_eq!(
            values.len(),
            1,
            "concurrent replacement must not merge two independent routing configurations"
        );
    }
}

#[test]
fn reused_connection_after_restart_does_not_inherit_or_fence_prior_tool_evidence() {
    let fixture = Fixture::new();
    fixture.migrate();
    let mut store = fixture.store();
    let connection = InstanceId::new("reused-connection").unwrap();
    let old = CallContext {
        identity: fixture.seat.identity.clone(),
        binding_key: fixture.seat.binding_key.clone(),
        connection_id: connection.clone(),
        task_id: "manual".into(),
    };
    let request = json!({"jsonrpc":"2.0","id":1,"method":"initialize"});
    mcp::response_written(&mut store, &old, &request).unwrap();
    assert!(!store
        .connection_facts(&old.identity, &connection)
        .unwrap()
        .is_empty());
    let new = cycle(&mut store, &fixture.seat);
    assert!(store
        .completed_connection_tools(&new.identity, &connection)
        .unwrap()
        .is_empty());
    assert!(store
        .connection_facts(&new.identity, &connection)
        .unwrap()
        .is_empty());
    let fresh = CallContext {
        identity: new.identity,
        binding_key: new.binding_key,
        connection_id: connection.clone(),
        task_id: "manual".into(),
    };
    mcp::response_written(&mut store, &fresh, &request).unwrap();
    assert_eq!(
        store
            .connection_facts(&fresh.identity, &connection)
            .unwrap(),
        vec!["initialize_response_written"]
    );
    assert!(store.connection_facts(&old.identity, &connection).is_err());
}
