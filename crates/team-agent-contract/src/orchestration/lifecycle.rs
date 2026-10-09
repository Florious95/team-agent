use super::store::*;
use super::Error;
use crate::contract::delivery::DeliveryEffect;
use crate::contract::descriptor::{
    ProviderDescriptor, ResourceDisposition, ResourceKind, ResumeMode,
};
use crate::contract::fork::*;
use crate::contract::hooks::*;
use crate::contract::plan::*;
use crate::contract::probe::ProcessIdentity;
use crate::contract::session::*;
use crate::contract::types::*;

/// Framework port, not an eighth adapter hook. Implementations must fence every
/// OS action by scope/endpoint + process birth identity. Control uses K2's sole
/// physical executor, never send-keys or an adapter-owned queue.
pub trait LifecycleHost {
    fn spawn(&mut self, seat: &SeatRecord, plan: &LaunchPlan) -> Result<ProcessIdentity, Error>;
    fn stop(&mut self, seat: &SeatRecord) -> Result<(), Error>;
    fn control(
        &mut self,
        seat: &SeatRecord,
        control: &NativeControl,
    ) -> Result<DeliveryEffect, Error>;
    fn session_evidence(&mut self, seat: &SeatRecord) -> Result<ScopedSessionEvidence, Error>;
    /// Must compare fresh owner/generation, exclusive inode/hash and sharing
    /// state. A receipt is attribution, NOT deletion authority. False preserves.
    fn remove_owned(&mut self, resource: &OwnedResourceReceipt) -> Result<bool, Error>;
}

pub struct Adapter<'a> {
    pub descriptor: &'a ProviderDescriptor,
    pub hooks: &'a ProviderHooks<'a>,
    pub catalog: Option<&'a CatalogObservation>,
}
pub struct Routing {
    pub pane: String,
    pub binding_key: String,
    pub server_key: String,
}

pub struct Lifecycle<'a> {
    pub store: &'a mut ContractStore,
    pub host: &'a mut dyn LifecycleHost,
    pub io: &'a mut dyn OwnedIo,
}

impl Lifecycle<'_> {
    /// F0 is pure. A rejected model/effort/resume never stops the old process.
    pub fn startup(
        &mut self,
        adapter: &Adapter<'_>,
        request: &LaunchRequest,
        routing: Routing,
        id: OperationId,
    ) -> Result<OperationRecord, Error> {
        if !matches!(request.operation, Operation::Fresh | Operation::Resume) {
            return Err(Error::Invalid("startup operation"));
        }
        let resolved = resolve_launch(adapter.descriptor, adapter.hooks, request, adapter.catalog)?;
        let plan = adapter.hooks.plan.require("H2")?.plan(&resolved)?;
        validate_launch_plan(adapter.descriptor, &resolved, &plan)?;
        let parent = if request.operation == Operation::Resume {
            let resume = request.resume.as_ref().ok_or(Error::Fence)?;
            let parent = self.store.assert_current(&resume.expected_source)?;
            if parent.session.as_ref() != Some(&resume.binding) {
                return Err(Error::Fence);
            }
            Some(parent)
        } else {
            None
        };
        let kind = if parent.is_some() {
            TransactionKind::Restart
        } else {
            TransactionKind::Startup
        };
        let target = self.target(request, routing)?;
        let mut operation = new_operation(id, kind, target, parent);
        self.store.begin(&operation)?;
        let result = (|| {
            if let Some(parent) = operation.parent.as_ref() {
                if parent.process.is_some() && parent.status != SeatStatus::Stopped {
                    operation.pending = Some("stop-parent".into());
                    self.store.save(&operation, false)?;
                    self.host
                        .stop(operation.parent.as_ref().ok_or(Error::Fence)?)?;
                    operation.parent.as_mut().ok_or(Error::Fence)?.status = SeatStatus::Stopped;
                    operation.pending = None;
                    self.store.save(&operation, false)?;
                }
            }
            self.launch(adapter, &resolved, &plan, &mut operation)
        })();
        self.finish(operation, result)
    }

    /// New-seat H2 is validated BEFORE H7 staging. Parent and target leases are
    /// acquired together. In-window branch never allocates or spawns a seat.
    pub fn fork(
        &mut self,
        adapter: &Adapter<'_>,
        request: &ForkRequest,
        launch: Option<(&LaunchRequest, Routing)>,
    ) -> Result<OperationRecord, Error> {
        let resolved = resolve_fork(adapter.descriptor, adapter.hooks, request)?;
        let parent = self.store.assert_current(&request.expected_source)?;
        if parent.session.as_ref() != Some(&request.source) || parent.status == SeatStatus::Unknown
        {
            return Err(Error::Fence);
        }
        let (kind, target, prepared) = match request.mode {
            ForkMode::InWindowBranch => {
                if launch.is_some() || parent.status == SeatStatus::Stopped {
                    return Err(Error::Invalid("in-window launch"));
                }
                (TransactionKind::InWindowBranch, parent.clone(), None)
            }
            ForkMode::NewSeatFullSnapshot | ForkMode::NativeNewSeat => {
                let (launch, routing) = launch.ok_or(Error::Invalid("fork launch required"))?;
                if launch.fork.as_deref() != Some(&resolved) {
                    return Err(Error::Fence);
                }
                let launch_resolved =
                    resolve_launch(adapter.descriptor, adapter.hooks, launch, adapter.catalog)?;
                let plan = adapter.hooks.plan.require("H2")?.plan(&launch_resolved)?;
                validate_launch_plan(adapter.descriptor, &launch_resolved, &plan)?;
                let kind = if request.mode == ForkMode::NewSeatFullSnapshot {
                    TransactionKind::NewSeatFullSnapshot
                } else {
                    TransactionKind::NativeNewSeat
                };
                (
                    kind,
                    self.target(launch, routing)?,
                    Some((launch_resolved, plan)),
                )
            }
        };
        let mut operation = new_operation(request.operation_id.clone(), kind, target, Some(parent));
        self.store.begin(&operation)?;
        let result = (|| {
            operation.phase = Phase::F1Stage;
            operation.pending = Some("fork-stage".into());
            self.store.save(&operation, false)?;
            let staged = {
                let mut io = JournaledIo {
                    store: self.store,
                    operation: &mut operation,
                    inner: self.io,
                    descriptor: adapter.descriptor,
                };
                adapter.hooks.fork.require("H7")?.stage(&resolved, &mut io)
            };
            let staged = match staged {
                Ok(plan) => plan,
                Err(failure) => return Err(Error::Contract(failure.error)),
            };
            validate_native_fork_plan(&resolved, &staged)?;
            if let NativeForkPlan::FullSnapshot { staging, .. } = &staged {
                if staging.resources != operation.resources {
                    return Err(Error::Fence);
                }
            }
            operation.pending = None;
            self.store.save(&operation, false)?;
            match staged {
                NativeForkPlan::InWindow { control } => {
                    // No new-seat registration/spawn phase for an in-window branch.
                    operation.phase = Phase::F1Stage;
                    operation.pending = Some("native-control".into());
                    operation.effect = DeliveryEffect::MayHaveSubmitted;
                    self.store.save(&operation, false)?;
                    operation.effect = operation
                        .effect
                        .retain_floor(self.host.control(&operation.target, &control)?);
                    let binding = self.capture(adapter, &operation.target)?;
                    if binding.native_session == request.source.native_session {
                        return Err(Error::Fence);
                    }
                    operation.target.session = Some(binding);
                    operation.target.status = SeatStatus::Starting; // old T2/T3b are invalid for the new SID
                    operation.pending = None;
                    self.commit(&mut operation)
                }
                NativeForkPlan::FullSnapshot { .. } | NativeForkPlan::NativeNewSeat { .. } => {
                    let (resolved, plan) = prepared.as_ref().ok_or(Error::Fence)?;
                    self.launch(adapter, resolved, plan, &mut operation)
                }
            }
        })();
        self.finish(operation, result)
    }

    pub fn teardown(
        &mut self,
        identity: &InstanceIdentity,
        id: OperationId,
    ) -> Result<OperationRecord, Error> {
        let target = self.store.assert_current(identity)?;
        let mut operation = new_operation(id, TransactionKind::Teardown, target, None);
        operation.resources = operation.target.resources.clone();
        self.store.begin(&operation)?;
        let result = (|| {
            if operation.target.process.is_some() && operation.target.status != SeatStatus::Stopped
            {
                operation.pending = Some("stop".into());
                self.store.save(&operation, false)?;
                self.host.stop(&operation.target)?;
            }
            operation.target.status = SeatStatus::Stopped;
            operation.pending = None;
            self.cleanup(&mut operation)?;
            self.commit(&mut operation)
        })();
        self.finish(operation, result)
    }

    /// Recovery only compensates; it never replays spawn, materialization,
    /// native control, message paste or submission. Unknown spawn/control stays
    /// fenced for an explicit, evidence-backed operator reconciliation.
    pub fn recover(&mut self, id: &OperationId) -> Result<OperationRecord, Error> {
        let mut operation = self.store.operation(id)?;
        if !matches!(operation.outcome, Outcome::Running | Outcome::NeedsRecovery) {
            return Ok(operation);
        }
        if operation.phase == Phase::F4Commit && operation.pending.is_none() {
            // F4 already committed; a lost F5 receipt is not permission to undo
            // the native branch or stop a successfully committed process.
            operation.phase = Phase::F5Receipt;
            operation.outcome = Outcome::Committed;
            self.store.save(&operation, true)?;
        } else {
            self.compensate(&mut operation)?;
        }
        Ok(operation)
    }

    fn target(&self, request: &LaunchRequest, routing: Routing) -> Result<SeatRecord, Error> {
        if request.identity.scope != *self.store.scope()
            || request.paths.runtime_root != self.store.root()
            || [&routing.pane, &routing.binding_key, &routing.server_key]
                .iter()
                .any(|v| v.trim().is_empty())
        {
            return Err(Error::Fence);
        }
        Ok(SeatRecord {
            identity: request.identity.clone(),
            provider: ProviderId::new(&request.provider)?,
            native: request.native.clone(),
            cwd: request.paths.cwd.clone(),
            endpoint: self.store.endpoint().into(),
            pane: routing.pane,
            binding_key: routing.binding_key,
            server_key: routing.server_key,
            profile: request.input_profile.clone().unwrap_or_default(),
            evidence_kind: request.evidence_kind,
            status: SeatStatus::Starting,
            process: None,
            session: None,
            resources: vec![],
            bootstrap_used: false,
        })
    }

    fn launch(
        &mut self,
        adapter: &Adapter<'_>,
        resolved: &ResolvedLaunch,
        plan: &LaunchPlan,
        operation: &mut OperationRecord,
    ) -> Result<(), Error> {
        operation.phase = Phase::F1Stage;
        operation.pending = Some("materialize".into());
        self.store.save(operation, false)?;
        if !plan.materialization.is_empty() {
            let start = operation.resources.len();
            let result = {
                let mut io = JournaledIo {
                    store: self.store,
                    operation,
                    inner: self.io,
                    descriptor: adapter.descriptor,
                };
                adapter
                    .hooks
                    .materialize
                    .require("H3")?
                    .materialize(&plan.materialization, &mut io)
            };
            let receipt = result.map_err(|failure| Error::Contract(failure.error))?;
            if receipt.resources != operation.resources[start..] {
                return Err(Error::Fence);
            }
            if plan
                .materialization
                .iter()
                .any(|r| !receipt.resources.iter().any(|v| v.path == r.path))
            {
                return Err(Error::Fence);
            }
        }
        operation.target.resources = operation.resources.clone();
        operation.phase = Phase::F2Register;
        operation.pending = None;
        self.store.save(operation, true)?;
        operation.phase = Phase::F3Spawn;
        operation.pending = Some("spawn".into());
        self.store.save(operation, true)?;
        let process = self.host.spawn(&operation.target, plan)?;
        if process.pid == 0
            || process.birth_identity.trim().is_empty()
            || process.executable != plan.executable
            || process.executable_sha256 != operation.target.native.executable_sha256
        {
            return Err(Error::Fence);
        }
        operation.target.process = Some(process);
        operation.pending = None;
        self.store.save(operation, true)?;
        if adapter.descriptor.session.fresh
            != crate::contract::descriptor::FreshSession::NotApplicable
        {
            let binding = self.capture(adapter, &operation.target)?;
            match resolved.expected_session() {
                ExpectedSession::Preassigned(id) if &binding.native_session != id => {
                    return Err(Error::Fence)
                }
                ExpectedSession::Resume(old) if binding.native_session != old.native_session => {
                    return Err(Error::Fence)
                }
                ExpectedSession::SnapshotTarget {
                    session, backing, ..
                } if &binding.native_session != session
                    || binding.backing != Some(backing.path()) =>
                {
                    return Err(Error::Fence)
                }
                ExpectedSession::CaptureAfterNativeFork { parent }
                    if &binding.native_session == parent =>
                {
                    return Err(Error::Fence)
                }
                _ => {}
            }
            operation.target.session = Some(binding);
        } else if resolved.request().operation != Operation::Fresh {
            return Err(Error::Invalid("session capture required"));
        }
        self.commit(operation)
    }

    fn capture(
        &mut self,
        adapter: &Adapter<'_>,
        seat: &SeatRecord,
    ) -> Result<ResumeBinding, Error> {
        let evidence = self.host.session_evidence(seat)?;
        if evidence.scope.identity != seat.identity
            || evidence.scope.endpoint != seat.endpoint
            || evidence.scope.pane != seat.pane
            || evidence.scope.binding != seat.binding_key
            || evidence.provider != seat.provider
            || evidence.native != seat.native
            || evidence.cwd != seat.cwd
            || evidence.evidence_kind != seat.evidence_kind
        {
            return Err(Error::Fence);
        }
        let binding = adapter.hooks.session.require("H4")?.bind(&evidence)?;
        validate_resume(
            &binding,
            &ResumeExpectation {
                provider: &seat.provider,
                source: &seat.identity,
                cwd: &seat.cwd,
                native: &seat.native,
                evidence_kind: seat.evidence_kind,
                mode: ResumeMode::ExactId,
            },
        )?;
        if binding.evidence_sha256 != evidence.evidence_sha256 || binding.origin != evidence.origin
        {
            return Err(Error::Fence);
        }
        Ok(binding)
    }

    fn commit(&mut self, operation: &mut OperationRecord) -> Result<(), Error> {
        operation.phase = Phase::F4Commit;
        operation.pending = None;
        self.store.save(operation, true)?;
        operation.phase = Phase::F5Receipt;
        operation.outcome = Outcome::Committed;
        self.store.save(operation, true)
    }
    fn finish(
        &mut self,
        mut operation: OperationRecord,
        result: Result<(), Error>,
    ) -> Result<OperationRecord, Error> {
        if let Err(error) = result {
            operation.failure = Some(error.to_string());
            // If persistence itself failed, no further host effects are safe.
            if error == Error::Database {
                return Err(error);
            }
            self.compensate(&mut operation)?;
        }
        Ok(operation)
    }
    fn compensate(&mut self, operation: &mut OperationRecord) -> Result<(), Error> {
        operation.outcome = Outcome::NeedsRecovery;
        self.store.save(operation, false)?;
        if matches!(
            operation.pending.as_deref(),
            Some("spawn" | "native-control" | "stop-parent")
        ) {
            // Cannot infer absence of process/input effects from a missing receipt.
            return Ok(());
        }
        if operation.kind == TransactionKind::InWindowBranch {
            return Ok(());
        }
        if operation.target.process.is_some() && operation.target.status != SeatStatus::Stopped {
            operation.pending = Some("compensating-stop".into());
            self.store.save(operation, true)?;
            if self.host.stop(&operation.target).is_err() {
                return Ok(());
            }
        }
        operation.target.status = SeatStatus::Stopped;
        operation.pending = None;
        if self.cleanup(operation).is_err() {
            self.store.save(operation, true)?;
            return Ok(());
        }
        operation.outcome = Outcome::Compensated;
        let publish = matches!(
            operation.phase,
            Phase::F2Register | Phase::F3Spawn | Phase::F4Commit | Phase::F5Receipt
        ) || operation.kind == TransactionKind::Teardown;
        self.store.save(operation, publish)
    }
    fn cleanup(&mut self, operation: &mut OperationRecord) -> Result<(), Error> {
        for resource in operation.resources.clone() {
            let removable = resource.owner == operation.target.identity
                && resource.path.root() == self.store.root()
                && resource.exclusive
                && matches!(resource.write_effect, ResourceWriteEffect::Written { .. })
                && resource.disposition == ResourceDisposition::OwnedRemovable
                && !matches!(
                    resource.kind,
                    ResourceKind::SessionBacking
                        | ResourceKind::GlobalSettings
                        | ResourceKind::NativeDatabase
                );
            operation.pending = Some("cleanup".into());
            self.store.save(operation, false)?;
            let removed = removable && self.host.remove_owned(&resource)?;
            if !removed && !operation.preserved.contains(&resource.path) {
                operation.preserved.push(resource.path);
            }
            operation.pending = None;
            self.store.save(operation, false)?;
        }
        Ok(())
    }
}

fn new_operation(
    id: OperationId,
    kind: TransactionKind,
    target: SeatRecord,
    parent: Option<SeatRecord>,
) -> OperationRecord {
    OperationRecord {
        id,
        kind,
        target,
        parent,
        phase: Phase::F0Preflight,
        pending: None,
        resources: vec![],
        effect: DeliveryEffect::NoEffect,
        outcome: Outcome::Running,
        preserved: vec![],
        failure: None,
    }
}

/// Interposes at the *actual* owned-I/O boundary. Even an H3/H7 error with an
/// empty receipt cannot erase effects already journaled through this port.
struct JournaledIo<'a> {
    store: &'a mut ContractStore,
    operation: &'a mut OperationRecord,
    inner: &'a mut dyn OwnedIo,
    descriptor: &'a ProviderDescriptor,
}
impl OwnedIo for JournaledIo<'_> {
    fn create_exclusive(
        &mut self,
        request: &OwnedResourceRequest,
    ) -> Result<OwnedResourceReceipt, PartialFailure> {
        let fail = |error| PartialFailure {
            error,
            receipt: MaterializeReceipt { resources: vec![] },
        };
        let disposition = self
            .descriptor
            .teardown
            .resources
            .iter()
            .find(|p| p.kind == request.kind)
            .map(|p| p.disposition);
        if request.owner != self.operation.target.identity
            || request.path.root() != self.store.root()
            || !matches!(
                disposition,
                Some(ResourceDisposition::OwnedRemovable | ResourceDisposition::OwnedPreserved)
            )
            || matches!(
                request.kind,
                ResourceKind::GlobalSettings | ResourceKind::NativeDatabase
            )
            || self
                .operation
                .resources
                .iter()
                .any(|r| r.path == request.path)
        {
            return Err(fail(ContractError::Mismatch("owned materialization grant")));
        }
        let index = self.operation.resources.len();
        self.operation.resources.push(OwnedResourceReceipt {
            path: request.path.clone(),
            owner: request.owner.clone(),
            operation: self.operation.id.clone(),
            kind: request.kind,
            disposition: disposition.unwrap_or(ResourceDisposition::Forbidden),
            write_effect: ResourceWriteEffect::MayHaveWritten,
            exclusive: false,
        });
        self.store
            .save(self.operation, false)
            .map_err(|_| fail(ContractError::Invalid("journal unavailable")))?;
        let result = self.inner.create_exclusive(request);
        match &result {
            Ok(receipt) => {
                if receipt.path != request.path
                    || receipt.owner != request.owner
                    || receipt.operation != self.operation.id
                    || receipt.kind != request.kind
                    || Some(receipt.disposition) != disposition
                {
                    return Err(fail(ContractError::Mismatch("owned write receipt")));
                }
                self.operation.resources[index] = receipt.clone();
            }
            Err(failure) => {
                if let Some(receipt) = failure.receipt.resources.iter().find(|r| {
                    r.path == request.path
                        && r.owner == request.owner
                        && r.operation == self.operation.id
                        && r.kind == request.kind
                        && Some(r.disposition) == disposition
                }) {
                    self.operation.resources[index] = receipt.clone();
                }
            }
        }
        self.store
            .save(self.operation, false)
            .map_err(|_| fail(ContractError::Invalid("journal unavailable after write")))?;
        result
    }
    fn read_bound_session(
        &mut self,
        binding: &ResumeBinding,
        bounds: ReadBounds,
    ) -> Result<ReadOutput, ReadFailure> {
        bounds.validate().map_err(ReadFailure::Error)?;
        if self
            .operation
            .parent
            .as_ref()
            .and_then(|p| p.session.as_ref())
            != Some(binding)
        {
            return Err(ReadFailure::Error(ContractError::Mismatch(
                "session read grant",
            )));
        }
        self.inner.read_bound_session(binding, bounds)
    }
    fn validate_configuration(
        &mut self,
        request: &OwnedValidationRequest,
    ) -> Result<ReadOutput, ReadFailure> {
        request.bounds.validate().map_err(ReadFailure::Error)?;
        if !self.operation.resources.contains(&request.resource) {
            return Err(ReadFailure::Error(ContractError::Mismatch(
                "configuration grant",
            )));
        }
        self.inner.validate_configuration(request)
    }
}
