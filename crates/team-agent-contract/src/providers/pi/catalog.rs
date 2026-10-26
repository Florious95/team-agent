//! `pi --version` and the native model table. Both run the PATH-selected public
//! entry itself, so a user wrapper sees every probe. No credential read, login,
//! static model list or other-installation fallback.
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::Instant;
use crate::contract::{descriptor::{CatalogSource, Effort}, hooks::*, plan::*, types::*};
use crate::host::{command::*, process::{fingerprint_file, resolve_cwd}, HostError, HostErrorKind};

/// `--no-approve` keeps project-local model/settings files out of the catalog,
/// exactly as they stay out of the launch.
pub const CATALOG_SOURCE: CatalogSource = CatalogSource { arguments: &["--no-approve", "--list-models"], schema: "pi-list-models-table-v1" };
pub const METADATA_HARNESS: &str = "pi-launcher";
pub const METADATA_UI: &str = "metadata";
pub(super) const MAX_IMAGE: u64 = 1024 * 1024 * 1024;
const COLUMNS: [&str; 6] = ["provider", "model", "context", "max-out", "thinking", "images"];
/// What a `thinking: yes` row proves: Pi offers every level up to high for a
/// reasoning model; xhigh/max exist only where a model maps them.
const REASONING: [Effort; 3] = [Effort::Low, Effort::Medium, Effort::High];

#[derive(Debug)]
pub enum ProbeError { Host(HostError), InvalidVersion { received: String, truncated: bool } }
impl From<HostError> for ProbeError { fn from(error: HostError) -> Self { Self::Host(error) } }
impl std::fmt::Display for ProbeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Host(error) => std::fmt::Display::fmt(error, f),
            Self::InvalidVersion { received, truncated } => write!(f, "invalid Pi version {received:?} (truncated={truncated}); expected major.minor.patch[-prerelease]"),
        }
    }
}
impl std::error::Error for ProbeError {}
/// Evergreen syntax, never equality with a surveyed release or binary hash.
pub fn parse_version(bytes: &[u8]) -> Result<String, ProbeError> {
    let invalid = || ProbeError::InvalidVersion { received: String::from_utf8_lossy(&bytes[..bytes.len().min(256)]).into_owned(), truncated: bytes.len() > 256 };
    let value = std::str::from_utf8(bytes).map_err(|_| invalid())?.trim();
    let (release, prerelease) = value.split_once('-').map_or((value, None), |(release, tag)| (release, Some(tag)));
    let parts: Vec<_> = release.split('.').collect();
    if parts.len() != 3 || parts.iter().any(|part| part.is_empty() || !part.bytes().all(|byte| byte.is_ascii_digit())
            || (part.len() > 1 && part.starts_with('0')) || part.parse::<u64>().is_err())
        || prerelease.is_some_and(|tag| tag.is_empty() || tag.len() > 64
            || !tag.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'.'))
    { return Err(invalid()); }
    Ok(value.into())
}
fn platform() -> Platform {
    if cfg!(target_os = "macos") { Platform::MacOs } else if cfg!(target_os = "linux") { Platform::Linux } else { Platform::Windows }
}
fn command(entry: &Path, args: &[&str], cwd: Option<PathBuf>, bounds: ReadBounds) -> CommandRequest {
    CommandRequest {
        executable: entry.into(), arguments: args.iter().map(|arg| (*arg).into()).collect(), cwd,
        environment: EnvironmentDelta { remove: BTreeSet::new(), set: Default::default() },
        stdin: None, budget: bounds.deadline, reject_stdout: None,
        limits: OutputLimits { stdout: bounds.max_output_bytes, stderr: bounds.max_output_bytes, stdin: 0 },
    }
}
/// Runs the PATH-selected entry itself, so a wrapper sees every probe too.
pub fn probe_launcher<R: CommandRunner>(command: &PublicCommand, runner: &mut R, bounds: ReadBounds) -> Result<NativeIdentity, ProbeError> {
    bounds.validate().map_err(|_| HostError::new("Pi probe bounds", HostErrorKind::Invalid))?;
    let started = Instant::now();
    let hash = fingerprint_file(&command.entry, MAX_IMAGE, bounds.deadline)?;
    let remaining = bounds.deadline.saturating_sub(started.elapsed());
    if remaining.is_zero() { return Err(HostError::new("Pi version deadline", HostErrorKind::Deadline).into()); }
    let output = runner.run(&self::command(&command.entry, &["--version"], None, ReadBounds { deadline: remaining, ..bounds }));
    if !output.success() || output.elapsed > remaining || output.stdout.len() > bounds.max_output_bytes {
        return Err(HostError::new("Pi version command", HostErrorKind::Command).into());
    }
    let version = parse_version(&output.stdout)?;
    if fingerprint_file(&command.entry, MAX_IMAGE, bounds.deadline.saturating_sub(started.elapsed()))? != hash {
        return Err(HostError::new("Pi entry changed", HostErrorKind::Conflict).into());
    }
    Ok(NativeIdentity { version, harness: METADATA_HARNESS.into(), ui: METADATA_UI.into(), platform: platform(), executable_sha256: hash })
}
pub fn validate_catalog_request(request: &CatalogRequest) -> Result<(), ContractError> {
    request.bounds.validate()?;
    request.native.validate()?;
    require_absolute(&request.executable, "Pi catalog executable")?;
    if request.provider.as_str() != super::ID || request.source != CATALOG_SOURCE
        || request.executable.file_name().is_none_or(|name| name != "pi")
        || request.native.harness != METADATA_HARNESS || request.native.ui != METADATA_UI
        || parse_version(request.native.version.as_bytes()).is_err()
    { return Err(ContractError::Mismatch("Pi catalog grant")); }
    Ok(())
}
pub struct NativeCatalog<R> { grant: CatalogRequest, runner: R }
impl<R: CommandRunner> NativeCatalog<R> {
    pub fn new(grant: CatalogRequest, runner: R) -> Result<Self, ContractError> { validate_catalog_request(&grant)?; Ok(Self { grant, runner }) }
    pub fn runner_mut(&mut self) -> &mut R { &mut self.runner }
}
impl<R: CommandRunner> BoundedReadHost for NativeCatalog<R> {
    fn read_catalog(&mut self, request: &CatalogRequest) -> Result<ReadOutput, ReadFailure> {
        if *request != self.grant { return Err(ReadFailure::Error(ContractError::Mismatch("captured Pi catalog request"))); }
        let started = Instant::now();
        let mismatch = || ReadFailure::Error(ContractError::Mismatch("Pi catalog installation/cwd"));
        let verify = || {
            if resolve_cwd(&request.cwd.path).map_err(|_| mismatch())? != request.cwd { return Err(mismatch()); }
            if fingerprint_file(&request.executable, MAX_IMAGE, request.bounds.deadline.saturating_sub(started.elapsed())).map_err(|_| mismatch())?
                != request.native.executable_sha256 { return Err(mismatch()); }
            Ok(())
        };
        verify()?;
        let remaining = request.bounds.deadline.saturating_sub(started.elapsed());
        if remaining.is_zero() { return Err(ReadFailure::TimedOut { elapsed: started.elapsed() }); }
        let output = self.runner.run(&command(&request.executable, CATALOG_SOURCE.arguments, Some(request.cwd.path.clone()), ReadBounds { deadline: remaining, ..request.bounds }));
        if output.end == CommandEnd::TimedOut || output.elapsed > remaining { return Err(ReadFailure::TimedOut { elapsed: started.elapsed().max(output.elapsed) }); }
        if output.end == CommandEnd::OutputLimit || output.stdout.len() > request.bounds.max_output_bytes { return Err(ReadFailure::OutputLimit { limit: request.bounds.max_output_bytes }); }
        if !output.success() { return match (output.end, output.exit_code) { (CommandEnd::Exited, Some(code)) => Err(ReadFailure::Exit { code }), _ => Err(ReadFailure::Unknown(super::UNVERIFIED)) }; }
        verify()?;
        Ok(ReadOutput { stdout: output.stdout, elapsed: started.elapsed(), exit_code: 0 })
    }
}
/// The table's own header fixes each column start: Pi pads every cell to its
/// column width and joins cells with two spaces, so a model id may contain
/// spaces. Each row's exact launch id is `provider/model`.
pub fn parse_catalog(bytes: &[u8]) -> Result<Vec<ModelRecord>, ContractError> {
    let invalid = || ContractError::Invalid("Pi model catalog table");
    let text = std::str::from_utf8(bytes).map_err(|_| invalid())?;
    let mut lines = text.lines();
    let header: Vec<char> = lines.next().ok_or_else(invalid)?.chars().collect();
    let mut starts = [0; COLUMNS.len()];
    let mut at = 0;
    for (index, name) in COLUMNS.iter().enumerate() {
        if index > 0 {
            let gap = header[at..].iter().take_while(|ch| **ch == ' ').count();
            if gap < 2 { return Err(invalid()); }
            at += gap;
        }
        if !header[at..].iter().copied().take(name.len()).eq(name.chars()) { return Err(invalid()); }
        starts[index] = at;
        at += name.len();
    }
    if header[at..].iter().any(|ch| *ch != ' ') { return Err(invalid()); }
    let mut ids = BTreeSet::new(); let mut models = vec![];
    for line in lines.filter(|line| !line.trim().is_empty()) {
        let row: Vec<char> = line.chars().collect();
        let cell = |index: usize| -> Result<String, ContractError> {
            let next = starts.get(index + 1).copied();
            let end = next.map_or(row.len(), |next| next - 2);
            if next.is_some_and(|next| next > row.len()) || starts[index] > end
                || next.is_some_and(|next| row[end..next].iter().any(|ch| *ch != ' '))
            { return Err(invalid()); }
            let value: String = row[starts[index]..end].iter().collect::<String>().trim_end().into();
            if value.is_empty() || value.starts_with(' ') || value.chars().any(char::is_control) { return Err(invalid()); }
            Ok(value)
        };
        let (provider, model) = (cell(0)?, cell(1)?);
        if provider.contains([' ', '/']) { return Err(invalid()); }
        let efforts = match cell(4)?.as_str() { "yes" => REASONING.to_vec(), "no" => vec![], _ => return Err(invalid()) };
        for index in [2, 3, 5] { cell(index)?; }
        let id = format!("{provider}/{model}");
        if !ids.insert(id.clone()) { return Err(ContractError::AmbiguousModel); }
        models.push(ModelRecord { id, efforts: Support::Supported(efforts) });
    }
    if models.is_empty() { return Err(invalid()); }
    Ok(models)
}
