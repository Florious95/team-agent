//! The public `codex` on PATH is what runs, user wrappers included. The image
//! that route execs into is only a verification target derived from the same
//! PATH: a standalone native `codex`, or the `node` that PATH selects when the
//! route ends in a `#!/usr/bin/env node` launcher script. Nothing is exec'd
//! from here, and no install directory is guessed.
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Instant;
use crate::contract::{hooks::ReadBounds, plan::*, session::CwdIdentity, types::*};
use crate::host::{command::*, process::{fingerprint_file, resolve_cwd}, HostError, HostErrorKind};
use super::catalog::{probe_launcher, ProbeError, CATALOG_SOURCE, MAX_IMAGE};

/// `env` re-resolves its operand through the PATH this route exports, so the
/// interpreter is the same PATH's first native `node`, never a guessed one.
fn env_node_interpreter(head: &[u8]) -> bool {
    let line = head.split(|byte| *byte == b'\n').next().unwrap_or_default();
    let Some(line) = std::str::from_utf8(line).ok().and_then(|line| line.trim_end().strip_prefix("#!")) else { return false };
    let mut words = line.split_whitespace();
    matches!((words.next(), words.next(), words.next()), (Some(env), Some("node"), None)
        if env.starts_with('/') && Path::new(env).file_name().is_some_and(|name| name == "env"))
}
/// The kernel image a `codex` route must exec into. Other `#!` scripts on the
/// route are user wrappers that conventionally exec a later match.
pub fn resolve_engine(command: &PublicCommand) -> Result<PathBuf, HostError> {
    for candidate in command.route() {
        let image = candidate.canonicalize().map_err(|error| HostError::io("Codex PATH route", error))?;
        let mut head = Vec::with_capacity(256);
        std::fs::File::open(&image).and_then(|file| file.take(256).read_to_end(&mut head))
            .map_err(|error| HostError::io("Codex PATH route image", error))?;
        if !head.starts_with(b"#!") { return Ok(image); }
        if env_node_interpreter(&head) { return PublicCommand::resolve("node", &command.search_path)?.native_image(); }
    }
    Err(HostError::new("Codex native image not on PATH after its wrappers", HostErrorKind::Unsupported))
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
    /// The pane execs bare `codex` through this PATH; `engine` is never exec'd.
    pub fn launcher(&self) -> LaunchEntrypoint { self.command.launch_entrypoint(self.launcher_native.executable_sha256) }
    pub fn bind_catalog(&self, catalog: &CatalogObservation) -> Result<CatalogObservation, ContractError> {
        if catalog.provider.as_str() != super::ID || catalog.native != self.launcher_native || catalog.schema != CATALOG_SOURCE.schema {
            return Err(ContractError::Mismatch("Codex catalog/runtime installation"));
        }
        let mut result = catalog.clone(); result.native = self.native.clone(); Ok(result)
    }
}
/// Does not launch a model or inspect a credential store.
pub fn probe_runtime<R: CommandRunner>(command: &PublicCommand, home: &Path, runner: &mut R, bounds: ReadBounds) -> Result<RuntimeInstallation, ProbeError> {
    let started = Instant::now();
    let home = resolve_cwd(home)?.path;
    let engine = resolve_engine(command)?;
    let hash = fingerprint_file(&engine, MAX_IMAGE, bounds.deadline)?;
    let launcher_native = probe_launcher(command, runner, ReadBounds { deadline: bounds.deadline.saturating_sub(started.elapsed()), ..bounds })?;
    if fingerprint_file(&engine, MAX_IMAGE, bounds.deadline.saturating_sub(started.elapsed()))? != hash {
        return Err(HostError::new("Codex updated during discovery", HostErrorKind::Conflict).into());
    }
    Ok(RuntimeInstallation { engine, command: command.clone(), home,
        native: NativeIdentity { harness: "codex".into(), ui: "tui".into(), executable_sha256: hash, ..launcher_native.clone() }, launcher_native })
}
/// Root calls this after resolving/owning the stable primary, before H3 effects.
/// A Git ancestor would make Codex show its folder-trust screen and inherited
/// `.codex/config.toml` layers would join the launch. Metadata only.
pub fn require_private_primary(cwd: &CwdIdentity) -> Result<(), HostError> {
    if resolve_cwd(&cwd.path)? != *cwd { return Err(HostError::new("Codex primary identity", HostErrorKind::Ownership)); }
    for ancestor in cwd.path.ancestors() {
        for path in [ancestor.join(".git"), ancestor.join(".codex/config.toml")] {
            match std::fs::symlink_metadata(path) {
                Ok(_) => return Err(HostError::new("Codex inherited project configuration", HostErrorKind::Ownership)),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
                Err(error) => return Err(HostError::io("Codex primary ancestry metadata", error)),
            }
        }
    }
    Ok(())
}
