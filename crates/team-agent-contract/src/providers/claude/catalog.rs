//! `claude --version` and the native model catalog: the stream-json
//! `initialize` control request, which answers from the installation's own
//! model picker without a model turn. Both run the PATH-selected public entry
//! itself, so a user wrapper sees every probe. No credential read, login,
//! static model list or other-installation fallback.
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::Instant;
use crate::contract::{descriptor::{CatalogSource, Effort}, hooks::*, plan::*, types::*};
use crate::host::{command::*, process::{fingerprint_file, resolve_cwd}, HostError, HostErrorKind};

/// Print mode with no settings sources, hooks, tools, MCP servers or session
/// file: only the installation's picker answers.
pub const CATALOG_SOURCE: CatalogSource = CatalogSource {
    arguments: &["--print", "--input-format", "stream-json", "--output-format", "stream-json", "--verbose",
        "--no-session-persistence", "--strict-mcp-config", "--mcp-config", r#"{"mcpServers":{}}"#,
        "--setting-sources", "", "--settings", r#"{"disableAllHooks":true}"#, "--tools", "", "--no-chrome"],
    schema: "claude-initialize-models-v1",
};
const REQUEST_ID: &str = "team-agent-model-catalog";
const INITIALIZE: &[u8] = b"{\"type\":\"control_request\",\"request_id\":\"team-agent-model-catalog\",\"request\":{\"subtype\":\"initialize\",\"hooks\":{},\"agents\":{},\"skills\":[]}}\n";
/// The same subscription identity the worker plan keeps: an ambient API key or
/// a parent Claude Code session marker never shapes the catalog.
pub(super) const DROPPED_ENVIRONMENT: [&str; 3] = ["ANTHROPIC_API_KEY", "ANTHROPIC_AUTH_TOKEN", "CLAUDECODE"];
pub const METADATA_HARNESS: &str = "claude-launcher";
pub const METADATA_UI: &str = "metadata";
pub(super) const MAX_IMAGE: u64 = 1024 * 1024 * 1024;

#[derive(Debug)]
pub enum ProbeError { Host(HostError), InvalidVersion { received: String, truncated: bool } }
impl From<HostError> for ProbeError { fn from(error: HostError) -> Self { Self::Host(error) } }
impl std::fmt::Display for ProbeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Host(error) => std::fmt::Display::fmt(error, f),
            Self::InvalidVersion { received, truncated } => write!(f, "invalid Claude Code version {received:?} (truncated={truncated}); expected major.minor.patch (Claude Code)"),
        }
    }
}
impl std::error::Error for ProbeError {}
/// `major.minor.patch[-prerelease]`: the version syntax alone, never equality
/// with a surveyed release or binary hash.
pub fn semver(value: &str) -> bool {
    let (release, prerelease) = value.split_once('-').map_or((value, None), |(release, tag)| (release, Some(tag)));
    let parts: Vec<_> = release.split('.').collect();
    parts.len() == 3 && parts.iter().all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit())
            && (part.len() == 1 || !part.starts_with('0')) && part.parse::<u64>().is_ok())
        && prerelease.is_none_or(|tag| !tag.is_empty() && tag.len() <= 64 && tag.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'.'))
}
pub fn parse_version(bytes: &[u8]) -> Result<String, ProbeError> {
    let invalid = || ProbeError::InvalidVersion { received: String::from_utf8_lossy(&bytes[..bytes.len().min(256)]).into_owned(), truncated: bytes.len() > 256 };
    let text = std::str::from_utf8(bytes).map_err(|_| invalid())?.trim();
    let value = text.strip_suffix(" (Claude Code)").filter(|value| semver(value)).ok_or_else(invalid)?;
    Ok(value.into())
}
fn platform() -> Platform {
    if cfg!(target_os = "macos") { Platform::MacOs } else if cfg!(target_os = "linux") { Platform::Linux } else { Platform::Windows }
}
fn command(entry: &Path, args: &[&str], cwd: Option<PathBuf>, stdin: Option<&[u8]>, bounds: ReadBounds) -> CommandRequest {
    let remove = if stdin.is_some() { DROPPED_ENVIRONMENT.iter().map(|key| (*key).into()).collect() } else { BTreeSet::new() };
    CommandRequest {
        executable: entry.into(), arguments: args.iter().map(|arg| (*arg).into()).collect(), cwd,
        environment: EnvironmentDelta { remove, set: Default::default() },
        stdin: stdin.map(<[u8]>::to_vec), budget: bounds.deadline, reject_stdout: None,
        limits: OutputLimits { stdout: bounds.max_output_bytes, stderr: bounds.max_output_bytes, stdin: stdin.map_or(0, <[u8]>::len) },
    }
}
/// Runs the PATH-selected entry itself, so a wrapper sees every probe too.
pub fn probe_launcher<R: CommandRunner>(command: &PublicCommand, runner: &mut R, bounds: ReadBounds) -> Result<NativeIdentity, ProbeError> {
    bounds.validate().map_err(|_| HostError::new("Claude probe bounds", HostErrorKind::Invalid))?;
    let started = Instant::now();
    let hash = fingerprint_file(&command.entry, MAX_IMAGE, bounds.deadline)?;
    let remaining = bounds.deadline.saturating_sub(started.elapsed());
    if remaining.is_zero() { return Err(HostError::new("Claude version deadline", HostErrorKind::Deadline).into()); }
    let output = runner.run(&self::command(&command.entry, &["--version"], None, None, ReadBounds { deadline: remaining, ..bounds }));
    if !output.success() || output.elapsed > remaining || output.stdout.len() > bounds.max_output_bytes {
        return Err(HostError::new("Claude version command", HostErrorKind::Command).into());
    }
    let version = parse_version(&output.stdout)?;
    if fingerprint_file(&command.entry, MAX_IMAGE, bounds.deadline.saturating_sub(started.elapsed()))? != hash {
        return Err(HostError::new("Claude entry changed", HostErrorKind::Conflict).into());
    }
    Ok(NativeIdentity { version, harness: METADATA_HARNESS.into(), ui: METADATA_UI.into(), platform: platform(), executable_sha256: hash })
}
pub fn validate_catalog_request(request: &CatalogRequest) -> Result<(), ContractError> {
    request.bounds.validate()?;
    request.native.validate()?;
    require_absolute(&request.executable, "Claude catalog executable")?;
    if request.provider.as_str() != super::ID || request.source != CATALOG_SOURCE
        || request.executable.file_name().is_none_or(|name| name != "claude")
        || request.native.harness != METADATA_HARNESS || request.native.ui != METADATA_UI
        || !semver(&request.native.version)
    { return Err(ContractError::Mismatch("Claude catalog grant")); }
    Ok(())
}
pub struct NativeCatalog<R> { grant: CatalogRequest, runner: R }
impl<R: CommandRunner> NativeCatalog<R> {
    pub fn new(grant: CatalogRequest, runner: R) -> Result<Self, ContractError> { validate_catalog_request(&grant)?; Ok(Self { grant, runner }) }
    pub fn runner_mut(&mut self) -> &mut R { &mut self.runner }
}
impl<R: CommandRunner> BoundedReadHost for NativeCatalog<R> {
    fn read_catalog(&mut self, request: &CatalogRequest) -> Result<ReadOutput, ReadFailure> {
        if *request != self.grant { return Err(ReadFailure::Error(ContractError::Mismatch("captured Claude catalog request"))); }
        let started = Instant::now();
        let mismatch = || ReadFailure::Error(ContractError::Mismatch("Claude catalog installation/cwd"));
        let verify = || {
            if resolve_cwd(&request.cwd.path).map_err(|_| mismatch())? != request.cwd { return Err(mismatch()); }
            if fingerprint_file(&request.executable, MAX_IMAGE, request.bounds.deadline.saturating_sub(started.elapsed())).map_err(|_| mismatch())?
                != request.native.executable_sha256 { return Err(mismatch()); }
            Ok(())
        };
        verify()?;
        let remaining = request.bounds.deadline.saturating_sub(started.elapsed());
        if remaining.is_zero() { return Err(ReadFailure::TimedOut { elapsed: started.elapsed() }); }
        let output = self.runner.run(&command(&request.executable, CATALOG_SOURCE.arguments, Some(request.cwd.path.clone()), Some(INITIALIZE), ReadBounds { deadline: remaining, ..request.bounds }));
        if output.end == CommandEnd::TimedOut || output.elapsed > remaining { return Err(ReadFailure::TimedOut { elapsed: started.elapsed().max(output.elapsed) }); }
        if output.end == CommandEnd::OutputLimit || output.stdout.len() > request.bounds.max_output_bytes { return Err(ReadFailure::OutputLimit { limit: request.bounds.max_output_bytes }); }
        if !output.success() { return match (output.end, output.exit_code) { (CommandEnd::Exited, Some(code)) => Err(ReadFailure::Exit { code }), _ => Err(ReadFailure::Unknown(super::UNVERIFIED)) }; }
        verify()?;
        Ok(ReadOutput { stdout: output.stdout, elapsed: started.elapsed(), exit_code: 0 })
    }
}
/// Picker values carry a context variant such as `[1m]`; `--model` takes them verbatim.
fn model_id(value: &str) -> bool { !value.is_empty() && value.len() <= 128 && value.bytes().all(|byte| byte.is_ascii_alphanumeric() || b"-_.[]".contains(&byte)) }
/// Exactly one `initialize` response for our request id. A response that
/// names its `claude_code_version` (2.1.309 does; 2.1.282 omits it) must name
/// the version the launcher probe saw; the read itself is fenced to the same
/// hash-checked entry either way. Every picker row is a launch ID by its exact resolved
/// model and, unless it is the moving `default`, by its own selector value.
/// An ID reached from several rows keeps only the Team efforts all of them list.
pub fn parse_catalog(bytes: &[u8], version: &str) -> Result<Vec<ModelRecord>, ContractError> {
    let invalid = || ContractError::Invalid("Claude initialize model catalog");
    let text = std::str::from_utf8(bytes).map_err(|_| invalid())?;
    let mut response = None;
    for line in text.lines() {
        let frame: serde_json::Value = serde_json::from_str(line).map_err(|_| invalid())?;
        if frame.get("type").and_then(serde_json::Value::as_str) != Some("control_response") { continue; }
        let inner = frame.get("response").ok_or_else(invalid)?;
        if response.is_some() || inner.get("request_id").and_then(serde_json::Value::as_str) != Some(REQUEST_ID)
            || inner.get("subtype").and_then(serde_json::Value::as_str) != Some("success")
        { return Err(invalid()); }
        response = Some(inner.get("response").ok_or_else(invalid)?.clone());
    }
    let response = response.ok_or_else(invalid)?;
    if response.get("claude_code_version").is_some_and(|reported| reported.as_str() != Some(version)) {
        return Err(ContractError::Mismatch("Claude catalog installation version"));
    }
    let rows = response.get("models").and_then(serde_json::Value::as_array).filter(|rows| !rows.is_empty()).ok_or_else(invalid)?;
    let (mut order, mut efforts) = (Vec::<String>::new(), BTreeMap::<String, Vec<Effort>>::new());
    let mut values = BTreeSet::new();
    for row in rows {
        let text = |key| row.get(key).and_then(serde_json::Value::as_str).filter(|id| model_id(id)).ok_or_else(invalid);
        let (value, resolved) = (text("value")?, text("resolvedModel")?);
        if !values.insert(value) { return Err(ContractError::AmbiguousModel); }
        let mut levels = vec![];
        for level in row.get("supportedEffortLevels").and_then(serde_json::Value::as_array).map_or(&[][..], Vec::as_slice) {
            let level = level.as_str().ok_or_else(invalid)?;
            if let Some(effort) = Effort::ALL.into_iter().find(|tier| tier.as_str() == level) {
                if levels.contains(&effort) { return Err(invalid()); }
                levels.push(effort);
            }
        }
        for id in [Some(resolved), (value != "default" && value != resolved).then_some(value)].into_iter().flatten() {
            match efforts.get_mut(id) {
                Some(known) => known.retain(|effort| levels.contains(effort)),
                None => { order.push(id.into()); efforts.insert(id.into(), levels.clone()); }
            }
        }
    }
    Ok(order.into_iter().map(|id| { let efforts = Support::Supported(efforts.remove(&id).unwrap_or_default()); ModelRecord { id, efforts } }).collect())
}
