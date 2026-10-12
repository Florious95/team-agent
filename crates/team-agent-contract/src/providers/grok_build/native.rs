//! Public wrapper routing and final native image are separate, generation-bound
//! identities. No installed version/hash pin, credential read, login or fallback.
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::Instant;
use crate::contract::{descriptor::CatalogSource, hooks::*, plan::*, types::*};
use crate::host::{command::*, process::{fingerprint_file, resolve_cwd}, HostError, HostErrorKind};

pub const CATALOG_SOURCE: CatalogSource = CatalogSource { arguments: &["models"], schema: "grok-models-text-v1" };
pub const METADATA_HARNESS: &str = "grok-launcher";
pub const METADATA_UI: &str = "metadata";
const MAX_IMAGE: u64 = 1024 * 1024 * 1024;

#[derive(Debug)]
pub enum ProbeError { Host(HostError), InvalidVersion { received: String, truncated: bool } }
impl From<HostError> for ProbeError { fn from(error: HostError) -> Self { Self::Host(error) } }
impl std::fmt::Display for ProbeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Host(error) => std::fmt::Display::fmt(error, f),
            Self::InvalidVersion { received, truncated } => write!(f, "invalid Grok version {received:?} (truncated={truncated}); expected grok major.minor.patch (hex revision)"),
        }
    }
}
impl std::error::Error for ProbeError {}
pub fn parse_version(bytes: &[u8]) -> Result<String, ProbeError> {
    let invalid = || ProbeError::InvalidVersion { received: String::from_utf8_lossy(&bytes[..bytes.len().min(256)]).into_owned(), truncated: bytes.len() > 256 };
    let text = std::str::from_utf8(bytes).map_err(|_| invalid())?.trim();
    let value = text.strip_prefix("grok ").ok_or_else(invalid)?;
    let (version, revision) = value.split_once(" (").ok_or_else(invalid)?;
    let revision = revision.strip_suffix(')').ok_or_else(invalid)?;
    let parts: Vec<_> = version.split('.').collect();
    if parts.len() != 3 || parts.iter().any(|part| part.is_empty()
        || !part.bytes().all(|byte| byte.is_ascii_digit()) || (part.len() > 1 && part.starts_with('0'))
        || part.parse::<u64>().is_err())
        || revision.is_empty() || revision.len() > 64 || !revision.bytes().all(|byte| byte.is_ascii_hexdigit())
    { return Err(invalid()); }
    Ok(value.into())
}
pub fn resolve_launcher(path: &Path) -> Result<PathBuf, HostError> {
    require_absolute(path, "Grok public launcher").map_err(|_| HostError::new("Grok launcher path", HostErrorKind::Invalid))?;
    let path = path.canonicalize().map_err(|error| HostError::io("Grok launcher resolution", error))?;
    let metadata = std::fs::metadata(&path).map_err(|error| HostError::io("Grok launcher metadata", error))?;
    if !metadata.is_file() || !matches!(path.file_name().and_then(|name| name.to_str()), Some("grok" | "grok-native")) {
        return Err(HostError::new("Grok public entry required", HostErrorKind::Invalid));
    }
    #[cfg(unix)] {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o111 == 0 { return Err(HostError::new("Grok entry executable", HostErrorKind::Invalid)); }
    }
    Ok(path)
}
/// This installation's observed public wrapper execs the Homebrew native alias.
/// Resolve the alias, not wrapper shell contents or its private proxy.env.
pub fn resolve_engine(launcher: &Path) -> Result<PathBuf, HostError> {
    let launcher = resolve_launcher(launcher)?;
    if launcher.file_name().is_some_and(|name| name == "grok-native") { return Ok(launcher); }
    if launcher != Path::new("/usr/local/bin/grok") {
        return Err(HostError::new("Grok wrapper route unverified", HostErrorKind::Unsupported));
    }
    let engine = resolve_launcher(Path::new("/opt/homebrew/bin/grok"))?;
    if engine.file_name().is_none_or(|name| name != "grok-native") {
        return Err(HostError::new("Grok final native image", HostErrorKind::Ownership));
    }
    Ok(engine)
}
/// Root calls this after resolving/owning the stable primary, before H3 effects.
/// Metadata only; no user/account config bodies or Git administrative files.
pub fn require_private_primary(cwd: &crate::contract::session::CwdIdentity) -> Result<(), HostError> {
    if resolve_cwd(&cwd.path)? != *cwd { return Err(HostError::new("Grok primary identity", HostErrorKind::Ownership)); }
    for ancestor in cwd.path.ancestors() {
        let mut paths = vec![ancestor.join(".git")];
        if ancestor != cwd.path.as_path() { paths.push(ancestor.join(".grok/config.toml")); }
        for path in paths {
            match std::fs::symlink_metadata(path) {
                Ok(_) => return Err(HostError::new("Grok inherited project configuration", HostErrorKind::Ownership)),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
                Err(error) => return Err(HostError::io("Grok primary ancestry metadata", error)),
            }
        }
    }
    Ok(())
}
fn command(entry: &Path, engine: &Path, args: &[&str], cwd: Option<PathBuf>, bounds: ReadBounds) -> CommandRequest {
    CommandRequest {
        executable: entry.into(), arguments: args.iter().map(|arg| (*arg).into()).collect(), cwd,
        environment: EnvironmentDelta { remove: Default::default(), set: std::collections::BTreeMap::from([("GROK_REAL".into(), engine.as_os_str().to_owned())]) },
        stdin: None, budget: bounds.deadline, reject_stdout: None,
        limits: OutputLimits { stdout: bounds.max_output_bytes, stderr: bounds.max_output_bytes, stdin: 0 },
    }
}
pub fn probe_launcher<R: CommandRunner>(entry: &Path, runner: &mut R, bounds: ReadBounds) -> Result<NativeIdentity, ProbeError> {
    bounds.validate().map_err(|_| HostError::new("Grok probe bounds", HostErrorKind::Invalid))?;
    let started = Instant::now();
    let entry = resolve_launcher(entry)?;
    let engine = resolve_engine(&entry)?;
    let hash = fingerprint_file(&entry, MAX_IMAGE, bounds.deadline)?;
    let remaining = bounds.deadline.saturating_sub(started.elapsed());
    if remaining.is_zero() { return Err(HostError::new("Grok version deadline", HostErrorKind::Deadline).into()); }
    let output = runner.run(&command(&entry, &engine, &["--version"], None, ReadBounds { deadline: remaining, ..bounds }));
    if !output.success() || output.elapsed > remaining || output.stdout.len() > bounds.max_output_bytes {
        return Err(HostError::new("Grok version command", HostErrorKind::Command).into());
    }
    let version = parse_version(&output.stdout)?;
    if fingerprint_file(&entry, MAX_IMAGE, bounds.deadline.saturating_sub(started.elapsed()))? != hash {
        return Err(HostError::new("Grok entry changed", HostErrorKind::Conflict).into());
    }
    Ok(NativeIdentity { version, harness: METADATA_HARNESS.into(), ui: METADATA_UI.into(),
        platform: if cfg!(target_os = "macos") { Platform::MacOs } else if cfg!(target_os = "linux") { Platform::Linux } else { Platform::Windows }, executable_sha256: hash })
}
#[derive(Clone)]
pub struct RuntimeInstallation {
    pub engine: PathBuf,
    pub native: NativeIdentity,
    launcher: PathBuf,
    launcher_native: NativeIdentity,
    home: PathBuf,
}
impl RuntimeInstallation {
    pub fn home(&self) -> &Path { &self.home }
    pub fn launcher(&self) -> Option<LaunchEntrypoint> {
        (self.launcher != self.engine).then(|| LaunchEntrypoint { executable: self.launcher.clone(), sha256: self.launcher_native.executable_sha256 })
    }
    pub fn bind_catalog(&self, catalog: &CatalogObservation) -> Result<CatalogObservation, ContractError> {
        if catalog.provider.as_str() != "grok_build" || catalog.native != self.launcher_native || catalog.schema != CATALOG_SOURCE.schema {
            return Err(ContractError::Mismatch("Grok catalog/runtime installation"));
        }
        let mut result = catalog.clone(); result.native = self.native.clone(); Ok(result)
    }
}
pub fn probe_runtime<R: CommandRunner>(entry: &Path, home: &Path, runner: &mut R, bounds: ReadBounds) -> Result<RuntimeInstallation, ProbeError> {
    let started = Instant::now();
    let home = resolve_cwd(home)?.path;
    let launcher = resolve_launcher(entry)?;
    let engine = resolve_engine(&launcher)?;
    let hash = fingerprint_file(&engine, MAX_IMAGE, bounds.deadline)?;
    let launcher_native = probe_launcher(&launcher, runner, ReadBounds { deadline: bounds.deadline.saturating_sub(started.elapsed()), ..bounds })?;
    if fingerprint_file(&engine, MAX_IMAGE, bounds.deadline.saturating_sub(started.elapsed()))? != hash {
        return Err(HostError::new("Grok updated during discovery", HostErrorKind::Conflict).into());
    }
    Ok(RuntimeInstallation { engine, launcher, home,
        native: NativeIdentity { harness: "grok-build".into(), ui: "tui".into(), executable_sha256: hash, ..launcher_native.clone() }, launcher_native })
}
pub fn validate_catalog_request(request: &CatalogRequest) -> Result<(), ContractError> {
    request.bounds.validate()?;
    request.native.validate()?;
    require_absolute(&request.executable, "Grok catalog executable")?;
    if request.provider.as_str() != "grok_build" || request.source != CATALOG_SOURCE
        || request.native.harness != METADATA_HARNESS || request.native.ui != METADATA_UI
        || parse_version(format!("grok {}", request.native.version).as_bytes()).is_err()
    { return Err(ContractError::Mismatch("Grok catalog grant")); }
    Ok(())
}
pub struct NativeCatalog<R> { grant: CatalogRequest, runner: R }
impl<R: CommandRunner> NativeCatalog<R> {
    pub fn new(grant: CatalogRequest, runner: R) -> Result<Self, ContractError> { validate_catalog_request(&grant)?; Ok(Self { grant, runner }) }
    pub fn runner_mut(&mut self) -> &mut R { &mut self.runner }
}
impl<R: CommandRunner> BoundedReadHost for NativeCatalog<R> {
    fn read_catalog(&mut self, request: &CatalogRequest) -> Result<ReadOutput, ReadFailure> {
        if *request != self.grant { return Err(ReadFailure::Error(ContractError::Mismatch("captured Grok catalog request"))); }
        let started = Instant::now();
        let mismatch = || ReadFailure::Error(ContractError::Mismatch("Grok catalog installation/cwd"));
        let engine = resolve_engine(&request.executable).map_err(|_| mismatch())?;
        let image = fingerprint_file(&engine, MAX_IMAGE, request.bounds.deadline).map_err(|_| mismatch())?;
        let verify = || {
            if resolve_cwd(&request.cwd.path).map_err(|_| mismatch())? != request.cwd { return Err(mismatch()); }
            for (path, expected) in [(&request.executable, request.native.executable_sha256), (&engine, image)] {
                if fingerprint_file(path, MAX_IMAGE, request.bounds.deadline.saturating_sub(started.elapsed())).map_err(|_| mismatch())? != expected { return Err(mismatch()); }
            }
            Ok(())
        };
        verify()?;
        let remaining = request.bounds.deadline.saturating_sub(started.elapsed());
        if remaining.is_zero() { return Err(ReadFailure::TimedOut { elapsed: started.elapsed() }); }
        let output = self.runner.run(&command(&request.executable, &engine, CATALOG_SOURCE.arguments, Some(request.cwd.path.clone()), ReadBounds { deadline: remaining, ..request.bounds }));
        if output.end == CommandEnd::TimedOut || output.elapsed > remaining { return Err(ReadFailure::TimedOut { elapsed: started.elapsed().max(output.elapsed) }); }
        if output.end == CommandEnd::OutputLimit || output.stdout.len() > request.bounds.max_output_bytes { return Err(ReadFailure::OutputLimit { limit: request.bounds.max_output_bytes }); }
        if !output.success() { return match (output.end, output.exit_code) { (CommandEnd::Exited, Some(code)) => Err(ReadFailure::Exit { code }), _ => Err(ReadFailure::Unknown(super::UNVERIFIED)) }; }
        verify()?;
        Ok(ReadOutput { stdout: output.stdout, elapsed: started.elapsed(), exit_code: 0 })
    }
}
fn model_id(value: &str) -> bool { !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte)) }
pub fn parse_catalog(bytes: &[u8]) -> Result<Vec<ModelRecord>, ContractError> {
    let invalid = || ContractError::Invalid("Grok authenticated catalog grammar");
    let text = std::str::from_utf8(bytes).map_err(|_| invalid())?;
    let mut lines = text.lines().map(str::trim).filter(|line| !line.is_empty());
    if lines.next() != Some("You are logged in with grok.com.") { return Err(invalid()); }
    let default = lines.next().and_then(|line| line.strip_prefix("Default model: ")).filter(|id| model_id(id)).ok_or_else(invalid)?;
    if lines.next() != Some("Available models:") { return Err(invalid()); }
    let mut ids = BTreeSet::new(); let mut models = vec![]; let mut marked = None;
    for row in lines {
        let row = row.strip_prefix("- ").or_else(|| row.strip_prefix("* ")).ok_or_else(invalid)?;
        let (id, is_default) = row.strip_suffix(" (default)").map_or((row, false), |id| (id, true));
        if !model_id(id) { return Err(invalid()); }
        if !ids.insert(id) { return Err(ContractError::AmbiguousModel); }
        if is_default && marked.replace(id).is_some() { return Err(invalid()); }
        models.push(ModelRecord { id: id.into(), efforts: Support::Unverified(super::UNVERIFIED) });
    }
    if models.is_empty() || !ids.contains(default) || marked != Some(default) { return Err(invalid()); }
    Ok(models)
}
