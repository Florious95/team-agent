//! Read-only public-launcher discovery. Its digest is NOT the bundled Node image
//! or full package identity and must never become a ProcessStamp for a worker.
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::contract::{descriptor::CatalogSource, hooks::*, plan::*, types::*};
use crate::host::{command::*, process::{fingerprint_file, resolve_cwd}, HostError, HostErrorKind};

pub const CATALOG_SOURCE: CatalogSource = CatalogSource {
    arguments: &["--list-models"],
    schema: "cursor-list-models-text-v1",
};
pub const METADATA_HARNESS: &str = "cursor-launcher";
pub const METADATA_UI: &str = "metadata";
const MAX_LAUNCHER_BYTES: u64 = 1024 * 1024;

pub fn resolve_launcher(launcher: &Path) -> Result<PathBuf, HostError> {
    require_absolute(launcher, "Cursor launcher")
        .map_err(|_| HostError::new("Cursor launcher path", HostErrorKind::Invalid))?;
    let path = launcher.canonicalize().map_err(|error| HostError::io("Cursor launcher resolution", error))?;
    let metadata = std::fs::metadata(&path).map_err(|error| HostError::io("Cursor launcher metadata", error))?;
    if !metadata.is_file() || path.file_name().is_none_or(|name| name != "cursor-agent") {
        return Err(HostError::new("Cursor direct public launcher required", HostErrorKind::Invalid));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o111 == 0 {
            return Err(HostError::new("Cursor launcher is not executable", HostErrorKind::Invalid));
        }
    }
    Ok(path)
}

#[derive(Debug)]
pub enum ProbeError {
    Host(HostError),
    InvalidVersion { received: String, truncated: bool },
}
impl From<HostError> for ProbeError {
    fn from(error: HostError) -> Self { Self::Host(error) }
}
impl std::fmt::Display for ProbeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Host(error) => std::fmt::Display::fmt(error, f),
            Self::InvalidVersion { received, truncated } => write!(f,
                "invalid Cursor version {received:?} (truncated={truncated}); expected YYYY.MM.DD-hex revision"),
        }
    }
}
impl std::error::Error for ProbeError {}

/// Evergreen syntax, never equality with a surveyed release or binary hash.
pub fn parse_version(stdout: &[u8]) -> Result<String, ProbeError> {
    let invalid = || ProbeError::InvalidVersion {
        received: String::from_utf8_lossy(&stdout[..stdout.len().min(256)]).into_owned(),
        truncated: stdout.len() > 256,
    };
    let text = std::str::from_utf8(stdout).map_err(|_| invalid())?.trim();
    let (date, revision) = text.split_once('-').ok_or_else(invalid)?;
    let parts: Vec<_> = date.split('.').collect();
    if parts.len() != 3 || parts[0].len() != 4 || parts[1].len() != 2 || parts[2].len() != 2
        || !parts.iter().all(|part| part.bytes().all(|byte| byte.is_ascii_digit()))
        || !parts[1].parse::<u8>().is_ok_and(|month| (1..=12).contains(&month))
        || !parts[2].parse::<u8>().is_ok_and(|day| (1..=31).contains(&day))
        || revision.is_empty() || revision.len() > 40
        || !revision.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(invalid());
    }
    Ok(text.into())
}
fn command(executable: &Path, arguments: &[&str], cwd: Option<PathBuf>, bounds: ReadBounds) -> CommandRequest {
    CommandRequest {
        executable: executable.into(), arguments: arguments.iter().map(|arg| (*arg).into()).collect(), cwd,
        environment: EnvironmentDelta { remove: Default::default(), set: Default::default() },
        stdin: None, budget: bounds.deadline, reject_stdout: None,
        limits: OutputLimits { stdout: bounds.max_output_bytes, stderr: bounds.max_output_bytes, stdin: 0 },
    }
}
pub fn probe_launcher<R: CommandRunner>(
    launcher: &Path, runner: &mut R, bounds: ReadBounds,
) -> Result<NativeIdentity, ProbeError> {
    bounds.validate().map_err(|_| HostError::new("Cursor probe bounds", HostErrorKind::Invalid))?;
    let launcher = resolve_launcher(launcher)?;
    let started = Instant::now();
    let hash = fingerprint_file(&launcher, MAX_LAUNCHER_BYTES, bounds.deadline)?;
    let remaining = bounds.deadline.saturating_sub(started.elapsed());
    if remaining.is_zero() { return Err(HostError::new("Cursor version deadline", HostErrorKind::Deadline).into()); }
    let output = runner.run(&command(&launcher, &["--version"], None, ReadBounds { deadline: remaining, ..bounds }));
    if !output.success() || output.stdout.len() > bounds.max_output_bytes || output.elapsed > remaining {
        return Err(HostError::new("Cursor version command", HostErrorKind::Command).into());
    }
    let version = parse_version(&output.stdout)?;
    if fingerprint_file(&launcher, MAX_LAUNCHER_BYTES, bounds.deadline.saturating_sub(started.elapsed()))? != hash {
        return Err(HostError::new("Cursor launcher changed during probe", HostErrorKind::Conflict).into());
    }
    Ok(NativeIdentity {
        version, harness: METADATA_HARNESS.into(), ui: METADATA_UI.into(),
        platform: if cfg!(target_os = "macos") { Platform::MacOs } else if cfg!(target_os = "linux") { Platform::Linux } else { Platform::Windows },
        executable_sha256: hash,
    })
}
pub(super) fn validate_catalog_request(request: &CatalogRequest) -> Result<(), ContractError> {
    request.bounds.validate()?;
    request.native.validate()?;
    require_absolute(&request.executable, "Cursor catalog launcher")?;
    require_absolute(&request.cwd.path, "Cursor catalog cwd")?;
    if request.provider.as_str() != "cursor" || request.source != CATALOG_SOURCE
        || request.native.harness != METADATA_HARNESS || request.native.ui != METADATA_UI
        || parse_version(request.native.version.as_bytes()).is_err()
        || request.executable.file_name().is_none_or(|name| name != "cursor-agent")
    {
        return Err(ContractError::Mismatch("Cursor catalog grant"));
    }
    Ok(())
}

/// Captures one exact read grant; the generic runner owns child I/O/deadline/reap.
/// No model/auth fallback, browser/login retry or unbounded subprocess is added.
pub struct NativeCatalog<R> { grant: CatalogRequest, runner: R }
impl<R: CommandRunner> NativeCatalog<R> {
    pub fn new(grant: CatalogRequest, runner: R) -> Result<Self, ContractError> {
        validate_catalog_request(&grant)?;
        Ok(Self { grant, runner })
    }
    pub fn runner_mut(&mut self) -> &mut R { &mut self.runner }
}
impl<R: CommandRunner> BoundedReadHost for NativeCatalog<R> {
    fn read_catalog(&mut self, request: &CatalogRequest) -> Result<ReadOutput, ReadFailure> {
        if *request != self.grant {
            return Err(ReadFailure::Error(ContractError::Mismatch("captured Cursor catalog request")));
        }
        let started = Instant::now();
        let verify = || -> Result<(), ReadFailure> {
            if resolve_cwd(&request.cwd.path).map_err(|_| ReadFailure::Error(ContractError::Mismatch("Cursor catalog cwd")))? != request.cwd {
                return Err(ReadFailure::Error(ContractError::Mismatch("Cursor catalog cwd")));
            }
            let remaining = request.bounds.deadline.saturating_sub(started.elapsed());
            if remaining.is_zero() { return Err(ReadFailure::TimedOut { elapsed: started.elapsed() }); }
            if fingerprint_file(&request.executable, MAX_LAUNCHER_BYTES, remaining)
                .map_err(|_| ReadFailure::Error(ContractError::Mismatch("Cursor catalog launcher")))?
                != request.native.executable_sha256
            {
                return Err(ReadFailure::Error(ContractError::Mismatch("Cursor catalog launcher")));
            }
            Ok(())
        };
        verify()?;
        let remaining = request.bounds.deadline.saturating_sub(started.elapsed());
        if remaining.is_zero() { return Err(ReadFailure::TimedOut { elapsed: started.elapsed() }); }
        let output = self.runner.run(&command(&request.executable, CATALOG_SOURCE.arguments,
            Some(request.cwd.path.clone()), ReadBounds { deadline: remaining, ..request.bounds }));
        if output.end == CommandEnd::TimedOut || output.elapsed > remaining {
            return Err(ReadFailure::TimedOut { elapsed: started.elapsed().max(output.elapsed) });
        }
        if output.end == CommandEnd::OutputLimit || output.stdout.len() > request.bounds.max_output_bytes {
            return Err(ReadFailure::OutputLimit { limit: request.bounds.max_output_bytes });
        }
        if !output.success() {
            return match (output.end, output.exit_code) {
                (CommandEnd::Exited, Some(code)) => Err(ReadFailure::Exit { code }),
                _ => Err(ReadFailure::Unknown(super::UNVERIFIED)),
            };
        }
        verify()?;
        Ok(ReadOutput { stdout: output.stdout, elapsed: started.elapsed(), exit_code: 0 })
    }
}

/// Observed text protocol. Descriptions/default/current labels are not aliases,
/// effort declarations or permission to silently choose a model.
pub fn parse_catalog(stdout: &[u8]) -> Result<Vec<ModelRecord>, ContractError> {
    let text = std::str::from_utf8(stdout).map_err(|_| ContractError::Invalid("Cursor catalog UTF-8"))?;
    let mut lines = text.lines().filter(|line| !line.trim().is_empty());
    if lines.next() != Some("Available models") { return Err(ContractError::Invalid("Cursor catalog header")); }
    let mut models = vec![];
    let mut ids = BTreeSet::new();
    let mut footer = false;
    for line in lines {
        if footer { return Err(ContractError::Invalid("Cursor catalog trailing text")); }
        if line.starts_with("Tip: use --model <id> (or /model <id> in interactive mode) to switch.")
            && !line.chars().any(char::is_control)
        {
            footer = true;
            continue;
        }
        let (id, description) = line.split_once(" - ").ok_or(ContractError::Invalid("Cursor catalog record"))?;
        if id.is_empty() || !id.bytes().all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte))
            || !nonblank(description) || description.chars().any(char::is_control)
        { return Err(ContractError::Invalid("Cursor catalog record")); }
        if !ids.insert(id) { return Err(ContractError::AmbiguousModel); }
        models.push(ModelRecord { id: id.into(), efforts: Support::Unverified(super::EFFORT) });
    }
    if models.is_empty() || !footer { return Err(ContractError::Invalid("Cursor catalog incomplete")); }
    Ok(models)
}
