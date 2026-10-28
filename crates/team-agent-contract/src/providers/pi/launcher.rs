//! The public `pi` on PATH is what runs, user wrappers included. The image that
//! route execs into is only a verification target derived from the same PATH:
//! a standalone native `pi`, or the `node` that PATH selects when the route
//! ends in the npm `#!/usr/bin/env node` launcher. Nothing is exec'd from here,
//! and no install directory is guessed.
use std::path::{Path, PathBuf};
use std::time::Instant;
use crate::contract::{hooks::ReadBounds, plan::*, types::*};
use crate::host::{command::*, process::{fingerprint_file, resolve_cwd}, HostError, HostErrorKind};
use super::catalog::{probe_launcher, ProbeError, CATALOG_SOURCE, MAX_IMAGE};

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
    /// The pane execs bare `pi` through this PATH; `engine` is never exec'd.
    pub fn launcher(&self) -> LaunchEntrypoint { self.command.launch_entrypoint(self.launcher_native.executable_sha256) }
    pub fn bind_catalog(&self, catalog: &CatalogObservation) -> Result<CatalogObservation, ContractError> {
        if catalog.provider.as_str() != super::ID || catalog.native != self.launcher_native || catalog.schema != CATALOG_SOURCE.schema {
            return Err(ContractError::Mismatch("Pi catalog/runtime installation"));
        }
        let mut result = catalog.clone(); result.native = self.native.clone(); Ok(result)
    }
}
/// Does not launch a model or inspect a credential store.
pub fn probe_runtime<R: CommandRunner>(command: &PublicCommand, home: &Path, runner: &mut R, bounds: ReadBounds) -> Result<RuntimeInstallation, ProbeError> {
    let started = Instant::now();
    let home = resolve_cwd(home)?.path;
    let engine = command.env_launcher_image("node")?;
    let hash = fingerprint_file(&engine, MAX_IMAGE, bounds.deadline)?;
    let launcher_native = probe_launcher(command, runner, ReadBounds { deadline: bounds.deadline.saturating_sub(started.elapsed()), ..bounds })?;
    if fingerprint_file(&engine, MAX_IMAGE, bounds.deadline.saturating_sub(started.elapsed()))? != hash {
        return Err(HostError::new("Pi updated during discovery", HostErrorKind::Conflict).into());
    }
    Ok(RuntimeInstallation { engine, command: command.clone(), home,
        native: NativeIdentity { harness: "pi".into(), ui: "tui".into(), executable_sha256: hash, ..launcher_native.clone() }, launcher_native })
}
