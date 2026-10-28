//! The public `claude` on PATH is what runs, user wrappers included. The image
//! that route execs into is only a verification target derived from the same
//! PATH: the route's first non-`#!` native image. The native installer names
//! that image after its release (`<semver>`, reached through a `claude`
//! symlink), so tmux reports the version as the pane command; such a name must
//! equal the version this same route reports, or the route execs another
//! installation than the one the fence would check. Nothing is exec'd from
//! here, and no install directory is guessed.
use std::path::{Path, PathBuf};
use std::time::Instant;
use crate::contract::{hooks::ReadBounds, plan::*, types::*};
use crate::host::{command::*, process::{fingerprint_file, resolve_cwd}, HostError, HostErrorKind};
use super::catalog::{probe_launcher, semver, ProbeError, CATALOG_SOURCE, MAX_IMAGE};

/// A version-named image must be the release this same route reports; any
/// other image name (an npm `claude` binary) carries no version claim.
pub fn version_fence(engine: &Path, version: &str) -> Result<(), HostError> {
    let name = engine.file_name().and_then(|name| name.to_str()).unwrap_or_default();
    if semver(name) && name != version {
        return Err(HostError::new("Claude PATH route reports another version than its version-named image", HostErrorKind::Conflict));
    }
    Ok(())
}
/// One public command route and the native image it must exec into.
/// Metadata discovery stays a separate identity until this bridge.
#[derive(Clone)]
pub struct RuntimeInstallation {
    pub engine: PathBuf,
    pub native: NativeIdentity,
    command: PublicCommand,
    launcher_native: NativeIdentity,
    home: PathBuf,
}
impl RuntimeInstallation {
    pub fn home(&self) -> &Path { &self.home }
    /// The pane execs bare `claude` through this PATH; `engine` is never exec'd.
    pub fn launcher(&self) -> LaunchEntrypoint { self.command.launch_entrypoint(self.launcher_native.executable_sha256) }
    pub fn bind_catalog(&self, catalog: &CatalogObservation) -> Result<CatalogObservation, ContractError> {
        if catalog.provider.as_str() != super::ID || catalog.native != self.launcher_native || catalog.schema != CATALOG_SOURCE.schema {
            return Err(ContractError::Mismatch("Claude catalog/runtime installation"));
        }
        let mut result = catalog.clone(); result.native = self.native.clone(); Ok(result)
    }
}
/// Does not launch a model or inspect a credential store.
pub fn probe_runtime<R: CommandRunner>(command: &PublicCommand, home: &Path, runner: &mut R, bounds: ReadBounds) -> Result<RuntimeInstallation, ProbeError> {
    let started = Instant::now();
    let home = resolve_cwd(home)?.path;
    let engine = command.native_image()?;
    let hash = fingerprint_file(&engine, MAX_IMAGE, bounds.deadline)?;
    let launcher_native = probe_launcher(command, runner, ReadBounds { deadline: bounds.deadline.saturating_sub(started.elapsed()), ..bounds })?;
    version_fence(&engine, &launcher_native.version)?;
    if fingerprint_file(&engine, MAX_IMAGE, bounds.deadline.saturating_sub(started.elapsed()))? != hash {
        return Err(HostError::new("Claude updated during discovery", HostErrorKind::Conflict).into());
    }
    Ok(RuntimeInstallation { engine, command: command.clone(), home,
        native: NativeIdentity { harness: "claude".into(), ui: "tui".into(), executable_sha256: hash, ..launcher_native.clone() }, launcher_native })
}
