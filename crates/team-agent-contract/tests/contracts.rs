//! Developer-owned public contract tests. The fixture provider is not Kiro or a native adapter.
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::path::PathBuf;
use std::time::Duration;

use team_agent_contract::contract::delivery::*;
use team_agent_contract::contract::descriptor::*;
use team_agent_contract::contract::fork::*;
use team_agent_contract::contract::hooks::*;
use team_agent_contract::contract::plan::*;
use team_agent_contract::contract::probe::*;
use team_agent_contract::contract::session::*;
use team_agent_contract::contract::types::*;

const NO: Reason = Reason { code: "fixture-not-supported", message: "Not supported by this test fixture" };
const HASH: Digest = Digest([1; 32]);
const POLICY_HASH: Digest = Digest([2; 32]);
const CANDIDATE_HASH: Digest = Digest([3; 32]);
static PROFILES: [InputProfile; 1] = [InputProfile {
    id: "fixture-tui", version: "1.0", harness: "fixture-engine", ui: "fixture-ui",
    platform: Platform::Linux, executable_sha256: HASH, policy_sha256: POLICY_HASH,
    operations: &[Operation::FirstBusiness, Operation::OrdinarySend, Operation::InWindowBranch],
    channel: Channel::Tmux,
    policy: Support::Supported(SubmitPolicy {
        paste_mode: PasteMode::Bracketed, payload_trailer: PayloadTrailer::None,
        initial_submit: PhysicalKey::Enter, confirmation_steps: &[], retry_budget: RetryBudget::Never,
        queue_flush: &[], timing: InputTiming {
            capture_interval: Duration::from_millis(10), stable_window: Duration::from_millis(20),
            paste_to_submit_floor: Duration::from_millis(30), deadline: Duration::from_secs(1),
        }, max_submit_keys: 1,
    }),
}];

fn descriptor() -> ProviderDescriptor {
    ProviderDescriptor {
        identity: IdentityFacet { id: "fixture", display_name: "Contract fixture", binary: "fixture-cli", aliases: &["fixture-alias"], leader: Support::Unsupported(NO) },
        model: ModelFacet { selection: Support::Supported(ModelSelection::ExactCatalogId), omitted: OmittedModel::Reject, catalog: Support::Supported(CatalogSource { arguments: &["models", "--json"], schema: "fixture-models-v1" }) },
        effort: EffortFacet { admission: [EffortAdmission::Pass, EffortAdmission::Pass, EffortAdmission::Pass, EffortAdmission::Pass, EffortAdmission::Reject(NO), EffortAdmission::Reject(NO)], carrier: Support::Supported(ValueCarrier::Flag("--effort")), inherit_team_default: false, model_dependent: true },
        auth: AuthFacet { subscription: Support::Supported(AuthMechanism::NativeExistingSession), official_api: Support::Unsupported(NO), compatible_api: Support::Unverified(NO) },
        bypass: BypassFacet { intent: Support::Supported(BypassPolicy { enabled_arguments: &["--trust"], requires_startup_consent: true }) },
        prompt: PromptFacet { carrier: Support::Supported(PromptCarrier::Argv) },
        mcp: McpFacet { carrier: Support::Supported(McpCarrier::Argv), scope: ResourceScope::RuntimeRoot, tools: &TEAM_TOOLS, excludes_ambient_configuration: true },
        tool_names: ToolNameFacet { naming: Support::Supported(ToolNaming::StaticServer { key: "owned-team" }) },
        session: SessionFacet { fresh: FreshSession::CaptureAfterLaunch, resume: Support::Supported(ResumeMode::ExactId) },
        fork: ForkFacet { in_window: Support::Supported(()), full_snapshot: Support::Unsupported(NO), native_new_seat: Support::Unverified(NO), allowed_auth: &[AuthMode::NativeSubscription] },
        input: InputFacet { profiles: Support::Supported(&PROFILES) },
        startup: StartupFacet { interactive: true, authorized_consent: Support::Supported(()) },
        probe_sources: ProbeSourcesFacet { process: Support::Supported(ProbeSource::Host), pane: Support::Supported(ProbeSource::NativeSurface), server: Support::Supported(ProbeSource::ServerLifecycle), client_binding: Support::Supported(ProbeSource::NativeRuntime), round_trip: Support::Supported(ProbeSource::NativeRuntime), semantic: Support::Unsupported(NO) },
        workspace: WorkspaceFacet { config_scope: ResourceScope::RuntimeRoot, shared_cwd: false, requires_materialization_lease: false },
        teardown: TeardownFacet { resources: &[
            ResourcePolicy { kind: ResourceKind::Prompt, disposition: ResourceDisposition::OwnedRemovable },
            ResourcePolicy { kind: ResourceKind::SessionBacking, disposition: ResourceDisposition::OwnedPreserved },
            ResourcePolicy { kind: ResourceKind::NativeDatabase, disposition: ResourceDisposition::Forbidden },
        ] },
    }
}

struct Fixture;
static FIXTURE: Fixture = Fixture;
impl CatalogHook for Fixture {
    fn discover(&self, _: &CatalogRequest, _: &mut dyn BoundedReadHost) -> Result<CatalogObservation, ReadFailure> { Err(ReadFailure::Unknown(NO)) }
}
impl PlanHook for Fixture {
    fn plan(&self, resolved: &ResolvedLaunch) -> Result<LaunchPlan, ContractError> {
        let request = resolved.request();
        let mut arguments: Vec<OsString> = vec!["chat".into()];
        let model = if let Some(model) = resolved.model() {
            let index = arguments.len(); arguments.push("--model".into()); arguments.push(model.into());
            CarrierUse::Specified(CarrierRef::Arguments(vec![index, index + 1]))
        } else { CarrierUse::NotRequested };
        let prompt = if let Some(prompt) = &request.prompt {
            let index = arguments.len(); arguments.push("--prompt".into()); arguments.push(prompt.into());
            CarrierUse::Specified(CarrierRef::Arguments(vec![index, index + 1]))
        } else { CarrierUse::NotRequested };
        let mcp = if request.mode == LaunchMode::FullWorker {
            let index = arguments.len(); arguments.push("--mcp".into()); arguments.push("owned-team".into());
            CarrierUse::Specified(CarrierRef::Arguments(vec![index, index + 1]))
        } else { CarrierUse::NotRequested };
        let bypass = if request.bypass {
            let index = arguments.len(); arguments.push("--trust".into());
            CarrierUse::Specified(CarrierRef::Arguments(vec![index]))
        } else { CarrierUse::NotRequested };
        let effort = match resolved.effort() {
            EffortResolution::Unspecified => CarrierUse::NotRequested,
            EffortResolution::Ignored { reason, .. } => CarrierUse::NotCarried(*reason),
            EffortResolution::Pass(effort) => {
                let index = arguments.len(); arguments.push("--effort".into()); arguments.push(effort.as_str().into());
                CarrierUse::Specified(CarrierRef::Arguments(vec![index, index + 1]))
            }
        };
        Ok(LaunchPlan {
            executable: request.paths.executable.clone(), arguments, cwd: request.paths.cwd.path.clone(),
            environment: EnvironmentDelta { remove: BTreeSet::new(), set: BTreeMap::new() },
            expected_session: request.resume.as_ref().map(|r| ExpectedSession::Resume(Box::new(r.binding.clone()))).unwrap_or(ExpectedSession::CaptureAfterLaunch),
            materialization: vec![], carriers: CarrierReport { model, prompt, mcp, bypass, effort },
        })
    }
}
impl MaterializeHook for Fixture {
    fn materialize(&self, _: &[OwnedResourceRequest], _: &mut dyn OwnedIo) -> Result<MaterializeReceipt, PartialFailure> {
        Err(PartialFailure { error: ContractError::Unsupported { field: "fixture I/O", reason: NO }, receipt: MaterializeReceipt { resources: vec![] } })
    }
}
impl SessionHook for Fixture {
    fn bind(&self, _: &ScopedSessionEvidence) -> Result<ResumeBinding, ContractError> { Err(ContractError::Unverified { field: "fixture capture", reason: NO }) }
}
impl SemanticReader for Fixture {
    fn read(&self, record: &NativeRecord) -> Probe<SemanticEvidence> {
        Probe { scope: record.scope.clone(), outcome: if record.bytes == b"busy" { ProbeOutcome::Observed(SemanticEvidence::Busy) } else { ProbeOutcome::Unknown(NO) } }
    }
}
impl InteractionHook for Fixture {
    fn interpret(&self, frame: &CaptureFrame) -> InteractionObservation {
        InteractionObservation { scope: frame.scope.clone(), surface: if frame.text == "fixture-composer" { InputSurface::ComposerReady } else { InputSurface::ShellOrUnknown }, predicate: None, current_message: frame.message.clone(), current_attempt: frame.attempt.clone(), paste_latch: frame.paste_latch.clone() }
    }
}
impl ForkHook for Fixture {
    fn stage(&self, _: &ResolvedFork, _: &mut dyn OwnedIo) -> Result<NativeForkPlan, PartialFailure> {
        Err(PartialFailure { error: ContractError::Unsupported { field: "fixture staging", reason: NO }, receipt: MaterializeReceipt { resources: vec![] } })
    }
}
fn hooks() -> ProviderHooks<'static> {
    ProviderHooks { catalog: HookBinding::Bound(&FIXTURE), plan: HookBinding::Bound(&FIXTURE), materialize: HookBinding::Bound(&FIXTURE), session: HookBinding::Bound(&FIXTURE), semantic: HookBinding::NotRequired(NO), interaction: HookBinding::Bound(&FIXTURE), fork: HookBinding::Bound(&FIXTURE) }
}
fn identity() -> InstanceIdentity {
    InstanceIdentity { scope: ScopeId::new("scope-one").unwrap(), seat: SeatId::new("worker-one").unwrap(), instance: InstanceId::new("instance-one").unwrap(), generation: Generation(1) }
}
fn native() -> NativeIdentity {
    NativeIdentity { version: "1.0".into(), harness: "fixture-engine".into(), ui: "fixture-ui".into(), platform: Platform::Linux, executable_sha256: HASH }
}
fn request() -> LaunchRequest {
    LaunchRequest {
        provider: "fixture".into(), operation: Operation::Fresh, mode: LaunchMode::FullWorker,
        auth: AuthMode::NativeSubscription, model: Some("native/vendor/model-X".into()),
        role_effort: Some("high".into()), team_effort: Some("max".into()), bypass: true,
        prompt: Some("quoted \"role\"\n你好".into()), identity: identity(), native: native(),
        paths: LaunchPaths { executable: "/Applications/Fixture CLI.app/bin/fixture".into(), candidate: "/tmp/contract candidate/bin".into(), cwd: CwdIdentity { path: "/tmp/工作 'one'".into(), identity: HASH }, runtime_root: "/tmp/owned root".into() },
        channel: Channel::Tmux, input_profile: Some("fixture-tui".into()), evidence_kind: EvidenceKind::Fixture,
        preassigned_session: None, resume: None,
    }
}
fn catalog() -> CatalogObservation {
    CatalogObservation { provider: ProviderId::new("fixture").unwrap(), native: native(), schema: "fixture-models-v1".into(), models: vec![ModelRecord { id: "native/vendor/model-X".into(), efforts: Support::Supported(vec![Effort::Medium, Effort::High]) }] }
}
fn resolved() -> ResolvedLaunch { resolve_launch(&descriptor(), &hooks(), &request(), Some(&catalog())).unwrap() }
fn policy() -> SubmitPolicy { PROFILES[0].policy.require("fixture").unwrap().clone() }
fn binding() -> ResumeBinding {
    let request = request();
    ResumeBinding { provider: ProviderId::new("fixture").unwrap(), native_session: NativeSessionId::new("native:session/one").unwrap(), storage: SessionStorage::Local, cwd: request.paths.cwd, native: request.native, source: request.identity, origin: CaptureOrigin::CurrentNativeSession, evidence_kind: EvidenceKind::Fixture, evidence_sha256: HASH, backing: None }
}
fn capability() -> CapabilityEvidence {
    CapabilityEvidence { provider: ProviderId::new("fixture").unwrap(), native: native(), profile_id: "fixture-tui".into(), policy_sha256: POLICY_HASH, candidate_sha256: CANDIDATE_HASH, evidence_sha256: HASH, kind: EvidenceKind::Fixture, operation: Operation::OrdinarySend, channel: Channel::Tmux }
}

#[test]
fn exact_provider_lookup_has_no_default_or_implicit_alias() {
    let d = descriptor(); let registry = [&d];
    assert_eq!(resolve_provider(&registry, "grok", NameMode::CanonicalOnly), Err(ContractError::UnknownProvider));
    assert!(resolve_provider(&registry, "fixture-alias", NameMode::CanonicalOnly).is_err());
    assert_eq!(resolve_provider(&registry, "fixture-alias", NameMode::ExplicitAliases).unwrap().identity.id, "fixture");
    assert_eq!(resolve_provider(&[&d, &d], "fixture", NameMode::CanonicalOnly), Err(ContractError::AmbiguousProvider));
}

#[test]
fn required_hooks_are_operation_specific_and_never_empty_success() {
    let mut h = hooks(); h.plan = HookBinding::NotRequired(NO);
    assert_eq!(validate_descriptor(&descriptor(), &h, Operation::Fresh), Err(ContractError::MissingHook("H2 PlanHook")));
    assert!(validate_descriptor(&descriptor(), &h, Operation::Catalog).is_ok());
    h = hooks(); h.catalog = HookBinding::Unverified(NO);
    assert!(matches!(validate_descriptor(&descriptor(), &h, Operation::Fresh), Err(ContractError::Unverified { .. })));
    h = hooks(); h.session = HookBinding::NotRequired(NO);
    assert!(validate_descriptor(&descriptor(), &h, Operation::Fresh).is_err());
    h = hooks(); h.interaction = HookBinding::NotRequired(NO);
    assert!(validate_descriptor(&descriptor(), &h, Operation::OrdinarySend).is_err());
    let mut d = descriptor(); d.prompt.carrier = Support::Supported(PromptCarrier::ScopedFile);
    h = hooks(); h.materialize = HookBinding::NotRequired(NO);
    assert!(validate_descriptor(&d, &h, Operation::Fresh).is_err());
    d = descriptor(); d.probe_sources.semantic = Support::Supported(ProbeSource::SessionRecords);
    assert!(validate_descriptor(&d, &hooks(), Operation::Fresh).is_err());
}

#[test]
fn exact_native_model_is_preserved_without_vendor_stripping_or_case_folding() {
    let value = resolved(); assert_eq!(value.model(), Some("native/vendor/model-X"));
    for wrong in ["model-X", "native/vendor/model-x", " native/vendor/model-X", "", "   "] {
        let mut r = request(); r.model = Some(wrong.into());
        assert!(resolve_launch(&descriptor(), &hooks(), &r, Some(&catalog())).is_err());
    }
    let mut r = request(); r.provider = "kiro".into();
    assert!(matches!(resolve_launch(&descriptor(), &hooks(), &r, Some(&catalog())), Err(ContractError::UnknownProvider)));
}

#[test]
fn catalog_is_bound_to_native_identity_and_schema_and_cannot_be_ambiguous() {
    let mut c = catalog(); c.native.harness = "another-engine".into();
    assert!(resolve_launch(&descriptor(), &hooks(), &request(), Some(&c)).is_err());
    c = catalog(); c.schema = "unknown-schema".into();
    assert!(resolve_launch(&descriptor(), &hooks(), &request(), Some(&c)).is_err());
    c = catalog(); c.models.push(c.models[0].clone());
    assert!(matches!(resolve_launch(&descriptor(), &hooks(), &request(), Some(&c)), Err(ContractError::AmbiguousModel)));
    assert!(resolve_launch(&descriptor(), &hooks(), &request(), None).is_err());
}

#[test]
fn effort_grammar_is_finite_and_does_not_downgrade() {
    assert_eq!(Effort::parse(" high "), Ok(Effort::High));
    for invalid in ["HIGH", "x-high", "", "auto", "extreme"] { assert_eq!(Effort::parse(invalid), Err(ContractError::UnknownEffort)); }
    let mut r = request(); r.role_effort = Some("max".into());
    assert!(matches!(resolve_launch(&descriptor(), &hooks(), &r, Some(&catalog())), Err(ContractError::EffortRejected(_))));
    r.role_effort = Some("low".into());
    assert!(matches!(resolve_launch(&descriptor(), &hooks(), &r, Some(&catalog())), Err(ContractError::EffortNotAvailableForModel)));
}

#[test]
fn team_effort_is_not_inherited_without_policy_and_role_wins() {
    let mut r = request(); r.role_effort = None;
    assert_eq!(resolve_launch(&descriptor(), &hooks(), &r, Some(&catalog())).unwrap().effort(), &EffortResolution::Unspecified);
    let mut d = descriptor(); d.effort.inherit_team_default = true;
    assert!(resolve_launch(&d, &hooks(), &r, Some(&catalog())).is_err());
    r.role_effort = Some("medium".into());
    assert_eq!(resolve_launch(&d, &hooks(), &r, Some(&catalog())).unwrap().effort(), &EffortResolution::Pass(Effort::Medium));
}

#[test]
fn ignored_effort_requires_an_explicit_carrier_report() {
    let mut d = descriptor(); d.effort.admission[Effort::High.index()] = EffortAdmission::IgnoreWithReason(NO);
    let value = resolve_launch(&d, &hooks(), &request(), Some(&catalog())).unwrap();
    let mut plan = FIXTURE.plan(&value).unwrap(); assert!(validate_launch_plan(&d, &value, &plan).is_ok());
    plan.carriers.effort = CarrierUse::NotRequested;
    assert!(validate_launch_plan(&d, &value, &plan).is_err());
}

#[test]
fn worker_launch_rejects_unsupported_auth_missing_prompt_unverified_profile_and_wrong_channel() {
    let mut r = request(); r.auth = AuthMode::OfficialApi;
    assert!(matches!(resolve_launch(&descriptor(), &hooks(), &r, Some(&catalog())), Err(ContractError::Unsupported { .. })));
    r = request(); r.prompt = None;
    assert!(resolve_launch(&descriptor(), &hooks(), &r, Some(&catalog())).is_err());
    r = request(); r.channel = Channel::Acp;
    assert!(resolve_launch(&descriptor(), &hooks(), &r, Some(&catalog())).is_err());
    let mut d = descriptor(); d.input.profiles = Support::Unverified(NO);
    assert!(matches!(resolve_launch(&d, &hooks(), &request(), Some(&catalog())), Err(ContractError::Unverified { .. })));
}

#[test]
fn worker_launch_rejects_ambient_mcp_and_global_mutation() {
    let mut d = descriptor(); d.mcp.excludes_ambient_configuration = false;
    assert!(resolve_launch(&d, &hooks(), &request(), Some(&catalog())).is_err());
    d = descriptor(); d.workspace.config_scope = ResourceScope::Global;
    assert!(resolve_launch(&d, &hooks(), &request(), Some(&catalog())).is_err());
    d = descriptor(); d.workspace.config_scope = ResourceScope::WorkingDirectory; d.workspace.shared_cwd = true;
    assert!(validate_descriptor(&d, &hooks(), Operation::Fresh).is_err());
    d.workspace.requires_materialization_lease = true;
    assert!(validate_descriptor(&d, &hooks(), Operation::Fresh).is_ok());
}

#[test]
fn plans_are_deterministic_and_keep_argv_boundaries() {
    let value = resolved(); let plan = FIXTURE.plan(&value).unwrap();
    assert!(plan == FIXTURE.plan(&value).unwrap());
    assert!(validate_launch_plan(&descriptor(), &value, &plan).is_ok());
    assert_eq!(plan.executable, request().paths.executable);
    assert_eq!(plan.cwd, request().paths.cwd.path);
    assert!(plan.arguments.contains(&OsString::from("quoted \"role\"\n你好")));
    assert!(plan.arguments.contains(&OsString::from("native/vendor/model-X")));
}

#[test]
fn missing_or_dangling_carriers_and_changed_effort_value_are_rejected() {
    let value = resolved(); let original = FIXTURE.plan(&value).unwrap();
    let mut p = original.clone(); p.carriers.mcp = CarrierUse::NotCarried(NO);
    assert!(validate_launch_plan(&descriptor(), &value, &p).is_err());
    p = original.clone(); p.carriers.prompt = CarrierUse::Specified(CarrierRef::Arguments(vec![usize::MAX]));
    assert!(validate_launch_plan(&descriptor(), &value, &p).is_err());
    p = original.clone(); p.carriers.effort = CarrierUse::Specified(CarrierRef::Arguments(vec![1, 2]));
    assert!(validate_launch_plan(&descriptor(), &value, &p).is_err());
    p = original; *p.arguments.last_mut().unwrap() = "medium".into();
    assert!(validate_launch_plan(&descriptor(), &value, &p).is_err());
}

#[test]
fn environment_unset_precedes_overlay_without_reading_process_environment() {
    let delta = EnvironmentDelta { remove: BTreeSet::from(["OLD".into(), "CURRENT".into()]), set: BTreeMap::from([("CURRENT".into(), OsString::from("new"))]) };
    let base = BTreeMap::from([("OLD".into(), OsString::from("old")), ("CURRENT".into(), OsString::from("stale")), ("KEEP".into(), OsString::from("same"))]);
    let result = delta.apply(&base).unwrap();
    assert!(!result.contains_key("OLD")); assert_eq!(result["CURRENT"], "new"); assert_eq!(result["KEEP"], "same");
    assert_eq!(base["CURRENT"], "stale");
    let bad = EnvironmentDelta { remove: BTreeSet::new(), set: BTreeMap::from([("BAD=KEY".into(), "value".into())]) };
    assert!(bad.validate().is_err());
}

#[test]
fn ownership_paths_and_resource_classification_fail_closed() {
    for relative in ["", "/absolute", "../escape", "a/../../escape"] {
        assert!(OwnedPath::new("/tmp/owned".into(), relative.into()).is_err());
    }
    assert!(OwnedPath::new("relative".into(), "prompt".into()).is_err());
    let value = resolved(); let mut p = FIXTURE.plan(&value).unwrap();
    p.materialization.push(OwnedResourceRequest { path: OwnedPath::new("/another-root".into(), "prompt".into()).unwrap(), owner: identity(), kind: ResourceKind::Prompt, contents: vec![] });
    assert!(validate_launch_plan(&descriptor(), &value, &p).is_err());
    p.materialization[0].path = OwnedPath::new(request().paths.runtime_root, "prompt".into()).unwrap();
    assert!(validate_launch_plan(&descriptor(), &value, &p).is_ok());
    p.materialization[0].kind = ResourceKind::NativeDatabase;
    assert!(validate_launch_plan(&descriptor(), &value, &p).is_err());
}

#[test]
fn submit_budgets_separate_confirmation_retry_wrap_and_queue() {
    let mut p = policy(); assert_eq!(p.key_upper_bound(), Ok(1));
    p.confirmation_steps = &[ConfirmationStep { predicate: "current-confirmation", key: PhysicalKey::Enter, deadline: Duration::from_millis(100) }]; p.max_submit_keys = 2;
    assert_eq!(p.key_upper_bound(), Ok(2)); assert!(p.validate().is_ok());
    p.retry_budget = RetryBudget::Guarded { predicate: "proven-no-effect", additional: 2, wrap_gap: 1 };
    assert!(p.validate().is_err());
    p.confirmation_steps = &[]; p.queue_flush = &[QueueAction { predicate: "current-queue", key: PhysicalKey::Enter, max_presses: 8, interval: Duration::from_millis(10) }]; p.max_submit_keys = 12;
    assert_eq!(p.key_upper_bound(), Ok(12)); assert!(p.validate().is_ok());
    p.max_submit_keys = 11; assert!(p.validate().is_err());
}

#[test]
fn invalid_timing_missing_guards_overflow_and_newline_submit_are_rejected() {
    let mut p = policy(); p.timing.capture_interval = Duration::ZERO; assert!(p.validate().is_err());
    p = policy(); p.retry_budget = RetryBudget::Guarded { predicate: "", additional: 1, wrap_gap: 0 }; assert!(p.validate().is_err());
    p = policy(); p.retry_budget = RetryBudget::Guarded { predicate: "no-effect", additional: u16::MAX, wrap_gap: 0 }; assert_eq!(p.key_upper_bound(), Err(ContractError::BudgetOverflow));
    p = policy(); p.initial_submit = PhysicalKey::LineFeed; assert!(p.validate().is_err());
    p = policy(); p.initial_submit = PhysicalKey::CtrlJ; assert!(p.validate().is_err());
    p = policy(); p.payload_trailer = PayloadTrailer::VerifiedBytes { bytes: b"\n", evidence: "" }; assert!(p.validate().is_err());
}

#[test]
fn submit_policy_resolution_requires_the_exact_profile_and_evidence_scope() {
    let d = descriptor(); let h = hooks(); let n = native(); let e = capability();
    let mut r = PolicyRequest { provider: "fixture", native: &n, profile_id: "fixture-tui", operation: Operation::OrdinarySend, channel: Channel::Tmux, evidence: &e, required_evidence_kind: EvidenceKind::Fixture, candidate_sha256: CANDIDATE_HASH };
    assert_eq!(resolve_submit_policy(&d, &h, &r).unwrap().policy().key_upper_bound(), Ok(1));
    r.required_evidence_kind = EvidenceKind::Native; assert!(resolve_submit_policy(&d, &h, &r).is_err());
    r.required_evidence_kind = EvidenceKind::Fixture; r.candidate_sha256 = HASH; assert!(resolve_submit_policy(&d, &h, &r).is_err());
    r.candidate_sha256 = CANDIDATE_HASH; r.channel = Channel::Acp; assert!(resolve_submit_policy(&d, &h, &r).is_err());
    r.channel = Channel::Tmux; r.operation = Operation::FirstBusiness; assert!(resolve_submit_policy(&d, &h, &r).is_err());
}

#[test]
fn fresh_and_resume_requests_cannot_silently_substitute_each_other() {
    let mut r = request(); r.operation = Operation::Resume;
    assert!(resolve_launch(&descriptor(), &hooks(), &r, Some(&catalog())).is_err());
    r.resume = Some(ResumeRequest { binding: binding(), expected_source: identity() });
    assert!(resolve_launch(&descriptor(), &hooks(), &r, Some(&catalog())).is_err());
    r.identity.generation = Generation(2);
    let value = resolve_launch(&descriptor(), &hooks(), &r, Some(&catalog())).unwrap();
    let mut p = FIXTURE.plan(&value).unwrap(); assert!(validate_launch_plan(&descriptor(), &value, &p).is_ok());
    p.expected_session = ExpectedSession::CaptureAfterLaunch; assert!(validate_launch_plan(&descriptor(), &value, &p).is_err());
    r.operation = Operation::Fresh; assert!(resolve_launch(&descriptor(), &hooks(), &r, Some(&catalog())).is_err());
}

#[test]
fn resume_rejects_other_harness_owner_cwd_cloud_and_missing_exact_path() {
    let b = binding(); let provider = ProviderId::new("fixture").unwrap();
    let source = identity(); let n = native(); let cwd = request().paths.cwd;
    let mut expected = ResumeExpectation { provider: &provider, source: &source, cwd: &cwd, native: &n, evidence_kind: EvidenceKind::Fixture, mode: ResumeMode::ExactId };
    assert!(validate_resume(&b, &expected).is_ok());
    let mut bad = b.clone(); bad.native.harness = "other-engine".into(); assert!(validate_resume(&bad, &expected).is_err());
    bad = b.clone(); bad.source.generation = Generation(99); assert!(validate_resume(&bad, &expected).is_err());
    bad = b.clone(); bad.cwd.identity = POLICY_HASH; assert!(validate_resume(&bad, &expected).is_err());
    bad = b.clone(); bad.storage = SessionStorage::Cloud; assert!(validate_resume(&bad, &expected).is_err());
    expected.mode = ResumeMode::ExactPath; assert!(validate_resume(&b, &expected).is_err());
}

fn fork_request(mode: ForkMode) -> ForkRequest {
    ForkRequest { mode, auth: AuthMode::NativeSubscription, source: binding(), expected_source: identity(), target: identity(), cwd: request().paths.cwd, native: native(), evidence_kind: EvidenceKind::Fixture, selected_turn: Some(1), operation_id: OperationId::new("fork-one").unwrap() }
}

#[test]
fn fork_families_require_distinct_admission_and_target_identity() {
    assert!(resolve_fork(&descriptor(), &hooks(), &fork_request(ForkMode::InWindowBranch)).is_ok());
    assert!(matches!(resolve_fork(&descriptor(), &hooks(), &fork_request(ForkMode::NewSeatFullSnapshot)), Err(ContractError::Unsupported { .. })));
    assert!(matches!(resolve_fork(&descriptor(), &hooks(), &fork_request(ForkMode::NativeNewSeat)), Err(ContractError::Unverified { .. })));
    let mut r = fork_request(ForkMode::InWindowBranch); r.target.seat = SeatId::new("other-seat").unwrap();
    assert!(resolve_fork(&descriptor(), &hooks(), &r).is_err());
    let mut h = hooks(); h.fork = HookBinding::NotRequired(NO);
    assert!(matches!(resolve_fork(&descriptor(), &h, &fork_request(ForkMode::InWindowBranch)), Err(ContractError::MissingHook("H7 ForkHook"))));
}

#[test]
fn effect_floor_never_decreases_after_observation_or_persistence_failure() {
    let all = [DeliveryEffect::NoEffect, DeliveryEffect::MayHavePasted, DeliveryEffect::PastedUnsubmitted, DeliveryEffect::MayHaveSubmitted, DeliveryEffect::Submitted];
    for before in all { for later in all { assert!(before.retain_floor(later) >= before); } }
    assert_eq!(DeliveryEffect::MayHaveSubmitted.retain_floor(DeliveryEffect::NoEffect), DeliveryEffect::MayHaveSubmitted);
}

fn scope() -> EvidenceScope {
    EvidenceScope { identity: identity(), endpoint: "/tmp/owned.sock".into(), pane: "%1".into(), binding: "binding-one".into(), session: None, sequence: 10, observed_at: Duration::from_millis(100), valid_for: Duration::from_millis(50), source: "fixture-owned-observation".into() }
}
fn process_identity() -> ProcessIdentity {
    ProcessIdentity { pid: 123, birth_identity: "birth-one".into(), executable: PathBuf::from("/fixture/bin"), executable_sha256: HASH }
}
fn expected<'a>(owner: &'a InstanceIdentity, process: &'a ProcessIdentity) -> EvidenceExpectation<'a> {
    EvidenceExpectation { identity: owner, endpoint: "/tmp/owned.sock", pane: "%1", binding: "binding-one", session: None, minimum_sequence: 10, now: Duration::from_millis(120), process, profile_id: "fixture-tui", server_key: "owned-team" }
}
fn process_probe() -> Probe<ProcessAliveEvidence> { Probe { scope: scope(), outcome: ProbeOutcome::Observed(ProcessAliveEvidence { process: process_identity() }) } }
fn pane_probe() -> Probe<PaneReadyEvidence> { Probe { scope: scope(), outcome: ProbeOutcome::Observed(PaneReadyEvidence { profile_id: "fixture-tui".into(), surface: InputSurface::ComposerReady }) } }
fn binding_probe() -> Probe<ClientBindingEvidence> { Probe { scope: scope(), outcome: ProbeOutcome::Observed(ClientBindingEvidence { server_key: "owned-team".into(), tools: TEAM_TOOLS.to_vec(), source: ClientBindingSource::NativeRegistry }) } }

#[test]
fn first_input_needs_current_composer_but_not_its_own_t3c_reply() {
    let owner = identity(); let process = process_identity(); let e = expected(&owner, &process);
    assert_eq!(evaluate_send_gate(&process_probe(), &pane_probe(), &binding_probe(), &e, Operation::FirstBusiness, BootstrapAllowance::Disabled), SendGate::ReadyForInput { consume_bootstrap: false });
    let mut pane = pane_probe(); pane.outcome = ProbeOutcome::Observed(PaneReadyEvidence { profile_id: "fixture-tui".into(), surface: InputSurface::ShellOrUnknown });
    assert!(matches!(evaluate_send_gate(&process_probe(), &pane, &binding_probe(), &e, Operation::FirstBusiness, BootstrapAllowance::Disabled), SendGate::Blocked(_)));
}

#[test]
fn unknown_capture_and_busy_are_not_permission_to_send_keys() {
    let owner = identity(); let process = process_identity(); let e = expected(&owner, &process);
    let mut pane = pane_probe(); pane.outcome = ProbeOutcome::Error { code: "capture-failed".into() };
    assert!(matches!(evaluate_send_gate(&process_probe(), &pane, &binding_probe(), &e, Operation::OrdinarySend, BootstrapAllowance::Disabled), SendGate::Blocked(_)));
    pane.outcome = ProbeOutcome::Observed(PaneReadyEvidence { profile_id: "fixture-tui".into(), surface: InputSurface::Busy });
    assert!(matches!(evaluate_send_gate(&process_probe(), &pane, &binding_probe(), &e, Operation::OrdinarySend, BootstrapAllowance::Disabled), SendGate::Blocked(_)));
}

#[test]
fn bootstrap_is_explicit_single_use_and_never_bypasses_unknown_surface() {
    let owner = identity(); let process = process_identity(); let e = expected(&owner, &process);
    let binding = Probe { scope: scope(), outcome: ProbeOutcome::Unknown(NO) };
    let allowance = BootstrapAllowance::AuthorizedFirstBusinessOnce { remaining: 1 };
    assert_eq!(evaluate_send_gate(&process_probe(), &pane_probe(), &binding, &e, Operation::FirstBusiness, allowance), SendGate::ReadyForInput { consume_bootstrap: true });
    for op in [Operation::FirstBusiness, Operation::OrdinarySend] {
        assert!(matches!(evaluate_send_gate(&process_probe(), &pane_probe(), &binding, &e, op, BootstrapAllowance::Disabled), SendGate::Blocked(_)));
    }
    for remaining in [0, 2] {
        assert!(matches!(evaluate_send_gate(&process_probe(), &pane_probe(), &binding, &e, Operation::FirstBusiness, BootstrapAllowance::AuthorizedFirstBusinessOnce { remaining }), SendGate::Blocked(_)));
    }
    let pane = Probe { scope: scope(), outcome: ProbeOutcome::Unknown(NO) };
    assert!(matches!(evaluate_send_gate(&process_probe(), &pane, &binding, &e, Operation::FirstBusiness, allowance), SendGate::Blocked(_)));
}

#[test]
fn stale_generation_endpoint_sequence_time_and_pid_cannot_reuse_ready() {
    let owner = identity(); let process = process_identity(); let e = expected(&owner, &process);
    let mut variants = Vec::new();
    let mut s = scope(); s.identity.generation = Generation(0); variants.push(s);
    s = scope(); s.endpoint = "/wrong.sock".into(); variants.push(s);
    s = scope(); s.sequence = 9; variants.push(s);
    s = scope(); s.observed_at = Duration::from_millis(1); variants.push(s);
    s = scope(); s.observed_at = Duration::from_millis(121); variants.push(s);
    for scope in variants {
        let pane = Probe { scope, outcome: pane_probe().outcome };
        assert!(matches!(evaluate_send_gate(&process_probe(), &pane, &binding_probe(), &e, Operation::OrdinarySend, BootstrapAllowance::Disabled), SendGate::Blocked(_)));
    }
    let mut p = process_identity(); p.birth_identity = "reused-pid".into();
    let p = Probe { scope: scope(), outcome: ProbeOutcome::Observed(ProcessAliveEvidence { process: p }) };
    assert!(matches!(evaluate_send_gate(&p, &pane_probe(), &binding_probe(), &e, Operation::OrdinarySend, BootstrapAllowance::Disabled), SendGate::Blocked(_)));
}

#[test]
fn independent_protocol_facts_are_preserved_when_process_probe_fails() {
    let protocol = Probe { scope: scope(), outcome: ProbeOutcome::Observed(NativeRoundTripEvidence { message: None, attempt: None, call_id: "call-one".into(), tool: LogicalTool::SendMessage, facts: vec![RoundTripFact::InvocationReceived, RoundTripFact::ResponseWritten] }) };
    let original = protocol.clone();
    let p = Probe { scope: scope(), outcome: ProbeOutcome::Unknown(NO) };
    let owner = identity(); let process = process_identity(); let e = expected(&owner, &process);
    assert!(matches!(evaluate_send_gate(&p, &pane_probe(), &binding_probe(), &e, Operation::OrdinarySend, BootstrapAllowance::Disabled), SendGate::Blocked(_)));
    assert_eq!(protocol, original);
    if let ProbeOutcome::Observed(value) = protocol.outcome { assert!(!value.facts.contains(&RoundTripFact::ClientConsumptionObserved)); }
}

#[test]
fn read_bounds_are_finite_and_positive() {
    assert!(ReadBounds { deadline: Duration::ZERO, max_output_bytes: 10 }.validate().is_err());
    assert!(ReadBounds { deadline: Duration::from_secs(1), max_output_bytes: 0 }.validate().is_err());
    assert!(ReadBounds { deadline: Duration::from_secs(1), max_output_bytes: 1024 }.validate().is_ok());
}
