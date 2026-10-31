use super::{
    projection::{self, Key, Projections, ReadRecords, WriteBatch},
    Error,
};
use crate::contract::{
    delivery::DeliveryEffect,
    plan::OwnedResourceReceipt,
    probe::ProcessIdentity,
    session::{CwdIdentity, ResumeBinding},
    types::*,
};
use serde::{Deserialize, Serialize};
use std::{
    cell::RefCell,
    collections::BTreeMap,
    path::{Path, PathBuf},
};

#[cfg(any(test, feature = "fixtures"))]
#[path = "store_fixtures.rs"]
mod fixtures;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SeatStatus {
    Starting,
    Ready,
    Stopped,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeatRecord {
    pub identity: InstanceIdentity,
    pub provider: ProviderId,
    pub native: NativeIdentity,
    pub cwd: CwdIdentity,
    pub endpoint: String,
    pub pane: String,
    pub binding_key: String,
    pub profile: String,
    pub server_key: String,
    pub evidence_kind: EvidenceKind,
    pub status: SeatStatus,
    pub process: Option<ProcessIdentity>,
    /// Captured K2 target; persisted atomically with the actual F3 routing. A
    /// deserialized value never authorizes transport without a live host recheck.
    #[serde(default)]
    pub physical: Option<crate::host::transport::TargetReceipt>,
    pub session: Option<ResumeBinding>,
    pub resources: Vec<OwnedResourceReceipt>,
    /// Captured descriptor grant; only these resource classes may use this
    /// seat's exact cwd identity. Host I/O must recheck the directory identity.
    pub workspace_resources: Vec<crate::contract::descriptor::ResourceKind>,
    pub bootstrap_used: bool,
}

impl SeatRecord {
    /// A receipt must describe this complete authority record before a live
    /// host check can authorize any transport or teardown.
    pub fn physical_target(&self) -> Result<&crate::host::transport::TargetReceipt, Error> {
        let target = self.physical.as_ref().ok_or(Error::Invalid("physical target missing"))?;
        if target.identity() != &self.identity || target.provider != self.provider || target.native != self.native
            || target.cwd != self.cwd || target.evidence_kind != self.evidence_kind
            || self.process.as_ref() != Some(&target.process.identity)
            || target.endpoint.to_str() != Some(self.endpoint.as_str()) || target.pane != self.pane
            || target.binding != self.binding_key
            || target.native_session.as_ref() != self.session.as_ref().map(|s| &s.native_session) {
            return Err(Error::Fence);
        }
        Ok(target)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Phase {
    F0Preflight,
    F1Stage,
    F2Register,
    F3Spawn,
    F4Commit,
    F5Receipt,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TransactionKind {
    Startup,
    Restart,
    InWindowBranch,
    NewSeatFullSnapshot,
    NativeNewSeat,
    Teardown,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Outcome {
    Running,
    Committed,
    Compensated,
    NeedsRecovery,
}

/// Write-ahead `pending` is durable BEFORE invoking a side-effecting host/hook.
/// On recovery it means uncertainty, never permission to replay the action.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StopReceipt {
    pub method: String,
    pub owned_process_absent: bool,
    pub owned_target_replaced: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperationRecord {
    pub id: OperationId,
    pub kind: TransactionKind,
    pub target: SeatRecord,
    pub parent: Option<SeatRecord>,
    pub phase: Phase,
    pub pending: Option<String>,
    #[serde(default)]
    pub spawn_observation: Option<crate::host::transport::SpawnObservation>,
    /// Native Fresh/Resume is separate from the authority Restart transaction.
    #[serde(default)]
    pub native_launch: Option<Operation>,
    pub resources: Vec<OwnedResourceReceipt>,
    pub effect: DeliveryEffect,
    pub outcome: Outcome,
    pub preserved: Vec<OwnedPath>,
    pub failure: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recovery_failure: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_receipt: Option<StopReceipt>,
}

#[path = "store_import.rs"]
mod import;
pub use import::LegacySnapshot;

/// Compatibility spelling; all implementation and runtime admission use file projections.
pub type ContractStore = ProjectionStore;

#[derive(Serialize, Deserialize)]
struct Manifest {
    v: u8,
    kind: String,
    scope: ScopeId,
    root: PathBuf,
    endpoint: String,
}
pub struct ProjectionStore {
    pub(crate) files: Projections,
    scope: ScopeId,
    root: PathBuf,
    endpoint: String,
    // Optimistic write tokens, not cached authority. Every save rechecks under locks.
    revisions: RefCell<BTreeMap<String, u64>>,
}
pub trait SeatAuthority {
    fn scope(&self) -> &ScopeId;
    fn root(&self) -> &Path;
    fn endpoint(&self) -> &str;
    fn seat(&self, seat: &SeatId) -> Result<Option<SeatRecord>, Error>;
    fn seats(&self) -> Result<Vec<SeatRecord>, Error>;
    fn propose_identity(&self, seat: &SeatId) -> Result<InstanceIdentity, Error>;
    fn assert_current(&self, identity: &InstanceIdentity) -> Result<SeatRecord, Error>;
    fn operation(&self, id: &OperationId) -> Result<OperationRecord, Error>;
    fn creation_receipt(
        &self,
        seat: &SeatId,
        identity: Option<&InstanceIdentity>,
    ) -> Result<Option<OperationRecord>, Error>;
    fn unfinished(&self) -> Result<Vec<OperationRecord>, Error>;
    fn leased(&self, seat: &SeatId) -> Result<bool, Error>;
    fn retire(&mut self, identity: &InstanceIdentity) -> Result<SeatRecord, Error>;
    fn retired(&self, identity: &InstanceIdentity) -> Result<bool, Error>;
    fn begin(&mut self, operation: &OperationRecord) -> Result<(), Error>;
    fn save(&mut self, operation: &OperationRecord, publish: bool) -> Result<(), Error>;
}

impl SeatAuthority for ProjectionStore {
    fn scope(&self) -> &ScopeId {
        ProjectionStore::scope(self)
    }
    fn root(&self) -> &Path {
        ProjectionStore::root(self)
    }
    fn endpoint(&self) -> &str {
        ProjectionStore::endpoint(self)
    }
    fn seat(&self, seat: &SeatId) -> Result<Option<SeatRecord>, Error> {
        ProjectionStore::seat(self, seat)
    }
    fn seats(&self) -> Result<Vec<SeatRecord>, Error> {
        ProjectionStore::seats(self)
    }
    fn propose_identity(&self, seat: &SeatId) -> Result<InstanceIdentity, Error> {
        ProjectionStore::propose_identity(self, seat)
    }
    fn assert_current(&self, identity: &InstanceIdentity) -> Result<SeatRecord, Error> {
        ProjectionStore::assert_current(self, identity)
    }
    fn operation(&self, id: &OperationId) -> Result<OperationRecord, Error> {
        ProjectionStore::operation(self, id)
    }
    fn creation_receipt(
        &self,
        seat: &SeatId,
        identity: Option<&InstanceIdentity>,
    ) -> Result<Option<OperationRecord>, Error> {
        ProjectionStore::creation_receipt(self, seat, identity)
    }
    fn unfinished(&self) -> Result<Vec<OperationRecord>, Error> {
        ProjectionStore::unfinished(self)
    }
    fn leased(&self, seat: &SeatId) -> Result<bool, Error> {
        ProjectionStore::leased(self, seat)
    }
    fn retire(&mut self, identity: &InstanceIdentity) -> Result<SeatRecord, Error> {
        ProjectionStore::retire(self, identity)
    }
    fn retired(&self, identity: &InstanceIdentity) -> Result<bool, Error> {
        ProjectionStore::retired(self, identity)
    }
    fn begin(&mut self, operation: &OperationRecord) -> Result<(), Error> {
        ProjectionStore::begin(self, operation)
    }
    fn save(&mut self, operation: &OperationRecord, publish: bool) -> Result<(), Error> {
        ProjectionStore::save(self, operation, publish)
    }
}

impl ProjectionStore {
    pub fn create(root: &Path, scope: ScopeId, endpoint: &str) -> Result<Self, Error> {
        if !root.is_absolute() || !nonblank(endpoint) {
            return Err(Error::Invalid("store identity"));
        }
        projection::validate_ancestors(root.parent().ok_or(Error::Fence)?)?;
        let mut builder = std::fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(root)?;
        let files = Projections::new(root, false);
        files.initialize()?;
        projection::immutable_json(
            &root.join("contract.json"),
            &Manifest {
                v: 1,
                kind: "contract_projection".into(),
                scope: scope.clone(),
                root: root.into(),
                endpoint: endpoint.into(),
            },
        )?;
        Ok(Self {
            files,
            scope,
            root: root.into(),
            endpoint: endpoint.into(),
            revisions: RefCell::new(BTreeMap::new()),
        })
    }
    pub fn open(root: &Path, scope: ScopeId, endpoint: &str) -> Result<Self, Error> {
        Self::open_mode(root, scope, endpoint, false)
    }
    pub fn open_readonly(root: &Path, scope: ScopeId, endpoint: &str) -> Result<Self, Error> {
        Self::open_mode(root, scope, endpoint, true)
    }
    fn open_mode(
        root: &Path,
        scope: ScopeId,
        endpoint: &str,
        readonly: bool,
    ) -> Result<Self, Error> {
        projection::validate_ancestors(root)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if std::fs::metadata(root)?.permissions().mode() & 0o077 != 0 {
                return Err(Error::Fence);
            }
        }
        let manifest: Manifest = projection::read_json(&root.join("contract.json"))?.ok_or(
            Error::Invalid("legacy contract store requires explicit migration"),
        )?;
        if manifest.v != 1
            || manifest.kind != "contract_projection"
            || manifest.scope != scope
            || manifest.root != root
            || manifest.endpoint != endpoint
        {
            return Err(Error::Fence);
        }
        Ok(Self {
            files: Projections::new(root, readonly),
            scope,
            root: root.into(),
            endpoint: endpoint.into(),
            revisions: RefCell::new(BTreeMap::new()),
        })
    }
    pub fn scope(&self) -> &ScopeId {
        &self.scope
    }
    pub fn root(&self) -> &Path {
        &self.root
    }
    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }
    pub fn seat(&self, seat: &SeatId) -> Result<Option<SeatRecord>, Error> {
        load_seat(&self.files, seat)
    }
    pub fn seats(&self) -> Result<Vec<SeatRecord>, Error> {
        let mut seats: Vec<SeatRecord> = self.files.list("seats")?;
        if seats.iter().any(|s| s.identity.scope != self.scope) {
            return Err(Error::Fence);
        }
        seats.sort_by(|a, b| a.identity.seat.as_str().cmp(b.identity.seat.as_str()));
        Ok(seats)
    }
    pub fn propose_identity(&self, seat: &SeatId) -> Result<InstanceIdentity, Error> {
        let generation: u64 = self
            .files
            .get(&Key::new("generations", seat.as_str()))?
            .unwrap_or(0);
        Ok(InstanceIdentity {
            scope: self.scope.clone(),
            seat: seat.clone(),
            generation: Generation(
                generation
                    .checked_add(1)
                    .ok_or(Error::Invalid("generation overflow"))?,
            ),
            instance: InstanceId::new(format!("instance-{}", projection::nonce()))?,
        })
    }
    pub fn operation(&self, id: &OperationId) -> Result<OperationRecord, Error> {
        let (revision, operation) = self
            .files
            .versioned::<OperationRecord>(&Key::new("operations", id.as_str()))?;
        let operation = operation.ok_or(Error::Fence)?;
        if operation.target.identity.scope != self.scope {
            return Err(Error::Fence);
        }
        self.revisions
            .borrow_mut()
            .insert(id.as_str().into(), revision);
        Ok(operation)
    }
    pub fn creation_receipt(
        &self,
        seat: &SeatId,
        identity: Option<&InstanceIdentity>,
    ) -> Result<Option<OperationRecord>, Error> {
        if identity.is_some_and(|i| i.scope != self.scope || i.seat != *seat) {
            return Err(Error::Fence);
        }
        let key = identity
            .map(|i| Key::new("creation_instances", i.instance.as_str()))
            .unwrap_or_else(|| Key::new("creation", seat.as_str()));
        let Some(id) = self.files.get::<OperationId>(&key)? else {
            return Ok(None);
        };
        let record = self.operation(&id)?;
        if record.target.identity.seat != *seat
            || identity.is_some_and(|i| i != &record.target.identity)
        {
            return Err(Error::Fence);
        }
        Ok(Some(record))
    }
    pub fn unfinished(&self) -> Result<Vec<OperationRecord>, Error> {
        Ok(self
            .files
            .list::<OperationRecord>("operations")?
            .into_iter()
            .filter(|r| matches!(r.outcome, Outcome::Running | Outcome::NeedsRecovery))
            .collect())
    }
    pub fn assert_current(&self, identity: &InstanceIdentity) -> Result<SeatRecord, Error> {
        current(&self.files, &self.scope, identity)
    }
    pub fn leased(&self, seat: &SeatId) -> Result<bool, Error> {
        Ok(self
            .files
            .get::<String>(&Key::new("leases", seat.as_str()))?
            .is_some())
    }
    pub fn retire(&mut self, identity: &InstanceIdentity) -> Result<SeatRecord, Error> {
        let current_seat = self.assert_current(identity)?;
        let mut keys = route_keys(&current_seat);
        keys.extend([
            Key::new("peers", identity.seat.as_str()),
            Key::new("seats", identity.seat.as_str()),
            Key::new("leases", identity.seat.as_str()),
            Key::new("retirements", identity.instance.as_str()),
        ]);
        let mut tx = self.files.write(keys)?;
        let seat = current(&tx, &self.scope, identity)?;
        if seat != current_seat {
            return Err(Error::Conflict);
        }
        if seat.status != SeatStatus::Stopped {
            return Err(Error::Invalid("only a stopped seat can retire"));
        }
        lifecycle_unleased(&tx, &identity.seat)?;
        tx.put(Key::new("retirements", identity.instance.as_str()), &seat)?;
        tx.delete(Key::new("seats", identity.seat.as_str()))?;
        tx.commit()?;
        Ok(seat)
    }
    pub fn retired(&self, identity: &InstanceIdentity) -> Result<bool, Error> {
        if identity.scope != self.scope {
            return Err(Error::Fence);
        }
        Ok(self
            .files
            .get::<SeatRecord>(&Key::new("retirements", identity.instance.as_str()))?
            .is_some_and(|s| s.identity == *identity))
    }
    pub(crate) fn begin(&mut self, operation: &OperationRecord) -> Result<(), Error> {
        let target = &operation.target.identity;
        let creates = !matches!(
            operation.kind,
            TransactionKind::InWindowBranch | TransactionKind::Teardown
        );
        if target.scope != self.scope || (creates && operation.target.endpoint != self.endpoint) {
            return Err(Error::fence("operation_begin_scope_or_endpoint_mismatch"));
        }
        let mut keys = operation_keys(operation);
        keys.extend([
            Key::new("generations", target.seat.as_str()),
            Key::new("instances", target.instance.as_str()),
            Key::new("creation", target.seat.as_str()),
            Key::new("creation_instances", target.instance.as_str()),
            Key::new("peers", target.seat.as_str()),
        ]);
        let mut tx = self.files.write(keys)?;
        if tx
            .get::<OperationRecord>(&Key::new("operations", operation.id.as_str()))?
            .is_some()
        {
            return Err(Error::Conflict);
        }
        if tx
            .get::<serde_json::Value>(&Key::new("peers", target.seat.as_str()))?
            .is_some()
        {
            return Err(Error::Conflict);
        }
        let old = load_seat(&tx, &target.seat)?;
        match operation.kind {
            TransactionKind::Startup
            | TransactionKind::NewSeatFullSnapshot
            | TransactionKind::NativeNewSeat => {
                if old.is_some() {
                    return Err(Error::Conflict);
                }
            }
            TransactionKind::Restart => {
                let parent = operation.parent.as_ref().ok_or_else(|| Error::fence("restart_parent_missing"))?;
                if old.as_ref() != Some(parent)
                    || target.seat != parent.identity.seat
                    || target.instance == parent.identity.instance
                    || target.generation <= parent.identity.generation
                {
                    return Err(Error::fence("restart_parent_or_new_generation_invalid"));
                }
            }
            TransactionKind::InWindowBranch | TransactionKind::Teardown => {
                if old.as_ref() != Some(&operation.target) {
                    return Err(Error::fence("operation_begin_target_changed"));
                }
            }
        }
        lifecycle_unleased(&tx, &target.seat)?;
        if let Some(parent) = &operation.parent {
            if current(&tx, &self.scope, &parent.identity)? != *parent {
                return Err(Error::fence("operation_begin_parent_changed"));
            }
            if parent.identity.seat != target.seat {
                lifecycle_unleased(&tx, &parent.identity.seat)?;
            }
        }
        claim_routes(&mut tx, operation)?;
        if creates {
            let previous: u64 = tx
                .get(&Key::new("generations", target.seat.as_str()))?
                .unwrap_or(0);
            if previous.checked_add(1) != Some(target.generation.0)
                || tx
                    .get::<InstanceIdentity>(&Key::new("instances", target.instance.as_str()))?
                    .is_some()
            {
                return Err(Error::fence("operation_begin_generation_stale_or_instance_reused"));
            }
            tx.put(
                Key::new("generations", target.seat.as_str()),
                &target.generation.0,
            )?;
            tx.put(Key::new("instances", target.instance.as_str()), target)?;
            tx.put(Key::new("creation", target.seat.as_str()), &operation.id)?;
            tx.put(
                Key::new("creation_instances", target.instance.as_str()),
                &operation.id,
            )?;
        }
        tx.put(Key::new("leases", target.seat.as_str()), &operation.id)?;
        if let Some(parent) = &operation.parent {
            if parent.identity.seat != target.seat {
                tx.put(
                    Key::new("leases", parent.identity.seat.as_str()),
                    &operation.id,
                )?;
            }
        }
        tx.put(Key::new("operations", operation.id.as_str()), operation)?;
        tx.commit()?;
        self.revisions
            .borrow_mut()
            .insert(operation.id.as_str().into(), 1);
        Ok(())
    }
    pub(crate) fn save(&mut self, operation: &OperationRecord, publish: bool) -> Result<(), Error> {
        let op_key = Key::new("operations", operation.id.as_str());
        let expected = *self
            .revisions
            .borrow()
            .get(operation.id.as_str())
            .ok_or_else(|| Error::fence("operation_save_revision_token_missing"))?;
        let previous: OperationRecord = self.files.get(&op_key)?.ok_or_else(|| op_key.fence("operation_save_record_missing"))?;
        if previous.target.identity != operation.target.identity
            || previous.kind != operation.kind
            || operation.target.identity.scope != self.scope
        {
            return Err(op_key.fence("operation_save_identity_or_kind_changed"));
        }
        let mut keys = operation_keys(operation);
        keys.extend(operation_keys(&previous));
        let mut tx = self.files.write(keys)?;
        tx.expect_revision(&op_key, expected)?;
        if tx
            .get::<String>(&Key::new("leases", operation.target.identity.seat.as_str()))?
            .as_deref()
            != Some(operation.id.as_str())
        {
            return Err(op_key.fence("operation_save_target_lease_not_owned"));
        }
        let rollback = operation.outcome == Outcome::Compensated;
        if operation.kind == TransactionKind::Restart
            && (rollback
                || (!publish && matches!(operation.phase, Phase::F0Preflight | Phase::F1Stage)))
        {
            if let Some(parent) = &operation.parent {
                tx.put(Key::new("seats", parent.identity.seat.as_str()), parent)?;
            }
        }
        if rollback
            && matches!(
                operation.kind,
                TransactionKind::Startup
                    | TransactionKind::NewSeatFullSnapshot
                    | TransactionKind::NativeNewSeat
            )
        {
            if let Some(current) = load_seat(&tx, &operation.target.identity.seat)? {
                if current.identity != operation.target.identity {
                    return Err(op_key.fence("operation_rollback_target_replaced"));
                }
                tx.delete(Key::new("seats", operation.target.identity.seat.as_str()))?;
            }
        } else if publish && !(rollback && operation.kind == TransactionKind::Restart) {
            claim_routes(&mut tx, operation)?;
            tx.put(
                Key::new("seats", operation.target.identity.seat.as_str()),
                &operation.target,
            )?;
        }
        tx.put(op_key, operation)?;
        if matches!(operation.outcome, Outcome::Committed | Outcome::Compensated) {
            for seat in std::iter::once(&operation.target.identity.seat).chain(
                operation
                    .parent
                    .iter()
                    .filter(|p| p.identity.seat != operation.target.identity.seat)
                    .map(|p| &p.identity.seat),
            ) {
                let key = Key::new("leases", seat.as_str());
                if tx.get::<String>(&key)?.as_deref() != Some(operation.id.as_str()) {
                    return Err(key.fence("operation_completion_lease_not_owned"));
                }
                tx.delete(key)?;
            }
        }
        tx.commit()?;
        self.revisions.borrow_mut().insert(
            operation.id.as_str().into(),
            expected.checked_add(1).ok_or(Error::Corrupt)?,
        );
        Ok(())
    }
}
pub(crate) fn load_seat(
    store: &impl ReadRecords,
    seat: &SeatId,
) -> Result<Option<SeatRecord>, Error> {
    store.get(&Key::new("seats", seat.as_str()))
}
pub(crate) fn current(
    store: &impl ReadRecords,
    scope: &ScopeId,
    identity: &InstanceIdentity,
) -> Result<SeatRecord, Error> {
    if &identity.scope != scope {
        return Err(Error::fence("current_identity_scope_mismatch"));
    }
    let record = load_seat(store, &identity.seat)?.ok_or_else(|| Error::fence("current_seat_missing"))?;
    if record.identity != *identity {
        return Err(Error::fence("current_instance_or_generation_replaced"));
    }
    Ok(record)
}
fn lifecycle_unleased(store: &impl ReadRecords, seat: &SeatId) -> Result<(), Error> {
    let Some(held) = store.get::<String>(&Key::new("leases", seat.as_str()))? else {
        return Ok(());
    };
    let operation: Option<OperationRecord> = store.get(&Key::new("operations", held))?;
    Err(
        if operation.is_some_and(|op| op.outcome == Outcome::NeedsRecovery) {
            Error::NeedsRecovery
        } else {
            Error::Conflict
        },
    )
}
pub(crate) fn unleased(store: &impl ReadRecords, seat: &SeatId) -> Result<(), Error> {
    if store
        .get::<String>(&Key::new("leases", seat.as_str()))?
        .is_some()
    {
        Err(Error::Conflict)
    } else {
        Ok(())
    }
}
pub(crate) fn next_id(_store: &impl ReadRecords, prefix: &str) -> Result<String, Error> {
    Ok(format!("{prefix}_{}", projection::nonce()))
}
fn route_keys(seat: &SeatRecord) -> Vec<Key> {
    let mut keys = vec![Key::new("bindings", &seat.binding_key)];
    // F0 reserves a unique binding, not the shared pre-spawn placeholder.
    // Published panes still have exclusive ownership, including fixture routes.
    if seat.pane != "unpublished" || seat.process.is_some() || seat.physical.is_some() {
        keys.push(Key::new("routes", format!("{}\0{}", seat.endpoint, seat.pane)));
    }
    keys
}
fn operation_keys(operation: &OperationRecord) -> Vec<Key> {
    let mut keys = route_keys(&operation.target);
    keys.extend([
        Key::new("operations", operation.id.as_str()),
        Key::new("peers", operation.target.identity.seat.as_str()),
        Key::new("seats", operation.target.identity.seat.as_str()),
        Key::new("leases", operation.target.identity.seat.as_str()),
    ]);
    if let Some(parent) = &operation.parent {
        keys.extend(route_keys(parent));
        keys.extend([
            Key::new("peers", parent.identity.seat.as_str()),
            Key::new("seats", parent.identity.seat.as_str()),
            Key::new("leases", parent.identity.seat.as_str()),
        ]);
    }
    keys
}
#[derive(Serialize, Deserialize)]
struct RouteClaim {
    seat: SeatId,
    operation: OperationId,
}
fn claim_routes(tx: &mut WriteBatch<'_>, operation: &OperationRecord) -> Result<(), Error> {
    let target = &operation.target;
    for key in route_keys(target) {
        if let Some(owner) = tx.get::<RouteClaim>(&key)? {
            if owner.seat != target.identity.seat {
                let live = load_seat(tx, &owner.seat)?.is_some_and(|s| {
                    s.status != SeatStatus::Stopped && route_keys(&s).contains(&key)
                });
                let pending = tx
                    .get::<OperationRecord>(&Key::new("operations", owner.operation.as_str()))?
                    .is_some_and(|o| {
                        matches!(o.outcome, Outcome::Running | Outcome::NeedsRecovery)
                            && route_keys(&o.target).contains(&key)
                    });
                if live || pending {
                    return Err(key.fence("physical_route_owned_by_another_seat"));
                }
            }
        }
        tx.put(
            key,
            &RouteClaim {
                seat: target.identity.seat.clone(),
                operation: operation.id.clone(),
            },
        )?;
    }
    Ok(())
}
