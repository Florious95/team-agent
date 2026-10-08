use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::path::PathBuf;

use super::descriptor::*;
use super::hooks::ProviderHooks;
use super::session::*;
use super::types::*;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelRecord { pub id: String, pub efforts: Support<Vec<Effort>> }

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CatalogObservation {
    pub provider: ProviderId,
    pub native: NativeIdentity,
    pub schema: String,
    pub models: Vec<ModelRecord>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EffortResolution {
    Unspecified,
    Pass(Effort),
    Ignored { requested: Effort, reason: Reason },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LaunchMode { LaunchOnly, FullWorker }

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LaunchPaths {
    pub executable: PathBuf,
    pub candidate: PathBuf,
    pub cwd: CwdIdentity,
    pub runtime_root: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResumeRequest { pub binding: ResumeBinding, pub expected_source: InstanceIdentity }

/// Inputs are supplied by the framework; this library never reads the ambient environment.
/// No Debug implementation: prompt and environment payloads must not enter incidental logs.
#[derive(Clone, PartialEq, Eq)]
pub struct LaunchRequest {
    pub provider: String,
    pub operation: Operation,
    pub mode: LaunchMode,
    pub auth: AuthMode,
    pub model: Option<String>,
    pub role_effort: Option<String>,
    pub team_effort: Option<String>,
    pub bypass: bool,
    pub prompt: Option<String>,
    pub identity: InstanceIdentity,
    pub native: NativeIdentity,
    pub paths: LaunchPaths,
    pub channel: Channel,
    pub input_profile: Option<String>,
    pub evidence_kind: EvidenceKind,
    pub preassigned_session: Option<NativeSessionId>,
    pub resume: Option<ResumeRequest>,
}

#[derive(Clone, PartialEq, Eq)]
pub struct ResolvedLaunch {
    request: LaunchRequest,
    provider: ProviderId,
    model: Option<String>,
    effort: EffortResolution,
}

impl ResolvedLaunch {
    pub fn request(&self) -> &LaunchRequest { &self.request }
    pub fn provider(&self) -> &ProviderId { &self.provider }
    pub fn model(&self) -> Option<&str> { self.model.as_deref() }
    pub fn effort(&self) -> &EffortResolution { &self.effort }
}

pub fn resolve_launch(
    descriptor: &ProviderDescriptor, hooks: &ProviderHooks<'_>, request: &LaunchRequest,
    catalog: Option<&CatalogObservation>,
) -> Result<ResolvedLaunch, ContractError> {
    if request.provider != descriptor.identity.id { return Err(ContractError::UnknownProvider); }
    if !matches!(request.operation, Operation::Fresh | Operation::Resume) {
        return Err(ContractError::Invalid("launch operation"));
    }
    validate_descriptor(descriptor, hooks, request.operation)?;
    request.native.validate()?;
    for path in [&request.paths.executable, &request.paths.candidate, &request.paths.cwd.path, &request.paths.runtime_root] {
        require_absolute(path, "launch path")?;
    }
    descriptor.auth.mechanism(request.auth).require("authentication mode")?;
    let model_selection = descriptor.model.selection.require("model selection")?;
    let provider = ProviderId::new(descriptor.identity.id)?;
    let model = request.model.clone();
    if let Some(id) = &model {
        if !nonblank(id) { return Err(ContractError::Invalid("model id")); }
    } else if descriptor.model.omitted == OmittedModel::Reject {
        return Err(ContractError::Invalid("explicit model required"));
    }
    let mut selected = None;
    if *model_selection == ModelSelection::ExactCatalogId && model.is_some() {
        let observation = catalog.ok_or(ContractError::Invalid("catalog evidence required"))?;
        let source = descriptor.model.catalog.require("catalog")?;
        if observation.provider != provider || observation.native != request.native || observation.schema != source.schema {
            return Err(ContractError::Mismatch("catalog identity"));
        }
        if observation.models.iter().any(|record| !nonblank(&record.id)) {
            return Err(ContractError::Invalid("catalog model id"));
        }
        let mut records = observation.models.iter().filter(|record| Some(&record.id) == model.as_ref());
        selected = Some(records.next().ok_or(ContractError::UnknownModel)?);
        if records.next().is_some() { return Err(ContractError::AmbiguousModel); }
    }
    let effort_text = request.role_effort.as_deref().or_else(|| {
        if descriptor.effort.inherit_team_default { request.team_effort.as_deref() } else { None }
    });
    let effort = if let Some(raw) = effort_text {
        let effort = Effort::parse(raw)?;
        match &descriptor.effort.admission[effort.index()] {
            EffortAdmission::Pass => {
                descriptor.effort.carrier.require("effort carrier")?;
                if descriptor.effort.model_dependent {
                    let record = selected.ok_or(ContractError::Invalid("model-specific effort evidence required"))?;
                    if !record.efforts.require("model efforts")?.contains(&effort) {
                        return Err(ContractError::EffortNotAvailableForModel);
                    }
                }
                EffortResolution::Pass(effort)
            }
            EffortAdmission::IgnoreWithReason(reason) => EffortResolution::Ignored { requested: effort, reason: *reason },
            EffortAdmission::Reject(reason) => return Err(ContractError::EffortRejected(*reason)),
        }
    } else { EffortResolution::Unspecified };
    if request.bypass {
        let bypass = descriptor.bypass.intent.require("bypass")?;
        if bypass.enabled_arguments.is_empty() || bypass.enabled_arguments.iter().any(|arg| !nonblank(arg)) {
            return Err(ContractError::Invalid("bypass arguments"));
        }
        if bypass.requires_startup_consent {
            descriptor.startup.authorized_consent.require("startup consent")?;
            hooks.interaction.require("H6 InteractionHook")?;
        }
    }
    if let Some(prompt) = &request.prompt {
        if !nonblank(prompt) { return Err(ContractError::Invalid("prompt")); }
        descriptor.prompt.carrier.require("prompt")?;
    }
    if request.mode == LaunchMode::FullWorker {
        if request.prompt.is_none() { return Err(ContractError::Invalid("worker prompt required")); }
        descriptor.mcp.carrier.require("worker MCP")?;
        if !descriptor.mcp.excludes_ambient_configuration { return Err(ContractError::Invalid("worker MCP isolation")); }
        let profile_id = request.input_profile.as_deref().ok_or(ContractError::ProfileUnavailable)?;
        let profiles = descriptor.input.profiles.require("input profiles")?;
        let profile = profiles.iter().find(|p| p.id == profile_id && p.matches_native(&request.native)
            && p.channel == request.channel).ok_or(ContractError::ProfileUnavailable)?;
        profile.policy.require("input profile")?.validate()?;
        if !profile.operations.contains(&Operation::FirstBusiness) || !profile.operations.contains(&Operation::OrdinarySend) {
            return Err(ContractError::Invalid("worker input operations"));
        }
    }
    // Global installers require a separate authorization path not present in this package.
    if descriptor.workspace.config_scope == ResourceScope::Global
        || descriptor.mcp.scope == ResourceScope::Global
        || matches!(descriptor.mcp.carrier, Support::Supported(McpCarrier::GlobalInstaller))
    {
        return Err(ContractError::Invalid("global materialization is not authorized"));
    }
    match request.operation {
        Operation::Fresh => {
            if request.resume.is_some() { return Err(ContractError::Invalid("fresh launch has resume binding")); }
            if (descriptor.session.fresh == FreshSession::Preassigned) != request.preassigned_session.is_some() {
                return Err(ContractError::Invalid("preassigned session"));
            }
        }
        Operation::Resume => {
            if request.preassigned_session.is_some() { return Err(ContractError::Invalid("resume cannot allocate a new session")); }
            let resume = request.resume.as_ref().ok_or(ContractError::Invalid("resume binding required"))?;
            let mode = *descriptor.session.resume.require("resume")?;
            validate_resume(&resume.binding, &ResumeExpectation {
                provider: &provider, source: &resume.expected_source, cwd: &request.paths.cwd,
                native: &request.native, evidence_kind: request.evidence_kind, mode,
            })?;
            if request.identity.scope != resume.expected_source.scope || request.identity.seat != resume.expected_source.seat
                || request.identity.generation <= resume.expected_source.generation
            {
                return Err(ContractError::Mismatch("restart generation"));
            }
        }
        _ => return Err(ContractError::Invalid("launch operation")),
    }
    Ok(ResolvedLaunch { request: request.clone(), provider, model, effort })
}

#[derive(Clone, PartialEq, Eq)]
pub struct EnvironmentDelta {
    pub remove: BTreeSet<String>,
    pub set: BTreeMap<String, OsString>,
}

impl EnvironmentDelta {
    pub fn validate(&self) -> Result<(), ContractError> {
        for key in self.remove.iter().chain(self.set.keys()) {
            if !valid_env_key(key) {
                return Err(ContractError::Invalid("environment key"));
            }
        }
        if self.set.values().any(|value| value.as_encoded_bytes().contains(&0)) { return Err(ContractError::Invalid("environment value")); }
        Ok(())
    }
    /// Explicit order: remove inherited values, then apply this launch's overlay.
    pub fn apply(&self, base: &BTreeMap<String, OsString>) -> Result<BTreeMap<String, OsString>, ContractError> {
        self.validate()?;
        let mut result = base.clone();
        for key in &self.remove { result.remove(key); }
        result.extend(self.set.clone());
        Ok(result)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CarrierRef { Arguments(Vec<usize>), Environment(String), Resource(OwnedPath) }

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CarrierUse {
    Specified(CarrierRef), NotRequested, NotCarried(Reason), Rejected(Reason),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CarrierReport {
    pub model: CarrierUse,
    pub prompt: CarrierUse,
    pub mcp: CarrierUse,
    pub bypass: CarrierUse,
    pub effort: CarrierUse,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExpectedSession {
    Preassigned(NativeSessionId), CaptureAfterLaunch, Resume(Box<ResumeBinding>), NotApplicable,
}

#[derive(Clone, PartialEq, Eq)]
pub struct OwnedResourceRequest {
    pub path: OwnedPath,
    pub owner: InstanceIdentity,
    pub kind: ResourceKind,
    pub contents: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OwnedResourceReceipt {
    pub path: OwnedPath,
    pub owner: InstanceIdentity,
    pub operation: OperationId,
    pub kind: ResourceKind,
    pub disposition: ResourceDisposition,
    pub bytes_sha256: Digest,
    pub exclusive: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MaterializeReceipt { pub resources: Vec<OwnedResourceReceipt> }

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PartialFailure { pub error: ContractError, pub receipt: MaterializeReceipt }

/// Structured executable/argv/environment, never a shell fragment. No Debug payload dump.
#[derive(Clone, PartialEq, Eq)]
pub struct LaunchPlan {
    pub executable: PathBuf,
    pub arguments: Vec<OsString>,
    pub cwd: PathBuf,
    pub environment: EnvironmentDelta,
    pub expected_session: ExpectedSession,
    pub materialization: Vec<OwnedResourceRequest>,
    pub carriers: CarrierReport,
}

fn validate_carrier(carrier: &CarrierUse, plan: &LaunchPlan) -> Result<(), ContractError> {
    let valid = match carrier {
        CarrierUse::Specified(CarrierRef::Arguments(indices)) => !indices.is_empty() && indices.iter().all(|i| *i < plan.arguments.len()),
        CarrierUse::Specified(CarrierRef::Environment(key)) => plan.environment.set.contains_key(key),
        CarrierUse::Specified(CarrierRef::Resource(path)) => plan.materialization.iter().any(|resource| &resource.path == path),
        _ => true,
    };
    if valid { Ok(()) } else { Err(ContractError::Invalid("carrier reference")) }
}

fn require_carried(carrier: &CarrierUse, requested: bool) -> Result<(), ContractError> {
    if (requested && matches!(carrier, CarrierUse::Specified(_))) || (!requested && *carrier == CarrierUse::NotRequested) {
        Ok(())
    } else { Err(ContractError::Invalid("requested value was not carried")) }
}

/// Validates plan structure/provenance, not the truth of native flags or encoded config.
/// Native interpretation still needs provider-specific tests and native evidence.
pub fn validate_launch_plan(descriptor: &ProviderDescriptor, resolved: &ResolvedLaunch, plan: &LaunchPlan) -> Result<(), ContractError> {
    let request = resolved.request();
    if descriptor.identity.id != resolved.provider().as_str() || plan.executable != request.paths.executable || plan.cwd != request.paths.cwd.path {
        return Err(ContractError::Mismatch("launch plan identity"));
    }
    if plan.arguments.iter().any(|arg| arg.as_encoded_bytes().contains(&0)) { return Err(ContractError::Invalid("argv contains NUL")); }
    plan.environment.validate()?;
    let expected = if let Some(resume) = &request.resume { ExpectedSession::Resume(Box::new(resume.binding.clone())) } else {
        match descriptor.session.fresh {
            FreshSession::CaptureAfterLaunch => ExpectedSession::CaptureAfterLaunch,
            FreshSession::NotApplicable => ExpectedSession::NotApplicable,
            FreshSession::Preassigned => ExpectedSession::Preassigned(request.preassigned_session.clone().ok_or(ContractError::Invalid("preassigned session"))?),
        }
    };
    if plan.expected_session != expected { return Err(ContractError::Mismatch("expected session")); }
    let mut paths = Vec::new();
    for resource in &plan.materialization {
        let scope = match resource.kind {
            ResourceKind::McpConfig => descriptor.mcp.scope,
            ResourceKind::SessionBacking | ResourceKind::RuntimeBridge => ResourceScope::RuntimeRoot,
            _ => descriptor.workspace.config_scope,
        };
        let root = match scope {
            ResourceScope::RuntimeRoot => &request.paths.runtime_root,
            ResourceScope::WorkingDirectory => &request.paths.cwd.path,
            ResourceScope::Global => return Err(ContractError::Invalid("global materialization")),
        };
        if resource.owner != request.identity || paths.contains(&resource.path)
            || resource.path.root() != root.as_path()
        { return Err(ContractError::Mismatch("materialization ownership")); }
        let policy = descriptor.teardown.resources.iter().find(|p| p.kind == resource.kind)
            .ok_or(ContractError::Invalid("unclassified resource"))?;
        if matches!(policy.disposition, ResourceDisposition::Forbidden | ResourceDisposition::Shared) {
            return Err(ContractError::Invalid("non-owned materialization"));
        }
        paths.push(resource.path.clone());
    }
    for carrier in [&plan.carriers.model, &plan.carriers.prompt, &plan.carriers.mcp, &plan.carriers.bypass, &plan.carriers.effort] { validate_carrier(carrier, plan)?; }
    require_carried(&plan.carriers.model, resolved.model().is_some())?;
    require_carried(&plan.carriers.prompt, request.prompt.is_some())?;
    require_carried(&plan.carriers.mcp, request.mode == LaunchMode::FullWorker)?;
    require_carried(&plan.carriers.bypass, request.bypass)?;
    match resolved.effort() {
        EffortResolution::Unspecified => require_carried(&plan.carriers.effort, false)?,
        EffortResolution::Pass(effort) => {
            require_carried(&plan.carriers.effort, true)?;
            if let ValueCarrier::Flag(flag) = descriptor.effort.carrier.require("effort carrier")? {
                let valid = match &plan.carriers.effort {
                    CarrierUse::Specified(CarrierRef::Arguments(indices)) if indices.len() == 2 => {
                        indices[0].checked_add(1) == Some(indices[1])
                            && plan.arguments[indices[0]] == *flag
                            && plan.arguments[indices[1]] == effort.as_str()
                    }
                    _ => false,
                };
                if !valid { return Err(ContractError::Invalid("effort flag/value carrier")); }
            }
        }
        EffortResolution::Ignored { reason, .. } => {
            if plan.carriers.effort != CarrierUse::NotCarried(*reason) { return Err(ContractError::Invalid("ignored effort must be reported")); }
        }
    }
    Ok(())
}
