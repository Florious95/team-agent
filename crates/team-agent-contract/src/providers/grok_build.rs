//! Grok Build contract leaf. Legacy `grok` remains a separate provider/state
//! family. Measured recipes do not themselves constitute Team G1–G5 acceptance.
pub mod interaction;
pub mod native;
use std::{collections::{BTreeMap, BTreeSet}, ffi::OsString, path::{Path, PathBuf}};
use crate::contract::{descriptor::*, fork::*, hooks::*, plan::*, types::*};

pub const UNVERIFIED: Reason = Reason { code: "grok-build-native-admission-missing", message: "This Grok capability lacks scoped native evidence; no legacy or other-provider fallback is allowed" };
const EFFORT: Reason = Reason { code: "grok-build-effort-not-native", message: "Native Grok accepts low, medium, high, xhigh; never clamp or silently drop effort" };
pub static GROK_BUILD_DESCRIPTOR: ProviderDescriptor = ProviderDescriptor {
    identity: IdentityFacet { id: "grok_build", display_name: "Grok Build (contract)", binary: "grok", aliases: &[], leader: Support::Unverified(UNVERIFIED) },
    model: ModelFacet { selection: Support::Supported(ModelSelection::ExactCatalogId), omitted: OmittedModel::Reject, catalog: Support::Supported(native::CATALOG_SOURCE) },
    effort: EffortFacet { admission: [EffortAdmission::Pass, EffortAdmission::Pass, EffortAdmission::Pass, EffortAdmission::Pass, EffortAdmission::Reject(EFFORT), EffortAdmission::Reject(EFFORT)], carrier: Support::Supported(ValueCarrier::Flag("--effort")), inherit_team_default: false, model_dependent: false },
    auth: AuthFacet { subscription: Support::Supported(AuthMechanism::NativeExistingSession), official_api: Support::Unverified(UNVERIFIED), compatible_api: Support::Unverified(UNVERIFIED) },
    bypass: BypassFacet { intent: Support::Supported(BypassPolicy { enabled_arguments: &["--always-approve"], requires_startup_consent: true }) },
    prompt: PromptFacet { carrier: Support::Supported(PromptCarrier::RuntimeBridge) },
    mcp: McpFacet { carrier: Support::Supported(McpCarrier::ScopedConfig), scope: ResourceScope::WorkingDirectory,
        tools: &TEAM_TOOLS,
        // Account/native tools coexist. Only this generation's owned server and
        // same-connection facts confer Team identity; ambient facts never do.
        isolation: McpIsolation::OwnedServerIsolated },
    tool_names: ToolNameFacet { naming: Support::Supported(ToolNaming::RuntimeBound) },
    session: SessionFacet { fresh: FreshSession::CaptureAfterLaunch, resume: Support::Supported(ResumeMode::ExactId) },
    fork: ForkFacet { in_window: Support::Unverified(UNVERIFIED), full_snapshot: Support::Unverified(UNVERIFIED), native_new_seat: Support::Unverified(UNVERIFIED), allowed_auth: &[] },
    input: InputFacet { profiles: Support::Supported(interaction::PROFILES) },
    startup: StartupFacet { interactive: true, authorized_consent: Support::Supported(()) },
    probe_sources: ProbeSourcesFacet { process: Support::Supported(ProbeSource::Host), pane: Support::Supported(ProbeSource::NativeSurface), server: Support::Supported(ProbeSource::ServerLifecycle), client_binding: Support::Supported(ProbeSource::NativeRuntime), round_trip: Support::Supported(ProbeSource::NativeRuntime), semantic: Support::Supported(ProbeSource::NativeSurface) },
    workspace: WorkspaceFacet { config_scope: ResourceScope::WorkingDirectory, shared_cwd: false, requires_materialization_lease: true },
    teardown: TeardownFacet { resources: &[
        ResourcePolicy { kind: ResourceKind::McpConfig, disposition: ResourceDisposition::OwnedRemovable },
        ResourcePolicy { kind: ResourceKind::Prompt, disposition: ResourceDisposition::OwnedRemovable },
        ResourcePolicy { kind: ResourceKind::SessionBacking, disposition: ResourceDisposition::OwnedPreserved },
        ResourcePolicy { kind: ResourceKind::GlobalSettings, disposition: ResourceDisposition::Forbidden },
        ResourcePolicy { kind: ResourceKind::NativeDatabase, disposition: ResourceDisposition::Forbidden },
    ] },
};
// Never derive Debug: argv/env/prompt values are not diagnostic payloads.
pub struct McpStdio { pub executable: PathBuf, pub arguments: Vec<String>, pub environment: BTreeMap<String, String> }
pub struct GrokAdapter { mcp: McpStdio, runtime: Option<native::RuntimeInstallation>, project: Option<PathBuf> }
impl GrokAdapter {
    pub fn new(mcp: McpStdio) -> Result<Self, ContractError> {
        require_absolute(&mcp.executable, "Grok MCP executable")?;
        if mcp.executable.to_str().is_none() || mcp.arguments.iter().any(|arg| arg.contains('\0')) { return Err(ContractError::Invalid("Grok MCP arguments")); }
        EnvironmentDelta { remove: BTreeSet::new(), set: mcp.environment.iter().map(|(key,value)| (key.clone(), value.into())).collect() }.validate()?;
        Ok(Self { mcp, runtime: None, project: None })
    }
    pub fn with_runtime(mut self, runtime: native::RuntimeInstallation) -> Self { self.runtime = Some(runtime); self }
    pub fn with_project(mut self, project: PathBuf) -> Result<Self, ContractError> { require_absolute(&project, "Grok project directory")?; self.project = Some(project); Ok(self) }
    pub fn hooks(&self) -> ProviderHooks<'_> { ProviderHooks { catalog: HookBinding::Bound(self), plan: HookBinding::Bound(self), materialize: HookBinding::Bound(self), session: HookBinding::Bound(self), semantic: HookBinding::Bound(self), interaction: HookBinding::Bound(self), fork: HookBinding::Bound(self) } }
    fn mcp_contents(&self, owner: &InstanceIdentity) -> Result<Vec<u8>, ContractError> {
        let key = server_key(owner);
        // JSON basic string escaping is a TOML basic-string subset for our bounded
        // strings. Reject controls whose JSON escapes are not TOML escapes.
        fn quote(value: &str) -> Result<String, ContractError> {
            if value.chars().any(|ch| ch.is_control() && !matches!(ch, '\n' | '\r' | '\t')) { return Err(ContractError::Invalid("Grok TOML control character")); }
            serde_json::to_string(value).map_err(|_| ContractError::Invalid("Grok TOML string"))
        }
        let args = self.mcp.arguments.iter().map(|arg| quote(arg)).collect::<Result<Vec<_>, _>>()?.join(", ");
        let mut text = format!("[mcp_servers.{key}]\ncommand = {}\nargs = [{args}]\nenabled = true\n", quote(self.mcp.executable.to_str().ok_or(ContractError::Invalid("Grok MCP path UTF-8"))?)?);
        if !self.mcp.environment.is_empty() {
            text.push_str(&format!("\n[mcp_servers.{key}.env]\n"));
            for (key, value) in &self.mcp.environment { text.push_str(&format!("{} = {}\n", quote(key)?, quote(value)?)); }
        }
        Ok(text.into_bytes())
    }
    pub fn mcp_resource(&self, owner: InstanceIdentity, cwd: PathBuf) -> Result<OwnedResourceRequest, ContractError> {
        let contents = self.mcp_contents(&owner)?;
        Ok(OwnedResourceRequest { owner, path: OwnedPath::new(cwd, ".grok/config.toml".into())?, kind: ResourceKind::McpConfig, contents })
    }
}
fn failure(error: ContractError, resources: Vec<OwnedResourceReceipt>) -> PartialFailure { PartialFailure { error, receipt: MaterializeReceipt { resources } } }
/// Stable, generation-specific namespace. A collision cannot substitute a
/// helper: the root still requires this exact instance's owned-process T3a and
/// same live connection call/write evidence before allowing ordinary input.
pub fn server_key(owner: &InstanceIdentity) -> String {
    let text = format!("grok-team-v1\0{}\0{}\0{}\0{}", owner.scope.as_str(), owner.seat.as_str(), owner.instance.as_str(), owner.generation.0);
    format!("team_{}", &crate::host::digest_hex(crate::host::digest(text.as_bytes()))[..32])
}
impl PlanHook for GrokAdapter {
    fn plan(&self, resolved: &ResolvedLaunch) -> Result<LaunchPlan, ContractError> {
        let r = resolved.request();
        let runtime = self.runtime.as_ref().ok_or(ContractError::Unverified { field: "Grok runtime installation", reason: UNVERIFIED })?;
        let project = self.project.as_ref().ok_or(ContractError::Invalid("Grok actual project"))?;
        if resolved.provider().as_str() != "grok_build" || r.native != runtime.native || r.paths.executable != runtime.engine
            || !interaction::PROFILES[0].matches_native(&r.native) || r.channel != Channel::Tmux || r.mode != LaunchMode::FullWorker
            || r.auth != AuthMode::NativeSubscription || self.mcp.executable != r.paths.candidate || !matches!(r.operation, Operation::Fresh | Operation::Resume)
        { return Err(ContractError::Mismatch("Grok native launch identity")); }
        if r.paths.cwd.path == *project { return Err(ContractError::Invalid("Grok private primary required")); }
        let prompt = r.prompt.as_ref().ok_or(ContractError::Invalid("Grok worker prompt"))?;
        let mcp = self.mcp_resource(r.identity.clone(), r.paths.cwd.path.clone())?;
        let prompt = OwnedResourceRequest { owner: r.identity.clone(), path: OwnedPath::new(r.paths.cwd.path.clone(), ".grok/team-agent-prompt.txt".into())?, kind: ResourceKind::Prompt, contents: prompt.as_bytes().to_vec() };
        // Grok has no --add-dir. An explicit project instruction does not claim
        // relative-cwd, project AGENTS or permissions equivalence with --cwd.
        let mut arguments: Vec<OsString> = vec!["--model".into(), resolved.model().ok_or(ContractError::Invalid("Grok exact model"))?.into(),
            "--cwd".into(), r.paths.cwd.path.as_os_str().to_owned(), "--rules".into(),
            format!("The actual task project is {}. Use explicit absolute paths into that project. The process working directory is private Team transport state, not the task repository.", project.display()).into(),
            "--leader-socket".into(), r.paths.cwd.path.join("grok-leader.sock").into_os_string()];
        let bypass = if r.bypass { let index = arguments.len(); arguments.push("--always-approve".into()); CarrierUse::Specified(CarrierRef::Arguments(vec![index])) } else { CarrierUse::NotRequested };
        let effort = match resolved.effort() {
            EffortResolution::Unspecified => CarrierUse::NotRequested,
            EffortResolution::Pass(effort) => { let index = arguments.len(); arguments.extend(["--effort".into(), effort.as_str().into()]); CarrierUse::Specified(CarrierRef::Arguments(vec![index, index + 1])) },
            EffortResolution::Ignored { .. } => return Err(ContractError::EffortRejected(EFFORT)),
        };
        match resolved.expected_session() {
            ExpectedSession::CaptureAfterLaunch => {},
            ExpectedSession::Resume(binding) => arguments.extend(["--resume".into(), binding.native_session.as_str().into()]),
            _ => return Err(ContractError::Invalid("Grok fresh/current or exact resume only")),
        }
        let public_exec = native::public_exec_path(&runtime.engine, &r.paths.cwd.path).map_err(|_| ContractError::Invalid("Grok public exec"))?;
        Ok(LaunchPlan { executable: runtime.engine.clone(), launcher: runtime.launcher(), arguments, executable_inputs: vec![], cwd: r.paths.cwd.path.clone(),
            environment: EnvironmentDelta { remove: BTreeSet::from(["XAI_API_KEY".into(), "GROK_API_KEY".into()]), set: BTreeMap::from([("HOME".into(), runtime.home().as_os_str().to_owned()), ("GROK_REAL".into(), public_exec.as_os_str().to_owned())]) },
            expected_session: resolved.expected_session().clone(),
            carriers: CarrierReport { model: CarrierUse::Specified(CarrierRef::Arguments(vec![0,1])), prompt: CarrierUse::Specified(CarrierRef::Resource(prompt.path.clone())), mcp: CarrierUse::Specified(CarrierRef::Resource(mcp.path.clone())), bypass, effort },
            materialization: vec![mcp, prompt] })
    }
}
impl MaterializeHook for GrokAdapter {
    fn materialize(&self, requests: &[OwnedResourceRequest], io: &mut dyn OwnedIo) -> Result<MaterializeReceipt, PartialFailure> {
        let first = requests.first().ok_or_else(|| failure(ContractError::Invalid("Grok owned resources"), vec![]))?;
        let expected = self.mcp_contents(&first.owner).map_err(|error| failure(error, vec![]))?;
        if requests.len() != 2 || requests[0].kind != ResourceKind::McpConfig || requests[0].path.relative() != Path::new(".grok/config.toml") || requests[0].contents != expected
            || requests[1].kind != ResourceKind::Prompt || requests[1].path.relative() != Path::new(".grok/team-agent-prompt.txt")
            || requests[1].owner != requests[0].owner || requests[1].path.root() != requests[0].path.root()
            || !std::str::from_utf8(&requests[1].contents).is_ok_and(nonblank)
        { return Err(failure(ContractError::Invalid("exact Grok owned MCP/prompt required"), vec![])); }
        let mut resources = vec![];
        for request in requests {
            let receipt = match io.create_exclusive(request) { Ok(receipt) => receipt, Err(error) => { resources.extend(error.receipt.resources); return Err(failure(error.error, resources)); } };
            let valid = receipt.path == request.path && receipt.owner == request.owner && receipt.kind == request.kind && receipt.exclusive
                && receipt.disposition == ResourceDisposition::OwnedRemovable
                && matches!(receipt.write_effect, ResourceWriteEffect::Written { bytes_sha256 } if bytes_sha256 == crate::host::digest(&request.contents));
            resources.push(receipt);
            if !valid { return Err(failure(ContractError::Mismatch("Grok materialization receipt"), resources)); }
        }
        Ok(MaterializeReceipt { resources })
    }
}
impl CatalogHook for GrokAdapter {
    fn discover(&self, request: &CatalogRequest, host: &mut dyn BoundedReadHost) -> Result<CatalogObservation, ReadFailure> {
        native::validate_catalog_request(request).map_err(ReadFailure::Error)?;
        let output = host.read_catalog(request)?;
        if output.elapsed > request.bounds.deadline { return Err(ReadFailure::TimedOut { elapsed: output.elapsed }); }
        if output.stdout.len() > request.bounds.max_output_bytes { return Err(ReadFailure::OutputLimit { limit: request.bounds.max_output_bytes }); }
        if output.exit_code != 0 { return Err(ReadFailure::Exit { code: output.exit_code }); }
        Ok(CatalogObservation { provider: request.provider.clone(), native: request.native.clone(), schema: request.source.schema.into(), models: native::parse_catalog(&output.stdout).map_err(ReadFailure::Error)? })
    }
}
impl ForkHook for GrokAdapter {
    fn stage(&self, _request: &ResolvedFork, _io: &mut dyn OwnedIo) -> Result<NativeForkPlan, PartialFailure> {
        Err(failure(ContractError::Unverified { field: "Grok fork lineage", reason: UNVERIFIED }, vec![]))
    }
}
pub enum ExitClass { CommandSucceeded, Unknown }
pub struct ExitClassification { pub class: ExitClass, pub native_effects_may_have_occurred: bool, pub delivery_floor: crate::contract::delivery::DeliveryEffect }
pub fn classify_exit(code: Option<i32>, prior: crate::contract::delivery::DeliveryEffect) -> ExitClassification {
    ExitClassification { class: if code == Some(0) { ExitClass::CommandSucceeded } else { ExitClass::Unknown }, native_effects_may_have_occurred: true, delivery_floor: prior }
}
