//! Select and fingerprint the actual chat engine, not the macOS dispatcher.
//! No installation, symlink repair, global PATH mutation or credential discovery.
use crate::contract::{descriptor::CatalogSource, hooks::*, plan::EnvironmentDelta, types::*};
use crate::host::{
    command::*,
    process::{fingerprint_file, resolve_cwd},
    HostError, HostErrorKind,
};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub const CHAT_CATALOG_ARGUMENTS: &[&str] = &["chat", "--list-models", "--format", "json"];
pub const AUTH_PORTAL_MARKER: &[u8] = b"Opening auth portal and logging in...";
/// CLI syntax and the models[]/model_id JSON schema are observed on 2.28.0.
pub const CHAT_CATALOG_SOURCE: CatalogSource = CatalogSource {
    arguments: CHAT_CATALOG_ARGUMENTS,
    schema: "kiro-2.28.0-list-models-json-v1",
};

/// `home` is supplied by the framework, not looked up by the provider. Only two
/// declared installation locations and the launcher's sibling are considered.
/// The first executable engine is selected; version failure is NOT a reason to
/// silently substitute another installation or launch a different provider.
pub fn resolve_helper(launcher: &Path, home: &Path) -> Result<PathBuf, HostError> {
    require_absolute(launcher, "Kiro launcher")
        .map_err(|_| HostError::new("Kiro launcher path", HostErrorKind::Invalid))?;
    require_absolute(home, "Kiro installation home")
        .map_err(|_| HostError::new("Kiro home path", HostErrorKind::Invalid))?;
    let mut candidates = vec![];
    if launcher
        .file_name()
        .is_some_and(|name| name == "kiro-cli-chat")
    {
        candidates.push(launcher.to_path_buf());
    } else {
        let canonical = launcher
            .canonicalize()
            .map_err(|e| HostError::io("Kiro launcher resolution", e))?;
        if let Some(parent) = canonical.parent() {
            candidates.push(parent.join("kiro-cli-chat"));
        }
        candidates.push(home.join(".local/bin/kiro-cli-chat"));
        #[cfg(target_os = "macos")]
        candidates.push(PathBuf::from(
            "/Applications/Kiro CLI.app/Contents/MacOS/kiro-cli-chat",
        ));
    }
    for candidate in candidates {
        let metadata = match std::fs::metadata(&candidate) {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(HostError::io("Kiro helper metadata", e)),
        };
        if !metadata.is_file() {
            continue;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if metadata.permissions().mode() & 0o111 == 0 {
                continue;
            }
        }
        return candidate
            .canonicalize()
            .map_err(|e| HostError::io("Kiro helper resolution", e));
    }
    Err(HostError::new(
        "Kiro chat helper unavailable",
        HostErrorKind::Unknown,
    ))
}

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

/// This establishes executable identity only, never T2/T3 or business readiness.
pub fn probe_engine<R: CommandRunner>(
    engine: &Path,
    runner: &mut R,
    bounds: ReadBounds,
) -> Result<NativeIdentity, EngineProbeError> {
    bounds
        .validate()
        .map_err(|_| HostError::new("Kiro probe bounds", HostErrorKind::Invalid))?;
    let started = Instant::now();
    let hash = fingerprint_file(engine, 1024 * 1024 * 1024, bounds.deadline)?;
    let budget = bounds.deadline.saturating_sub(started.elapsed());
    if budget.is_zero() {
        return Err(HostError::new("Kiro version deadline", HostErrorKind::Deadline).into());
    }
    let result = runner.run(&CommandRequest {
        executable: engine.to_path_buf(),
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
        return Err(HostError::new("Kiro engine version command", HostErrorKind::Command).into());
    }
    let version = parse_version(&result.stdout)?;
    let remaining = bounds.deadline.saturating_sub(started.elapsed());
    if remaining.is_zero() {
        return Err(HostError::new("Kiro version deadline", HostErrorKind::Deadline).into());
    }
    if fingerprint_file(engine, 1024 * 1024 * 1024, remaining)? != hash {
        return Err(
            HostError::new("Kiro engine changed during version probe", HostErrorKind::Conflict).into(),
        );
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
        harness: "v2".into(),
        ui: "tui".into(),
        platform,
        executable_sha256: hash,
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
    require_absolute(&grant.executable, "Kiro catalog engine")?;
    if grant.provider.as_str() != "kiro"
        || !is_release_version(&grant.native.version)
        || grant.source != CHAT_CATALOG_SOURCE
        || grant
            .executable
            .file_name()
            .is_none_or(|name| name != "kiro-cli-chat")
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
            1024 * 1024 * 1024,
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
