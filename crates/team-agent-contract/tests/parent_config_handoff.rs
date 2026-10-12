//! Fixed-name config handoff is receipt-bound and happens only after a positive
//! parent stop. Controlled lifecycle ports; no native process or account access.
mod support;
use support::*;
use team_agent_contract::contract::{delivery::*, descriptor::*, fork::*, hooks::*, plan::*, types::*};
use team_agent_contract::orchestration::{lifecycle::*, store::*, Error};

struct FixedPlan;
impl PlanHook for FixedPlan {
    fn plan(&self, resolved: &ResolvedLaunch) -> Result<LaunchPlan, ContractError> {
        let mut plan = Fake.plan(resolved)?;
        let path = OwnedPath::new(resolved.request().paths.runtime_root.clone(), "fixed-config".into())?;
        plan.materialization[0].path = path.clone();
        plan.carriers.prompt = CarrierUse::Specified(CarrierRef::Resource(path));
        Ok(plan)
    }
}
#[derive(Default)]
struct Host { inner: FakeHost, refuse_removal: bool }
impl LifecycleHost for Host {
    fn spawn(&mut self, d: &ProviderDescriptor, r: &ResolvedLaunch, seat: &SeatRecord, plan: &LaunchPlan) -> Result<SpawnedProcess, Error> {
        if self.inner.spawned > 0 { assert_eq!(self.inner.removed, 1, "retire parent before new spawn"); }
        self.inner.spawn(d, r, seat, plan)
    }
    fn stop(&mut self, seat: &SeatRecord) -> Result<(), Error> { self.inner.stop(seat) }
    fn control(&mut self, seat: &SeatRecord, control: &NativeControl) -> Result<DeliveryEffect, Error> { self.inner.control(seat, control) }
    fn session_evidence(&mut self, seat: &SeatRecord) -> Result<ScopedSessionEvidence, Error> { self.inner.session_evidence(seat) }
    fn remove_owned(&mut self, resource: &OwnedResourceReceipt) -> Result<bool, Error> {
        assert!(self.inner.stopped > 0, "no delete while parent is alive");
        if self.refuse_removal { return Ok(false); }
        self.inner.remove_owned(resource)
    }
}
fn run(fail_stop: bool, refuse_removal: bool) -> (OperationRecord, Host, usize) {
    let sandbox = Sandbox::new();
    let mut store = sandbox.store();
    let d = descriptor();
    let mut h = hooks(); h.plan = HookBinding::Bound(&FixedPlan);
    let adapter = Adapter { descriptor: &d, hooks: &h, catalog: None };
    let mut host = Host::default();
    let mut io = FakeIo::new("start");
    let r = request(store.root(), "worker");
    let first = Lifecycle { store: &mut store, host: &mut host, io: &mut io }
        .startup(&adapter, &r, routing("worker"), OperationId::new("start").unwrap()).unwrap();
    assert_eq!(first.outcome, Outcome::Committed);
    let mut resume = r;
    resume.operation = Operation::Resume;
    resume.identity.instance = InstanceId::new("worker-2").unwrap();
    resume.identity.generation = Generation(2);
    resume.resume = Some(ResumeRequest {
        binding: first.target.session.clone().unwrap(), expected_source: first.target.identity.clone(),
    });
    host.inner.fail_stop = fail_stop; host.refuse_removal = refuse_removal;
    io.operation = OperationId::new("restart").unwrap();
    let record = Lifecycle { store: &mut store, host: &mut host, io: &mut io }
        .startup(&adapter, &resume, routing("worker"), OperationId::new("restart").unwrap()).unwrap();
    (record, host, io.writes)
}
#[test]
fn exact_conflicting_parent_config_is_retired_between_stop_and_new_materialization() {
    let (record, host, writes) = run(false, false);
    assert_eq!(record.outcome, Outcome::Committed);
    assert_eq!(host.inner.stopped, 1);
    assert_eq!(host.inner.removed, 1);
    assert_eq!(host.inner.spawned, 2);
    assert_eq!(writes, 2);
    assert!(record.preserved.is_empty());
    assert_eq!(record.parent.unwrap().status, SeatStatus::Stopped);
}
#[test]
fn unconfirmed_stop_or_cleanup_preserves_parent_and_never_rewrites_or_respawns() {
    for (fail_stop, refuse_removal, pending) in [(true, false, "stop-parent"), (false, true, "cleanup-parent")] {
        let (record, host, writes) = run(fail_stop, refuse_removal);
        assert_eq!(record.outcome, Outcome::NeedsRecovery);
        assert_eq!(record.pending.as_deref(), Some(pending));
        assert_eq!(host.inner.spawned, 1);
        assert_eq!(writes, 1);
        assert_eq!(host.inner.removed, 0);
        assert_eq!(record.preserved.len(), 1);
    }
}
