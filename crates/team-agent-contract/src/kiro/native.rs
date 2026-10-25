//! The public `kiro-cli` command on PATH is what runs, user wrappers included;
//! the vendor dispatcher starts its own chat helper. The dispatcher image is
//! only a verification target derived from that PATH route. No installation,
//! symlink repair, global PATH mutation, install-directory guess or credential read.
use crate::contract::{descriptor::CatalogSource, hooks::*, plan::*, types::*};
use crate::host::{
    command::*,
    process::{fingerprint_file, resolve_cwd},
    HostError, HostErrorKind,
};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub const CHAT_CATALOG_ARGUMENTS: &[&str] = &["chat", "--list-models", "--format", "json"];
pub const AUTH_PORTAL_MARKER: &[u8] = b"Opening auth portal and logging in...";
pub const METADATA_HARNESS: &str = "kiro-launcher";
pub const METADATA_UI: &str = "metadata";
const MAX_IMAGE: u64 = 1024 * 1024 * 1024;
/// CLI syntax and the models[]/model_id JSON schema are observed on 2.28.0.
pub const CHAT_CATALOG_SOURCE: CatalogSource = CatalogSource {
    arguments: CHAT_CATALOG_ARGUMENTS,
    schema: "kiro-2.28.0-list-models-json-v1",
};

#[derive(Debug)]
pub enum EngineProbeError {
    Host(HostError),
    InvalidVersion { received: String, truncated: bool },
}
impl From<HostError> for EngineProbeError {
    fn from(error: HostError) -> Self {
        Self::Host(error)
    }
}
impl std::fmt::Display for EngineProbeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Host(error) => std::fmt::Display::fmt(error, f),
            Self::InvalidVersion { received, truncated } => write!(
                f,
                "invalid Kiro version response {received:?}{}; expected kiro-cli[-chat] major.minor.patch",
                if *truncated { " (truncated to 256 bytes)" } else { "" }
            ),
        }
    }
}
impl std::error::Error for EngineProbeError {}

/// Release syntax only, not a claim about native UI or tool compatibility.
pub fn is_release_version(version: &str) -> bool {
    let mut parts = version.split('.');
    (0..3).all(|_| {
        parts.next().is_some_and(|part| {
            !part.is_empty()
                && part.bytes().all(|byte| byte.is_ascii_digit())
                && (part == "0" || !part.starts_with('0'))
        })
    }) && parts.next().is_none()
}

pub fn parse_version(stdout: &[u8]) -> Result<String, EngineProbeError> {
    // Version banners are a bounded diagnostic exception, never raw argv/env or
    // arbitrary command captures. Debug formatting escapes terminal controls.
    let invalid = || EngineProbeError::InvalidVersion {
        received: String::from_utf8_lossy(&stdout[..stdout.len().min(256)]).into_owned(),
        truncated: stdout.len() > 256,
    };
    let text = std::str::from_utf8(stdout).map_err(|_| invalid())?.trim();
    let version = text
        .strip_prefix("kiro-cli ")
        .or_else(|| text.strip_prefix("kiro-cli-chat "))
        .filter(|version| is_release_version(version))
        .ok_or_else(invalid)?;
    Ok(version.into())
}

/// Metadata identity of the PATH-selected entry. It runs that entry itself, so
/// a wrapper sees the probe too. Never T2/T3 or business readiness.
pub fn probe_launcher<R: CommandRunner>(
    command: &PublicCommand,
    runner: &mut R,
    bounds: ReadBounds,
) -> Result<NativeIdentity, EngineProbeError> {
    bounds
        .validate()
        .map_err(|_| HostError::new("Kiro probe bounds", HostErrorKind::Invalid))?;
    let started = Instant::now();
    let hash = fingerprint_file(&command.entry, MAX_IMAGE, bounds.deadline)?;
    let budget = bounds.deadline.saturating_sub(started.elapsed());
    if budget.is_zero() {
        return Err(HostError::new("Kiro version deadline", HostErrorKind::Deadline).into());
    }
    let result = runner.run(&CommandRequest {
        executable: command.entry.clone(),
        arguments: vec!["--version".into()],
        cwd: None,
        environment: EnvironmentDelta {
            remove: Default::default(),
            set: Default::default(),
        },
        stdin: None,
        budget,
        reject_stdout: Some(AUTH_PORTAL_MARKER),
        limits: OutputLimits {
            stdout: bounds.max_output_bytes,
            stderr: bounds.max_output_bytes,
            stdin: 0,
        },
    });
    if !result.success() {
        return Err(HostError::new("Kiro version command", HostErrorKind::Command).into());
    }
    let version = parse_version(&result.stdout)?;
    let remaining = bounds.deadline.saturating_sub(started.elapsed());
    if remaining.is_zero() {
        return Err(HostError::new("Kiro version deadline", HostErrorKind::Deadline).into());
    }
    if fingerprint_file(&command.entry, MAX_IMAGE, remaining)? != hash {
        return Err(HostError::new(
            "Kiro launcher changed during version probe",
            HostErrorKind::Conflict,
        )
        .into());
    }
    let platform = if cfg!(target_os = "macos") {
        Platform::MacOs
    } else if cfg!(target_os = "linux") {
        Platform::Linux
    } else {
        Platform::Windows
    };
    Ok(NativeIdentity {
        version,
        harness: METADATA_HARNESS.into(),
        ui: METADATA_UI.into(),
        platform,
        executable_sha256: hash,
    })
}

/// One public `kiro-cli` route and the dispatcher image it must run as.
#[derive(Clone)]
pub struct RuntimeInstallation {
    pub engine: PathBuf,
    pub native: NativeIdentity,
    command: PublicCommand,
    launcher_native: NativeIdentity,
}
impl RuntimeInstallation {
    /// The pane execs bare `kiro-cli` through this PATH; `engine` is never exec'd.
    pub fn launcher(&self) -> LaunchEntrypoint {
        self.command.launch_entrypoint(self.launcher_native.executable_sha256)
    }
    pub fn bind_catalog(&self, catalog: &CatalogObservation) -> Result<CatalogObservation, ContractError> {
        if catalog.provider.as_str() != "kiro"
            || catalog.native != self.launcher_native
            || catalog.schema != CHAT_CATALOG_SOURCE.schema
        {
            return Err(ContractError::Mismatch("Kiro catalog/runtime installation"));
        }
        let mut bound = catalog.clone();
        bound.native = self.native.clone();
        Ok(bound)
    }
}

/// The native `kiro-cli` dispatcher on the PATH route keeps the pane PID while
/// its chat helper runs; a new release is captured by version and hash here.
pub fn probe_runtime<R: CommandRunner>(
    command: &PublicCommand,
    runner: &mut R,
    bounds: ReadBounds,
) -> Result<RuntimeInstallation, EngineProbeError> {
    let started = Instant::now();
    let engine = command.native_image()?;
    let hash = fingerprint_file(&engine, MAX_IMAGE, bounds.deadline)?;
    let launcher_native = probe_launcher(
        command,
        runner,
        ReadBounds { deadline: bounds.deadline.saturating_sub(started.elapsed()), ..bounds },
    )?;
    if fingerprint_file(&engine, MAX_IMAGE, bounds.deadline.saturating_sub(started.elapsed()))? != hash {
        return Err(HostError::new("Kiro updated during discovery", HostErrorKind::Conflict).into());
    }
    Ok(RuntimeInstallation {
        engine,
        command: command.clone(),
        native: NativeIdentity {
            harness: "v2".into(),
            ui: "tui".into(),
            executable_sha256: hash,
            ..launcher_native.clone()
        },
        launcher_native,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommandFailure {
    AuthRequired,
    HelperLaunchFailed,
    NotStarted,
    TimedOut,
    OutputLimit,
    NativeExit(i32),
    Unknown,
}
/// The observed dispatcher failure does not establish ENOENT, bad permissions,
/// auth failure, or native tool effects. It is not fixed by adding PATH.
pub fn command_failure(receipt: &CommandReceipt) -> Option<CommandFailure> {
    if receipt
        .stdout
        .windows(AUTH_PORTAL_MARKER.len())
        .any(|bytes| bytes == AUTH_PORTAL_MARKER)
    {
        return Some(CommandFailure::AuthRequired);
    }
    if receipt.success() {
        return None;
    }
    if receipt.end == CommandEnd::Exited && receipt.exit_code == Some(1) {
        if let Ok(stderr) = std::str::from_utf8(&receipt.stderr) {
            if stderr.lines().any(|line| {
                line.trim()
                    .strip_prefix("error: failed to launch ")
                    .is_some_and(|path| {
                        Path::new(path).is_absolute()
                            && Path::new(path)
                                .file_name()
                                .is_some_and(|name| name == "kiro-cli-chat")
                    })
            }) {
                return Some(CommandFailure::HelperLaunchFailed);
            }
        }
    }
    Some(match receipt.end {
        CommandEnd::NotStarted => CommandFailure::NotStarted,
        CommandEnd::TimedOut => CommandFailure::TimedOut,
        CommandEnd::OutputLimit => CommandFailure::OutputLimit,
        CommandEnd::Exited => receipt
            .exit_code
            .map(CommandFailure::NativeExit)
            .unwrap_or(CommandFailure::Unknown),
        _ => CommandFailure::Unknown,
    })
}

pub fn discovery_bounds() -> ReadBounds {
    ReadBounds {
        deadline: Duration::from_secs(30),
        max_output_bytes: 1024 * 1024,
    }
}

/// Captured read-only command whitelist. Construction does not execute anything.
/// Native catalog invocation itself can open a browser when the installation is
/// unauthenticated, even with stdin closed. Call only in an operator-authorized
/// discovery stage; this guard cancels the observed flow, not its earlier effects.
/// No retry, login, model fallback or schema promotion is performed here.
pub struct CatalogReader<R> {
    grant: CatalogRequest,
    runner: R,
}
pub(super) fn validate_catalog_request(grant: &CatalogRequest) -> Result<(), ContractError> {
    grant.bounds.validate()?;
    require_absolute(&grant.cwd.path, "Kiro catalog cwd")?;
    grant.native.validate()?;
    require_absolute(&grant.executable, "Kiro catalog launcher")?;
    if grant.provider.as_str() != "kiro"
        || !is_release_version(&grant.native.version)
        || grant.native.harness != METADATA_HARNESS
        || grant.native.ui != METADATA_UI
        || grant.source != CHAT_CATALOG_SOURCE
        || grant
            .executable
            .file_name()
            .is_none_or(|name| name != "kiro-cli")
    {
        return Err(ContractError::Mismatch("Kiro catalog grant"));
    }
    Ok(())
}
impl<R: CommandRunner> CatalogReader<R> {
    pub fn new(grant: CatalogRequest, runner: R) -> Result<Self, ContractError> {
        validate_catalog_request(&grant)?;
        Ok(Self { grant, runner })
    }
    pub fn runner_mut(&mut self) -> &mut R {
        &mut self.runner
    }
}
impl<R: CommandRunner> BoundedReadHost for CatalogReader<R> {
    fn read_catalog(&mut self, request: &CatalogRequest) -> Result<ReadOutput, ReadFailure> {
        if *request != self.grant {
            return Err(ReadFailure::Error(ContractError::Mismatch(
                "captured Kiro catalog request",
            )));
        }
        let started = Instant::now();
        if resolve_cwd(&request.cwd.path)
            .map_err(|_| ReadFailure::Error(ContractError::Mismatch("catalog cwd")))?
            != request.cwd
        {
            return Err(ReadFailure::Error(ContractError::Mismatch("catalog cwd")));
        }
        let hash = fingerprint_file(
            &request.executable,
            MAX_IMAGE,
            request.bounds.deadline.saturating_sub(started.elapsed()),
        )
        .map_err(|_| ReadFailure::Error(ContractError::Mismatch("catalog executable")))?;
        if hash != request.native.executable_sha256 {
            return Err(ReadFailure::Error(ContractError::Mismatch(
                "catalog executable",
            )));
        }
        let budget = request.bounds.deadline.saturating_sub(started.elapsed());
        if budget.is_zero() {
            return Err(ReadFailure::TimedOut {
                elapsed: started.elapsed(),
            });
        }
        let result = self.runner.run(&CommandRequest {
            executable: request.executable.clone(),
            arguments: CHAT_CATALOG_ARGUMENTS
                .iter()
                .map(|arg| (*arg).into())
                .collect(),
            cwd: Some(request.cwd.path.clone()),
            environment: EnvironmentDelta {
                remove: Default::default(),
                set: Default::default(),
            },
            stdin: None,
            budget,
            limits: OutputLimits {
                stdout: request.bounds.max_output_bytes,
                stderr: request.bounds.max_output_bytes,
                stdin: 0,
            },
            reject_stdout: Some(AUTH_PORTAL_MARKER),
        });
        match command_failure(&result) {
            Some(CommandFailure::AuthRequired) => return Err(ReadFailure::AuthRequired),
            Some(CommandFailure::TimedOut) => {
                return Err(ReadFailure::TimedOut {
                    elapsed: started.elapsed(),
                })
            }
            Some(CommandFailure::OutputLimit) => {
                return Err(ReadFailure::OutputLimit {
                    limit: request.bounds.max_output_bytes,
                })
            }
            Some(CommandFailure::NativeExit(code)) => return Err(ReadFailure::Exit { code }),
            Some(_) => {
                return Err(ReadFailure::Unknown(Reason {
                    code: "kiro-catalog-no-exit",
                    message: "The bounded catalog command did not exit successfully",
                }))
            }
            None => {}
        }
        // Complete JSON + real exit 0 is the raw read boundary only. H1 separately
        // validates the observed model schema before producing an observation.
        let _: serde_json::Value = serde_json::from_slice(&result.stdout)
            .map_err(|_| ReadFailure::Error(ContractError::Invalid("Kiro catalog JSON")))?;
        Ok(ReadOutput {
            stdout: result.stdout,
            elapsed: started.elapsed(),
            exit_code: 0,
        })
    }
}
