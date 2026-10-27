//! Codex CLI contract leaf, canonical key `codex_tui`. It is the only Codex
//! worker runtime: a role declaring `codex`, `codex_cli` or `codex-cli`
//! selects it. The `team-agent codex` leader and app-server keep their own path.
//! Measured recipes do not themselves constitute Team G1–G5 acceptance.
pub mod catalog;
pub mod descriptor;
pub mod interaction;
pub mod launcher;
pub use descriptor::{CODEX_DESCRIPTOR, ID, UNVERIFIED};
use std::{collections::{BTreeMap, BTreeSet}, ffi::OsString, path::{Path, PathBuf}};
use crate::contract::{descriptor::*, fork::*, hooks::*, plan::*, types::*};

const PROMPT_FILE: &str = "team-agent-prompt.txt";
// Never derive Debug: argv/env/prompt values are not diagnostic payloads.
pub struct McpStdio { pub executable: PathBuf, pub arguments: Vec<String>, pub environment: BTreeMap<String, String> }
pub struct CodexAdapter { mcp: McpStdio, runtime: Option<launcher::RuntimeInstallation>, project: Option<PathBuf> }
/// JSON basic string escaping is a TOML basic-string subset for these bounded
/// strings. Reject controls whose JSON escapes are not TOML escapes.
fn toml_string(value: &str) -> Result<String, ContractError> {
    if value.chars().any(|ch| ch.is_control() && !matches!(ch, '\n' | '\r' | '\t')) { return Err(ContractError::Invalid("Codex TOML control character")); }
    serde_json::to_string(value).map_err(|_| ContractError::Invalid("Codex TOML string"))
}
impl CodexAdapter {
    pub fn new(mcp: McpStdio) -> Result<Self, ContractError> {
        require_absolute(&mcp.executable, "Codex MCP executable")?;
        if mcp.executable.to_str().is_none() || mcp.arguments.iter().any(|arg| arg.contains('\0')) { return Err(ContractError::Invalid("Codex MCP arguments")); }
        EnvironmentDelta { remove: BTreeSet::new(), set: mcp.environment.iter().map(|(key, value)| (key.clone(), value.into())).collect() }.validate()?;
        Ok(Self { mcp, runtime: None, project: None })
    }
    pub fn with_runtime(mut self, runtime: launcher::RuntimeInstallation) -> Self { self.runtime = Some(runtime); self }
    pub fn with_project(mut self, project: PathBuf) -> Result<Self, ContractError> { require_absolute(&project, "Codex project directory")?; self.project = Some(project); Ok(self) }
    pub fn hooks(&self) -> ProviderHooks<'_> { ProviderHooks { catalog: HookBinding::Bound(self), plan: HookBinding::Bound(self), materialize: HookBinding::Bound(self), session: HookBinding::Bound(self), semantic: HookBinding::Bound(self), interaction: HookBinding::Bound(self), fork: HookBinding::Bound(self) } }
    /// The owned stdio server as `-c` overrides for this invocation only. The
    /// owned tools never prompt; every other approval follows the bypass intent.
    /// They also stay in the initial tool list: for a catalog model with
    /// `supports_search_tool`, Codex otherwise defers every MCP tool behind its
    /// own `tool_search`, whose `limit: Option<usize>` rejects a model's `5.0`
    /// before any search runs, so the team tools are never found (#394).
    pub fn mcp_overrides(&self, owner: &InstanceIdentity) -> Result<Vec<OsString>, ContractError> {
        let key = server_key(owner);
        let args = self.mcp.arguments.iter().map(|arg| toml_string(arg)).collect::<Result<Vec<_>, _>>()?.join(", ");
        let mut values = vec![
            format!("mcp_servers.{key}.command={}", toml_string(self.mcp.executable.to_str().ok_or(ContractError::Invalid("Codex MCP path UTF-8"))?)?),
            format!("mcp_servers.{key}.args=[{args}]"),
        ];
        if !self.mcp.environment.is_empty() {
            let env = self.mcp.environment.iter().map(|(key, value)| Ok(format!("{} = {}", toml_string(key)?, toml_string(value)?))).collect::<Result<Vec<_>, ContractError>>()?;
            values.push(format!("mcp_servers.{key}.env={{ {} }}", env.join(", ")));
        }
        values.extend([format!("mcp_servers.{key}.tool_timeout_sec=600.0"), format!("mcp_servers.{key}.default_tools_approval_mode=\"approve\""),
            format!("mcp_servers.{key}.omit_tools_from=[\"deferred\"]")]);
        Ok(values.into_iter().flat_map(|value| [OsString::from("-c"), value.into()]).collect())
    }
}
fn failure(error: ContractError, resources: Vec<OwnedResourceReceipt>) -> PartialFailure { PartialFailure { error, receipt: MaterializeReceipt { resources } } }
/// Stable, generation-specific namespace. A collision cannot substitute a
/// helper: the root still requires this exact instance's owned-process T3a and
/// same live connection call/write evidence before allowing ordinary input.
pub fn server_key(owner: &InstanceIdentity) -> String {
    let text = format!("codex-team-v1\0{}\0{}\0{}\0{}", owner.scope.as_str(), owner.seat.as_str(), owner.instance.as_str(), owner.generation.0);
    format!("team_{}", &crate::host::digest_hex(crate::host::digest(text.as_bytes()))[..32])
}
fn session_uuid(id: &str) -> bool {
    id.len() == 36 && id.bytes().enumerate().all(|(i, byte)| if [8, 13, 18, 23].contains(&i) { byte == b'-' } else { byte.is_ascii_hexdigit() })
}
impl PlanHook for CodexAdapter {
    fn plan(&self, resolved: &ResolvedLaunch) -> Result<LaunchPlan, ContractError> {
        let r = resolved.request();
        let runtime = self.runtime.as_ref().ok_or(ContractError::Unverified { field: "Codex runtime installation", reason: UNVERIFIED })?;
        let project = self.project.as_ref().ok_or(ContractError::Invalid("Codex actual project"))?;
        if resolved.provider().as_str() != ID || r.native != runtime.native || r.paths.executable != runtime.engine
            || r.paths.launcher != runtime.launcher()
            || !interaction::PROFILES[0].matches_native(&r.native) || r.channel != Channel::Tmux || r.mode != LaunchMode::FullWorker
            || r.auth != AuthMode::NativeSubscription || self.mcp.executable != r.paths.candidate
            || !matches!(r.operation, Operation::Fresh | Operation::Resume | Operation::NativeNewSeat)
        { return Err(ContractError::Mismatch("Codex native launch identity")); }
        if r.paths.cwd.path == *project { return Err(ContractError::Invalid("Codex private primary required")); }
        let prompt = r.prompt.as_ref().ok_or(ContractError::Invalid("Codex worker prompt"))?;
        let prompt = OwnedResourceRequest { owner: r.identity.clone(), path: OwnedPath::new(r.paths.cwd.path.clone(), PROMPT_FILE.into())?, kind: ResourceKind::Prompt, contents: prompt.as_bytes().to_vec() };
        // `codex fork <uuid>` resolves the parent thread by ID from any cwd and
        // starts a new thread (`forked_from_id`) in this seat's own cwd.
        let session = match resolved.expected_session() {
            ExpectedSession::CaptureAfterLaunch => None,
            ExpectedSession::Resume(binding) if session_uuid(binding.native_session.as_str()) => Some(("resume", binding.native_session.as_str())),
            ExpectedSession::CaptureAfterNativeFork { parent } if session_uuid(parent.as_str()) => Some(("fork", parent.as_str())),
            _ => return Err(ContractError::Invalid("Codex fresh/current, exact resume or native fork only")),
        };
        // The update modal is disabled for this invocation; the private non-Git
        // cwd has no trust screen. --no-daemon keeps the engine in the owned pane.
        let mut arguments: Vec<OsString> = session.map(|(subcommand, _)| OsString::from(subcommand)).into_iter().collect();
        arguments.extend(["--no-alt-screen".into(), "--no-daemon".into(), "-c".into(), "check_for_update_on_startup=false".into()]);
        let model = arguments.len();
        arguments.extend(["--model".into(), resolved.model().ok_or(ContractError::Invalid("Codex exact model"))?.into(),
            "--add-dir".into(), project.as_os_str().to_owned(), "-c".into(),
            format!("developer_instructions={}", toml_string(&format!("The actual task project is {}. Use explicit absolute paths into that project. The process working directory is private Team transport state, not the task repository.", project.display()))?).into()]);
        let start = arguments.len();
        arguments.extend(self.mcp_overrides(&r.identity)?);
        let mcp = CarrierUse::Specified(CarrierRef::Arguments((start..arguments.len()).collect()));
        let bypass = if r.bypass { let index = arguments.len(); arguments.push("--dangerously-bypass-approvals-and-sandbox".into()); CarrierUse::Specified(CarrierRef::Arguments(vec![index])) } else { CarrierUse::NotRequested };
        let effort = match resolved.effort() {
            EffortResolution::Unspecified => CarrierUse::NotRequested,
            EffortResolution::Pass(effort) => { let index = arguments.len(); arguments.extend(["-c".into(), format!("model_reasoning_effort={}", effort.as_str()).into()]); CarrierUse::Specified(CarrierRef::Arguments(vec![index, index + 1])) },
            EffortResolution::Ignored { reason, .. } => return Err(ContractError::EffortRejected(*reason)),
        };
        arguments.extend(session.map(|(_, id)| OsString::from(id)));
        // The pane execs bare `codex` through PATH, so the user's wrapper runs and
        // keeps the name. The engine is only the image that route must exec into.
        Ok(LaunchPlan { executable: runtime.engine.clone(), launcher: r.paths.launcher.clone(), arguments, executable_inputs: vec![], cwd: r.paths.cwd.path.clone(),
            environment: EnvironmentDelta { remove: BTreeSet::from(["OPENAI_API_KEY".into(), "CODEX_API_KEY".into()]),
                set: BTreeMap::from([("HOME".into(), runtime.home().as_os_str().to_owned())]) },
            expected_session: resolved.expected_session().clone(),
            carriers: CarrierReport { model: CarrierUse::Specified(CarrierRef::Arguments(vec![model, model + 1])), prompt: CarrierUse::Specified(CarrierRef::Resource(prompt.path.clone())), mcp, bypass, effort },
            materialization: vec![prompt] })
    }
}
impl MaterializeHook for CodexAdapter {
    fn materialize(&self, requests: &[OwnedResourceRequest], io: &mut dyn OwnedIo) -> Result<MaterializeReceipt, PartialFailure> {
        let [request] = requests else { return Err(failure(ContractError::Invalid("exact Codex owned prompt required"), vec![])); };
        if request.kind != ResourceKind::Prompt || request.path.relative() != Path::new(PROMPT_FILE)
            || !std::str::from_utf8(&request.contents).is_ok_and(nonblank)
        { return Err(failure(ContractError::Invalid("exact Codex owned prompt required"), vec![])); }
        let receipt = io.create_exclusive(request).map_err(|error| failure(error.error, error.receipt.resources))?;
        let valid = receipt.path == request.path && receipt.owner == request.owner && receipt.kind == request.kind && receipt.exclusive
            && receipt.disposition == ResourceDisposition::OwnedRemovable
            && matches!(receipt.write_effect, ResourceWriteEffect::Written { bytes_sha256 } if bytes_sha256 == crate::host::digest(&request.contents));
        if !valid { return Err(failure(ContractError::Mismatch("Codex materialization receipt"), vec![receipt])); }
        Ok(MaterializeReceipt { resources: vec![receipt] })
    }
}
impl CatalogHook for CodexAdapter {
    fn discover(&self, request: &CatalogRequest, host: &mut dyn BoundedReadHost) -> Result<CatalogObservation, ReadFailure> {
        catalog::validate_catalog_request(request).map_err(ReadFailure::Error)?;
        let output = host.read_catalog(request)?;
        if output.elapsed > request.bounds.deadline { return Err(ReadFailure::TimedOut { elapsed: output.elapsed }); }
        if output.stdout.len() > request.bounds.max_output_bytes { return Err(ReadFailure::OutputLimit { limit: request.bounds.max_output_bytes }); }
        if output.exit_code != 0 { return Err(ReadFailure::Exit { code: output.exit_code }); }
        Ok(CatalogObservation { provider: request.provider.clone(), native: request.native.clone(), schema: request.source.schema.into(), models: catalog::parse_catalog(&output.stdout).map_err(ReadFailure::Error)? })
    }
}
/// Codex's own `codex fork <uuid>` creates the child thread; nothing is staged.
impl ForkHook for CodexAdapter {
    fn stage(&self, request: &ResolvedFork, _io: &mut dyn OwnedIo) -> Result<NativeForkPlan, PartialFailure> {
        native_new_seat_stage(request).map_err(|error| failure(error, vec![]))
    }
}
