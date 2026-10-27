//! Claude Code contract leaf, canonical key `claude_tui`. The legacy `claude`
//! and `claude_code` worker, leader and fork paths keep their own runtime.
//! Measured recipes do not themselves constitute Team G1–G5 acceptance.
pub mod catalog;
pub mod descriptor;
pub mod interaction;
pub mod launcher;
pub use descriptor::{CLAUDE_DESCRIPTOR, ID, UNVERIFIED};
use std::{collections::{BTreeMap, BTreeSet}, ffi::OsString, path::{Path, PathBuf}};
use crate::contract::{descriptor::*, fork::*, hooks::*, plan::*, session::{NativeSessionId, ResumeBinding}, types::*};

const PROMPT_FILE: &str = "team-agent-prompt.txt";
// Never derive Debug: argv/env/prompt values are not diagnostic payloads.
pub struct McpStdio { pub executable: PathBuf, pub arguments: Vec<String>, pub environment: BTreeMap<String, String> }
pub struct ClaudeAdapter { mcp: McpStdio, runtime: Option<launcher::RuntimeInstallation>, project: Option<PathBuf> }
impl ClaudeAdapter {
    pub fn new(mcp: McpStdio) -> Result<Self, ContractError> {
        require_absolute(&mcp.executable, "Claude MCP executable")?;
        if mcp.executable.to_str().is_none() || mcp.arguments.iter().any(|arg| arg.contains('\0')) { return Err(ContractError::Invalid("Claude MCP arguments")); }
        EnvironmentDelta { remove: BTreeSet::new(), set: mcp.environment.iter().map(|(key, value)| (key.clone(), value.into())).collect() }.validate()?;
        Ok(Self { mcp, runtime: None, project: None })
    }
    pub fn with_runtime(mut self, runtime: launcher::RuntimeInstallation) -> Self { self.runtime = Some(runtime); self }
    pub fn with_project(mut self, project: PathBuf) -> Result<Self, ContractError> { require_absolute(&project, "Claude project directory")?; self.project = Some(project); Ok(self) }
    pub fn hooks(&self) -> ProviderHooks<'_> { ProviderHooks { catalog: HookBinding::Bound(self), plan: HookBinding::Bound(self), materialize: HookBinding::Bound(self), session: HookBinding::Bound(self), semantic: HookBinding::Bound(self), interaction: HookBinding::Bound(self), fork: HookBinding::Bound(self) } }
    /// The owned stdio server as one `--mcp-config` JSON value for this
    /// invocation only, plus the allow rule that keeps its own Team tools from
    /// prompting. Every other permission follows the bypass intent.
    pub fn mcp_arguments(&self, owner: &InstanceIdentity) -> Result<[OsString; 4], ContractError> {
        let key = server_key(owner);
        let mut server = serde_json::json!({ "type": "stdio", "command": self.mcp.executable.to_str().ok_or(ContractError::Invalid("Claude MCP path UTF-8"))?, "args": self.mcp.arguments });
        if !self.mcp.environment.is_empty() { server["env"] = serde_json::json!(self.mcp.environment); }
        let config = serde_json::json!({ "mcpServers": { key.as_str(): server } });
        Ok(["--mcp-config".into(), config.to_string().into(), "--allowedTools".into(), format!("mcp__{key}").into()])
    }
}
fn failure(error: ContractError, resources: Vec<OwnedResourceReceipt>) -> PartialFailure { PartialFailure { error, receipt: MaterializeReceipt { resources } } }
/// Stable, generation-specific namespace. A collision cannot substitute a
/// helper: the root still requires this exact instance's owned-process T3a and
/// same live connection call/write evidence before allowing ordinary input.
pub fn server_key(owner: &InstanceIdentity) -> String {
    let text = format!("claude-team-v1\0{}\0{}\0{}\0{}", owner.scope.as_str(), owner.seat.as_str(), owner.instance.as_str(), owner.generation.0);
    format!("team_{}", &crate::host::digest_hex(crate::host::digest(text.as_bytes()))[..32])
}
/// The fresh native session ID `--session-id` names, derived from this exact
/// instance (a new instance per generation), formatted as a version-4 UUID.
/// Claude refuses an ID it already holds, so a replayed launch cannot adopt one.
pub fn session_id(owner: &InstanceIdentity) -> NativeSessionId {
    let text = format!("claude-team-session-v1\0{}\0{}\0{}\0{}", owner.scope.as_str(), owner.seat.as_str(), owner.instance.as_str(), owner.generation.0);
    let mut bytes = crate::host::digest(text.as_bytes()).0;
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex = crate::host::digest_hex(crate::contract::types::Digest(bytes));
    NativeSessionId::new(format!("{}-{}-{}-{}-{}", &hex[..8], &hex[8..12], &hex[12..16], &hex[16..20], &hex[20..32])).expect("hex UUID is a valid session ID")
}
pub(crate) fn session_uuid(id: &str) -> bool {
    id.len() == 36 && id.bytes().enumerate().all(|(i, byte)| if [8, 13, 18, 23].contains(&i) { byte == b'-' } else { byte.is_ascii_hexdigit() })
}
/// The local transcript a native fork reads: `<home>/.claude/projects/<cwd
/// with every UTF-16 unit that is not ASCII alphanumeric replaced by '-'>/
/// <uuid>.jsonl`. `--resume <uuid>` resolves only the current cwd's directory,
/// so a new seat in its own private primary must pass this absolute path.
/// Pure: nothing is read. Longer names are hashed natively and are refused
/// here rather than guessed.
pub fn transcript_path(home: &Path, source: &ResumeBinding) -> Result<PathBuf, ContractError> {
    if let Some(path) = &source.backing {
        return Ok(path.clone());
    }
    if !session_uuid(source.native_session.as_str()) {
        return Err(ContractError::Invalid("Claude session UUID"));
    }
    require_absolute(home, "Claude home")?;
    let cwd = source.cwd.path.to_str().ok_or(ContractError::Invalid("Claude transcript cwd UTF-8"))?;
    let directory: String = cwd.encode_utf16()
        .map(|unit| match u8::try_from(unit) { Ok(byte) if byte.is_ascii_alphanumeric() => char::from(byte), _ => '-' })
        .collect();
    if directory.len() > 200 {
        return Err(ContractError::Unverified { field: "Claude long-path transcript directory", reason: UNVERIFIED });
    }
    Ok(home.join(".claude").join("projects").join(directory).join(format!("{}.jsonl", source.native_session.as_str())))
}
impl PlanHook for ClaudeAdapter {
    fn plan(&self, resolved: &ResolvedLaunch) -> Result<LaunchPlan, ContractError> {
        let r = resolved.request();
        let runtime = self.runtime.as_ref().ok_or(ContractError::Unverified { field: "Claude runtime installation", reason: UNVERIFIED })?;
        let project = self.project.as_ref().ok_or(ContractError::Invalid("Claude actual project"))?;
        if resolved.provider().as_str() != ID || r.native != runtime.native || r.paths.executable != runtime.engine
            || r.paths.launcher != runtime.launcher()
            || !interaction::PROFILES[0].matches_native(&r.native) || r.channel != Channel::Tmux || r.mode != LaunchMode::FullWorker
            || r.auth != AuthMode::NativeSubscription || self.mcp.executable != r.paths.candidate
            || !matches!(r.operation, Operation::Fresh | Operation::Resume | Operation::NativeNewSeat)
        { return Err(ContractError::Mismatch("Claude native launch identity")); }
        if r.paths.cwd.path == *project { return Err(ContractError::Invalid("Claude private primary required")); }
        let prompt = r.prompt.as_ref().ok_or(ContractError::Invalid("Claude worker prompt"))?;
        let prompt = OwnedResourceRequest { owner: r.identity.clone(), path: OwnedPath::new(r.paths.cwd.path.clone(), PROMPT_FILE.into())?, kind: ResourceKind::Prompt, contents: prompt.as_bytes().to_vec() };
        let session: Vec<OsString> = match resolved.expected_session() {
            ExpectedSession::Preassigned(id) if *id == session_id(&r.identity) => vec!["--session-id".into(), id.as_str().into()],
            ExpectedSession::Resume(binding) if session_uuid(binding.native_session.as_str()) => vec!["--resume".into(), binding.native_session.as_str().into()],
            // Native fork into this new seat: the parent transcript by absolute
            // path, a new session under the derived child ID, and this seat's
            // name instead of the copied parent title. The parent is only read.
            ExpectedSession::PreassignedNativeFork { parent, child } if *child == session_id(&r.identity) && session_uuid(parent.as_str()) => {
                let fork = r.fork.as_ref().ok_or(ContractError::Invalid("Claude fork source"))?;
                let source = &fork.request().source;
                if source.native_session != *parent { return Err(ContractError::Mismatch("Claude fork parent")); }
                vec!["--resume".into(), transcript_path(runtime.home(), source)?.into_os_string(), "--fork-session".into(),
                    "--session-id".into(), child.as_str().into(), "--name".into(), r.identity.seat.as_str().into()]
            }
            ExpectedSession::PendingAllocation => Vec::new(), // pure preflight; host refuses to spawn
            _ => return Err(ContractError::Invalid("Claude derived fresh session, exact resume or native fork only")),
        };
        // Several options are variadic (`--add-dir`, `--mcp-config`,
        // `--allowedTools`): each is followed by another option, never an operand.
        let mut arguments: Vec<OsString> = vec!["--model".into(), resolved.model().ok_or(ContractError::Invalid("Claude exact model"))?.into(),
            "--add-dir".into(), project.as_os_str().to_owned(), "--append-system-prompt".into(),
            format!("The actual task project is {}. Use explicit absolute paths into that project. The process working directory is private Team transport state, not the task repository.", project.display()).into()];
        let start = arguments.len();
        arguments.extend(self.mcp_arguments(&r.identity)?);
        let mcp = CarrierUse::Specified(CarrierRef::Arguments((start..arguments.len()).collect()));
        let bypass = if r.bypass { let index = arguments.len(); arguments.push("--dangerously-skip-permissions".into()); CarrierUse::Specified(CarrierRef::Arguments(vec![index])) } else { CarrierUse::NotRequested };
        let effort = match resolved.effort() {
            EffortResolution::Unspecified => CarrierUse::NotRequested,
            EffortResolution::Pass(effort) => { let index = arguments.len(); arguments.extend(["--effort".into(), effort.as_str().into()]); CarrierUse::Specified(CarrierRef::Arguments(vec![index, index + 1])) },
            EffortResolution::Ignored { reason, .. } => return Err(ContractError::EffortRejected(*reason)),
        };
        arguments.extend(session);
        // The pane execs bare `claude` through PATH, so the user's wrapper runs and
        // keeps the name. The engine is only the image that route must exec into.
        Ok(LaunchPlan { executable: runtime.engine.clone(), launcher: r.paths.launcher.clone(), arguments, executable_inputs: vec![], cwd: r.paths.cwd.path.clone(),
            environment: EnvironmentDelta { remove: catalog::DROPPED_ENVIRONMENT.iter().map(|key| (*key).into()).collect(),
                set: BTreeMap::from([("HOME".into(), runtime.home().as_os_str().to_owned())]) },
            expected_session: resolved.expected_session().clone(),
            carriers: CarrierReport { model: CarrierUse::Specified(CarrierRef::Arguments(vec![0, 1])), prompt: CarrierUse::Specified(CarrierRef::Resource(prompt.path.clone())), mcp, bypass, effort },
            materialization: vec![prompt] })
    }
}
impl MaterializeHook for ClaudeAdapter {
    fn materialize(&self, requests: &[OwnedResourceRequest], io: &mut dyn OwnedIo) -> Result<MaterializeReceipt, PartialFailure> {
        let [request] = requests else { return Err(failure(ContractError::Invalid("exact Claude owned prompt required"), vec![])); };
        if request.kind != ResourceKind::Prompt || request.path.relative() != Path::new(PROMPT_FILE)
            || !std::str::from_utf8(&request.contents).is_ok_and(nonblank)
        { return Err(failure(ContractError::Invalid("exact Claude owned prompt required"), vec![])); }
        let receipt = io.create_exclusive(request).map_err(|error| failure(error.error, error.receipt.resources))?;
        let valid = receipt.path == request.path && receipt.owner == request.owner && receipt.kind == request.kind && receipt.exclusive
            && receipt.disposition == ResourceDisposition::OwnedRemovable
            && matches!(receipt.write_effect, ResourceWriteEffect::Written { bytes_sha256 } if bytes_sha256 == crate::host::digest(&request.contents));
        if !valid { return Err(failure(ContractError::Mismatch("Claude materialization receipt"), vec![receipt])); }
        Ok(MaterializeReceipt { resources: vec![receipt] })
    }
}
impl CatalogHook for ClaudeAdapter {
    fn discover(&self, request: &CatalogRequest, host: &mut dyn BoundedReadHost) -> Result<CatalogObservation, ReadFailure> {
        catalog::validate_catalog_request(request).map_err(ReadFailure::Error)?;
        let output = host.read_catalog(request)?;
        if output.elapsed > request.bounds.deadline { return Err(ReadFailure::TimedOut { elapsed: output.elapsed }); }
        if output.stdout.len() > request.bounds.max_output_bytes { return Err(ReadFailure::OutputLimit { limit: request.bounds.max_output_bytes }); }
        if output.exit_code != 0 { return Err(ReadFailure::Exit { code: output.exit_code }); }
        Ok(CatalogObservation { provider: request.provider.clone(), native: request.native.clone(), schema: request.source.schema.into(),
            models: catalog::parse_catalog(&output.stdout, &request.native.version).map_err(ReadFailure::Error)? })
    }
}
/// Claude's own `--resume <path> --fork-session` performs the copy; nothing is staged.
impl ForkHook for ClaudeAdapter {
    fn stage(&self, request: &ResolvedFork, _io: &mut dyn OwnedIo) -> Result<NativeForkPlan, PartialFailure> {
        native_new_seat_stage(request).map_err(|error| failure(error, vec![]))
    }
}
