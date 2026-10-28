//! Pi coding agent contract leaf, canonical key `pi_tui`. The legacy `pi`
//! worker and leader keep their own path. Use `pi_tui` to select this leaf.
//! Measured recipes do not themselves constitute Team G1–G5 acceptance.
pub mod catalog;
pub mod descriptor;
pub mod interaction;
pub mod launcher;
pub use descriptor::{ID, PI_DESCRIPTOR, UNVERIFIED};
use std::{collections::{BTreeMap, BTreeSet}, ffi::OsString, path::PathBuf};
use crate::contract::{descriptor::*, fork::*, hooks::*, plan::*, types::*};

const MCP_FILE: &str = "team-agent-mcp.ts";
const PROMPT_FILE: &str = "team-agent-prompt.txt";
const TOOLS: [&str; 3] = ["send_message", "report_result", "get_team_status"];
// Never derive Debug: argv/env/prompt values are not diagnostic payloads.
pub struct McpStdio { pub executable: PathBuf, pub arguments: Vec<String>, pub environment: BTreeMap<String, String> }
pub struct PiAdapter { mcp: McpStdio, runtime: Option<launcher::RuntimeInstallation>, project: Option<PathBuf> }
impl PiAdapter {
    pub fn new(mcp: McpStdio) -> Result<Self, ContractError> {
        require_absolute(&mcp.executable, "Pi MCP executable")?;
        if mcp.executable.to_str().is_none() || mcp.arguments.iter().any(|arg| arg.contains('\0')) { return Err(ContractError::Invalid("Pi MCP arguments")); }
        EnvironmentDelta { remove: BTreeSet::new(), set: mcp.environment.iter().map(|(key, value)| (key.clone(), value.into())).collect() }.validate()?;
        Ok(Self { mcp, runtime: None, project: None })
    }
    pub fn with_runtime(mut self, runtime: launcher::RuntimeInstallation) -> Self { self.runtime = Some(runtime); self }
    pub fn with_project(mut self, project: PathBuf) -> Result<Self, ContractError> { require_absolute(&project, "Pi project directory")?; self.project = Some(project); Ok(self) }
    pub fn hooks(&self) -> ProviderHooks<'_> { ProviderHooks { catalog: HookBinding::Bound(self), plan: HookBinding::Bound(self), materialize: HookBinding::Bound(self), session: HookBinding::Bound(self), semantic: HookBinding::Bound(self), interaction: HookBinding::Bound(self), fork: HookBinding::Bound(self) } }
    /// The owned Pi extension: this generation's stdio server as JSON data,
    /// then the fixed registration module. Nothing else is interpolated.
    fn mcp_contents(&self, owner: &InstanceIdentity) -> Result<Vec<u8>, ContractError> {
        let command = self.mcp.executable.to_str().ok_or(ContractError::Invalid("Pi MCP path UTF-8"))?;
        let server = serde_json::json!({ "key": server_key(owner), "command": command, "args": self.mcp.arguments, "env": self.mcp.environment, "tools": TOOLS });
        Ok(format!("const teamMcp = {server};\n{}", include_str!("pi/team_mcp.js")).into_bytes())
    }
    pub fn mcp_resource(&self, owner: InstanceIdentity, cwd: PathBuf) -> Result<OwnedResourceRequest, ContractError> {
        let contents = self.mcp_contents(&owner)?;
        let relative = PathBuf::from("pi").join(server_key(&owner)).join(MCP_FILE);
        Ok(OwnedResourceRequest { owner, path: OwnedPath::new(cwd, relative)?, kind: ResourceKind::McpConfig, contents })
    }
}
fn failure(error: ContractError, resources: Vec<OwnedResourceReceipt>) -> PartialFailure { PartialFailure { error, receipt: MaterializeReceipt { resources } } }
/// Stable, generation-specific namespace. A collision cannot substitute a
/// helper: the root still requires this exact instance's owned-process T3a and
/// same live connection call/write evidence before allowing ordinary input.
/// `mcp__team_<32 hex>__report_result` stays under Pi's 64-character tool name.
pub fn server_key(owner: &InstanceIdentity) -> String {
    let text = format!("pi-team-v1\0{}\0{}\0{}\0{}", owner.scope.as_str(), owner.seat.as_str(), owner.instance.as_str(), owner.generation.0);
    format!("team_{}", &crate::host::digest_hex(crate::host::digest(text.as_bytes()))[..32])
}
fn session_uuid(id: &str) -> bool {
    id.len() == 36 && id.bytes().enumerate().all(|(i, byte)| if [8, 13, 18, 23].contains(&i) { byte == b'-' } else { byte.is_ascii_hexdigit() })
}
impl PlanHook for PiAdapter {
    fn plan(&self, resolved: &ResolvedLaunch) -> Result<LaunchPlan, ContractError> {
        let r = resolved.request();
        let runtime = self.runtime.as_ref().ok_or(ContractError::Unverified { field: "Pi runtime installation", reason: UNVERIFIED })?;
        let project = self.project.as_ref().ok_or(ContractError::Invalid("Pi actual project"))?;
        if resolved.provider().as_str() != ID || r.native != runtime.native || r.paths.executable != runtime.engine
            || r.paths.launcher != runtime.launcher()
            || !interaction::PROFILES[0].matches_native(&r.native) || r.channel != Channel::Tmux || r.mode != LaunchMode::FullWorker
            || r.auth != AuthMode::NativeSubscription || self.mcp.executable != r.paths.candidate || !matches!(r.operation, Operation::Fresh | Operation::Resume)
            || r.bypass
        { return Err(ContractError::Mismatch("Pi native launch identity")); }
        if r.paths.cwd.path != *project { return Err(ContractError::Mismatch("Pi authentic workspace cwd")); }
        let prompt = r.prompt.as_ref().ok_or(ContractError::Invalid("Pi worker prompt"))?;
        let mcp = self.mcp_resource(r.identity.clone(), r.paths.runtime_root.clone())?;
        let prompt = OwnedResourceRequest { owner: r.identity.clone(), path: OwnedPath::new(r.paths.runtime_root.clone(), mcp.path.relative().with_file_name(PROMPT_FILE))?, kind: ResourceKind::Prompt, contents: prompt.as_bytes().to_vec() };
        let resume = match resolved.expected_session() {
            ExpectedSession::CaptureAfterLaunch => None,
            ExpectedSession::Resume(binding) if session_uuid(binding.native_session.as_str()) => Some(binding.native_session.as_str()),
            _ => return Err(ContractError::Invalid("Pi fresh/current or exact resume only")),
        };
        // Decide project trust without a startup modal. Team context and the
        // MCP extension are explicit, generation-owned launch inputs; the
        // process itself stays in the authentic workspace.
        let mut arguments: Vec<OsString> = vec!["--no-approve".into(), "-e".into(), mcp.path.path().into_os_string()];
        let model = arguments.len();
        arguments.extend(["--model".into(), resolved.model().ok_or(ContractError::Invalid("Pi exact model"))?.into(),
            "--append-system-prompt".into(), prompt.path.path().into_os_string()]);
        let effort = match resolved.effort() {
            EffortResolution::Unspecified => CarrierUse::NotRequested,
            EffortResolution::Pass(effort) => { let index = arguments.len(); arguments.extend(["--thinking".into(), effort.as_str().into()]); CarrierUse::Specified(CarrierRef::Arguments(vec![index, index + 1])) },
            EffortResolution::Ignored { reason, .. } => CarrierUse::NotCarried(*reason),
        };
        // An exact ID resolves only in the authentic cwd's session store; a missing
        // one exits instead of starting fresh.
        if let Some(id) = resume { arguments.extend(["--session".into(), id.into()]); }
        // The pane execs bare `pi` through PATH, so the user's wrapper runs and
        // keeps the name. The engine is only the image that route must exec into.
        Ok(LaunchPlan { executable: runtime.engine.clone(), launcher: r.paths.launcher.clone(), arguments, executable_inputs: vec![], cwd: r.paths.cwd.path.clone(),
            environment: EnvironmentDelta { remove: BTreeSet::new(), set: BTreeMap::from([("HOME".into(), runtime.home().as_os_str().to_owned())]) },
            expected_session: resolved.expected_session().clone(),
            carriers: CarrierReport { model: CarrierUse::Specified(CarrierRef::Arguments(vec![model, model + 1])), prompt: CarrierUse::Specified(CarrierRef::Resource(prompt.path.clone())),
                mcp: CarrierUse::Specified(CarrierRef::Resource(mcp.path.clone())), bypass: CarrierUse::NotRequested, effort },
            materialization: vec![mcp, prompt] })
    }
}
impl MaterializeHook for PiAdapter {
    fn materialize(&self, requests: &[OwnedResourceRequest], io: &mut dyn OwnedIo) -> Result<MaterializeReceipt, PartialFailure> {
        let first = requests.first().ok_or_else(|| failure(ContractError::Invalid("Pi owned resources"), vec![]))?;
        let expected = self.mcp_contents(&first.owner).map_err(|error| failure(error, vec![]))?;
        let relative = PathBuf::from("pi").join(server_key(&first.owner));
        if requests.len() != 2 || requests[0].kind != ResourceKind::McpConfig || requests[0].path.relative() != relative.join(MCP_FILE) || requests[0].contents != expected
            || requests[1].kind != ResourceKind::Prompt || requests[1].path.relative() != relative.join(PROMPT_FILE)
            || requests[1].owner != requests[0].owner || requests[1].path.root() != requests[0].path.root()
            || !std::str::from_utf8(&requests[1].contents).is_ok_and(nonblank)
        { return Err(failure(ContractError::Invalid("exact Pi owned extension/prompt required"), vec![])); }
        let mut resources = vec![];
        for request in requests {
            let receipt = match io.create_exclusive(request) { Ok(receipt) => receipt, Err(error) => { resources.extend(error.receipt.resources); return Err(failure(error.error, resources)); } };
            let valid = receipt.path == request.path && receipt.owner == request.owner && receipt.kind == request.kind && receipt.exclusive
                && receipt.disposition == ResourceDisposition::OwnedRemovable
                && matches!(receipt.write_effect, ResourceWriteEffect::Written { bytes_sha256 } if bytes_sha256 == crate::host::digest(&request.contents));
            resources.push(receipt);
            if !valid { return Err(failure(ContractError::Mismatch("Pi materialization receipt"), resources)); }
        }
        Ok(MaterializeReceipt { resources })
    }
}
impl CatalogHook for PiAdapter {
    fn discover(&self, request: &CatalogRequest, host: &mut dyn BoundedReadHost) -> Result<CatalogObservation, ReadFailure> {
        catalog::validate_catalog_request(request).map_err(ReadFailure::Error)?;
        let output = host.read_catalog(request)?;
        if output.elapsed > request.bounds.deadline { return Err(ReadFailure::TimedOut { elapsed: output.elapsed }); }
        if output.stdout.len() > request.bounds.max_output_bytes { return Err(ReadFailure::OutputLimit { limit: request.bounds.max_output_bytes }); }
        if output.exit_code != 0 { return Err(ReadFailure::Exit { code: output.exit_code }); }
        Ok(CatalogObservation { provider: request.provider.clone(), native: request.native.clone(), schema: request.source.schema.into(), models: catalog::parse_catalog(&output.stdout).map_err(ReadFailure::Error)? })
    }
}
impl ForkHook for PiAdapter {
    fn stage(&self, _request: &ResolvedFork, _io: &mut dyn OwnedIo) -> Result<NativeForkPlan, PartialFailure> {
        Err(failure(ContractError::Unverified { field: "Pi fork lineage", reason: UNVERIFIED }, vec![]))
    }
}
