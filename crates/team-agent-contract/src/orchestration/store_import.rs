//! Explicit offline import boundary. No SQLite dependency and no physical effects.
use super::*;
use crate::orchestration::{
    forward::FrameworkPeer,
    mcp::{CallRecord, FactRecord, ToolEvidence},
    protocol::ConnectionRecord,
};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LegacySnapshot {
    pub scope: ScopeId,
    pub endpoint: String,
    pub root: PathBuf,
    pub source: Value,
    pub seats: Vec<SeatRecord>,
    /// SQLite row order preserves the original creation-receipt ordering.
    pub operations: Vec<OperationRecord>,
    pub generations: BTreeMap<String, u64>,
    pub instances: Vec<InstanceIdentity>,
    pub leases: BTreeMap<String, String>,
    pub retirements: Vec<SeatRecord>,
    pub connections: Vec<ConnectionRecord>,
    pub calls: Vec<(InstanceIdentity, String, Value, Value)>,
    pub facts: Vec<(InstanceIdentity, String, String)>,
    pub peers: Vec<FrameworkPeer>,
    pub result_route: Option<String>,
}

impl ProjectionStore {
    /// Publish a complete scoped snapshot, with the authority manifest LAST.
    /// Interrupted imports resume from the immutable source snapshot, never
    /// combine rows from two different legacy snapshots or replay an outbox.
    pub fn import_legacy(
        root: &Path,
        scope: ScopeId,
        endpoint: &str,
        source: LegacySnapshot,
    ) -> Result<(), Error> {
        projection::validate_ancestors(root)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if std::fs::metadata(root)?.permissions().mode() & 0o077 != 0 {
                return Err(Error::fence("legacy_import_root_permissions"));
            }
        }
        let _import_lock = projection::lock_file(&root.join("legacy-import.lock"))?;
        if root.join("contract.json").exists() {
            Self::open_readonly(root, scope, endpoint)?;
            return Ok(());
        }
        projection::validate_ancestors(root)?;
        if source.scope != scope || source.endpoint != endpoint || source.root != root {
            return Err(Error::Invalid(
                "legacy import scope, endpoint or root mismatch",
            ));
        }
        let archive = root.join("legacy-import.json");
        if projection::read_json::<LegacySnapshot>(&archive)?.is_none() {
            match projection::immutable_json(&archive, &source) {
                Ok(()) => (),
                Err(error) if !archive.exists() => return Err(error),
                Err(_) => (),
            }
        }
        let snapshot: LegacySnapshot = projection::read_json(&archive)?.ok_or(Error::Corrupt)?;
        if snapshot.scope != scope || snapshot.endpoint != endpoint || snapshot.root != root {
            return Err(Error::Invalid("legacy import snapshot identity mismatch"));
        }
        let mut records = BTreeMap::<Key, Value>::new();
        let check = |identity: &InstanceIdentity| -> Result<(), Error> {
            if identity.scope != scope {
                return Err(Error::Invalid("legacy record crosses scope"));
            }
            Ok(())
        };
        for seat in snapshot.seats.iter().chain(&snapshot.retirements) {
            check(&seat.identity)?;
        }
        for seat in &snapshot.seats {
            if !snapshot.instances.contains(&seat.identity)
                || snapshot
                    .generations
                    .get(seat.identity.seat.as_str())
                    .copied()
                    .unwrap_or(0)
                    < seat.identity.generation.0
            {
                return Err(Error::Invalid(
                    "legacy seat generation or instance ledger mismatch",
                ));
            }
            records.insert(
                Key::new("seats", seat.identity.seat.as_str()),
                serde_json::to_value(seat)?,
            );
        }
        for operation in &snapshot.operations {
            check(&operation.target.identity)?;
            if let Some(parent) = &operation.parent {
                check(&parent.identity)?;
            }
            records.insert(
                Key::new("operations", operation.id.as_str()),
                serde_json::to_value(operation)?,
            );
            if matches!(
                operation.kind,
                TransactionKind::Startup
                    | TransactionKind::NewSeatFullSnapshot
                    | TransactionKind::NativeNewSeat
                    | TransactionKind::Restart
            ) {
                records.insert(
                    Key::new("creation", operation.target.identity.seat.as_str()),
                    serde_json::to_value(&operation.id)?,
                );
                records.insert(
                    Key::new(
                        "creation_instances",
                        operation.target.identity.instance.as_str(),
                    ),
                    serde_json::to_value(&operation.id)?,
                );
            }
        }
        for (seat, generation) in &snapshot.generations {
            SeatId::new(seat.clone())?;
            records.insert(
                Key::new("generations", seat),
                serde_json::to_value(generation)?,
            );
        }
        for identity in &snapshot.instances {
            check(identity)?;
            if snapshot
                .generations
                .get(identity.seat.as_str())
                .copied()
                .unwrap_or(0)
                < identity.generation.0
            {
                return Err(Error::Invalid("legacy generation regressed"));
            }
            records.insert(
                Key::new("instances", identity.instance.as_str()),
                serde_json::to_value(identity)?,
            );
        }
        for (seat, operation) in &snapshot.leases {
            SeatId::new(seat.clone())?;
            OperationId::new(operation.clone())?;
            records.insert(Key::new("leases", seat), serde_json::to_value(operation)?);
        }
        for seat in &snapshot.retirements {
            records.insert(
                Key::new("retirements", seat.identity.instance.as_str()),
                serde_json::to_value(seat)?,
            );
        }
        let mut connections = BTreeMap::<String, Vec<InstanceId>>::new();
        for connection in &snapshot.connections {
            check(&connection.identity)?;
            records.insert(
                Key::new("connections", connection.connection.as_str()),
                serde_json::to_value(connection)?,
            );
            connections
                .entry(connection.identity.instance.as_str().into())
                .or_default()
                .push(connection.connection.clone());
        }
        for (instance, ids) in connections {
            records.insert(
                Key::new("connection_index", instance),
                serde_json::to_value(ids)?,
            );
        }
        for (caller, key, request, response) in &snapshot.calls {
            check(caller)?;
            records.insert(
                Key::new(
                    "calls",
                    crate::orchestration::mcp::call_address(caller, key)?,
                ),
                serde_json::to_value(CallRecord {
                    caller: caller.clone(),
                    key: key.clone(),
                    request: request.clone(),
                    response: response.clone(),
                })?,
            );
        }
        let mut facts = BTreeMap::<String, FactRecord>::new();
        for (caller, key, fact) in &snapshot.facts {
            check(caller)?;
            let address = crate::orchestration::mcp::call_address(caller, key)?;
            facts
                .entry(address)
                .or_insert_with(|| FactRecord {
                    caller: caller.clone(),
                    key: key.clone(),
                    facts: BTreeSet::new(),
                })
                .facts
                .insert(fact.clone());
        }
        for (address, record) in &facts {
            records.insert(Key::new("facts", address), serde_json::to_value(record)?);
        }
        for connection in &snapshot.connections {
            let mut evidence = ToolEvidence {
                caller: connection.identity.clone(),
                connection: connection.connection.clone(),
                calls: BTreeMap::new(),
                facts: BTreeSet::new(),
            };
            let prefix = format!("{}:", connection.connection.as_str());
            for record in facts.values().filter(|r| r.caller == connection.identity) {
                let Some(id) = record.key.strip_prefix(&prefix) else {
                    continue;
                };
                let Ok(id) = serde_json::from_str::<Value>(id) else {
                    continue;
                };
                if !(id.is_string() || id.is_i64() || id.is_u64()) {
                    continue;
                }
                evidence.facts.extend(record.facts.iter().cloned());
                if record.facts.contains("invocation_received")
                    && record.facts.contains("response_written")
                {
                    let address =
                        crate::orchestration::mcp::call_address(&record.caller, &record.key)?;
                    if let Some(call) = records.get(&Key::new("calls", address)) {
                        if let Some(name @ ("send_message" | "report_result" | "get_team_status")) =
                            call.pointer("/request/name").and_then(Value::as_str)
                        {
                            evidence.calls.insert(name.into(), record.key.clone());
                        }
                    }
                }
            }
            records.insert(
                ToolEvidence::key(&connection.identity, &connection.connection)?,
                serde_json::to_value(evidence)?,
            );
        }
        for peer in &snapshot.peers {
            if !nonblank(&peer.recipient) || !nonblank(&peer.route) || !nonblank(&peer.provider) {
                return Err(Error::Invalid("legacy framework route"));
            }
            records.insert(
                Key::new("peers", &peer.recipient),
                serde_json::to_value(peer)?,
            );
        }
        records.insert(
            Key::new("framework_config", "result_route"),
            serde_json::to_value(&snapshot.result_route)?,
        );
        let active_operations: Vec<_> = snapshot
            .operations
            .iter()
            .filter(|op| {
                matches!(op.outcome, Outcome::Running | Outcome::NeedsRecovery)
                    || snapshot.seats.iter().any(|s| {
                        s.identity == op.target.identity && s.status != SeatStatus::Stopped
                    })
            })
            .collect();
        let keys = records
            .keys()
            .cloned()
            .chain(
                active_operations
                    .iter()
                    .flat_map(|op| route_keys(&op.target)),
            )
            .collect::<Vec<_>>();
        let files = Projections::new(root, false);
        files.initialize()?;
        let mut tx = files.write(keys)?;
        for (key, value) in records {
            tx.put(key, &value)?;
        }
        for operation in active_operations {
            claim_routes(&mut tx, operation)?;
        }
        tx.commit()?;
        let manifest = Manifest {
            v: 1,
            kind: "contract_projection".into(),
            scope: scope.clone(),
            endpoint: endpoint.into(),
            root: root.into(),
        };
        match projection::immutable_json(&root.join("contract.json"), &manifest) {
            Ok(()) => Ok(()),
            Err(_) if root.join("contract.json").exists() => {
                Self::open_readonly(root, scope, endpoint).map(|_| ())
            }
            Err(error) => Err(error),
        }
    }
}
