//! Provider-neutral K2/K3 bridge. This owns no queue, retry scheduler, native
//! grammar or credentials. Input evidence comes from the registered contract;
//! absent protocol/profile evidence is a refusal, never synthetic readiness.
use std::path::PathBuf;
use std::time::Duration;

use super::{lifecycle::*, store::SeatRecord, supervisor::*, Error};
use crate::contract::{
    delivery::*, descriptor::*, fork::NativeControl, hooks::*, plan::*, session::CaptureOrigin,
    types::*,
};
use crate::host::{
    clock::Clock,
    command::RealCommandRunner,
    digest, digest_hex,
    files::ScopedDirectory,
    materialize::remove_quiescent,
    process::{resolve_cwd, ProcessState},
    tmux::{HostLimits, TmuxHost},
    transport::*,
    HostError,
};
use crate::runtime::{delivery::*, journal::FileJournal, probes::*};

pub struct Registration<'a> {
    pub descriptor: &'a ProviderDescriptor,
    pub hooks: &'a ProviderHooks<'a>,
    pub controls: &'a [ControlDefinition],
}
pub struct PhysicalSettings {
    pub tmux: PathBuf,
    pub limits: HostLimits,
    pub action_budget: Duration,
    pub freshness: Duration,
    pub journal_bytes: usize,
}
/// One operation context. The shared store still owns lifecycle/outbox leases.
/// A reopened context does not refresh old protocol evidence or reset bootstrap.
pub struct PhysicalRuntime<'a> {
    registration: Registration<'a>,
    settings: PhysicalSettings,
    operation: OperationId,
    clock: &'a dyn Clock,
    quiescent: Option<SeatRecord>,
    pub evidence: Vec<CapabilityEvidence>,
    pub protocol: Option<ProtocolSnapshot>,
    pub bootstrap: Option<&'a mut dyn BootstrapCommit>,
}
fn host_error(error: HostError) -> Error {
    Error::Host(error.operation)
}
impl<'a> PhysicalRuntime<'a> {
    pub fn new(
        registration: Registration<'a>,
        settings: PhysicalSettings,
        operation: OperationId,
        clock: &'a dyn Clock,
    ) -> Result<Self, Error> {
        require_absolute(&settings.tmux, "tmux executable")?;
        if settings.action_budget.is_zero()
            || settings.freshness.is_zero()
            || settings.journal_bytes == 0
        {
            return Err(Error::Invalid("physical runtime bounds"));
        }
        Ok(Self {
            registration,
            settings,
            operation,
            clock,
            quiescent: None,
            evidence: vec![],
            protocol: None,
            bootstrap: None,
        })
    }
    /// Fresh T1/T2 plus independently supplied protocol facts. Capturing a pane
    /// never upgrades server writes into a native client binding.
    pub fn readiness(&self, seat: &SeatRecord) -> Result<ReadinessSample, Error> {
        let target = self.target(seat)?;
        let deadline = self.deadline()?;
        let mut host = self.reopen(target, deadline)?;
        let capture = host
            .capture(target, self.clock, deadline, self.settings.freshness)
            .map_err(host_error)?;
        let frame = CaptureFrame {
            scope: capture.scope,
            text: if capture.mode == crate::host::transport::PaneMode::Normal {
                capture.text
            } else {
                String::new()
            },
            baseline: None,
            profile_id: seat.profile.clone(),
            operation: Operation::OrdinarySend,
            message: None,
            attempt: None,
            after_step: None,
            paste_latch: PasteLatch::NeverSeen,
        };
        let interaction = self
            .registration
            .hooks
            .interaction
            .require("H6 Interaction")?;
        let protocol = self.protocol.clone().unwrap_or_else(|| {
            collect_protocol(
                target,
                &seat.identity.instance,
                &seat.server_key,
                &[],
                self.clock.now(),
                self.settings.freshness,
            )
        });
        Ok(ReadinessSample {
            process: collect_process(target, self.clock, self.settings.freshness),
            pane: collect_pane(&frame, *interaction),
            binding: protocol.binding,
            server: protocol.server,
            minimum_sequence: 0,
            now: self.clock.now(),
        })
    }
    /// Current registry capture, not a server-side tools/list inference. The
    /// caller serializes this with its seat lifecycle/outbox owner.
    pub fn client_binding(
        &mut self,
        seat: &SeatRecord,
        panel: &crate::runtime::native_panel::NativePanelPolicy,
    ) -> Result<crate::contract::probe::Probe<crate::contract::probe::ClientBindingEvidence>, Error>
    {
        let target = self.target(seat)?.clone();
        let policy = self.policy(&target, &seat.profile, Operation::ToolInspect)?;
        if panel.profile_id != seat.profile
            || panel.server_key != seat.server_key
            || panel.policy_sha256 != policy.profile().policy_sha256
        {
            return Err(Error::Fence);
        }
        if self.control(seat, &NativeControl::InspectTools)? != DeliveryEffect::Submitted {
            return Err(Error::Host("native registry control unconfirmed"));
        }
        let deadline = self.deadline()?;
        let mut host = self.reopen(&target, deadline)?;
        let mut journal = FileJournal::new(
            ScopedDirectory::reopen(target.directory.clone()).map_err(host_error)?,
            self.settings.journal_bytes,
        )
        .map_err(host_error)?;
        crate::runtime::native_panel::capture_and_close(
            &mut host,
            crate::runtime::native_panel::PanelRequest {
                target: &target,
                interaction: *self
                    .registration
                    .hooks
                    .interaction
                    .require("H6 Interaction")?,
                policy: panel,
                operation: &self.operation,
                clock: self.clock,
                deadline,
                freshness: self.settings.freshness,
            },
            &mut journal,
        )
        .map_err(host_error)
    }
    fn deadline(&self) -> Result<Duration, Error> {
        self.clock
            .now()
            .checked_add(self.settings.action_budget)
            .ok_or(Error::Invalid("physical deadline"))
    }
    fn target<'b>(&self, seat: &'b SeatRecord) -> Result<&'b TargetReceipt, Error> {
        let target = seat
            .physical
            .as_ref()
            .ok_or(Error::Invalid("physical target missing"))?;
        if target.identity() != &seat.identity
            || target.provider != seat.provider
            || target.native != seat.native
            || target.cwd != seat.cwd
            || target.evidence_kind != seat.evidence_kind
            || seat.process.as_ref() != Some(&target.process.identity)
            || target.endpoint.to_str() != Some(seat.endpoint.as_str())
            || target.pane != seat.pane
            || target.binding != seat.binding_key
            || target.native_session.as_ref() != seat.session.as_ref().map(|s| &s.native_session)
            || seat.provider.as_str() != self.registration.descriptor.identity.id
        {
            return Err(Error::Fence);
        }
        Ok(target)
    }
    fn reopen(
        &self,
        target: &TargetReceipt,
        deadline: Duration,
    ) -> Result<TmuxHost<RealCommandRunner>, Error> {
        TmuxHost::restore_owned(
            target.clone(),
            self.settings.tmux.clone(),
            self.settings.limits,
            RealCommandRunner::default(),
            self.clock,
            deadline,
        )
        .map_err(host_error)
    }
    fn policy(
        &self,
        target: &TargetReceipt,
        profile: &str,
        operation: Operation,
    ) -> Result<ResolvedSubmitPolicy<'_>, Error> {
        let mut matching = self.evidence.iter().filter(|e| {
            e.provider == target.provider
                && e.native == target.native
                && e.profile_id == profile
                && e.operation == operation
                && e.channel == Channel::Tmux
                && e.kind == target.evidence_kind
                && e.candidate_sha256 == target.candidate_sha256
        });
        let evidence = matching
            .next()
            .ok_or(Error::Invalid("input evidence missing"))?;
        if matching.next().is_some() {
            return Err(Error::Invalid("ambiguous input evidence"));
        }
        Ok(resolve_submit_policy(
            self.registration.descriptor,
            self.registration.hooks,
            &PolicyRequest {
                provider: target.provider.as_str(),
                native: &target.native,
                profile_id: profile,
                operation,
                channel: Channel::Tmux,
                evidence,
                required_evidence_kind: target.evidence_kind,
                candidate_sha256: target.candidate_sha256,
            },
        )?)
    }
}
impl LifecycleHost for PhysicalRuntime<'_> {
    fn spawn(
        &mut self,
        descriptor: &ProviderDescriptor,
        resolved: &ResolvedLaunch,
        seat: &SeatRecord,
        plan: &LaunchPlan,
    ) -> Result<SpawnedProcess, Error> {
        if descriptor != self.registration.descriptor
            || resolved.request().identity != seat.identity
            || resolved.provider() != &seat.provider
            || resolved.request().native != seat.native
            || resolved.request().paths.cwd != seat.cwd
        {
            return Err(Error::Fence);
        }
        // All descriptor/plan admission precedes even the private directory create.
        validate_launch_plan(descriptor, resolved, plan)?;
        let key = serde_json::to_vec(&seat.identity)?;
        let name = format!("native-{}", &digest_hex(digest(&key))[..24]);
        let directory = ScopedDirectory::create(
            &resolved.request().paths.runtime_root.join(name),
            seat.identity.clone(),
        )
        .map_err(host_error)?;
        let mut host = TmuxHost::new(
            directory,
            self.settings.tmux.clone(),
            self.settings.limits,
            RealCommandRunner::default(),
        )
        .map_err(host_error)?;
        let spawned = host.spawn_owned(
            descriptor,
            resolved,
            plan,
            &MaterializeReceipt {
                resources: seat.resources.clone(),
            },
            self.clock,
            self.deadline()?,
        );
        // Missing/partial spawn remains K3 pending+fenced; never guess a PID or replay.
        if let Some(error) = spawned.problem {
            return Err(host_error(error));
        }
        let target = spawned
            .target
            .ok_or(Error::Host("spawn target not observed"))?;
        Ok(SpawnedProcess {
            process: target.process.identity.clone(),
            endpoint: target
                .endpoint
                .to_str()
                .ok_or(Error::Invalid("endpoint utf8"))?
                .into(),
            pane: target.pane.clone(),
            binding_key: target.binding.clone(),
            physical: Some(target),
        })
    }
    fn stop(&mut self, seat: &SeatRecord) -> Result<(), Error> {
        let target = self.target(seat)?.clone();
        let deadline = self.deadline()?;
        let mut host = self.reopen(&target, deadline)?;
        let receipt = host.close_owned_pane(&target, self.clock, deadline);
        if receipt.pane_close.outcome != StepOutcome::Confirmed
            || receipt.native != ProcessState::Exited
        {
            return Err(Error::Host("owned native exit not observed"));
        }
        self.quiescent = Some(seat.clone());
        // Preserve endpoint, attempt journals and host diagnostics. Never kill-server,
        // unlink a socket, remove a shared cwd, or recurse over another instance.
        Ok(())
    }
    fn control(
        &mut self,
        seat: &SeatRecord,
        control: &NativeControl,
    ) -> Result<DeliveryEffect, Error> {
        let target = self.target(seat)?.clone();
        let kind = match control {
            NativeControl::InspectSession => ControlKind::InspectSession,
            NativeControl::InspectTools => ControlKind::InspectTools,
            NativeControl::BranchCurrent => ControlKind::BranchCurrent,
            NativeControl::BranchToTurn(_) => ControlKind::BranchToTurn,
            NativeControl::Exit => ControlKind::Exit,
        };
        let mut definitions = self
            .registration
            .controls
            .iter()
            .filter(|d| d.profile_id == seat.profile && d.kind == kind);
        let definition = definitions
            .next()
            .ok_or(Error::Invalid("native control not registered"))?;
        if definitions.next().is_some() {
            return Err(Error::Invalid("ambiguous native control"));
        }
        let input = PreparedInput::control(self.operation.clone(), control, definition)?;
        let policy = self.policy(&target, &seat.profile, definition.operation)?;
        let attempt = AttemptId::new(format!("control-{}", self.operation.as_str()))?;
        let deadline = self.deadline()?;
        let mut host = self.reopen(&target, deadline)?;
        let mut journal = FileJournal::new(
            ScopedDirectory::reopen(target.directory.clone()).map_err(host_error)?,
            self.settings.journal_bytes,
        )
        .map_err(host_error)?;
        // Controls use their explicit surface guard, not the business T3 gate.
        // Empty protocol facts stay Unknown and are never written as bindings.
        let protocol = collect_protocol(
            &target,
            &seat.identity.instance,
            &seat.server_key,
            &[],
            self.clock.now(),
            self.settings.freshness,
        );
        let report = inject_with_contract(
            &mut host,
            InjectionRequest {
                target: &target,
                input: &input,
                attempt: &attempt,
                operation: definition.operation,
                policy: &policy,
                protocol: &protocol,
                protocol_requirement: ProtocolRequirement::ServerAndClientBinding,
                server_key: &seat.server_key,
                deadline,
                freshness: self.settings.freshness,
                bootstrap: None,
                lane: None,
            },
            &mut journal,
            None,
            self.clock,
        );
        if report.persistence != PersistenceState::Durable || !report.problems.is_empty() {
            return Err(Error::Host("native control outcome uncertain"));
        }
        Ok(report.effect_floor)
    }
    fn session_evidence(&mut self, seat: &SeatRecord) -> Result<ScopedSessionEvidence, Error> {
        // A provider must explicitly register this control. Passive-only adapters
        // retain their existing capture path. Never query newest/global sessions.
        if self.registration.controls.iter().any(|control| {
            control.profile_id == seat.profile && control.kind == ControlKind::InspectSession
        }) {
            let deadline = self.deadline()?;
            loop {
                let ready = self.readiness(seat)?;
                if matches!(ready.pane.outcome, crate::contract::probe::ProbeOutcome::Observed(ref pane) if pane.surface == InputSurface::ComposerReady)
                {
                    break;
                }
                if self.clock.now() >= deadline {
                    return Err(Error::Host("native composer startup timed out"));
                }
                self.clock.sleep(self.settings.limits.poll_interval);
            }
            if self.control(seat, &NativeControl::InspectSession)? != DeliveryEffect::Submitted {
                return Err(Error::Host("native session control unconfirmed"));
            }
        }
        let target = self.target(seat)?.clone();
        let deadline = self.deadline()?;
        let mut host = self.reopen(&target, deadline)?;
        let _lane = host.acquire_lane(&target).map_err(host_error)?;
        let frame = host
            .capture(&target, self.clock, deadline, self.settings.freshness)
            .map_err(host_error)?;
        let record = frame.text.into_bytes();
        Ok(ScopedSessionEvidence {
            scope: frame.scope,
            provider: seat.provider.clone(),
            native: seat.native.clone(),
            cwd: seat.cwd.clone(),
            origin: CaptureOrigin::CurrentNativeSession,
            evidence_kind: seat.evidence_kind,
            evidence_sha256: digest(&record),
            record,
        })
    }
    fn remove_owned(&mut self, resource: &OwnedResourceReceipt) -> Result<bool, Error> {
        let Some(seat) = &self.quiescent else {
            return Ok(false);
        };
        if !seat.resources.contains(resource) || resource.owner != seat.identity {
            return Err(Error::Fence);
        }
        let root = if seat.workspace_resources.contains(&resource.kind) {
            seat.cwd.clone()
        } else {
            let target = self.target(seat)?;
            let runtime = target.directory.path.parent().ok_or(Error::Fence)?;
            if resource.path.root() != runtime {
                return Err(Error::Fence);
            }
            resolve_cwd(runtime).map_err(host_error)?
        };
        remove_quiescent(&root, &seat.identity, resource).map_err(host_error)
    }
}
impl DeliveryHost for PhysicalRuntime<'_> {
    fn deliver(&mut self, job: &DeliveryJob) -> Result<DeliveryReceipt, Error> {
        if job.channel != Channel::Tmux {
            return Err(Error::Invalid("physical delivery channel"));
        }
        let target = self.target(&job.target)?.clone();
        // A borrow of bootstrap must not borrow the descriptor/evidence registry.
        let bootstrap = self.bootstrap.take();
        let policy = self.policy(&target, &job.target.profile, job.operation)?;
        if policy.profile().policy_sha256 != job.policy_sha256 {
            return Err(Error::Fence);
        }
        let protocol = self
            .protocol
            .as_ref()
            .ok_or(Error::Invalid("protocol evidence missing"))?;
        let envelope = PreparedEnvelope::from_logical(&job.envelope)?;
        let input = PreparedInput::business(&envelope);
        let deadline = self.deadline()?;
        let mut host = self.reopen(&target, deadline)?;
        let mut journal = FileJournal::new(
            ScopedDirectory::reopen(target.directory.clone()).map_err(host_error)?,
            self.settings.journal_bytes,
        )
        .map_err(host_error)?;
        let report = deliver_envelope(
            &mut host,
            InjectionRequest {
                target: &target,
                input: &input,
                attempt: &job.attempt,
                operation: job.operation,
                policy: &policy,
                protocol,
                protocol_requirement: ProtocolRequirement::ServerAndClientBinding,
                server_key: &job.target.server_key,
                deadline,
                freshness: self.settings.freshness,
                bootstrap,
                lane: None,
            },
            &mut journal,
            None,
            self.clock,
        );
        report
            .business_receipt()
            .ok_or(Error::Invalid("business receipt missing"))
    }
}
