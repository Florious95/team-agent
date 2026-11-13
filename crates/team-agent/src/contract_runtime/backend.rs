//! Framework scope and owned native startup. No legacy Provider enum or adapter
//! is used here. Native admission is resolved before creating the new store.
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use team_agent_contract::contract::{
    descriptor::AuthMode,
    hooks::*,
    plan::*,
    session::{CwdIdentity, ResumeBinding},
    types::*,
};
use team_agent_contract::host::{
    clock::RealClock,
    command::*,
    digest, digest_hex,
    materialize::ScopedMaterializer,
    process::{fingerprint_file, resolve_cwd},
    tmux::{HostLimits, SharedTmux, TmuxHost},
    transport::PhysicalTransport,
};
use team_agent_contract::orchestration::{lifecycle::*, physical::*, store::*, Error};
use thiserror::Error as ThisError;

use crate::transport::Transport;

use super::{
    config::{MemberRoute, RoleConfig, RuntimeFamily},
    discovery::{self, Discovery, DiscoveryError},
    forward::FrameworkContext,
    registry::{self, NativeAdapter},
};

#[derive(Debug, ThisError)]
pub enum BackendError {
    #[error("native migration: {0}")]
    Migration(String),
    #[error("native_authority_lost: {root} has no contract manifest, legacy database, or import snapshot; temporary runtime files may have been purged. Restore native authority from backup before preserving sessions; migration cannot recreate missing authority")]
    AuthorityLost { root: PathBuf },
    #[error("native configuration: {0}")]
    Configuration(#[from] super::config::ConfigError),
    #[error("native discovery: {0}")]
    Discovery(#[from] DiscoveryError),
    #[error("contract runtime: {0}")]
    Runtime(#[from] Error),
    #[error("native contract: {0:?}")]
    Contract(#[from] ContractError),
    #[error("native host: {0}")]
    Host(#[from] team_agent_contract::host::HostError),
    #[error("native runtime I/O: {0}")]
    Io(#[from] std::io::Error),
    #[error("native runtime metadata is invalid")]
    Metadata,
    #[error("session_preserve_unavailable: {0}; choose --session fresh to start a new conversation")]
    Continuity(String),
    #[error("ambiguous recorded native scopes: {0}; select the authoritative binding before continuing")]
    AmbiguousScope(String),
    #[error("native lifecycle fence: {reason}")]
    Fence { reason: &'static str },
    #[error("native stop refused (NoEffect) operation={operation:?} reason={reason:?}")]
    StopRefused { operation: OperationId, reason: Option<String> },
    #[error("native shutdown operation={operation:?} phase={phase:?} outcome={outcome:?} failure={failure:?}")]
    Shutdown {
        operation: OperationId,
        phase: Phase,
        outcome: Outcome,
        failure: Option<String>,
    },
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    pub version: u32,
    pub workspace: CwdIdentity,
    pub team: String,
    pub scope: ScopeId,
    pub endpoint: String,
    pub candidate: PathBuf,
    pub candidate_sha256: Digest,
    pub tmux: PathBuf,
    pub transport_root: PathBuf,
}
impl Binding {
    pub fn framework(&self) -> FrameworkContext {
        FrameworkContext {
            workspace: self.workspace.clone(),
            team: self.team.clone(),
            native_scope: self.scope.clone(),
        }
    }
}

pub struct Backend {
    pub store: ProjectionStore,
    pub binding: Binding,
    // Mutators share the same directory fence used by authority cutover.
    _scope_lock: Option<std::fs::File>,
}

fn computed_scope(workspace: &CwdIdentity, team: &str) -> Result<ScopeId, BackendError> {
    if team.trim().is_empty() || team.chars().any(char::is_control) {
        return Err(BackendError::Metadata);
    }
    let encoded = serde_json::to_vec(&(workspace, team)).map_err(|_| BackendError::Metadata)?;
    Ok(ScopeId::new(format!(
        "contract-{}",
        &digest_hex(digest(&encoded))[..32]
    ))?)
}
/// Released bindings contain a boot-dependent CWD digest. Keep their recorded
/// scope: native identities and receipts embed it and must never be renumbered.
pub fn scope_for(workspace: &CwdIdentity, team: &str) -> Result<ScopeId, BackendError> {
    let current = computed_scope(workspace, team)?;
    match read_binding_file(&root_for(&workspace.path, &current).join("framework-binding.json")) {
        Ok(binding) if binding.version == 1 && binding.workspace == *workspace
            && binding.team == team && binding.scope == current && binding.endpoint == endpoint(&current) => return Ok(current),
        Ok(_) => return Err(BackendError::Metadata),
        Err(BackendError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {},
        Err(error) => return Err(error),
    }
    let parent = root_for(&workspace.path, &current).parent().ok_or(BackendError::Metadata)?.to_owned();
    let entries = match std::fs::read_dir(parent) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(current),
        Err(error) => return Err(error.into()),
    };
    use std::os::unix::fs::MetadataExt;
    let legacy = team_agent_contract::host::process::legacy_cwd(&workspace.path)?;
    let mut recorded = Vec::new();
    let mut stable_exact = None;
    let mut legacy_exact = None;
    for entry in entries {
        let entry = entry?;
        if !entry.file_type()?.is_dir() { continue; }
        let binding = match matching_binding(&entry.path().join("framework-binding.json"), &workspace.path, team) {
            Ok(Some(binding)) => binding,
            Ok(None) => continue,
            Err(BackendError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        if binding.workspace.path != workspace.path || binding.team != team { continue; }
        let metadata = entry.metadata()?;
        if binding.version != 1 || metadata.mode() & 0o777 != 0o700
            || metadata.uid() != unsafe { libc::geteuid() }
            || binding.endpoint != endpoint(&binding.scope)
            || entry.file_name() != binding.scope.as_str()
            || (team_agent_contract::host::process::stable_cwd(&binding.workspace)
                && binding.workspace != *workspace)
        { return Err(BackendError::Metadata); }
        let exact = if binding.workspace == *workspace { &mut stable_exact } else { &mut legacy_exact };
        if (binding.workspace == *workspace || binding.workspace == legacy)
            && exact.replace(binding.scope.clone()).is_some()
        {
            return Err(BackendError::AmbiguousScope("multiple exact CWD matches".into()));
        }
        recorded.push(binding.scope);
    }
    if let Some(exact) = stable_exact.or(legacy_exact) { return Ok(exact); }
    if recorded.len() > 1 {
        return Err(BackendError::AmbiguousScope(recorded.iter().map(ScopeId::as_str).collect::<Vec<_>>().join(", ")));
    }
    Ok(recorded.pop().unwrap_or(current))
}

pub fn root_for(workspace: &Path, scope: &ScopeId) -> PathBuf {
    crate::model::paths::runtime_dir(workspace)
        .join("contract")
        .join(scope.as_str())
}
fn endpoint(scope: &ScopeId) -> String {
    format!("framework:{}", scope.as_str())
}

pub fn read_binding(workspace: &Path, team: &str) -> Result<Binding, BackendError> {
        let workspace = resolve_cwd(workspace)?;
        let scope = scope_for(&workspace, team)?;
        let root = root_for(&workspace.path, &scope);
        let binding = read_binding_file(&root.join("framework-binding.json"))?;
        if binding.version != 1
            || binding.workspace.path != workspace.path
            || binding.team != team
            || binding.scope != scope
            || binding.endpoint != endpoint(&scope)
        {
            return Err(BackendError::Metadata);
        }
        Ok(binding)
}

fn read_binding_file(file_path: &Path) -> Result<Binding, BackendError> {
    serde_json::from_value(read_binding_value(file_path)?).map_err(|_| BackendError::Metadata)
}

fn matching_binding(file_path: &Path, workspace: &Path, team: &str) -> Result<Option<Binding>, BackendError> {
    let value = read_binding_value(file_path)?;
    if value["team"] != team || value["workspace"]["path"] != serde_json::json!(workspace) {
        return Ok(None);
    }
    serde_json::from_value(value).map(Some).map_err(|_| BackendError::Metadata)
}

fn read_binding_value(file_path: &Path) -> Result<serde_json::Value, BackendError> {
        let metadata = std::fs::symlink_metadata(file_path)?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(BackendError::Metadata);
        }
        let mut bytes = Vec::new();
        std::fs::File::open(file_path)?
            .take(65537)
            .read_to_end(&mut bytes)?;
        if bytes.len() > 65536 {
            return Err(BackendError::Metadata);
        }
        serde_json::from_slice(&bytes).map_err(|_| BackendError::Metadata)
}

/// Inspect authority sources without opening SQLite, probing providers, or writing.
/// An absent manifest is not evidence that an importable legacy store exists.
fn check_authority(binding: &Binding) -> Result<(), BackendError> {
    let present = |name: &str| -> Result<bool, BackendError> {
        match std::fs::symlink_metadata(binding.transport_root.join(name)) {
            Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => Ok(true),
            Ok(_) => Err(BackendError::Metadata),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error.into()),
        }
    };
    if present("contract.json")? { return Ok(()); }
    if present("contract.db")? || present("legacy-import.json")? {
        return Err(BackendError::Migration(format!(
            "legacy contract store requires explicit migration; run team-agent migrate --workspace {} --team {}",
            crate::cli::shell_quote(&binding.workspace.path.to_string_lossy()),
            crate::cli::shell_quote(&binding.team))));
    }
    Err(BackendError::AuthorityLost { root: binding.transport_root.clone() })
}

pub fn authority_check(workspace: &Path, team: &str) -> Result<(), BackendError> {
    match read_binding(workspace, team) {
        Ok(binding) => check_authority(&binding),
        Err(BackendError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

pub fn authority_is_missing(error: &BackendError) -> bool {
    matches!(error, BackendError::AuthorityLost { .. })
        || matches!(error, BackendError::Io(error) if error.kind() == std::io::ErrorKind::NotFound)
        || matches!(error, BackendError::Runtime(Error::Io(std::io::ErrorKind::NotFound)))
        || matches!(error, BackendError::Migration(message) if message.starts_with("legacy contract store requires explicit migration"))
}

pub fn observation_error(team: &str, seat: &str, error: &BackendError) -> String {
    if matches!(error, BackendError::AuthorityLost { .. } | BackendError::Migration(_)) {
        error.to_string()
    } else if authority_is_missing(error) {
        format!("native_authority_missing: {seat} has no native contract binding for team {team} in this workspace")
    } else {
        error.to_string()
    }
}

pub fn authority_recovery_action(workspace: &Path, team: &str, seat: &str) -> String {
    if let Err(error @ BackendError::AuthorityLost { .. }) = authority_check(workspace, team) {
        if !read_binding(workspace, team).is_ok_and(|binding| fresh_recovery_allowed(&binding)) {
            return error.to_string();
        }
        return format!("{error}; after explicitly accepting fresh conversations, use team-agent team restart --allow-fresh --workspace {} --team {} (recovery refuses live or unobservable old carriers)",
            crate::cli::shell_quote(&workspace.to_string_lossy()), crate::cli::shell_quote(team));
    }
    let legacy = read_binding(workspace, team).is_ok_and(|binding|
        binding.transport_root.join("contract.db").is_file()
            || binding.transport_root.join("legacy-import.json").is_file());
    let workspace = crate::cli::shell_quote(&workspace.to_string_lossy());
    let team = crate::cli::shell_quote(team);
    if legacy {
        format!("run team-agent migrate --workspace {workspace} --team {team}")
    } else {
        format!("run team-agent restart {} --workspace {workspace} --team {team} to respawn it under this release", crate::cli::shell_quote(seat))
    }
}

impl Backend {
    pub fn open(workspace: &Path, team: &str) -> Result<Self, BackendError> {
        Self::open_mode(workspace, team, false)
    }
    pub fn open_readonly(workspace: &Path, team: &str) -> Result<Self, BackendError> {
        Self::open_mode(workspace, team, true)
    }
    fn open_mode(workspace: &Path, team: &str, readonly: bool) -> Result<Self, BackendError> {
        let mut binding = read_binding(workspace, team)?;
        let scope_lock = if readonly { None } else {
            let directory = std::fs::File::open(root_for(&binding.workspace.path, &binding.scope))?;
            directory.lock_shared()?;
            binding = read_binding(workspace, team)?;
            Some(directory)
        };
        check_authority(&binding)?;
        // Build provenance is diagnostic, not an ownership or lifecycle fence.
        // New launches use this invocation, including relocated binaries.
        binding.candidate = std::env::current_exe()?.canonicalize()?;
        binding.candidate_sha256 = executing_candidate_sha()?;
        // The client is selected by this invocation's PATH, not an old install
        // location. Server/process ownership still comes from the saved receipts.
        binding.tmux = discovery::public_command("tmux")?.entry;
        let scope = binding.scope.clone();
        let store = if readonly {
            ProjectionStore::open_readonly(&binding.transport_root, scope, &binding.endpoint)
        } else {
            ProjectionStore::open(&binding.transport_root, scope, &binding.endpoint)
        }.map_err(|error| match error {
            Error::Invalid("legacy contract store requires explicit migration") => BackendError::Migration(format!(
                "legacy contract store requires explicit migration; run team-agent migrate --workspace {} --team {}",
                crate::cli::shell_quote(&binding.workspace.path.to_string_lossy()), crate::cli::shell_quote(team))),
            error => BackendError::Runtime(error),
        })?;
        Ok(Self { store, binding, _scope_lock: scope_lock })
    }

    /// Called only after the caller's pure/native-discovery preflight. Exclusive
    /// roots are not adopted; partial initialization stays visible on failure.
    fn create(
        workspace: CwdIdentity,
        team: &str,
        candidate: PathBuf,
        tmux: PathBuf,
    ) -> Result<Self, BackendError> {
        use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
        let scope = scope_for(&workspace, team)?;
        let root = root_for(&workspace.path, &scope);
        let parent = root.parent().ok_or(BackendError::Metadata)?;
        let base = parent.parent().ok_or(BackendError::Metadata)?;
        if base.canonicalize()? != base {
            return Err(BackendError::Metadata);
        }
        match std::fs::symlink_metadata(parent) {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
            Ok(_) => return Err(BackendError::Metadata),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                std::fs::DirBuilder::new().mode(0o700).create(parent)?;
            }
            Err(error) => return Err(error.into()),
        }
        let candidate_sha256 =
            fingerprint_file(&candidate, 1024 * 1024 * 1024, Duration::from_secs(10))?;
        std::fs::DirBuilder::new().mode(0o700).create(&root)?;
        // Authority and preserved native resources must outlive OS temp cleanup.
        // The shared workspace tmux endpoint is independently short; this root
        // contains files, not a sockaddr_un path, even for long workspace names.
        let transport_root = root.join("authority");
        let store = ProjectionStore::create(&transport_root, scope.clone(), &endpoint(&scope))?;
        let binding = Binding {
            version: 1,
            workspace,
            team: team.into(),
            scope: scope.clone(),
            endpoint: endpoint(&scope),
            candidate,
            candidate_sha256,
            tmux,
            transport_root,
        };
        let bytes = serde_json::to_vec(&binding).map_err(|_| BackendError::Metadata)?;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(root.join("framework-binding.json"))?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        let directory = std::fs::File::open(&root)?;
        directory.lock_shared()?;
        directory.sync_all()?;
        Ok(Self { store, binding, _scope_lock: Some(directory) })
    }
}

fn workspace_socket(path: &Path) -> Result<Option<(u64, u64)>, BackendError> {
    use std::os::unix::fs::{FileTypeExt, MetadataExt};
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    // Root already uses libc for UID checks; this syscall has no pointer input.
    let uid = unsafe { libc::geteuid() };
    if !metadata.file_type().is_socket() || metadata.uid() != uid {
        return Err(ContractError::Mismatch("workspace socket owner/type").into());
    }
    Ok(Some((metadata.dev(), metadata.ino())))
}

/// The selected root transport is authority; ambient TMUX is never a selector.
/// This runs only in the owning lifecycle action, after native F0 admission.
pub(super) fn workspace_session_lock(binding: &Binding) -> Result<std::fs::File, BackendError> {
    use std::os::unix::fs::OpenOptionsExt;
    let file = std::fs::OpenOptions::new().read(true).write(true).create(true).truncate(false)
        .mode(0o600).custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(root_for(&binding.workspace.path, &binding.scope).join("workspace-session.lock"))?;
    file.lock()?;
    Ok(file)
}

pub(super) fn workspace_tmux(
    binding: &Binding,
    seat: &SeatId,
    claim: Option<&team_agent_contract::host::transport::TargetReceipt>,
) -> Result<(SharedTmux, crate::tmux_backend::TmuxBackend, String), BackendError> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt};
    let selected = crate::state::selector::resolve_active_team_readonly(
        &binding.workspace.path, Some(&binding.team), crate::state::selector::SelectorMode::RuntimeOnly,
    ).map_err(|_| ContractError::Mismatch("workspace topology scope"))?;
    if selected.run_workspace != binding.workspace.path || selected.team_key != binding.team {
        return Err(BackendError::Fence { reason: "workspace_topology_scope" });
    }
    crate::lifecycle::launch::ensure_owner_allowed_for_state(&selected.state,
        Some(&crate::model::ids::AgentId::new(seat.as_str())))
        .map_err(|_| ContractError::Mismatch("workspace topology caller ownership"))?;
    let transport = crate::lifecycle::restart::lifecycle_worker_tmux_backend_selection_for_state(
        &selected.run_workspace, &selected.state,
    ).map_err(|_| ContractError::Mismatch("workspace topology transport"))?.backend;
    let endpoint = transport.tmux_endpoint().filter(|s| !s.is_empty())
        .ok_or(ContractError::Invalid("explicit workspace tmux endpoint required"))?;
    let named = !Path::new(&endpoint).is_absolute();
    let endpoint = if named {
        crate::tmux_backend::socket_path_for_name(&endpoint)
            .ok_or(ContractError::Invalid("workspace tmux socket name"))?
    } else { PathBuf::from(endpoint) };
    let parent = endpoint.parent().ok_or(ContractError::Invalid("workspace socket parent"))?;
    if !parent.exists() && named && claim.is_none() {
        if let Err(error) = std::fs::DirBuilder::new().mode(0o700).create(parent) {
            if error.kind() != std::io::ErrorKind::AlreadyExists { return Err(error.into()); }
        }
    }
    let metadata = std::fs::symlink_metadata(parent)?;
    let physical_parent = parent.canonicalize()?;
    if !metadata.is_dir() || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o777 != 0o700 || (!named && physical_parent != parent)
    { return Err(ContractError::Mismatch("workspace socket parent ownership").into()); }
    let endpoint = physical_parent.join(endpoint.file_name().ok_or(ContractError::Invalid("workspace socket name"))?);
    let session_name = selected.state.get("session_name").and_then(serde_json::Value::as_str)
        .filter(|s| !s.is_empty() && !s.chars().any(|c| c.is_control() || c == ':'))
        .ok_or(ContractError::Invalid("workspace team session name"))?.to_owned();
    let generation = crate::state::projection::session_generation(&selected.state)
        .unwrap_or(&session_name).to_owned();
    let socket = workspace_socket(&endpoint)?;
    let mut session = None;
    if socket.is_some() {
        let result = RealCommandRunner::default().run(&CommandRequest {
            executable: binding.tmux.clone(), arguments: vec!["-u".into(), "-S".into(), endpoint.as_os_str().to_owned(),
                "-N".into(), "list-sessions".into(), "-F".into(), "#{session_id}:#{session_name}".into()],
            cwd: None, environment: EnvironmentDelta { remove: Default::default(), set: Default::default() },
            stdin: None, budget: Duration::from_secs(5), limits: OutputLimits { stdout: 65536, stderr: 4096, stdin: 1 },
            reject_stdout: None,
        });
        if result.success() {
            let text = std::str::from_utf8(&result.stdout).map_err(|_| ContractError::Invalid("workspace session metadata"))?;
            for line in text.lines() {
                let (id, name) = line.split_once(':').ok_or(ContractError::Invalid("workspace session metadata"))?;
                if !id.strip_prefix('$').is_some_and(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())) {
                    return Err(ContractError::Invalid("workspace session ID").into());
                }
                if name == session_name {
                    if session.replace(id.to_owned()).is_some() { return Err(BackendError::Fence { reason: "workspace_session_ambiguous" }); }
                }
            }
        } else if !(result.end == CommandEnd::Exited && result.exit_code == Some(1) && result.child_reaped
            && result.stdout.is_empty() && result.stderr == format!("no server running on {}\n", endpoint.display()).as_bytes())
        { return Err(ContractError::Mismatch("workspace server observation unavailable").into()); }
        if workspace_socket(&endpoint)? != socket { return Err(BackendError::Fence { reason: "workspace_socket_changed" }); }
    }
    if let Some(id) = &session {
        let owner = transport.session_owner(&crate::transport::SessionName::new(&session_name))
            .map_err(|_| ContractError::Mismatch("workspace session owner observation"))?;
        let may_claim = claim.is_some_and(|target| target.shared_endpoint && target.created_session
            && target.identity().scope == binding.scope && &target.identity().seat == seat
            && target.endpoint == endpoint && target.session == *id
            && Some((target.socket_device, target.socket_inode)) == socket);
        if !owner.as_ref().is_some_and(|owner| Path::new(&owner.workspace) == binding.workspace.path
            && owner.team == binding.team && owner.generation == generation)
            && !(owner.is_none() && may_claim)
        { return Err(ContractError::Mismatch("workspace session ownership").into()); }
    } else if claim.is_some() { return Err(ContractError::Mismatch("committed workspace session disappeared").into()); }
    if claim.is_some_and(|target| target.endpoint != endpoint
        || session.as_deref() != Some(target.session.as_str()))
    { return Err(ContractError::Mismatch("committed workspace target changed").into()); }
    Ok((SharedTmux { endpoint, socket, session_name, session,
        caller: Some((binding.workspace.path.clone(), binding.team.clone())) }, transport, generation))
}

/// The canonical seat is saved first. Only an exact current target that
/// actually created this shared session may seed a missing root owner marker.
pub(super) fn bind_workspace_session(workspace: &Path, team: &str, seat: &SeatId) -> Result<(), BackendError> {
    let backend = Backend::open_readonly(workspace, team)?;
    let current = backend.store.seat(seat)?.ok_or(BackendError::Metadata)?;
    let target = current.physical.as_ref().ok_or(BackendError::Metadata)?;
    if !target.shared_endpoint { return Ok(()); }
    let clock = RealClock::new();
    let deadline = Duration::from_secs(5);
    let mut host = TmuxHost::restore_owned(target.clone(), backend.binding.tmux.clone(),
        settings(&backend.binding, registry::provider(current.provider.as_str())?).limits,
        RealCommandRunner::default(), &clock, deadline)?;
    host.validate_target(target, &clock, deadline)?;
    let (topology, transport, generation) = workspace_tmux(&backend.binding, seat, Some(target))?;
    let session = crate::transport::SessionName::new(&topology.session_name);
    transport.set_session_owner_with_generation(&session, &backend.binding.workspace.path, team, &generation)
        .map_err(|_| ContractError::Mismatch("workspace session owner commit"))?;
    let owner = transport.session_owner(&session).map_err(|_| ContractError::Mismatch("workspace session owner readback"))?;
    if !owner.is_some_and(|owner| Path::new(&owner.workspace) == backend.binding.workspace.path
        && owner.team == team && owner.generation == generation)
    { return Err(BackendError::Fence { reason: "workspace_session_owner_changed" }); }
    host.validate_target(target, &clock, deadline)?;
    Ok(())
}

/// A session-wide legacy kill must not bypass canonical teardown of any
/// contract window, including a paused or currently unselected seat.
pub(crate) fn has_shared_windows(workspace: &Path, team: &str) -> Result<bool, BackendError> {
    let cwd = resolve_cwd(workspace)?;
    if !root_for(&cwd.path, &scope_for(&cwd, team)?).exists() { return Ok(false); }
    let backend = Backend::open_readonly(&cwd.path, team)?;
    Ok(backend.store.seats()?.iter().any(|seat|
        seat.physical.as_ref().is_some_and(|target| target.shared_endpoint)))
}

pub(super) fn settings(binding: &Binding, provider: registry::NativeProvider) -> PhysicalSettings {
    PhysicalSettings {
        tmux: binding.tmux.clone(),
        limits: HostLimits {
            command_output: OutputLimits {
                stdout: 256 * 1024,
                stderr: 65536,
                stdin: 2 * 1024 * 1024,
            },
            max_script_bytes: 128 * 1024,
            max_capture_bytes: 256 * 1024,
            max_payload_bytes: 1024 * 1024,
            max_executable_bytes: 1024 * 1024 * 1024,
            poll_interval: Duration::from_millis(100),
            columns: 120,
            rows: 40,
        },
        action_budget: provider.action_budget(),
        freshness: Duration::from_secs(2),
        journal_bytes: 2 * 1024 * 1024,
    }
}
pub(super) fn adapter(
    binding: &Binding,
    identity: &InstanceIdentity,
    provider: &ProviderId,
) -> Result<NativeAdapter, BackendError> {
    Ok(registry::provider(provider.as_str())?.adapter(
        binding.candidate.clone(),
        vec![
            "contract-mcp".into(),
            "--workspace".into(),
            binding
                .workspace
                .path
                .to_str()
                .ok_or(BackendError::Metadata)?
                .into(),
            "--team".into(),
            binding.team.clone(),
            "--seat".into(),
            identity.seat.as_str().into(),
            "--instance".into(),
            identity.instance.as_str().into(),
            "--generation".into(),
            identity.generation.0.to_string(),
        ],
        BTreeMap::new(),
    )?.with_launch_context(binding.workspace.path.clone(), None)?)
}
pub(super) fn launch_adapter(
    binding: &Binding,
    identity: &InstanceIdentity,
    provider: &ProviderId,
    discovered: &Discovery,
) -> Result<NativeAdapter, BackendError> {
    Ok(adapter(binding, identity, provider)?.with_launch_context(
        binding.workspace.path.clone(), discovered.runtime.clone(),
    )?)
}
pub(super) fn request(
    binding: &Binding,
    role: &RoleConfig,
    identity: InstanceIdentity,
    discovered: &Discovery,
) -> Result<LaunchRequest, BackendError> {
    let descriptor = registry::provider(role.provider.as_str())?.descriptor();
    if discovered.catalog.provider != role.provider
        || discovered.catalog.native != discovered.native
    {
        return Err(ContractError::Mismatch("role native catalog").into());
    }
    // Metadata-only discovery cannot authorize a native-subscription worker.
    descriptor.auth.subscription.require("native subscription launch")?;
    let profiles = descriptor.input.profiles.require("native input profiles")?;
    let mut matches = profiles.iter().filter(|profile| {
        profile.matches_native(&discovered.native)
            && profile.channel == Channel::Tmux
            && profile.operations.contains(&Operation::FirstBusiness)
    });
    let profile = matches.next().ok_or(ContractError::ProfileUnavailable)?;
    if matches.next().is_some() {
        return Err(ContractError::AmbiguousProfile.into());
    }
    profile.policy.require("native submit policy")?;
    // The worker execs exactly the public command its runtime route was probed on.
    let launcher = discovered.runtime.as_ref()
        .ok_or(ContractError::Invalid("native runtime installation"))?
        .launcher();
    Ok(LaunchRequest {
        provider: role.provider.as_str().into(),
        operation: Operation::Fresh,
        mode: LaunchMode::FullWorker,
        auth: AuthMode::NativeSubscription,
        model: Some(role.model.clone()),
        role_effort: role.effort.clone(),
        team_effort: None,
        bypass: role.bypass,
        prompt: Some(role.prompt.text.clone()),
        identity,
        native: discovered.native.clone(),
        paths: LaunchPaths {
            launcher,
            executable: discovered.engine.clone(),
            candidate: binding.candidate.clone(),
            cwd: binding.workspace.clone(),
            runtime_root: binding.transport_root.clone(),
        },
        channel: Channel::Tmux,
        input_profile: Some(profile.id.into()),
        evidence_kind: EvidenceKind::Native,
        preassigned_session: None,
        resume: None,
        fork: None,
    })
}

/// Team-wide native discovery/admission before the shared launcher creates any
/// seats (including legacy peers). Bounded read-only native commands run; no
/// runtime store, worker config, private socket, or worker process is created.
pub fn preflight(team: &super::config::TeamConfig) -> Result<(), BackendError> {
    if team.roles.iter().all(|role| role.paused) {
        return Ok(());
    }
    let mut discoveries = BTreeMap::new();
    let candidate = std::env::current_exe()?.canonicalize()?;
    let candidate_sha256 =
        fingerprint_file(&candidate, 1024 * 1024 * 1024, Duration::from_secs(10))?;
    let binding = Binding {
        version: 1,
        workspace: team.workspace.clone(),
        team: team.selector.clone(),
        scope: team.scope.clone(),
        endpoint: endpoint(&team.scope),
        candidate,
        candidate_sha256,
        tmux: discovery::public_command("tmux")?.entry,
        transport_root: "/tmp/preflight-no-write".into(),
    };
    for role in team.roles.iter().filter(|role| !role.paused) {
        let selected = registry::provider(role.provider.as_str())?;
        if !discoveries.contains_key(&role.provider) {
            discoveries.insert(
                role.provider.clone(),
                discovery::discover_for_start(&role.provider, &team.workspace.path)?,
            );
        }
        let discovered = discoveries.get(&role.provider).ok_or(BackendError::Metadata)?;
        let identity = InstanceIdentity {
            scope: team.scope.clone(),
            seat: role.id.clone(),
            instance: InstanceId::new("preflight")?,
            generation: Generation(1),
        };
        let adapter = launch_adapter(&binding, &identity, &role.provider, discovered)?;
        let hooks = adapter.hooks();
        let request = request(&binding, role, identity, discovered)?;
        let resolved = resolve_launch_preflight(
            selected.descriptor(),
            &hooks,
            &request,
            Some(&discovered.catalog),
        )?;
        validate_launch_plan(selected.descriptor(), &resolved, &adapter.plan(&resolved)?)?;
    }
    Ok(())
}

#[derive(Debug)]
enum ExistingStart {
    Committed(OperationRecord),
    FinishReceipt(OperationRecord),
    Pending(OperationRecord),
    Resume(SeatRecord),
    Blocked(OperationRecord),
}

// Receipt classification is read-only, never native admission or permission
// to replay an unfinished host effect. Check parent leases before old success.
fn existing_start(backend: &Backend, seat: &SeatId) -> Result<Option<ExistingStart>, BackendError> {
    let unfinished: Vec<_> = backend.store.unfinished()?.into_iter().filter(|record| involves_seat(record, seat)).collect();
    if unfinished.len() > 1 { return Err(BackendError::Fence { reason: "unfinished_operation_ambiguous" }); }
    if let Some(record) = unfinished.into_iter().next() {
        if record.target.identity.scope != backend.binding.scope { return Err(BackendError::Fence { reason: "unfinished_scope_mismatch" }); }
        return Ok(Some(if record.phase == Phase::F4Commit && record.pending.is_none()
            && matches!(record.kind, TransactionKind::Startup | TransactionKind::Restart
                | TransactionKind::NewSeatFullSnapshot | TransactionKind::NativeNewSeat)
        { ExistingStart::FinishReceipt(record) }
        else if record.outcome == Outcome::Running { ExistingStart::Pending(record) }
        else { ExistingStart::Blocked(record) }));
    }
    let Some(current) = backend.store.seat(seat)? else {
        return match backend.store.creation_receipt(seat, None)? {
            None => Ok(None),
            Some(record) if record.outcome == Outcome::Compensated => Ok(None),
            // remove/reset ended this identity explicitly; the next start is fresh.
            Some(record) if backend.store.retired(&record.target.identity)? => Ok(None),
            Some(_) => Err(BackendError::Fence { reason: "creation_receipt_without_current_seat" }),
        };
    };
    if current.identity.scope != backend.binding.scope { return Err(BackendError::Fence { reason: "current_scope_mismatch" }); }
    let record = backend.store.creation_receipt(seat, Some(&current.identity))?.ok_or(BackendError::Fence { reason: "creation_receipt_missing" })?;
    if record.outcome != Outcome::Committed || record.phase != Phase::F5Receipt || record.pending.is_some() {
        return Ok(Some(ExistingStart::Blocked(record)));
    }
    Ok(Some(if current.status == SeatStatus::Stopped { ExistingStart::Resume(current) }
        else { ExistingStart::Committed(record) }))
}

pub(crate) fn executing_candidate_sha() -> Result<Digest, BackendError> {
    static SHA: std::sync::OnceLock<Digest> = std::sync::OnceLock::new();
    if let Some(sha) = SHA.get() { return Ok(*sha); }
    let path = std::env::current_exe()?.canonicalize()?;
    let sha = fingerprint_file(&path, 1024 * 1024 * 1024, Duration::from_secs(10))?;
    let _ = SHA.set(sha);
    Ok(sha)
}

pub(crate) fn validate_delivery_target(binding: &Binding, seat: &SeatRecord, budget: Duration) -> Result<(), BackendError> {
    let target = seat.physical_target()?;
    if seat.evidence_kind != EvidenceKind::Native || !matches!(seat.status, SeatStatus::Starting | SeatStatus::Ready) {
        return Err(BackendError::Fence { reason: "delivery_target_not_current_native_owner" });
    }
    let clock = RealClock::new();
    let mut host = TmuxHost::restore_owned(target.clone(), binding.tmux.clone(),
        settings(binding, registry::provider(seat.provider.as_str())?).limits,
        RealCommandRunner::default(), &clock, budget)?;
    host.validate_target(target, &clock, budget)?;
    Ok(())
}

/// Read one bounded, ownership-fenced surface through the provider's native
/// semantic reader. Never infer activity from a live PID or expose pane text.
pub(super) fn observe_activity(binding: &Binding, seat: &SeatRecord) -> Result<Option<team_agent_contract::contract::probe::SemanticEvidence>, BackendError> {
    use team_agent_contract::contract::probe::ProbeOutcome;
    let target = seat.physical_target()?;
    let budget = Duration::from_millis(500);
    let clock = RealClock::new();
    let mut host = TmuxHost::restore_owned(target.clone(), binding.tmux.clone(),
        settings(binding, registry::provider(seat.provider.as_str())?).limits,
        RealCommandRunner::default(), &clock, budget)?;
    let frame = host.capture(target, &clock, budget, budget)?;
    let adapter = adapter(binding, &seat.identity, &seat.provider)?;
    let observed = adapter.hooks().semantic.require("native activity observation")?.read(&NativeRecord {
        scope: frame.scope, kind: NativeRecordKind::Surface, bytes: frame.text.into_bytes(),
    });
    Ok(match observed.outcome { ProbeOutcome::Observed(activity) => Some(activity), _ => None })
}

fn validate_existing(backend: &Backend, role: &RoleConfig, record: &OperationRecord) -> Result<SeatRecord, BackendError> {
    let current = backend.store.assert_current(&record.target.identity)?;
    if current.evidence_kind != EvidenceKind::Native || current.provider != role.provider
        || current.status == SeatStatus::Unknown
        || serde_json::to_vec(&read_role(&backend.binding, &current.identity)?).map_err(|_| BackendError::Metadata)?
            != serde_json::to_vec(role).map_err(|_| BackendError::Metadata)?
    { return Err(ContractError::Mismatch("existing native launch configuration").into()); }
    let target = current.physical.as_ref().ok_or(BackendError::Metadata)?;
    let clock = RealClock::new();
    let deadline = Duration::from_secs(5);
    let mut host = TmuxHost::restore_owned(target.clone(), backend.binding.tmux.clone(),
        settings(&backend.binding, registry::provider(current.provider.as_str())?).limits,
        RealCommandRunner::default(), &clock, deadline)?;
    host.validate_target(target, &clock, deadline)?;
    Ok(current)
}

pub(super) fn receipt_error(record: OperationRecord) -> BackendError {
    let preserve = record.native_launch == Some(Operation::Resume);
    let failure = match (record.failure, record.recovery_failure) {
        (Some(first), Some(recovery)) => Some(format!("{first}; recovery_failure={recovery}")),
        (None, Some(recovery)) => Some(format!("recovery_failure={recovery}")),
        (first, None) => first,
    };
    let error = BackendError::Shutdown { operation: record.id, phase: record.phase,
        outcome: record.outcome, failure: failure.or(record.pending) };
    if preserve {
        BackendError::Continuity(format!("native resume was not confirmed; {error}; reconcile the retained operation before retrying"))
    } else { error }
}

pub(super) fn require_stopped(record: &OperationRecord) -> Result<(), BackendError> {
    if matches!(record.outcome, Outcome::Committed | Outcome::Compensated) && record.target.status == SeatStatus::Stopped {
        return Ok(());
    }
    if record.kind == TransactionKind::Teardown && record.phase == Phase::F0Preflight
        && record.outcome == Outcome::Compensated && record.effect == team_agent_contract::contract::delivery::DeliveryEffect::NoEffect {
        return Err(BackendError::StopRefused { operation: record.id.clone(), reason: record.failure.clone() });
    }
    Err(receipt_error(record.clone()))
}

/// Plan only against this selected store and exact row (or an unbound seed).
/// A stale parent can be repaired only by its actual creation receipt, never
/// by selecting the greatest generation or adopting a discovered process.
pub(crate) fn start_needs_reconciliation(workspace: &Path, team: &str, seat: &SeatId, row: &serde_json::Value) -> Result<bool, BackendError> {
    let backend = Backend::open_readonly(workspace, team)?;
    let class = existing_start(&backend, seat)?;
    let current = backend.store.seat(seat)?;
    if current.as_ref().is_some_and(|seat| !registry::same_provider(row.get("provider").and_then(serde_json::Value::as_str).unwrap_or(""), seat.provider.as_str())) {
        return Err(BackendError::Fence { reason: "planner_provider_mismatch" });
    }
    let matches = |identity: &InstanceIdentity| {
        row.get("contract_scope").and_then(serde_json::Value::as_str) == Some(identity.scope.as_str())
            && row.get("contract_instance").and_then(serde_json::Value::as_str) == Some(identity.instance.as_str())
            && row.get("generation").and_then(serde_json::Value::as_u64) == Some(identity.generation.0)
    };
    let seed = ["contract_scope", "contract_instance", "generation", "session_id"].iter()
        .all(|key| row.get(*key).is_none_or(serde_json::Value::is_null));
    let record = match &class {
        Some(ExistingStart::Committed(record) | ExistingStart::FinishReceipt(record)) => Some(record),
        Some(ExistingStart::Pending(record) | ExistingStart::Blocked(record))
            if matches!(record.outcome, Outcome::Running | Outcome::NeedsRecovery) => Some(record),
        _ => None,
    };
    if !seed && !current.as_ref().is_some_and(|seat| matches(&seat.identity))
        && !record.and_then(|record| record.parent.as_ref()).is_some_and(|parent| matches(&parent.identity)
            && row.get("session_id").and_then(serde_json::Value::as_str)
                == parent.session.as_ref().map(|session| session.native_session.as_str()))
    { return Err(BackendError::Fence { reason: "planner_identity_mismatch" }); }
    let needs_projection = seed || current.as_ref().is_some_and(|seat| !matches(&seat.identity)
        || seat.status == SeatStatus::Starting
        || row.get("session_id").and_then(serde_json::Value::as_str) != seat.session.as_ref().map(|session| session.native_session.as_str())
        || (seat.status == SeatStatus::Stopped && row.get("status").and_then(serde_json::Value::as_str) != Some("stopped"))
        || (seat.status != SeatStatus::Stopped && row.get("status").and_then(serde_json::Value::as_str) != Some("running")))
        || matches!(class, Some(ExistingStart::FinishReceipt(_) | ExistingStart::Pending(_) | ExistingStart::Blocked(_)));
    if needs_projection { return Ok(true); }
    if let Some(current) = current.filter(|current| current.status != SeatStatus::Stopped) {
        if let Some(target) = current.physical.as_ref().filter(|target| target.shared_endpoint) {
            // A crash after JSON projection but before the owner marker is not
            // a verified no-op. Reuse only this exact creator receipt, under
            // the existing lifecycle lock; a foreign marker still refuses.
            let (topology, transport, _) = workspace_tmux(&backend.binding, seat, Some(target))?;
            return Ok(transport.session_owner(&crate::transport::SessionName::new(&topology.session_name))
                .map_err(|_| ContractError::Mismatch("workspace session owner observation"))?.is_none());
        }
    }
    Ok(false)
}

/// Read-only preflight, reused by the CLI before any role write or stop.
pub(crate) fn preflight_start_role(workspace: &Path, team: &str, role: &RoleConfig) -> Result<bool, BackendError> {
    let backend = Backend::open_readonly(workspace, team)?;
    match existing_start(&backend, &role.id)? {
        Some(ExistingStart::Committed(record))
            if backend.store.assert_current(&record.target.identity)?.physical.as_ref().is_some_and(|target|
                matches!(
                    team_agent_contract::host::process::sample_process(&target.process),
                    team_agent_contract::host::process::ProcessState::Exited | team_agent_contract::host::process::ProcessState::Replaced)) => {
            prepare_resume(workspace, team, &role.id, Some(role))?;
        },
        Some(ExistingStart::FinishReceipt(record)) if record.target.identity.seat != role.id => return Ok(false),
        Some(ExistingStart::Committed(record) | ExistingStart::FinishReceipt(record)) => { validate_existing(&backend, role, &record)?; },
        Some(ExistingStart::Resume(_)) => { prepare_resume(workspace, team, &role.id, Some(role))?; },
        // The mutating start reconciles interrupted work under the seat flock.
        // Read-only preflight must not wedge that path.
        Some(ExistingStart::Pending(_)) => return Ok(false),
        Some(ExistingStart::Blocked(record)) if matches!(record.outcome, Outcome::Running | Outcome::NeedsRecovery) => return Ok(false),
        Some(ExistingStart::Blocked(record)) => return Err(receipt_error(record)),
        None => return Ok(true), // Caller must run full-team fresh preflight before writes.
    }
    Ok(false)
}

pub(super) fn native_seat_lock(workspace: &Path, team: &str, seat: &SeatId, additional: bool)
    -> Result<crate::lifecycle::lock::LifecycleLockGuard, BackendError>
{
    let agent = crate::model::ids::AgentId::new(seat.as_str());
    let request = crate::lifecycle::lock::LifecycleLockRequest {
        workspace, operation: "native-lifecycle", team: Some(team), agent_id: Some(&agent),
    };
    let guard = if additional { crate::lifecycle::lock::try_acquire_seat_lifecycle_lock(request) }
        else { crate::lifecycle::lock::acquire_seat_lifecycle_lock(request) };
    guard.map_err(|error| std::io::Error::other(error.to_string()).into())
}

pub(crate) fn involves_seat(operation: &OperationRecord, seat: &SeatId) -> bool {
    operation.target.identity.seat == *seat || operation.parent.as_ref().is_some_and(|parent| parent.identity.seat == *seat)
}

fn recover_incomplete(backend: &mut Backend, seat: &SeatId) -> Result<Option<OperationRecord>, BackendError> {
    recover_incomplete_until(backend, seat, None)
}

fn recover_incomplete_until(backend: &mut Backend, seat: &SeatId, deadline: Option<std::time::Instant>) -> Result<Option<OperationRecord>, BackendError> {
    for pending in backend.store.unfinished()?.into_iter().filter(|operation| involves_seat(operation, seat)
        && matches!(operation.outcome, Outcome::Running | Outcome::NeedsRecovery)) {
        let recovered = finish_receipt_until(backend, pending, deadline)?;
        if matches!(recovered.outcome, Outcome::Running | Outcome::NeedsRecovery) { return Ok(Some(recovered)); }
    }
    Ok(None)
}

fn reconcile_start(backend: &mut Backend, seat: &SeatId) -> Result<(), BackendError> {
    if let Some(record) = recover_incomplete(backend, seat)? { return Err(receipt_error(record)); }
    // ContractStore compensation already restores a restart's parent or removes
    // a failed new seat. Re-classify that authority; never synthesize a resume.
    Ok(())
}

fn finish_receipt(backend: &mut Backend, record: OperationRecord) -> Result<OperationRecord, BackendError> {
    finish_receipt_until(backend, record, None)
}

fn finish_receipt_until(backend: &mut Backend, record: OperationRecord, deadline: Option<std::time::Instant>) -> Result<OperationRecord, BackendError> {
    if record.target.identity.scope != backend.binding.scope
        || record.parent.as_ref().is_some_and(|parent| parent.identity.scope != backend.binding.scope) {
        return Err(BackendError::Fence { reason: "recovery_scope_mismatch" });
    }
    let _target = native_seat_lock(&backend.binding.workspace.path, &backend.binding.team, &record.target.identity.seat, true)?;
    let _parent = record.parent.as_ref().filter(|parent| parent.identity.seat != record.target.identity.seat)
        .map(|parent| native_seat_lock(&backend.binding.workspace.path, &backend.binding.team, &parent.identity.seat, true)).transpose()?;
    let selected = registry::provider(record.target.provider.as_str())?;
    let adapter = adapter(&backend.binding, &record.target.identity, &record.target.provider)?;
    let hooks = adapter.hooks();
    let clock = RealClock::new();
    let mut host = PhysicalRuntime::new(Registration { descriptor: selected.descriptor(), hooks: &hooks,
        controls: selected.controls(), startup: selected.startup(), session_panel: selected.session_panel() },
        settings(&backend.binding, selected), record.id.clone(), &clock)?;
    let executing_candidate = executing_candidate_sha()?;
    host.executing_candidate = Some(executing_candidate);
    host.evidence = policy_evidence(&record.target.provider, &record.target.native, executing_candidate)?;
    if let Some(deadline) = deadline { host.cap_budget(remaining_stop_budget(deadline)?)?; }
    Ok(Lifecycle { store: &mut backend.store, host: &mut host, io: &mut NoNewIo }.recover(&record.id)?)
}

/// Read-only absence proof for an unmaterialized declaration. Existing native
/// provenance fails closed; missing authority never permits creating a new scope.
pub(crate) fn seat_authority_absent(workspace: &Path, team: &str, seat: &SeatId) -> Result<bool, BackendError> {
    let cwd = resolve_cwd(workspace)?;
    let scope = scope_for(&cwd, team)?;
    let root = root_for(&cwd.path, &scope);
    if !root.exists() { return Ok(true); }
    let backend = match Backend::open_readonly(workspace, team) {
        Ok(backend) => backend,
        Err(error @ BackendError::AuthorityLost { .. }) => {
            let selected = crate::state::selector::resolve_active_team_readonly(
                workspace, Some(team), crate::state::selector::SelectorMode::RuntimeOnly,
            ).map_err(|_| error)?;
            let row = &selected.state["agents"][seat.as_str()];
            if row.is_object() && !super::framework::is_contract(row)
                && ["contract_instance", "native_pane", "native_endpoint"].iter().all(|key| row[*key].is_null())
            { return Ok(true); }
            if selected.state["agents"].as_object().is_some_and(|agents| !agents.contains_key(seat.as_str())) {
                // write_role precedes native spawn, including a spawn whose row
                // was never projected. Match this seat, not a shared ID prefix.
                let legacy = format!("role-{}.json", seat.as_str());
                let prefix = format!("role-{}-instance-", seat.as_str());
                let mut witnessed = false;
                for entry in std::fs::read_dir(&root)? {
                    let name = entry?.file_name();
                    let name = name.to_string_lossy();
                    if name == legacy || name.strip_prefix(&prefix).and_then(|tail| tail.strip_suffix(".json"))
                        .is_some_and(|nonce| nonce.len() == 64 && nonce.bytes().all(|byte| byte.is_ascii_hexdigit()))
                    {
                        witnessed = true;
                        break;
                    }
                }
                if !witnessed { return Ok(true); }
            }
            return Err(BackendError::AuthorityLost { root: read_binding(workspace, team)?.transport_root });
        }
        Err(error) => return Err(error),
    };
    Ok(existing_start(&backend, seat)?.is_none() && !backend.store.leased(seat)?)
}

/// No creation receipt, unfinished lease or seat exists for this ID.
pub(super) fn seat_unclaimed(backend: &Backend, seat: &SeatId) -> Result<bool, BackendError> {
    Ok(existing_start(backend, seat)?.is_none())
}

/// Team-level forwarding peers (legacy members and the leader) that every new
/// native seat's framework route depends on. Shared by fresh start and fork.
pub(super) fn publish_framework_routes(backend: &mut Backend, members: &[MemberRoute]) -> Result<(), BackendError> {
    publish_framework_routes_kept(backend, members, None)
}
fn publish_framework_routes_kept(backend: &mut Backend, members: &[MemberRoute], keep: Option<&team_agent_contract::orchestration::forward::FrameworkPeer>) -> Result<(), BackendError> {
    let route = backend.binding.framework().route()?;
    let mut peers = Vec::new();
    for member in members.iter().filter(|member| member.runtime == RuntimeFamily::Legacy) {
        // Admitted native seats remain native even when their editable role
        // files use a legacy provider alias. Do not publish them as peers.
        if keep.is_some_and(|peer| peer.recipient == member.id)
            || backend.store.seat(&SeatId::new(member.id.clone())?)?.is_some()
            || backend.store.leased(&SeatId::new(member.id.clone())?)? {
            continue;
        }
        peers.push(team_agent_contract::orchestration::forward::FrameworkPeer {
            recipient: member.id.clone(),
            route: route.clone(),
            provider: member.provider.clone(),
        });
    }
    peers.push(team_agent_contract::orchestration::forward::FrameworkPeer {
        recipient: "leader".into(),
        route: route.clone(),
        provider: "framework".into(),
    });
    if let Some(peer) = keep { peers.push(peer.clone()); }
    backend.store.set_framework_routes(&peers, Some(&route))?;
    Ok(())
}

/// The native session ID a launch names before spawn, for the leaves whose
/// fresh session is `Preassigned`. Shared by fresh start and a native fork's child.
pub(super) fn preassign_session(
    request: &mut LaunchRequest,
    discovered: &Discovery,
) -> Result<(), BackendError> {
    if matches!(&discovered.runtime, Some(registry::NativeRuntime::Claude(_))) {
        request.preassigned_session = Some(team_agent_contract::providers::claude::session_id(&request.identity));
    }
    if matches!(&discovered.runtime, Some(registry::NativeRuntime::Cursor(_))) {
        request.preassigned_session = Some(team_agent_contract::providers::cursor::session_id(&request.identity));
    }
    Ok(())
}

/// Fresh startup is distinct from resume. An existing generation is never
/// silently replaced with a new native session.
pub fn start(
    workspace: &Path,
    team: &str,
    role: &RoleConfig,
    members: &[MemberRoute],
) -> Result<(Binding, OperationRecord), BackendError> {
    start_with_peer_transfer(workspace, team, role, members, false)
}

pub(crate) fn start_with_peer_transfer(
    workspace: &Path, team: &str, role: &RoleConfig, members: &[MemberRoute], transfer_peer: bool,
) -> Result<(Binding, OperationRecord), BackendError> {
    let _ownership = native_seat_lock(workspace, team, &role.id, false)?;
    let cwd = resolve_cwd(workspace)?;
    let scope = scope_for(&cwd, team)?;
    if root_for(&cwd.path, &scope).exists() {
        let mut backend = Backend::open(&cwd.path, team)?;
        reconcile_start(&mut backend, &role.id)?;
        match existing_start(&backend, &role.id)? {
            Some(ExistingStart::Committed(record)) => {
                let current = backend.store.assert_current(&record.target.identity)?;
                if current.physical.as_ref().is_some_and(|target|
                    matches!(
                        team_agent_contract::host::process::sample_process(&target.process),
                        team_agent_contract::host::process::ProcessState::Exited | team_agent_contract::host::process::ProcessState::Replaced)) {
                    let prepared = prepare_resume(workspace, team, &role.id, Some(role))?;
                    return Ok((backend.binding, resume(prepared)?));
                }
                validate_existing(&backend, role, &record)?;
                return Ok((backend.binding, record));
            },
            Some(ExistingStart::FinishReceipt(record)) => {
                validate_existing(&backend, role, &record)?;
                let record = finish_receipt(&mut backend, record)?;
                return Ok((backend.binding, record));
            },
            Some(ExistingStart::Resume(current)) => {
                if current.provider != role.provider { return Err(BackendError::Fence { reason: "resume_provider_mismatch" }); }
                let prepared = prepare_resume(workspace, team, &role.id, Some(role))?;
                return Ok((backend.binding, resume(prepared)?));
            },
            Some(ExistingStart::Pending(record) | ExistingStart::Blocked(record)) => return Err(receipt_error(record)),
            None => {},
        }
    }
    let selected = registry::provider(role.provider.as_str())?;
    let descriptor = selected.descriptor();
    let discovered = discovery::discover_for_start(&role.provider, workspace)?;
    let cwd = resolve_cwd(workspace)?;
    let scope = scope_for(&cwd, team)?;
    let candidate = std::env::current_exe()?.canonicalize()?;
    let tmux = discovery::public_command("tmux")?.entry;
    let candidate_sha256 =
        fingerprint_file(&candidate, 1024 * 1024 * 1024, Duration::from_secs(10))?;
    let provisional = Binding {
        version: 1,
        workspace: cwd.clone(),
        team: team.into(),
        scope: scope.clone(),
        endpoint: endpoint(&scope),
        candidate: candidate.clone(),
        candidate_sha256,
        tmux: tmux.clone(),
        transport_root: PathBuf::from("/tmp/preflight-no-write"),
    };
    let provisional_identity = InstanceIdentity {
        scope: scope.clone(),
        seat: role.id.clone(),
        instance: InstanceId::new("preflight")?,
        generation: Generation(1),
    };
    let probe_adapter = launch_adapter(&provisional, &provisional_identity, &role.provider, &discovered)?;
    let probe_hooks = probe_adapter.hooks();
    let preflight = request(&provisional, role, provisional_identity, &discovered)?;
    let preflight = resolve_launch_preflight(
        descriptor,
        &probe_hooks,
        &preflight,
        Some(&discovered.catalog),
    )?;
    let preflight_plan = probe_adapter.plan(&preflight)?;
    validate_launch_plan(descriptor, &preflight, &preflight_plan)?;
    let mut backend = if root_for(&cwd.path, &scope).exists() {
        Backend::open(&cwd.path, team)?
    } else {
        Backend::create(cwd, team, candidate, tmux)?
    };
    if existing_start(&backend, &role.id)?.is_some() {
        return Err(Error::Conflict.into());
    }
    let peer = backend.store.framework_peer(&role.id)?;
    // Preserve the route until F0 atomically acquires the seat and its rollback receipt.
    let members: Vec<_> = members.iter().filter(|member| member.id != role.id.as_str()).cloned().collect();
    publish_framework_routes_kept(&mut backend, &members, peer.as_ref())?;
    let identity = backend.store.propose_identity(&role.id)?;
    let server_key = selected.server_key_for(&identity)?;
    write_role(&backend.binding, &identity, role)?;
    let adapter = launch_adapter(&backend.binding, &identity, &role.provider, &discovered)?;
    let hooks = adapter.hooks();
    let mut request = request(&backend.binding, role, identity, &discovered)?;
    request.paths.cwd = super::workspace::bind_cwd(&backend.binding, selected, &role.id, None)?;
    preassign_session(&mut request, &discovered)?;
    let resolved = resolve_launch(
        descriptor,
        &hooks,
        &request,
        Some(&discovered.catalog),
    )?;
    let plan = adapter.plan(&resolved)?;
    validate_launch_plan(descriptor, &resolved, &plan)?;
    let _session_creation = workspace_session_lock(&backend.binding)?;
    let (workspace_topology, _, _) = workspace_tmux(&backend.binding, &role.id, None)?;
    let operation = OperationId::new(format!("start-{}", request.identity.instance.as_str()))?;
    let mut io = ScopedMaterializer::new(
        descriptor,
        &resolved,
        &plan,
        operation.clone(),
        RealCommandRunner::default(),
    )?;
    let clock = RealClock::new();
    let mut physical = PhysicalRuntime::new(
        Registration {
            descriptor,
            hooks: &hooks,
            controls: selected.controls(),
            startup: selected.startup(),
            session_panel: selected.session_panel(),
        },
        settings(&backend.binding, selected),
        operation.clone(),
        &clock,
    )?;
    physical.executing_candidate = Some(candidate_sha256);
    physical.evidence = policy_evidence(&role.provider, &request.native, candidate_sha256)?;
    physical.workspace_tmux = Some(workspace_topology);
    let adapter = Adapter { descriptor, hooks: &hooks, catalog: Some(&discovered.catalog) };
    let routing = Routing {
        pane: "unpublished".into(),
        binding_key: format!("binding-{}", request.identity.instance.as_str()),
        server_key: server_key.into(),
    };
    let mut lifecycle = Lifecycle { store: &mut backend.store, host: &mut physical, io: &mut io };
    let record = if let Some(peer) = peer.as_ref().filter(|_| transfer_peer) {
        lifecycle.startup_replacing_peer(&adapter, &request, routing, operation, peer)?
    } else {
        lifecycle.startup(&adapter, &request, routing, operation)?
    };
    if record.outcome == Outcome::Committed && record.target.physical.is_some() {
        bind_workspace_session(&backend.binding.workspace.path, &backend.binding.team, &record.target.identity.seat)?;
    }
    Ok((backend.binding, record))
}

/// A fully admitted resume proposal. It is still effect-free: no parent stop,
/// session allocation, snapshot write or config replacement has occurred.
pub struct PreparedResume {
    binding: Binding,
    role: RoleConfig,
    discovered: Discovery,
    request: LaunchRequest,
    parent: InstanceIdentity,
    original_session: Option<ResumeBinding>,
}

impl PreparedResume {
    pub(crate) fn seat(&self) -> &SeatId { &self.request.identity.seat }
    pub(crate) fn session(&self) -> Option<&str> {
        self.request.resume.as_ref().map(|resume| resume.binding.native_session.as_str())
    }
    pub(crate) fn native_launch(&self) -> Operation { self.request.operation }
}

pub(crate) fn prepare_restart(
    workspace: &Path,
    team: &str,
    directory: &Path,
    state: &serde_json::Value,
) -> Result<Vec<PreparedResume>, BackendError> {
    let config = super::framework::team_config(workspace, directory, team, None)?.ok_or(BackendError::Metadata)?;
    let mut prepared = Vec::new();
    let agents = state.get("agents").and_then(serde_json::Value::as_object).ok_or(BackendError::Metadata)?;
    for (id, _) in agents.iter().filter(|(_, row)| {
        super::framework::is_contract(row)
            && row["paused"] != true
            && !row["status"].as_str().is_some_and(|status| status.eq_ignore_ascii_case("paused"))
            && !crate::state::projection::agent_stopped_for_restart(row)
    }) {
        let Some(role) = config.roles.iter().find(|role| role.id.as_str() == id) else {
            // Removed roles are not silently re-created from an old snapshot.
            continue;
        };
        if role.paused { continue; }
        // The native store is authoritative even if its framework mirror is
        // stale. Only explicit start-agent may resume a stopped generation.
        let backend = Backend::open_readonly(workspace, team)?;
        let seat = backend.store.seat(&role.id)?.ok_or(BackendError::Metadata)?;
        if seat.status == SeatStatus::Stopped { continue; }
        prepared.push(prepare_resume(workspace, team, &role.id, Some(role))?);
    }
    Ok(prepared)
}

/// Metadata-only availability check: missing/empty is not resumable; an I/O or
/// identity error is not evidence of absence. Never read private transcript text.
fn resumable_session(parent: &SeatRecord, home: Option<&Path>) -> Result<Option<ResumeBinding>, BackendError> {
    let Some(source) = parent.session.as_ref() else { return Ok(None); };
    team_agent_contract::contract::session::validate_resume(source,
        &team_agent_contract::contract::session::ResumeExpectation {
            provider: &parent.provider, source: &parent.identity, cwd: &parent.cwd, native: &parent.native,
            evidence_kind: parent.evidence_kind, mode: team_agent_contract::contract::descriptor::ResumeMode::ExactId,
        })?;
    if parent.provider.as_str() == "claude_tui" {
        let path = team_agent_contract::providers::claude::transcript_path(
            home.ok_or(ContractError::Invalid("Claude native home unavailable"))?, source)?;
        match std::fs::metadata(path) {
            Ok(metadata) if metadata.is_file() && metadata.len() > 0 => {},
            Ok(metadata) if metadata.is_file() => return Ok(None),
            Ok(_) => return Err(ContractError::Invalid("Claude transcript is not a file").into()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        }
    }
    Ok(Some(source.clone()))
}

fn resumable_session_in_scope(binding: &Binding, parent: &SeatRecord, home: Option<&Path>) -> Result<Option<ResumeBinding>, BackendError> {
    resumable_session(parent, home)?.map(|mut source| {
        source.cwd = super::workspace::current_cwd(binding, &source.cwd)?;
        Ok(source)
    }).transpose()
}

pub fn prepare_resume(
    workspace: &Path,
    team: &str,
    seat: &SeatId,
    role: Option<&RoleConfig>,
) -> Result<PreparedResume, BackendError> {
    let backend = Backend::open_readonly(workspace, team)?;
    let parent = backend.store.seat(seat)?.ok_or(BackendError::Metadata)?;
    if parent.status == SeatStatus::Unknown {
        return Err(BackendError::Fence { reason: "resume_parent_unknown" });
    }
    let role = match role {
        Some(role) => role.clone(),
        None => read_role(&backend.binding, &parent.identity)?,
    };
    if role.id != parent.identity.seat || role.provider != parent.provider {
        return Err(ContractError::Mismatch("resume role provider").into());
    }
    super::workspace::current_cwd(&backend.binding, &parent.cwd)?;
    let discovered = discovery::discover_runtime(&role.provider, workspace)?;
    let identity = backend.store.propose_identity(seat)?;
    let adapter = launch_adapter(&backend.binding, &identity, &role.provider, &discovered)?;
    let hooks = adapter.hooks();
    let source = resumable_session_in_scope(&backend.binding, &parent, discovered.runtime.as_ref().and_then(|runtime| match runtime {
        registry::NativeRuntime::Claude(runtime) => Some(runtime.home()), _ => None,
    }))?;
    let mut request = request(&backend.binding, &role, identity, &discovered)?;
    let selected = registry::provider(role.provider.as_str())?;
    request.paths.cwd = super::workspace::bind_cwd(&backend.binding, selected, &role.id,
        source.as_ref().map(|_| &parent.cwd))?;
    if let Some(source) = source {
        request.operation = Operation::Resume;
        request.resume = Some(ResumeRequest { binding: source, expected_source: parent.identity.clone() });
    } else if matches!(&discovered.runtime, Some(registry::NativeRuntime::Claude(_))) {
        // Pure identity-derived allocation, as in normal Fresh startup. Never run
        // Cursor's native allocation command during this read-only preflight.
        preassign_session(&mut request, &discovered)?;
    }
    let resolved = resolve_launch(selected.descriptor(), &hooks, &request, Some(&discovered.catalog))?;
    validate_launch_plan(selected.descriptor(), &resolved, &adapter.plan(&resolved)?)?;
    Ok(PreparedResume { binding: backend.binding, role, discovered, request,
        parent: parent.identity, original_session: parent.session })
}

pub fn resume(prepared: PreparedResume) -> Result<OperationRecord, BackendError> {
    let _ownership = native_seat_lock(&prepared.binding.workspace.path, &prepared.binding.team, &prepared.role.id, false)?;
    let mut backend = Backend::open(&prepared.binding.workspace.path, &prepared.binding.team)?;
    if serde_json::to_vec(&backend.binding).map_err(|_| BackendError::Metadata)?
        != serde_json::to_vec(&prepared.binding).map_err(|_| BackendError::Metadata)?
    {
        return Err(BackendError::Fence { reason: "resume_scope_binding_changed" });
    }
    let parent = backend.store.assert_current(&prepared.parent)?;
    if parent.session != prepared.original_session {
        return Err(BackendError::Fence { reason: "resume_session_changed" });
    }
    if resumable_session_in_scope(&backend.binding, &parent, prepared.discovered.runtime.as_ref().and_then(|runtime| match runtime {
        registry::NativeRuntime::Claude(runtime) => Some(runtime.home()), _ => None,
    }))?
        != prepared.request.resume.as_ref().map(|resume| resume.binding.clone()) {
        return Err(BackendError::Fence { reason: "resume_availability_changed" });
    }
    let selected = registry::provider(prepared.role.provider.as_str())?;
    let descriptor = selected.descriptor();
    let adapter = launch_adapter(&backend.binding, &prepared.request.identity, &prepared.role.provider, &prepared.discovered)?;
    let hooks = adapter.hooks();
    // Revalidate the exact plan before publishing the generation snapshot.
    let resolved = resolve_launch(descriptor, &hooks, &prepared.request, Some(&prepared.discovered.catalog))?;
    let plan = adapter.plan(&resolved)?;
    validate_launch_plan(descriptor, &resolved, &plan)?;
    write_role(&backend.binding, &prepared.request.identity, &prepared.role)?;
    let shared_parent = parent.physical.as_ref().is_some_and(|target| target.shared_endpoint);
    let _session_creation = workspace_session_lock(&backend.binding)?;
    let workspace_topology = if shared_parent {
        // Validate root topology before stopping. Capture again only after the
        // authorized stop, since its last pane may destroy the old session ID.
        let (before, _, _) = workspace_tmux(&backend.binding, &prepared.role.id, None)?;
        if parent.status != SeatStatus::Stopped {
            let target = parent.physical.as_ref().ok_or(BackendError::Metadata)?;
            // Current topology is owner-validated above. An absent native
            // process can resume there without requiring its former session.
            let native_absent = matches!(team_agent_contract::host::process::sample_process(&target.process),
                team_agent_contract::host::process::ProcessState::Exited | team_agent_contract::host::process::ProcessState::Replaced);
            if !native_absent && (before.endpoint != target.endpoint
                || before.session.as_deref() != Some(target.session.as_str()))
            {
                return Err(ContractError::Mismatch("resume workspace session").into());
            }
            let stopped = stop(&backend.binding.workspace.path, &backend.binding.team, &prepared.role.id)?;
            if stopped.outcome != Outcome::Committed {
                return Err(receipt_error(stopped));
            }
        }
        Some(workspace_tmux(&backend.binding, &prepared.role.id, None)?.0)
    } else if parent.status == SeatStatus::Stopped {
        // A stopped pre-shared seat has no server to preserve. Launch its next
        // generation on the current workspace endpoint, not a socket beneath
        // the relocated (potentially long) authority path.
        Some(workspace_tmux(&backend.binding, &prepared.role.id, None)?.0)
    } else { None };
    let operation = OperationId::new(format!("resume-{}", prepared.request.identity.instance.as_str()))?;
    let mut io = ScopedMaterializer::new(descriptor, &resolved, &plan, operation.clone(), RealCommandRunner::default())?;
    let clock = RealClock::new();
    let mut physical = PhysicalRuntime::new(
        Registration {
            descriptor, hooks: &hooks, controls: selected.controls(),
            startup: selected.startup(), session_panel: selected.session_panel(),
        },
        settings(&backend.binding, selected), operation.clone(), &clock,
    )?;
    let executing_candidate = executing_candidate_sha()?;
    physical.executing_candidate = Some(executing_candidate);
    physical.evidence = policy_evidence(&prepared.role.provider, &prepared.request.native, executing_candidate)?;
    physical.workspace_tmux = workspace_topology;
    let mut lifecycle = Lifecycle { store: &mut backend.store, host: &mut physical, io: &mut io };
    let adapter = Adapter { descriptor, hooks: &hooks, catalog: Some(&prepared.discovered.catalog) };
    let routing = Routing {
        pane: "unpublished".into(), binding_key: format!("binding-{}", prepared.request.identity.instance.as_str()),
        server_key: selected.server_key_for(&prepared.request.identity)?,
    };
    let record = if prepared.request.operation == Operation::Fresh {
        lifecycle.restart_fresh(&adapter, &prepared.request, routing, operation, &prepared.parent)?
    } else {
        lifecycle.startup(&adapter, &prepared.request, routing, operation)?
    };
    if record.outcome == Outcome::Committed && record.target.physical.is_some() {
        bind_workspace_session(&backend.binding.workspace.path, &backend.binding.team, &record.target.identity.seat)?;
    }
    Ok(record)
}

fn role_path(binding: &Binding, identity: &InstanceIdentity) -> PathBuf {
    root_for(&binding.workspace.path, &binding.scope).join(format!(
        "role-{}-{}.json", identity.seat.as_str(), identity.instance.as_str(),
    ))
}
pub(super) fn write_role(binding: &Binding, identity: &InstanceIdentity, role: &RoleConfig) -> Result<(), BackendError> {
    use std::os::unix::fs::OpenOptionsExt;
    if identity.scope != binding.scope || identity.seat != role.id {
        return Err(BackendError::Metadata);
    }
    // Each generation has an immutable source; an old stopped generation's
    // model/instructions never change when a new role is admitted.
    let snapshot = serde_json::to_vec(role).map_err(|_| BackendError::Metadata)?;
    let mut file = std::fs::OpenOptions::new().write(true).create_new(true)
        .mode(0o600).custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(role_path(binding, identity))?;
    file.write_all(&snapshot)?;
    file.sync_all()?;
    Ok(())
}
pub(crate) fn read_role(binding: &Binding, identity: &InstanceIdentity) -> Result<RoleConfig, BackendError> {
    let mut path = role_path(binding, identity);
    // Existing generation-one deployments retain their immutable source.
    if identity.generation == Generation(1) {
        match std::fs::symlink_metadata(&path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                path = root_for(&binding.workspace.path, &binding.scope)
                    .join(format!("role-{}.json", identity.seat.as_str()));
            }
            Err(error) => return Err(error.into()),
            Ok(_) => {},
        }
    }
    let metadata = std::fs::symlink_metadata(&path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(BackendError::Metadata);
    }
    let mut bytes = Vec::new();
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    let file = std::fs::OpenOptions::new().read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC).open(path)?;
    let opened = file.metadata()?;
    if metadata.dev() != opened.dev() || metadata.ino() != opened.ino() {
        return Err(BackendError::Metadata);
    }
    file.take(4 * 1024 * 1024 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > 4 * 1024 * 1024 {
        return Err(BackendError::Metadata);
    }
    let role: RoleConfig = serde_json::from_slice(&bytes).map_err(|_| BackendError::Metadata)?;
    if role.id != identity.seat || identity.scope != binding.scope
        || registry::descriptor(role.provider.as_str())
            .is_none_or(|descriptor| descriptor.identity.id != role.provider.as_str())
        || digest(role.prompt.text.as_bytes()) != role.prompt.sha256
    {
        return Err(BackendError::Metadata);
    }
    Ok(role)
}

pub(super) fn policy_evidence(
    provider: &ProviderId,
    native: &NativeIdentity,
    candidate_sha256: Digest,
) -> Result<Vec<CapabilityEvidence>, ContractError> {
    let descriptor = registry::provider(provider.as_str())?.descriptor();
    let profiles = descriptor.input.profiles.require("native input profiles")?;
    let mut evidence = Vec::new();
    for profile in profiles.iter().filter(|profile| profile.matches_native(native)) {
        // Unknown submit recipes never produce Native evidence, even when the
        // provider has a readable catalog or a recognizable TUI surface.
        profile.policy.require("native submit policy")?;
        let binding_sha256 = digest(
            format!(
                "{}-runtime-policy-v1\n{native:?}\n{}\n{:?}\n{candidate_sha256:?}",
                descriptor.identity.id, profile.id, profile.policy_sha256,
            ).as_bytes(),
        );
        evidence.extend(profile.operations.iter().map(|operation| CapabilityEvidence {
            provider: provider.clone(),
            native: native.clone(),
            profile_id: profile.id.into(),
            policy_sha256: profile.policy_sha256,
            channel: profile.channel,
            operation: *operation,
            kind: EvidenceKind::Native,
            candidate_sha256,
            evidence_sha256: binding_sha256,
        }));
    }
    Ok(evidence)
}

struct NoNewIo;
impl OwnedIo for NoNewIo {
    fn create_exclusive(
        &mut self,
        _: &OwnedResourceRequest,
    ) -> Result<OwnedResourceReceipt, PartialFailure> {
        Err(PartialFailure {
            error: ContractError::Invalid("teardown cannot materialize"),
            receipt: MaterializeReceipt {
                resources: Vec::new(),
            },
        })
    }
    fn read_bound_session(
        &mut self,
        _: &ResumeBinding,
        _: ReadBounds,
    ) -> Result<ReadOutput, ReadFailure> {
        Err(ReadFailure::Error(ContractError::Invalid(
            "teardown cannot read session backing",
        )))
    }
    fn validate_configuration(
        &mut self,
        _: &OwnedValidationRequest,
    ) -> Result<ReadOutput, ReadFailure> {
        Err(ReadFailure::Error(ContractError::Invalid(
            "teardown cannot run a validator",
        )))
    }
}

/// Retire a stopped seat's current identity. None: the store holds no seat
/// for this ID (never started, or already retired).
pub(crate) fn retire(workspace: &Path, team: &str, seat: &SeatId) -> Result<Option<SeatRecord>, BackendError> {
    let mut backend = Backend::open(workspace, team)?;
    let Some(current) = backend.store.seat(seat)? else {
        return Ok(None);
    };
    Ok(Some(backend.store.retire(&current.identity)?))
}

pub fn temporary_authority(binding: &Binding) -> bool {
    let path = &binding.transport_root;
    !path.starts_with(&binding.workspace.path)
        && (path.starts_with("/tmp") || path.starts_with("/private/tmp")
            || path.starts_with(std::env::temp_dir())
            || std::env::temp_dir().canonicalize().is_ok_and(|root| path.starts_with(root)))
}

/// Storage-location admission only; explicit consent and carrier/owner fences
/// remain mandatory in recovery. Never follow a scoped path out through a link.
pub(crate) fn fresh_recovery_allowed(binding: &Binding) -> bool {
    let root = root_for(&binding.workspace.path, &binding.scope);
    if !root.canonicalize().is_ok_and(|canonical| canonical == root) { return false; }
    let old = &binding.transport_root;
    let temporary = old.parent().is_some_and(|parent| {
        parent == Path::new("/tmp") || parent == Path::new("/private/tmp")
            || Path::new("/tmp").canonicalize().is_ok_and(|temp| parent == temp)
    }) && old.file_name().is_some_and(|name| name.to_string_lossy().starts_with("ta-"));
    if temporary { return true; }
    let Ok(relative) = old.strip_prefix(&root) else { return false; };
    if relative.as_os_str().is_empty()
        || !relative.components().all(|part| matches!(part, std::path::Component::Normal(_)))
    { return false; }
    let mut ancestor = root;
    for part in relative.components() {
        ancestor.push(part.as_os_str());
        match std::fs::symlink_metadata(&ancestor) {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return true,
            _ => return false,
        }
    }
    true
}

fn quiescent_authority(store: &ProjectionStore) -> Result<bool, BackendError> {
    if !store.unfinished()?.is_empty() { return Ok(false); }
    for seat in store.seats()? {
        if seat.status != SeatStatus::Stopped || store.leased(&seat.identity.seat)?
            || seat.physical.as_ref().is_some_and(|target| {
                !matches!(team_agent_contract::host::process::sample_process(&target.process),
                    team_agent_contract::host::process::ProcessState::Exited | team_agent_contract::host::process::ProcessState::Replaced)
            })
        { return Ok(false); }
    }
    Ok(true)
}

/// Keep the winning v1 binding byte-compatible with released binaries. Only
/// retire inactive siblings; never delete their stores or guess between live ones.
pub(crate) fn retire_inactive_siblings(binding: &Binding, state: &serde_json::Value) -> Result<(), BackendError> {
    use std::os::unix::fs::MetadataExt;
    let parent = root_for(&binding.workspace.path, &binding.scope).parent().ok_or(BackendError::Metadata)?.to_owned();
    for entry in std::fs::read_dir(parent)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() || entry.file_name() == binding.scope.as_str() { continue; }
        let path = entry.path().join("framework-binding.json");
        let sibling = match matching_binding(&path, &binding.workspace.path, &binding.team) {
            Ok(Some(sibling)) => sibling,
            Ok(None) => continue,
            Err(BackendError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        if sibling.workspace.path != binding.workspace.path || sibling.team != binding.team { continue; }
        crate::lifecycle::launch::ensure_owner_allowed_for_state(state, None)
            .map_err(|error| BackendError::Migration(error.to_string()))?;
        let metadata = entry.metadata()?;
        if sibling.version != 1 || entry.file_name() != sibling.scope.as_str()
            || sibling.endpoint != endpoint(&sibling.scope) || sibling.endpoint == binding.endpoint
            || metadata.uid() != unsafe { libc::geteuid() } || metadata.mode() & 0o777 != 0o700
        { return Err(BackendError::Metadata); }
        let directory = std::fs::File::open(entry.path())?;
        if directory.try_lock().is_err() { continue; }
        let absent = match check_authority(&sibling) {
            Err(BackendError::AuthorityLost { .. }) => true,
            Ok(()) => quiescent_authority(&ProjectionStore::open_readonly(
                &sibling.transport_root, sibling.scope.clone(), &sibling.endpoint)?)?,
            _ => false,
        };
        let carriers_absent = state["agents"].as_object().is_some_and(|agents| agents.iter().all(|(id, row)| {
            !super::framework::is_contract(row)
                || row["contract_scope"].as_str().is_some_and(|scope| scope != sibling.scope.as_str())
                || ["contract_instance", "native_endpoint", "native_pane"].iter().all(|key| row[*key].is_null())
                || discovery::public_command("tmux").is_ok_and(|tmux|
                    super::recovery::carrier_absent(id, row, &tmux.entry).is_ok())
        }));
        if !absent || !carriers_absent {
            eprintln!("warning: sibling native scope {} retained: authority/carrier activity is not excluded", sibling.scope.as_str());
            continue;
        }
        let latest = read_binding_file(&path)?;
        if serde_json::to_vec(&latest).map_err(|_| BackendError::Metadata)?
            != serde_json::to_vec(&sibling).map_err(|_| BackendError::Metadata)?
        { return Err(BackendError::Fence { reason: "sibling_binding_changed" }); }
        let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_err(|_| BackendError::Metadata)?.as_nanos();
        std::fs::rename(path, entry.path().join(format!("framework-binding.retired-{nonce}.json")))?;
        directory.sync_all()?;
    }
    Ok(())
}

/// Generated launchers use POSIX single-quoted values. Re-quote only complete
/// absolute path arguments; never substitute path-like text inside a prompt.
fn rebase_launch_script(bytes: &[u8], old: &Path, new: &Path) -> Result<Vec<u8>, BackendError> {
    let mut result = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'\'' { result.push(bytes[index]); index += 1; continue; }
        let start = index;
        index += 1;
        let mut value = Vec::new();
        while index < bytes.len() {
            if bytes[index..].starts_with(b"'\\''") { value.push(b'\''); index += 4; }
            else if bytes[index] == b'\'' { index += 1; break; }
            else { value.push(bytes[index]); index += 1; }
        }
        let relocated = std::str::from_utf8(&value).ok().and_then(|text| Path::new(text).strip_prefix(old).ok()).map(|relative| new.join(relative));
        if let Some(path) = relocated {
            result.extend(team_agent_contract::host::shell::quote(path.as_os_str())?);
        } else {
            result.extend_from_slice(&bytes[start..index]);
        }
    }
    Ok(result)
}

fn copy_authority(source: &Path, target: &Path, old_root: &Path, new_root: &Path) -> Result<(), BackendError> {
    use std::os::unix::fs::DirBuilderExt;
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        if source == old_root && ["projection", "contract.json", "contract.db", "contract.db-wal", "contract.db-shm"].iter().any(|name| entry.file_name() == *name) {
            continue;
        }
        let path = target.join(entry.file_name());
        let kind = entry.file_type()?;
        if kind.is_dir() {
            std::fs::DirBuilder::new().mode(0o700).create(&path)?;
            copy_authority(&entry.path(), &path, old_root, new_root)?;
        } else if kind.is_file() {
            std::fs::copy(entry.path(), &path)?;
            if entry.file_name() == "launch.sh" && source.parent() == Some(old_root)
                && source.file_name().is_some_and(|name| name.to_string_lossy().starts_with("native-"))
            {
                let bytes = rebase_launch_script(&std::fs::read(&path)?, old_root, new_root)?;
                let mut file = std::fs::OpenOptions::new().write(true).truncate(true).open(&path)?;
                file.write_all(&bytes)?;
            }
            std::fs::File::open(&path)?.sync_all()?;
        } else if kind.is_symlink() {
            let link = std::fs::read_link(entry.path())?;
            let link = link.strip_prefix(old_root).map_or_else(|_| link.clone(), |relative| new_root.join(relative));
            std::os::unix::fs::symlink(link, path)?;
        }
        // Dead Unix sockets are not authority and cannot be copied.
    }
    std::fs::File::open(target)?.sync_all()?;
    Ok(())
}

/// Rewrite path values, not substrings in prompts or native history. Stable CWD
/// evidence is renewed only after verifying the source incarnation we just copied.
fn rebase_value(value: &mut serde_json::Value, old: &Path, new: &Path) -> Result<(), Error> {
    use team_agent_contract::host::process::{cwd_matches, stable_cwd};
    if value.as_object().is_some_and(|object| object.len() == 2 && object.contains_key("path") && object.contains_key("identity")) {
        let mut cwd: CwdIdentity = serde_json::from_value(value.clone())?;
        if let Ok(relative) = cwd.path.strip_prefix(old) {
            let path = new.join(relative);
            if stable_cwd(&cwd) {
                if !cwd_matches(&cwd).map_err(Error::HostDiagnostic)? { return Err(Error::Fence); }
                cwd = resolve_cwd(&path).map_err(Error::HostDiagnostic)?;
            } else {
                cwd.path = path;
            }
            *value = serde_json::to_value(cwd)?;
            return Ok(());
        }
    }
    match value {
        serde_json::Value::String(text) => {
            if let Ok(relative) = Path::new(text).strip_prefix(old) { *text = new.join(relative).to_string_lossy().into_owned(); }
        }
        serde_json::Value::Array(values) => for value in values { rebase_value(value, old, new)?; },
        serde_json::Value::Object(values) => for (key, value) in values {
            if !matches!(key.as_str(), "failure" | "recovery_failure") { rebase_value(value, old, new)?; }
        },
        _ => {},
    }
    Ok(())
}

fn references_root(value: &serde_json::Value, root: &Path) -> bool {
    match value {
        serde_json::Value::String(text) => Path::new(text).starts_with(root),
        serde_json::Value::Array(values) => values.iter().any(|value| references_root(value, root)),
        serde_json::Value::Object(values) => values.iter().any(|(key, value)|
            !matches!(key.as_str(), "failure" | "recovery_failure") && references_root(value, root)),
        _ => false,
    }
}

fn rebase_json(path: &Path, old: &Path, new: &Path) -> Result<(), BackendError> {
    if !std::fs::symlink_metadata(path)?.file_type().is_file() { return Err(BackendError::Metadata); }
    let mut value: serde_json::Value = serde_json::from_reader(std::fs::File::open(path)?).map_err(|_| BackendError::Metadata)?;
    let fields: &[&str] = if path.file_name().is_some_and(|name| name == "legacy-import.json") {
        &["root", "seats", "operations", "retirements", "connections"]
    } else { &[] };
    if fields.is_empty() {
        rebase_value(&mut value, old, new).map_err(|error| BackendError::Continuity(error.to_string()))?;
        if references_root(&value, old) { return Err(BackendError::Metadata); }
    } else {
        for field in fields {
            if let Some(value) = value.get_mut(*field) {
                rebase_value(value, old, new).map_err(|error| BackendError::Continuity(error.to_string()))?;
                if references_root(value, old) { return Err(BackendError::Metadata); }
            }
        }
    }
    let mut file = std::fs::OpenOptions::new().write(true).truncate(true).open(path)?;
    file.write_all(&serde_json::to_vec(&value).map_err(|_| BackendError::Metadata)?)?;
    file.sync_all()?;
    Ok(())
}

/// Called under the scope directory lock; keep the original tree untouched.
fn relocate_authority(binding: &mut Binding, root: &Path) -> Result<bool, BackendError> {
    if !temporary_authority(binding) { return Ok(false); }
    let source = ProjectionStore::open_readonly(&binding.transport_root, binding.scope.clone(), &binding.endpoint)?;
    let seats = source.seats()?;
    if !quiescent_authority(&source)? {
        eprintln!("warning: temporary native authority {} retained while native seats are active; stop them and run team-agent migrate --team {}", binding.transport_root.display(), binding.team);
        return Ok(false);
    }
    let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_err(|_| BackendError::Metadata)?.as_nanos();
    let target = root.join(format!("authority-{}-{nonce}", std::process::id()));
    let copied = ProjectionStore::create(&target, binding.scope.clone(), &binding.endpoint)?;
    let old = &binding.transport_root;
    copy_authority(old, &target, old, &target)?;
    source.copy_projection_to(&copied, &mut |value| {
        rebase_value(value, old, &target)?;
        if references_root(value, old) { return Err(Error::Fence); }
        Ok(())
    }).map_err(|error| BackendError::Continuity(error.to_string()))?;
    // CWD records join the authority snapshot, so the one binding swap also
    // selects their new paths. Old scope-level records remain valid until then.
    for entry in std::fs::read_dir(root)? {
        let entry = entry?;
        let name = entry.file_name();
        if name.to_string_lossy().starts_with("cwd-") && name.to_string_lossy().ends_with(".json")
            && !name.to_string_lossy().contains(".pre-durable-")
        {
            if !entry.file_type()?.is_file() { return Err(BackendError::Metadata); }
            let destination = target.join(&name);
            if !destination.exists() { std::fs::copy(entry.path(), &destination)?; }
        }
    }
    for entry in std::fs::read_dir(&target)? {
        let entry = entry?;
        let name = entry.file_name();
        if name == "legacy-import.json" || (name.to_string_lossy().starts_with("cwd-") && name.to_string_lossy().ends_with(".json")) {
            rebase_json(&entry.path(), old, &target)?;
        }
    }
    let mut expected = serde_json::to_value(&seats).map_err(|_| BackendError::Metadata)?;
    rebase_value(&mut expected, old, &target).map_err(|error| BackendError::Continuity(error.to_string()))?;
    let copied = ProjectionStore::open_readonly(&target, binding.scope.clone(), &binding.endpoint)?;
    let actual = serde_json::to_value(copied.seats()?).map_err(|_| BackendError::Metadata)?;
    if actual != expected || references_root(&actual, old) || !copied.unfinished()?.is_empty() { return Err(BackendError::Metadata); }
    std::fs::File::open(&target)?.sync_all()?;
    binding.transport_root = target;
    Ok(true)
}

pub fn migrate_if_required(workspace: &Path, team: &str) -> Result<bool, BackendError> {
    let binding = match read_binding(workspace, team) {
        Ok(binding) => binding,
        Err(BackendError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    };
    if let Err(error @ BackendError::AuthorityLost { .. }) = check_authority(&binding) {
        return Err(error);
    }
    if temporary_authority(&binding) || !binding.transport_root.join("contract.json").exists() {
        migrate(workspace, team)?;
        return Ok(true);
    }
    Ok(false)
}

/// Explicit, scoped release cutover. Existing physical receipts remain intact;
/// future launches use the invoking candidate. No native body is replayed.
pub fn migrate(workspace: &Path, team: &str) -> Result<serde_json::Value, BackendError> {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    let selected = crate::state::selector::resolve_active_team_readonly(workspace, Some(team), crate::state::selector::SelectorMode::RuntimeOnly)
        .map_err(|error| BackendError::Migration(error.to_string()))?;
    crate::lifecycle::launch::ensure_owner_allowed_for_state(&selected.state, None)
        .map_err(|error| BackendError::Migration(error.to_string()))?;
    let workspace = selected.run_workspace.as_path();
    let team = selected.team_key.as_str();
    crate::coordinator::retire_legacy_coordinator(workspace).map_err(BackendError::Migration)?;
    let mut binding = match read_binding(workspace, team) {
        Ok(binding) => binding,
        Err(BackendError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            let cwd = resolve_cwd(workspace)?;
            let root = root_for(&cwd.path, &scope_for(&cwd, team)?);
            if root.try_exists()? {
                return Err(BackendError::Migration(
                    "native binding is missing; existing authority was retained, not reset".into(),
                ));
            }
            return Ok(serde_json::json!({"ok":true,"status":"not_required", "team":team,
                "reason":"no_native_contract_state", "legacy_state_preserved":true,"replayed":0}));
        }
        Err(error) => return Err(error),
    };
    let root = root_for(&binding.workspace.path, &binding.scope);
    if root.canonicalize()? != root { return Err(BackendError::Fence { reason: "migration_root_alias" }); }
    let directory = std::fs::File::open(&root)?;
    match directory.try_lock() {
        Ok(()) => {},
        Err(std::fs::TryLockError::WouldBlock) if temporary_authority(&binding) => {
            let warning = format!("temporary authority {} migration deferred: native operations are active; retry team-agent migrate --team {}", binding.transport_root.display(), team);
            eprintln!("warning: {warning}");
            return Ok(serde_json::json!({"ok":true,"status":"deferred","warning":warning}));
        }
        Err(std::fs::TryLockError::WouldBlock) => return Err(BackendError::Fence { reason: "authority_cutover_busy" }),
        Err(std::fs::TryLockError::Error(error)) => return Err(error.into()),
    }
    // Re-read after acquiring the cutover lock so a concurrent invocation is
    // idempotent and cannot restore an older candidate binding.
    binding = read_binding(workspace, team)?;
    match check_authority(&binding) {
        Ok(()) | Err(BackendError::Migration(_)) => {},
        Err(error) => return Err(error),
    }
    let mut result = crate::importer::contract::migrate(&binding.transport_root, binding.scope.clone(), &binding.endpoint)
        .map_err(|error| BackendError::Migration(error.to_string()))?;
    let candidate = std::env::current_exe()?.canonicalize()?;
    let fingerprint = fingerprint_file(&candidate, 1024 * 1024 * 1024, Duration::from_secs(10))?;
    let relocated = relocate_authority(&mut binding, &root)?;
    if binding.candidate != candidate || binding.candidate_sha256 != fingerprint || relocated {
        let path = root.join("framework-binding.json");
        let metadata = std::fs::symlink_metadata(&path)?;
        if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.uid() != unsafe { libc::geteuid() }
            || metadata.nlink() != 1 || metadata.mode() & 0o022 != 0 { return Err(BackendError::Fence { reason: "migration_binding_ownership" }); }
        let before = std::fs::read(&path)?;
        binding.candidate = candidate;
        binding.candidate_sha256 = fingerprint;
        let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_err(|_| BackendError::Metadata)?.as_nanos();
        if relocated {
            let archive = root.join(format!("framework-binding.pre-durable-{nonce}.json"));
            let mut saved = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(archive)?;
            saved.write_all(&before)?;
            saved.sync_all()?;
        }
        let temporary = root.join(format!("migration-binding-{}-{nonce}.tmp", std::process::id()));
        let mut file = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&temporary)?;
        let written = (|| -> Result<(), BackendError> {
            file.write_all(&serde_json::to_vec(&binding).map_err(|_| BackendError::Metadata)?)?;
            file.sync_all()?;
            if std::fs::read(&path)? != before { return Err(BackendError::Fence { reason: "migration_binding_changed" }); }
            std::fs::rename(&temporary, &path)?;
            directory.sync_all()?;
            Ok(())
        })();
        if written.is_err() { let _ = std::fs::remove_file(&temporary); }
        written?;
    }
    if relocated {
        for entry in std::fs::read_dir(&binding.transport_root)? {
            let name = entry?.file_name();
            if name.to_string_lossy().starts_with("cwd-") && name.to_string_lossy().ends_with(".json") {
                let old_record = root.join(&name);
                if old_record.exists() {
                    let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_err(|_| BackendError::Metadata)?.as_nanos();
                    std::fs::rename(&old_record, root.join(format!("pre-durable-{nonce}-{}", name.to_string_lossy())))?;
                    let staged = root.join(format!("cwd-publish-{nonce}.tmp"));
                    std::fs::copy(binding.transport_root.join(&name), &staged)?;
                    std::fs::File::open(&staged)?.sync_all()?;
                    std::fs::rename(staged, old_record)?;
                }
            }
        }
        directory.sync_all()?;
    }
    retire_inactive_siblings(&binding, &selected.state)?;
    let authority = Backend::open_readonly(&binding.workspace.path, team)?;
    result["seat_status"] = serde_json::json!(authority.store.seats()?.into_iter().map(|seat| {
        serde_json::json!({"seat":seat.identity.seat,"candidate":"current",
            "live_foreign":false,"next_command":null})
    }).collect::<Vec<_>>());
    result["transport_root"] = serde_json::json!(binding.transport_root);
    if temporary_authority(&binding) {
        result["warning"] = serde_json::json!("temporary authority migration deferred: stop native seats and retry team-agent migrate");
    }
    result["team"] = serde_json::json!(team);
    result["candidate"] = serde_json::json!(binding.candidate);
    result["maintenance"] = serde_json::json!("Use only this release for subsequent lifecycle operations");
    Ok(result)
}

pub fn stop(workspace: &Path, team: &str, seat: &SeatId) -> Result<OperationRecord, BackendError> {
    stop_with_budget(workspace, team, seat, Duration::from_secs(20))
}

fn remaining_stop_budget(deadline: std::time::Instant) -> Result<Duration, BackendError> {
    let remaining = deadline.saturating_duration_since(std::time::Instant::now());
    if remaining.is_zero() { return Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "native stop deadline exhausted").into()); }
    Ok(remaining)
}

pub(super) fn stop_with_budget(workspace: &Path, team: &str, seat: &SeatId, budget: Duration) -> Result<OperationRecord, BackendError> {
    let deadline = std::time::Instant::now() + budget;
    remaining_stop_budget(deadline)?;
    migrate_if_required(workspace, team)?;
    let _ownership = native_seat_lock(workspace, team, seat, false)?;
    let mut backend = Backend::open(workspace, team)?;
    if let Some(record) = recover_incomplete_until(&mut backend, seat, Some(deadline))? { return Ok(record); }
    let Some(target) = backend.store.seat(seat)? else {
        // Compensation can remove an interrupted startup's provisional seat.
        // Its durable cleanup receipt is an honest already-absent stop result.
        return backend.store.creation_receipt(seat, None)?
            .filter(|record| record.outcome == Outcome::Compensated && record.target.status == SeatStatus::Stopped)
            .ok_or_else(|| Error::Invalid("unknown native seat").into());
    };
    let nonce = backend.store.propose_identity(seat)?.instance;
    let operation = OperationId::new(format!("stop-{}", nonce.as_str()))?;
    let selected = registry::provider(target.provider.as_str())?;
    let adapter = adapter(&backend.binding, &target.identity, &target.provider)?;
    let hooks = adapter.hooks();
    let clock = RealClock::new();
    let mut physical = PhysicalRuntime::new(
        Registration {
            descriptor: selected.descriptor(),
            hooks: &hooks,
            controls: selected.controls(),
            startup: selected.startup(),
            session_panel: selected.session_panel(),
        },
        settings(&backend.binding, selected),
        operation.clone(),
        &clock,
    )?;
    let executing_candidate = executing_candidate_sha()?;
    physical.executing_candidate = Some(executing_candidate);
    physical.evidence = policy_evidence(&target.provider, &target.native, executing_candidate)?;
    physical.cap_budget(remaining_stop_budget(deadline)?)?;
    Ok(Lifecycle {
        store: &mut backend.store,
        host: &mut physical,
        io: &mut NoNewIo,
    }
    .teardown(&target.identity, operation)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launcher_relocation_quotes_paths_without_rewriting_prompt_text() {
        let old = Path::new("/tmp/ta-owned");
        let new = Path::new("/durable/owner's authority");
        let script = b"cd '/tmp/ta-owned/workspaces/a'\nexec 'provider' 'prompt mentions /tmp/ta-owned/workspaces/a' '/tmp/ta-owned-other'";
        let rebased = rebase_launch_script(script, old, new).unwrap();
        let cwd = team_agent_contract::host::shell::quote(new.join("workspaces/a").as_os_str()).unwrap();
        assert!(rebased.windows(cwd.len()).any(|window| window == cwd));
        let text = String::from_utf8(rebased).unwrap();
        assert!(text.contains("'prompt mentions /tmp/ta-owned/workspaces/a'"));
        assert!(text.contains("'/tmp/ta-owned-other'"));
    }

    #[test]
    fn relocated_snapshot_keeps_session_and_reopens_cwd_after_purge() {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        use team_agent_contract::contract::session::{CaptureOrigin, NativeSessionId, SessionStorage};
        let mut fixture = RoleFixture::new();
        let original = fixture.canonical(SeatStatus::Stopped, Phase::F4Commit, Outcome::Committed, None);
        let mut seat = original.store.seat(&fixture.identity.seat).unwrap().unwrap();
        let old = fixture.root.with_extension("legacy-authority");
        std::fs::rename(&original.binding.transport_root, &old).unwrap();
        let manifest_path = old.join("contract.json");
        let mut manifest: serde_json::Value = serde_json::from_slice(&std::fs::read(&manifest_path).unwrap()).unwrap();
        manifest["root"] = serde_json::json!(old);
        std::fs::write(manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
        fixture.binding.transport_root = old.clone();
        let workspaces = old.join("workspaces");
        let cwd = workspaces.join("worker");
        std::fs::DirBuilder::new().mode(0o700).create(&workspaces).unwrap();
        std::fs::DirBuilder::new().mode(0o700).create(&cwd).unwrap();
        seat.cwd = resolve_cwd(&cwd).unwrap();
        seat.session = Some(ResumeBinding { provider: seat.provider.clone(), native_session: NativeSessionId::new("preserved-session").unwrap(),
            storage: SessionStorage::Local, cwd: seat.cwd.clone(), native: seat.native.clone(), source: seat.identity.clone(),
            origin: CaptureOrigin::CurrentNativeSession, evidence_kind: seat.evidence_kind, evidence_sha256: digest(b"session"), backing: None });
        fixture.store().fixture_put_seat(&seat).unwrap();
        let root = root_for(&fixture.root, &fixture.binding.scope);
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::write(root.join("cwd-worker.json"), serde_json::to_vec(&serde_json::json!({"version":2,"seat":"worker","cwd":seat.cwd})).unwrap()).unwrap();
        // A previous interrupted attempt must not wedge the next relocation.
        std::fs::create_dir(root.join("authority")).unwrap();
        let mut binding = fixture.binding.clone();
        assert!(relocate_authority(&mut binding, &root).unwrap());
        let copied = ProjectionStore::open_readonly(&binding.transport_root, binding.scope.clone(), &binding.endpoint).unwrap();
        let moved = copied.seat(&fixture.identity.seat).unwrap().unwrap();
        assert_eq!(moved.identity, seat.identity);
        assert_eq!(moved.session.as_ref().unwrap().native_session, seat.session.as_ref().unwrap().native_session);
        assert_ne!(moved.cwd.identity, seat.cwd.identity);
        assert!(moved.cwd.path.starts_with(&binding.transport_root));
        drop(original);
        std::fs::remove_dir_all(old).unwrap();
        assert_eq!(super::super::workspace::prepare(&binding, &fixture.identity.seat).unwrap(), moved.cwd);
        assert_eq!(resumable_session_in_scope(&binding, &moved, None).unwrap(), moved.session);
    }

    #[test]
    fn legacy_sibling_retirement_preserves_the_winner_and_requires_inactive_carriers() {
        use std::os::unix::fs::DirBuilderExt;
        let fixture = RoleFixture::new();
        let mut winner = fixture.binding.clone();
        winner.workspace = team_agent_contract::host::process::legacy_cwd(&fixture.root).unwrap();
        winner.scope = computed_scope(&winner.workspace, &winner.team).unwrap();
        winner.endpoint = endpoint(&winner.scope);
        winner.transport_root = fixture.root.join("missing-authority");
        let winner_root = root_for(&fixture.root, &winner.scope);
        std::fs::DirBuilder::new().mode(0o700).create(&winner_root).unwrap();
        let original = serde_json::to_vec(&winner).unwrap();
        std::fs::write(winner_root.join("framework-binding.json"), &original).unwrap();
        let mut stale = winner.clone();
        stale.workspace.identity.0[0] ^= 2;
        stale.scope = computed_scope(&stale.workspace, &stale.team).unwrap();
        stale.endpoint = endpoint(&stale.scope);
        let stale_root = root_for(&fixture.root, &stale.scope);
        std::fs::DirBuilder::new().mode(0o700).create(&stale_root).unwrap();
        std::fs::write(stale_root.join("framework-binding.json"), serde_json::to_vec(&stale).unwrap()).unwrap();
        let foreign = winner_root.parent().unwrap().join("unrelated-schema");
        std::fs::DirBuilder::new().mode(0o700).create(&foreign).unwrap();
        std::fs::write(foreign.join("framework-binding.json"), serde_json::to_vec(&serde_json::json!({
            "version":99,"workspace":{"path":fixture.root},"team":"other-team","future_field":true
        })).unwrap()).unwrap();
        assert_eq!(read_binding(&fixture.root, "test").unwrap().scope, winner.scope);
        assert!(stale_root.join("framework-binding.json").exists());
        let unknown = serde_json::json!({"agents":{"worker":{"runtime_family":"contract",
            "contract_scope":stale.scope.as_str(),"contract_instance":"unknown-carrier"}}});
        retire_inactive_siblings(&winner, &unknown).unwrap();
        assert!(stale_root.join("framework-binding.json").exists());
        retire_inactive_siblings(&winner, &serde_json::json!({"agents":{}})).unwrap();
        assert!(!stale_root.join("framework-binding.json").exists());
        assert!(std::fs::read_dir(&stale_root).unwrap().any(|entry| entry.unwrap().file_name().to_string_lossy().starts_with("framework-binding.retired-")));
        assert_eq!(std::fs::read(winner_root.join("framework-binding.json")).unwrap(), original);
        // A subsequent boot can still adopt the single recorded scope.
        winner.workspace.identity.0[0] ^= 4;
        std::fs::write(winner_root.join("framework-binding.json"), serde_json::to_vec(&winner).unwrap()).unwrap();
        assert_eq!(read_binding(&fixture.root, "test").unwrap().scope, winner.scope);
    }

    struct RoleFixture { root: PathBuf, binding: Binding, role: RoleConfig, identity: InstanceIdentity }
    impl RoleFixture {
        fn new() -> Self {
            use std::os::unix::fs::DirBuilderExt;
            // Parallel tests can read the same clock tick; the counter keeps roots unique.
            static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let root = std::env::temp_dir().join(format!("contract-role-{}-{}-{}", std::process::id(),
                std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos(),
                NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)));
            std::fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
            let root = root.canonicalize().unwrap();
            let cwd = resolve_cwd(&root).unwrap();
            let scope = scope_for(&cwd, "test").unwrap();
            std::fs::create_dir_all(root_for(&cwd.path, &scope)).unwrap();
            let seat = SeatId::new("worker").unwrap();
            let identity = InstanceIdentity { scope: scope.clone(), seat: seat.clone(), instance: InstanceId::new("first").unwrap(), generation: Generation(1) };
            let binding = Binding { version: 1, workspace: cwd, team: "test".into(), scope,
                endpoint: "controlled".into(), candidate: "/controlled/candidate".into(), candidate_sha256: digest(b"candidate"),
                tmux: "/controlled/tmux".into(), transport_root: "/controlled/transport".into() };
            let role = RoleConfig { id: seat, provider: ProviderId::new("cursor").unwrap(), source: root.join("role.md"),
                role: "worker".into(), model: "first-model".into(), effort: None, bypass: false, paused: false,
                body: "bounded fixture".into(), prompt: super::super::prompt::Prompt {
                    text: "bounded fixture".into(), sha256: digest(b"bounded fixture"), sources: vec![],
                } };
            Self { root, binding, role, identity }
        }

        fn store(&self) -> ContractStore {
            ContractStore::open(&self.binding.transport_root, self.binding.scope.clone(), &self.binding.endpoint).unwrap()
        }

        fn save_operation(&self, operation: &OperationRecord) {
            self.store().fixture_put_operation(operation).unwrap();
        }

        fn recovering_teardown(&mut self, pending: Option<&str>) -> Backend {
            let backend = self.canonical(SeatStatus::Stopped, Phase::F2Register, Outcome::NeedsRecovery, pending);
            let mut operation = backend.store.operation(&OperationId::new("controlled-start").unwrap()).unwrap();
            operation.kind = TransactionKind::Teardown;
            self.save_operation(&operation);
            backend
        }

        // Receipt-only fixtures never authorize a native action; the runtime
        // must separately revalidate candidate, role, socket and process birth.
        fn canonical(&mut self, status: SeatStatus, phase: Phase, outcome: Outcome, pending: Option<&str>) -> Backend {
            self.binding.transport_root = self.binding.workspace.path.join("native-store");
            self.binding.endpoint = endpoint(&self.binding.scope);
            let mut store = ContractStore::create(&self.binding.transport_root, self.binding.scope.clone(), &self.binding.endpoint).unwrap();
            let target = SeatRecord { identity: self.identity.clone(), provider: self.role.provider.clone(),
                native: NativeIdentity { version: "fixture".into(), harness: "fixture".into(), ui: "controlled".into(),
                    platform: Platform::MacOs, executable_sha256: digest(b"fixture") },
                cwd: self.binding.workspace.clone(), endpoint: self.binding.endpoint.clone(), pane: "%1".into(),
                binding_key: "controlled".into(), profile: "fixture".into(), server_key: "fixture".into(),
                evidence_kind: EvidenceKind::Fixture, status, process: None, physical: None, session: None,
                resources: vec![], workspace_resources: vec![], bootstrap_used: false };
            let operation = OperationRecord { id: OperationId::new("controlled-start").unwrap(), kind: TransactionKind::Startup,
                target: target.clone(), parent: None, phase, pending: pending.map(str::to_owned), spawn_observation: None, native_launch: None, resources: vec![],
                effect: team_agent_contract::contract::delivery::DeliveryEffect::NoEffect, outcome, preserved: vec![], failure: None, recovery_failure: None, stop_receipt: None };
            store.fixture_put_seat(&target).unwrap();
            store.fixture_put_operation(&operation).unwrap();
            if matches!(outcome, Outcome::Running | Outcome::NeedsRecovery) {
                store.fixture_set_lease(&self.identity.seat, Some(&operation.id)).unwrap();
            }
            std::fs::write(root_for(&self.binding.workspace.path, &self.binding.scope).join("framework-binding.json"),
                serde_json::to_vec(&self.binding).unwrap()).unwrap();
            drop(store);
            Backend::open_readonly(&self.binding.workspace.path, "test").unwrap()
        }
    }
    impl Drop for RoleFixture { fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.root); } }

    #[test]
    fn recovered_stop_intent_never_masks_native_seats_or_creation_receipts() {
        let mut fixture = RoleFixture::new();
        let backend = fixture.canonical(SeatStatus::Ready, Phase::F4Commit, Outcome::Committed, None);
        let row = serde_json::json!({"runtime_family":"contract","explicit_stop":true});
        assert!(!super::super::framework::recovery_stopped(&backend, &fixture.identity.seat, &row).unwrap());
        drop(backend);
        fixture.store().fixture_delete_seat(&fixture.identity.seat).unwrap();
        let backend = Backend::open_readonly(&fixture.binding.workspace.path, "test").unwrap();
        assert!(!super::super::framework::recovery_stopped(&backend, &fixture.identity.seat, &row).unwrap(),
            "a missing seat with a creation receipt is not an omitted recovery seat");
    }

    #[test]
    fn recovered_stopped_projection_does_not_invent_native_authority() {
        let mut fixture = RoleFixture::new();
        fixture.binding.transport_root = fixture.root.join("recovered-authority");
        fixture.binding.endpoint = endpoint(&fixture.binding.scope);
        let mut store = ContractStore::create(&fixture.binding.transport_root,
            fixture.binding.scope.clone(), &fixture.binding.endpoint).unwrap();
        std::fs::write(root_for(&fixture.root, &fixture.binding.scope).join("framework-binding.json"),
            serde_json::to_vec(&fixture.binding).unwrap()).unwrap();
        let row = serde_json::json!({"runtime_family":"contract","provider":"cursor",
            "explicit_stop":true,"contract_instance":"lost-instance","session_id":"lost-session",
            "native_endpoint":"/stale/socket","pane_id":"%1","pane_pid":123});
        let project = |row: Option<&serde_json::Value>| super::super::framework::project_seat_with_row(
            &fixture.root, "test", "worker", row);
        let stopped = project(Some(&row)).unwrap();
        assert_eq!(stopped["status"], "stopped");
        for key in ["contract_instance", "session_id", "native_endpoint", "pane_id", "pane_pid"] {
            assert!(stopped[key].is_null(), "stale {key} is not recovered authority");
        }
        assert_eq!(stopped["session_observation"], "unobserved");
        assert!(project(None).is_err());
        assert!(project(Some(&serde_json::json!({"runtime_family":"contract"}))).is_err());
        assert!(project(Some(&serde_json::json!({"explicit_stop":true}))).is_err());
        let lease = OperationId::new("in-flight").unwrap();
        store.fixture_set_lease(&fixture.identity.seat, Some(&lease)).unwrap();
        assert!(project(Some(&row)).is_err(), "stop intent must not hide an in-flight lease");
        store.fixture_set_lease(&fixture.identity.seat, None).unwrap();
        std::fs::remove_file(fixture.binding.transport_root.join("contract.json")).unwrap();
        assert!(matches!(project(Some(&row)), Err(BackendError::AuthorityLost { .. })),
            "missing authority must not be presented as stopped");
    }

    fn assert_receipt_class(status: SeatStatus, phase: Phase, outcome: Outcome, pending: Option<&str>, expected: &str) {
            let mut fixture = RoleFixture::new();
            let backend = fixture.canonical(status, phase, outcome, pending);
            let before = serde_json::to_vec(&backend.store.seat(&fixture.identity.seat).unwrap()).unwrap();
            let receipt = backend.store.creation_receipt(&fixture.identity.seat, Some(&fixture.identity)).unwrap().unwrap();
            assert_eq!(receipt.phase, phase);
            assert_eq!(receipt.outcome, outcome);
            let result = existing_start(&backend, &fixture.identity.seat);
            assert_eq!(serde_json::to_vec(&backend.store.seat(&fixture.identity.seat).unwrap()).unwrap(), before);
            assert_eq!(backend.store.operation(&receipt.id).unwrap(), receipt);
            let classified = result.unwrap_or_else(|error| panic!("{expected} must not collapse to unconditional Conflict: {error}"));
            let matches = match classified {
                Some(ExistingStart::Committed(record)) => expected == "committed" && record.id == receipt.id,
                Some(ExistingStart::FinishReceipt(record)) => expected == "finish" && record.id == receipt.id,
                Some(ExistingStart::Pending(record)) => expected == "pending" && record.id == receipt.id,
                Some(ExistingStart::Resume(record)) => expected == "resume" && record.identity == fixture.identity,
                Some(ExistingStart::Blocked(record)) => expected == "blocked" && record.id == receipt.id,
                None => false,
            };
            assert!(matches, "wrong receipt class for {expected}");
    }

    #[test]
    fn native_lifecycle_construction_is_guarded_in_every_production_entrypoint() {
        // Assembly guard supplements the real flock/recovery tests; it does not
        // claim physical provider acceptance.
        let source = include_str!("backend.rs").split("#[cfg(test)]\nmod tests").next().unwrap();
        assert_eq!(source.matches("Lifecycle {").count(), 4);
        for entry in ["fn finish_receipt(", "pub fn start(", "pub fn resume(", "pub fn stop("] {
            let before_effect = source.split_once(entry).unwrap().1.split_once("Lifecycle {").unwrap().0;
            assert!(before_effect.contains("native_seat_lock("), "{entry}");
        }
        let fork = include_str!("fork.rs").split_once("fn native_fork(").unwrap().1.split_once("Lifecycle {").unwrap().0;
        assert!(fork.contains("_source_ownership = backend::native_seat_lock"));
        assert!(fork.contains("_target_ownership = backend::native_seat_lock"));
    }

    #[test]
    fn start_reconciliation_recovers_running_and_needs_recovery_startups_without_replay() {
        for outcome in [Outcome::Running, Outcome::NeedsRecovery] {
            let mut fixture = RoleFixture::new();
            drop(fixture.canonical(SeatStatus::Starting, Phase::F2Register, outcome, None));
            let mut backend = Backend::open(&fixture.root, "test").unwrap();
            let _ownership = native_seat_lock(&fixture.root, "test", &fixture.identity.seat, false).unwrap();
            reconcile_start(&mut backend, &fixture.identity.seat).unwrap();
            assert!(existing_start(&backend, &fixture.identity.seat).unwrap().is_none());
            assert!(backend.store.seat(&fixture.identity.seat).unwrap().is_none());
            assert!(backend.store.unfinished().unwrap().is_empty());
            let stopped = stop(&fixture.root, "test", &fixture.identity.seat).unwrap();
            assert_eq!(stopped.outcome, Outcome::Compensated);
            assert_eq!(stop(&fixture.root, "test", &fixture.identity.seat).unwrap().id, stopped.id);
        }
    }

    #[test]
    fn unresolved_pi_and_absent_or_empty_claude_transcripts_select_fresh_without_reading_content() {
        let mut fixture = RoleFixture::new();
        let backend = fixture.canonical(SeatStatus::Stopped, Phase::F5Receipt, Outcome::Committed, None);
        let mut parent = backend.store.seat(&fixture.identity.seat).unwrap().unwrap();
        parent.provider = ProviderId::new("pi_tui").unwrap();
        assert_eq!(resumable_session(&parent, None).unwrap(), None);
        parent.provider = ProviderId::new("claude_tui").unwrap();
        parent.session = Some(ResumeBinding {
            provider: parent.provider.clone(), native_session: team_agent_contract::contract::session::NativeSessionId::new("76156673-0000-4000-8000-000000000001").unwrap(),
            storage: team_agent_contract::contract::session::SessionStorage::Local, cwd: parent.cwd.clone(),
            native: parent.native.clone(), source: parent.identity.clone(),
            origin: team_agent_contract::contract::session::CaptureOrigin::OwnedExitHint,
            evidence_kind: parent.evidence_kind, evidence_sha256: digest(b"session-observation"), backing: None,
        });
        let path = team_agent_contract::providers::claude::transcript_path(&fixture.root, parent.session.as_ref().unwrap()).unwrap();
        assert_eq!(resumable_session(&parent, Some(&fixture.root)).unwrap(), None);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"").unwrap();
        assert_eq!(resumable_session(&parent, Some(&fixture.root)).unwrap(), None);
        std::fs::write(&path, b"controlled fixture transcript\n").unwrap();
        assert_eq!(resumable_session(&parent, Some(&fixture.root)).unwrap(), parent.session);
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        assert!(resumable_session(&parent, Some(&fixture.root)).is_err());
    }

    #[test]
    fn stop_budget_and_no_effect_refusal_never_claim_a_live_seat_stopped() {
        let mut fixture = RoleFixture::new();
        let backend = fixture.canonical(SeatStatus::Starting, Phase::F4Commit, Outcome::Committed, None);
        let original = backend.store.operation(&OperationId::new("controlled-start").unwrap()).unwrap();
        assert!(stop_with_budget(&fixture.root, "test", &fixture.identity.seat, Duration::ZERO)
            .unwrap_err().to_string().contains("native stop deadline exhausted"));
        assert_eq!(backend.store.operation(&original.id).unwrap(), original);
        let mut refused = original;
        refused.kind = TransactionKind::Teardown;
        refused.phase = Phase::F0Preflight;
        refused.outcome = Outcome::Compensated;
        refused.failure = Some("read-only preflight NoEffect".into());
        assert!(matches!(require_stopped(&refused), Err(BackendError::StopRefused { .. })));
        refused.target.status = SeatStatus::Stopped;
        assert!(require_stopped(&refused).is_ok());
    }

    #[test]
    fn unfinished_restart_parent_routes_to_locked_reconciliation_without_planner_writes() {
        for outcome in [Outcome::Running, Outcome::NeedsRecovery] {
            let mut fixture = RoleFixture::new();
            fixture.identity.generation = Generation(2);
            fixture.binding.candidate = std::env::current_exe().unwrap().canonicalize().unwrap();
            fixture.binding.candidate_sha256 = fingerprint_file(&fixture.binding.candidate, 1024 * 1024 * 1024, Duration::from_secs(10)).unwrap();
            let backend = fixture.canonical(SeatStatus::Starting, Phase::F2Register, outcome, Some("spawn"));
            let mut operation = backend.store.operation(&OperationId::new("controlled-start").unwrap()).unwrap();
            operation.kind = TransactionKind::Restart;
            let mut parent = operation.target.clone();
            parent.identity.instance = InstanceId::new("previous-instance").unwrap();
            parent.identity.generation = Generation(1);
            parent.status = SeatStatus::Stopped;
            operation.parent = Some(parent.clone());
            fixture.save_operation(&operation);
            let row = serde_json::json!({"provider":"cursor", "status":"stopped",
                "contract_scope":parent.identity.scope, "contract_instance":parent.identity.instance,
                "generation":parent.identity.generation});
            assert!(start_needs_reconciliation(&fixture.root, "test", &fixture.identity.seat, &row).unwrap());
            assert_eq!(backend.store.operation(&operation.id).unwrap(), operation);
            for (key, value) in [("contract_instance", serde_json::json!("foreign")),
                ("generation", serde_json::json!(3)), ("session_id", serde_json::json!("foreign"))] {
                let mut forged = row.clone(); forged[key] = value;
                assert!(matches!(start_needs_reconciliation(&fixture.root, "test", &fixture.identity.seat, &forged),
                    Err(BackendError::Fence { reason: "planner_identity_mismatch" })), "{key}");
            }
        }
    }

    #[test]
    fn stop_recovers_running_restart_and_stops_the_restored_parent() {
        let mut fixture = RoleFixture::new();
        fixture.identity.generation = Generation(2);
        let backend = fixture.canonical(SeatStatus::Starting, Phase::F2Register, Outcome::Running, None);
        let mut operation = backend.store.operation(&OperationId::new("controlled-start").unwrap()).unwrap();
        operation.kind = TransactionKind::Restart;
        let mut parent = operation.target.clone();
        parent.identity.instance = InstanceId::new("previous-instance").unwrap();
        parent.identity.generation = Generation(1);
        parent.status = SeatStatus::Stopped;
        operation.parent = Some(parent.clone());
        fixture.save_operation(&operation);
        let stopped = stop(&fixture.root, "test", &fixture.identity.seat).unwrap();
        assert_eq!(stopped.outcome, Outcome::Committed);
        assert_eq!(stopped.target.identity, parent.identity);
        assert!(backend.store.unfinished().unwrap().is_empty());
        assert_eq!(backend.store.operation(&operation.id).unwrap().outcome, Outcome::Compensated);
    }

    #[test]
    fn parent_recovery_fences_both_seats_without_waiting_in_reverse_lock_order() {
        let mut fixture = RoleFixture::new();
        drop(fixture.canonical(SeatStatus::Starting, Phase::F2Register, Outcome::Running, None));
        let mut backend = Backend::open(&fixture.root, "test").unwrap();
        let mut operation = backend.store.operation(&OperationId::new("controlled-start").unwrap()).unwrap();
        operation.kind = TransactionKind::NewSeatFullSnapshot;
        let mut parent = operation.target.clone();
        parent.identity.seat = SeatId::new("parent").unwrap();
        parent.identity.instance = InstanceId::new("parent-instance").unwrap();
        parent.pane = "parent-pane".into(); parent.binding_key = "parent-binding".into();
        parent.status = SeatStatus::Stopped;
        parent.session = Some(ResumeBinding { provider: parent.provider.clone(), native_session: team_agent_contract::contract::session::NativeSessionId::new("parent-session").unwrap(),
            storage: team_agent_contract::contract::session::SessionStorage::Local, cwd: parent.cwd.clone(), native: parent.native.clone(), source: parent.identity.clone(),
            origin: team_agent_contract::contract::session::CaptureOrigin::OwnedExitHint, evidence_kind: EvidenceKind::Fixture, evidence_sha256: digest(b"parent-session"), backing: None });
        operation.parent = Some(parent.clone());
        fixture.save_operation(&operation);
        let mut store = fixture.store();
        store.fixture_put_seat(&parent).unwrap();
        store.fixture_set_lease(&parent.identity.seat, Some(&operation.id)).unwrap();
        drop(store);
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let root = fixture.root.clone(); let child = fixture.identity.seat.clone();
        let holder = std::thread::spawn(move || {
            let _guard = native_seat_lock(&root, "test", &child, false).unwrap();
            ready_tx.send(()).unwrap(); let _ = release_rx.recv();
        });
        ready_rx.recv().unwrap();
        let _parent = native_seat_lock(&fixture.root, "test", &parent.identity.seat, false).unwrap();
        let began = std::time::Instant::now();
        let blocked = recover_incomplete(&mut backend, &parent.identity.seat);
        let elapsed = began.elapsed();
        release_tx.send(()).unwrap(); holder.join().unwrap();
        assert!(blocked.is_err()); assert!(elapsed < Duration::from_secs(1));
        assert!(backend.store.leased(&parent.identity.seat).unwrap());
        assert!(recover_incomplete(&mut backend, &parent.identity.seat).unwrap().is_none());
        assert!(backend.store.seat(&fixture.identity.seat).unwrap().is_none());
        assert_eq!(backend.store.seat(&parent.identity.seat).unwrap(), Some(parent));
        assert!(backend.store.unfinished().unwrap().is_empty());
    }

    #[test]
    fn stop_recovers_a_stopped_teardown_before_beginning_the_next_operation() {
        let mut fixture = RoleFixture::new();
        let backend = fixture.recovering_teardown(None);
        let receipt = stop(&fixture.root, "test", &fixture.identity.seat).unwrap();
        assert_eq!(receipt.outcome, Outcome::Committed);
        assert_eq!(backend.store.operation(&OperationId::new("controlled-start").unwrap()).unwrap().outcome, Outcome::Compensated);
        assert!(!backend.store.leased(&fixture.identity.seat).unwrap());
        assert_eq!(backend.store.seat(&fixture.identity.seat).unwrap().unwrap().status, SeatStatus::Stopped);
    }

    #[test]
    fn stop_preserves_uncertain_recovery_but_does_not_block_a_sibling() {
        let mut fixture = RoleFixture::new();
        let backend = fixture.recovering_teardown(Some("native-control"));
        let mut sibling = backend.store.seat(&fixture.identity.seat).unwrap().unwrap();
        sibling.identity.seat = SeatId::new("sibling").unwrap();
        sibling.identity.instance = InstanceId::new("sibling-instance").unwrap();
        sibling.pane = "sibling-pane".into();
        sibling.binding_key = "sibling-binding".into();
        sibling.server_key = "sibling-server".into();
        fixture.store().fixture_put_seat(&sibling).unwrap();
        let receipt = stop(&fixture.root, "test", &fixture.identity.seat).unwrap();
        assert_eq!(receipt.id.as_str(), "controlled-start");
        assert_eq!(receipt.outcome, Outcome::NeedsRecovery);
        assert!(backend.store.leased(&fixture.identity.seat).unwrap());
        let sibling_receipt = stop(&fixture.root, "test", &sibling.identity.seat).unwrap();
        assert_eq!(sibling_receipt.outcome, Outcome::Committed);
        assert_eq!(backend.store.unfinished().unwrap().len(), 1);
    }

    #[test]
    fn startup_receipt_red_committed_is_reused_not_conflicted() {
        assert_receipt_class(SeatStatus::Ready, Phase::F5Receipt, Outcome::Committed, None, "committed");
    }
    #[test]
    fn startup_receipt_red_f4_is_receipt_completion_not_fresh() {
        assert_receipt_class(SeatStatus::Starting, Phase::F4Commit, Outcome::Running, None, "finish");
    }
    #[test]
    fn startup_receipt_red_inflight_is_pending_not_replayed() {
        assert_receipt_class(SeatStatus::Starting, Phase::F3Spawn, Outcome::Running, Some("spawn"), "pending");
    }
    #[test]
    fn startup_receipt_red_stopped_is_resume_not_fresh() {
        assert_receipt_class(SeatStatus::Stopped, Phase::F5Receipt, Outcome::Committed, None, "resume");
    }
    #[test]
    fn startup_receipt_red_uncertain_is_blocked_with_original_receipt() {
        assert_receipt_class(SeatStatus::Starting, Phase::F3Spawn, Outcome::NeedsRecovery, Some("spawn"), "blocked");
    }

    #[test]
    fn startup_receipt_projection_repair_refuses_foreign_or_guessed_identity() {
        let mut fixture = RoleFixture::new();
        fixture.binding.candidate = std::env::current_exe().unwrap().canonicalize().unwrap();
        fixture.binding.candidate_sha256 = fingerprint_file(&fixture.binding.candidate, 1024 * 1024 * 1024, Duration::from_secs(10)).unwrap();
        let backend = fixture.canonical(SeatStatus::Starting, Phase::F4Commit, Outcome::Running, None);
        let original = backend.store.creation_receipt(&fixture.identity.seat, Some(&fixture.identity)).unwrap().unwrap();
        let seed = serde_json::json!({"provider":"cursor","status":"unknown"});
        assert!(start_needs_reconciliation(&fixture.root, "test", &fixture.identity.seat, &seed).unwrap());
        let exact = serde_json::json!({"provider":"cursor","status":"unknown","contract_scope":fixture.identity.scope,
            "contract_instance":fixture.identity.instance,"generation":fixture.identity.generation});
        assert!(start_needs_reconciliation(&fixture.root, "test", &fixture.identity.seat, &exact).unwrap());
        for (key, value) in [("provider", serde_json::json!("grok_build")), ("contract_scope", serde_json::json!("foreign")),
            ("contract_instance", serde_json::json!("another")), ("generation", serde_json::json!(2))] {
            let mut forged = exact.clone(); forged[key] = value;
            assert!(start_needs_reconciliation(&fixture.root, "test", &fixture.identity.seat, &forged).is_err(), "{key}");
        }
        assert_eq!(backend.store.operation(&original.id).unwrap(), original);
        assert!(preflight_start_role(&fixture.root, "test", &fixture.role).is_err(), "classification is not Native admission");
    }

    #[test]
    fn startup_receipt_fixture_cannot_authorize_reuse_or_native_input() {
        let mut fixture = RoleFixture::new();
        let backend = fixture.canonical(SeatStatus::Ready, Phase::F5Receipt, Outcome::Committed, None);
        let record = backend.store.creation_receipt(&fixture.identity.seat, Some(&fixture.identity)).unwrap().unwrap();
        assert!(validate_existing(&backend, &fixture.role, &record).is_err());
        assert_eq!(backend.store.operation(&record.id).unwrap(), record);
    }

    #[test]
    fn startup_receipt_unfinished_parent_wins_over_old_committed_generation() {
        let mut fixture = RoleFixture::new();
        let backend = fixture.canonical(SeatStatus::Ready, Phase::F5Receipt, Outcome::Committed, None);
        let mut interrupted = backend.store.creation_receipt(&fixture.identity.seat, Some(&fixture.identity)).unwrap().unwrap();
        interrupted.parent = Some(interrupted.target.clone());
        interrupted.id = OperationId::new("uncertain-restart").unwrap();
        interrupted.kind = TransactionKind::Restart;
        interrupted.target.identity.instance = InstanceId::new("second").unwrap();
        interrupted.target.identity.generation = Generation(2);
        interrupted.phase = Phase::F3Spawn;
        interrupted.pending = Some("spawn".into());
        interrupted.outcome = Outcome::NeedsRecovery;
        fixture.save_operation(&interrupted);
        assert!(matches!(existing_start(&backend, &fixture.identity.seat).unwrap(),
            Some(ExistingStart::Blocked(record)) if record == interrupted));
        assert_eq!(backend.store.seat(&fixture.identity.seat).unwrap().unwrap().identity, fixture.identity);
    }

    #[test]
    fn startup_receipt_missing_committed_seat_refuses_but_compensated_allows_fresh_proposal() {
        for outcome in [Outcome::Committed, Outcome::Compensated] {
            let mut fixture = RoleFixture::new();
            let backend = fixture.canonical(SeatStatus::Stopped, Phase::F5Receipt, outcome, None);
            fixture.store().fixture_delete_seat(&fixture.identity.seat).unwrap();
            let result = existing_start(&backend, &fixture.identity.seat);
            if outcome == Outcome::Committed { assert!(result.is_err()); }
            else { assert!(result.unwrap().is_none()); }
            assert_eq!(backend.store.propose_identity(&fixture.identity.seat).unwrap().generation, Generation(2));
        }
    }

    #[test]
    fn retired_identity_lets_its_seat_id_start_fresh_and_a_live_seat_never_retires() {
        {
            let mut fixture = RoleFixture::new();
            let backend = fixture.canonical(SeatStatus::Ready, Phase::F5Receipt, Outcome::Committed, None);
            let mut writer = ContractStore::open(&fixture.binding.transport_root,
                fixture.binding.scope.clone(), &fixture.binding.endpoint).unwrap();
            assert!(writer.retire(&fixture.identity).is_err());
            assert!(matches!(existing_start(&backend, &fixture.identity.seat).unwrap(),
                Some(ExistingStart::Committed(_))));
        }
        let mut fixture = RoleFixture::new();
        let backend = fixture.canonical(SeatStatus::Stopped, Phase::F5Receipt, Outcome::Committed, None);
        assert!(matches!(existing_start(&backend, &fixture.identity.seat).unwrap(),
            Some(ExistingStart::Resume(_))));
        let mut writer = ContractStore::open(&fixture.binding.transport_root,
            fixture.binding.scope.clone(), &fixture.binding.endpoint).unwrap();
        assert_eq!(writer.retire(&fixture.identity).unwrap().identity, fixture.identity);
        // Never a resume of the retired session, and never the missing-seat fence.
        assert!(existing_start(&backend, &fixture.identity.seat).unwrap().is_none());
        assert!(seat_unclaimed(&backend, &fixture.identity.seat).unwrap());
        assert_eq!(backend.store.propose_identity(&fixture.identity.seat).unwrap().generation, Generation(2));
    }

    #[test]
    fn startup_receipt_lookup_refuses_cross_scope_and_generation_fallback() {
        let mut fixture = RoleFixture::new();
        let backend = fixture.canonical(SeatStatus::Ready, Phase::F5Receipt, Outcome::Committed, None);
        let mut wrong = fixture.identity.clone(); wrong.generation = Generation(2);
        assert!(backend.store.creation_receipt(&fixture.identity.seat, Some(&wrong)).is_err());
        wrong = fixture.identity.clone(); wrong.scope = ScopeId::new("foreign").unwrap();
        assert!(backend.store.creation_receipt(&fixture.identity.seat, Some(&wrong)).is_err());
        assert!(backend.store.creation_receipt(&SeatId::new("absent").unwrap(), None).unwrap().is_none());
    }

    #[test]
    fn restart_skips_stopped_mirror_and_stopped_native_store_before_resume_discovery() {
        for provider in ["cursor", "grok_build"] {
            let mut fixture = RoleFixture::new();
            let directory = fixture.root.join("roles");
            std::fs::create_dir_all(directory.join("agents")).unwrap();
            std::fs::write(directory.join("TEAM.md"), "---\nname: test\n---\nFixture\n").unwrap();
            std::fs::write(directory.join("agents/worker.md"),
                format!("---\nprovider: {provider}\nmodel: unavailable-fixture-model\n---\nFixture\n")).unwrap();
            let stopped = serde_json::json!({"agents":{"worker":{"provider":provider,"status":"stopped","session_id":"preserved","generation":1}}});
            // No framework binding/store/native installation exists yet. A
            // stopped mirror must not even attempt to open the native store.
            assert!(prepare_restart(&fixture.root, "test", &directory, &stopped).unwrap().is_empty());

            fixture.binding.transport_root = fixture.binding.workspace.path.join("native-store");
            fixture.binding.endpoint = endpoint(&fixture.binding.scope);
            let mut store = ContractStore::create(&fixture.binding.transport_root, fixture.binding.scope.clone(), &fixture.binding.endpoint).unwrap();
            let native = NativeIdentity { version: "fixture".into(), harness: "unavailable".into(), ui: "tui".into(),
                platform: Platform::MacOs, executable_sha256: digest(b"fixture") };
            let seat = SeatRecord {
                identity: fixture.identity.clone(), provider: ProviderId::new(provider).unwrap(), native,
                cwd: fixture.binding.workspace.clone(), endpoint: fixture.binding.endpoint.clone(),
                pane: "unpublished".into(), binding_key: "fixture".into(), profile: "unavailable".into(), server_key: "fixture".into(),
                evidence_kind: EvidenceKind::Native, status: SeatStatus::Stopped, process: None, physical: None,
                session: None, resources: vec![], workspace_resources: vec![], bootstrap_used: false,
            };
            // Test-only canonical fixture, never a live-store recovery shortcut.
            store.fixture_put_seat(&seat).unwrap();
            std::fs::write(root_for(&fixture.binding.workspace.path, &fixture.binding.scope).join("framework-binding.json"),
                serde_json::to_vec(&fixture.binding).unwrap()).unwrap();
            let stale_running = serde_json::json!({"agents":{"worker":{"provider":provider,"status":"running"}}});
            assert!(prepare_restart(&fixture.root, "test", &directory, &stale_running).unwrap().is_empty());
            assert_eq!(store.seat(&fixture.identity.seat).unwrap(), Some(seat));
            assert_eq!(store.propose_identity(&fixture.identity.seat).unwrap().generation, Generation(2));
        }
    }

    #[test]
    fn live_and_stopped_seats_open_after_candidate_upgrade_or_relocation() {
        for status in [SeatStatus::Ready, SeatStatus::Stopped] {
            for relocated in [false, true] {
                let mut fixture = RoleFixture::new();
                fixture.binding.candidate = if relocated { fixture.root.join("old-missing-binary") }
                    else { std::env::current_exe().unwrap().canonicalize().unwrap() };
                fixture.binding.candidate_sha256 = Digest([0; 32]);
                let original = fixture.canonical(status, Phase::F5Receipt, Outcome::Committed, None);
                let path = root_for(&fixture.root, &fixture.binding.scope).join("framework-binding.json");
                let before = std::fs::read(&path).unwrap();
                let seat = original.store.seat(&fixture.identity.seat).unwrap();
                let reopened = Backend::open_readonly(&fixture.root, "test").unwrap();
                assert_eq!(reopened.binding.candidate, std::env::current_exe().unwrap().canonicalize().unwrap());
                assert_eq!(reopened.binding.candidate_sha256, executing_candidate_sha().unwrap());
                assert_eq!(reopened.store.seat(&fixture.identity.seat).unwrap(), seat);
                assert_eq!(std::fs::read(&path).unwrap(), before);
            }
        }
    }

    #[test]
    fn native_business_tools_use_independent_inbox_without_submitted_tasks() {
        use team_agent_contract::orchestration::mcp::CallContext;
        let mut fixture = RoleFixture::new();
        fixture.role.provider = ProviderId::new("grok_build").unwrap();
        let _backend = fixture.canonical(SeatStatus::Ready, Phase::F5Receipt, Outcome::Committed, None);
        let mut backend = Backend::open(&fixture.root, "test").unwrap();
        let state = serde_json::json!({"active_team_key":"test", "teams":{"test":{
            "team_key":"test", "team_dir":fixture.root, "agents":{"worker":{"provider":"grok","status":"running"}}
        }}});
        let state_file = crate::state::persist::runtime_state_path(&fixture.root);
        std::fs::write(state_file, serde_json::to_vec(&state).unwrap()).unwrap();
        let tools = crate::mcp_server::TeamOrchestratorTools::with_identity(
            &fixture.root, Some(crate::model::ids::AgentId::new("worker")),
            Some(crate::model::ids::TeamKey::new("test")));
        let context = CallContext { identity: fixture.identity.clone(), binding_key:"controlled".into(),
            connection_id:InstanceId::new("direct-callback").unwrap(), task_id:String::new() };
        assert!(backend.store.submitted_task(&fixture.identity).unwrap().is_none());
        for (id, name, arguments) in [
            (1, "send_message", serde_json::json!({"to":"leader","content":"unbound native callback","mailbox":true})),
            (2, "get_team_status", serde_json::json!({})),
            (3, "report_result", serde_json::json!({"summary":"independent native result"})),
        ] {
            let request = serde_json::json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{"name":name,"arguments":arguments}});
            let result = super::super::mcp::direct_request(&mut backend.store, &context, &request, &tools).unwrap().unwrap();
            assert_eq!(result["result"]["isError"], false, "{result}");
        }
        let inbox = crate::inbox_store::read(&fixture.root, "leader", 10, Some("test")).unwrap();
        assert_eq!(inbox["messages"].as_array().unwrap().len(), 2);
        assert!(!fixture.binding.transport_root.join("contract.db").exists(), "no SQLite ledger at all");
        let mut foreign = context.clone(); foreign.identity.generation = Generation(2);
        let request = serde_json::json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"send_message","arguments":{"to":"leader","content":"must not arrive","mailbox":true}}});
        assert!(super::super::mcp::direct_request(&mut backend.store, &foreign, &request, &tools).is_err());
    }

    #[test]
    fn grok_projection_exposes_public_provider_and_keeps_internal_contract_id() {
        let mut fixture = RoleFixture::new();
        fixture.role.provider = ProviderId::new("grok_build").unwrap();
        let _backend = fixture.canonical(SeatStatus::Ready, Phase::F5Receipt, Outcome::Committed, None);
        write_role(&fixture.binding, &fixture.identity, &fixture.role).unwrap();
        let projected = super::super::framework::project_seat(&fixture.root, "test", fixture.identity.seat.as_str()).unwrap();
        assert_eq!(projected["provider"], "grok");
        assert_eq!(projected["contract_provider"], "grok_build");
    }

    #[test]
    fn native_snapshot_publishes_the_private_pane_endpoint_pair() {
        let mut fixture = RoleFixture::new();
        fixture.role.provider = ProviderId::new("pi_tui").unwrap();
        let _backend = fixture.canonical(SeatStatus::Ready, Phase::F5Receipt, Outcome::Committed, None);
        let mut operation = fixture.store().operation(&OperationId::new("controlled-start").unwrap()).unwrap();
        operation.failure = Some("session observation deferred: capture-failed; tmux capture: Deadline".into());
        fixture.save_operation(&operation);
        write_role(&fixture.binding, &fixture.identity, &fixture.role).unwrap();
        let state = super::super::framework::project_seat(
            &fixture.binding.workspace.path, "test", "worker",
        ).unwrap();
        assert_eq!(state["pane_id"], "%1");
        assert_eq!(state["pane_id"], state["native_pane"]);
        assert_eq!(state["tmux_endpoint"], fixture.binding.endpoint);
        assert_eq!(state["tmux_endpoint"], state["native_endpoint"]);
        assert!(state["session_name"].is_null());
        assert!(state["session_id"].is_null());
        assert_eq!(state["session_observation"], "unobserved");
        assert_eq!(state["session_warning"], "session observation deferred: capture-failed; tmux capture: Deadline");
        assert_eq!(state["mcp_binding"], "unverified");
    }

    #[test]
    fn role_snapshots_are_instance_scoped_and_exclusive() {
        let fixture = RoleFixture::new();
        write_role(&fixture.binding, &fixture.identity, &fixture.role).unwrap();
        let mut second = fixture.identity.clone();
        second.instance = InstanceId::new("second").unwrap();
        second.generation = Generation(2);
        let mut role = fixture.role.clone();
        role.model = "second-model".into();
        write_role(&fixture.binding, &second, &role).unwrap();
        assert_eq!(read_role(&fixture.binding, &fixture.identity).unwrap().model, "first-model");
        assert_eq!(read_role(&fixture.binding, &second).unwrap().model, "second-model");
        assert!(write_role(&fixture.binding, &fixture.identity, &role).is_err());
        assert_eq!(read_role(&fixture.binding, &fixture.identity).unwrap().model, "first-model");
    }

    #[test]
    fn legacy_role_fallback_is_only_for_missing_generation_one_not_symlinks() {
        let fixture = RoleFixture::new();
        let old = root_for(&fixture.binding.workspace.path, &fixture.binding.scope).join("role-worker.json");
        std::fs::write(&old, serde_json::to_vec(&fixture.role).unwrap()).unwrap();
        assert_eq!(read_role(&fixture.binding, &fixture.identity).unwrap().model, "first-model");
        let mut later = fixture.identity.clone();
        later.generation = Generation(2);
        assert!(read_role(&fixture.binding, &later).is_err());
        std::os::unix::fs::symlink(fixture.root.join("missing"), role_path(&fixture.binding, &fixture.identity)).unwrap();
        assert!(matches!(read_role(&fixture.binding, &fixture.identity), Err(BackendError::Metadata)));
    }

    #[test]
    fn mismatched_role_owner_cannot_write_a_snapshot() {
        let fixture = RoleFixture::new();
        let mut identity = fixture.identity.clone();
        identity.seat = SeatId::new("other").unwrap();
        assert!(matches!(write_role(&fixture.binding, &identity, &fixture.role), Err(BackendError::Metadata)));
        assert!(!role_path(&fixture.binding, &identity).exists());
    }

    #[test]
    fn shutdown_error_exposes_the_durable_operation_failure_not_metadata() {
        let failure = "native instance not alive or replaced: Unknown; stage=check-bound; expected_pid=44123; process_state=Some(Replaced)";
        let error = BackendError::Shutdown {
            operation: OperationId::new("stop-instance-test").unwrap(),
            phase: Phase::F0Preflight,
            outcome: Outcome::NeedsRecovery,
            failure: Some(failure.into()),
        };
        let shown = error.to_string();
        for expected in ["stop-instance-test", "F0Preflight", "NeedsRecovery", failure] {
            assert!(shown.contains(expected), "{shown}");
        }
        assert!(!shown.contains("metadata is invalid"));
    }

    #[test]
    fn cursor_metadata_never_mints_runtime_policy_evidence() {
        let provider = ProviderId::new("cursor").unwrap();
        let mut native = NativeIdentity {
            version: "2026.09.29-a1b2c3".into(),
            harness: "cursor-launcher".into(),
            ui: "metadata".into(),
            platform: Platform::MacOs,
            executable_sha256: digest(b"launcher only"),
        };
        let candidate = digest(b"candidate");
        assert!(policy_evidence(&provider, &native, candidate).unwrap().is_empty());
        native.harness = "cursor-agent".into();
        native.ui = "tui".into();
        // Admission supplies a separately image-fenced runtime identity. The
        // recipe still cannot be borrowed from metadata or another provider.
        assert!(!policy_evidence(&provider, &native, candidate).unwrap().is_empty());
        // Even a Kiro-looking image cannot reuse its evidence under Cursor.
        native.harness = "v2".into();
        native.version = "2.29.0".into();
        assert!(policy_evidence(&provider, &native, candidate).unwrap().is_empty());
    }

    #[test]
    fn cursor_request_refuses_metadata_launch_and_wrong_provider_catalog() {
        let provider = ProviderId::new("cursor").unwrap();
        let scope = ScopeId::new("controlled").unwrap();
        let seat = SeatId::new("worker").unwrap();
        let binding = Binding {
            version: 1,
            workspace: CwdIdentity { path: "/controlled".into(), identity: digest(b"cwd") },
            team: "demo".into(), scope: scope.clone(), endpoint: "/controlled/socket".into(),
            candidate: "/controlled/candidate".into(), candidate_sha256: digest(b"candidate"),
            tmux: "/controlled/tmux".into(), transport_root: "/controlled/runtime".into(),
        };
        let role = RoleConfig {
            id: seat.clone(), provider: provider.clone(), source: "/controlled/role.md".into(),
            role: "worker".into(), model: "gpt-5.6-luna-xhigh".into(), effort: None,
            bypass: true, paused: false, body: "role".into(),
            prompt: super::super::prompt::Prompt { text: "prompt".into(), sha256: digest(b"prompt"), sources: vec![] },
        };
        let identity = InstanceIdentity { scope, seat, instance: InstanceId::new("preflight").unwrap(), generation: Generation(1) };
        let native = NativeIdentity {
            version: "2026.09.29-a1b2c3".into(), harness: "cursor-launcher".into(),
            ui: "metadata".into(), platform: Platform::MacOs, executable_sha256: digest(b"launcher"),
        };
        let mut discovery = Discovery {
            command: team_agent_contract::host::command::PublicCommand {
                name: "cursor-agent".into(), search_path: "/controlled".into(), entry: "/controlled/cursor-agent".into(),
            },
            runtime: None,
            engine: "/controlled/cursor-agent".into(), native: native.clone(),
            catalog: CatalogObservation { provider, native,
                schema: team_agent_contract::providers::cursor::native::CATALOG_SOURCE.schema.into(), models: vec![] },
        };
        assert!(matches!(request(&binding, &role, identity.clone(), &discovery),
            Err(BackendError::Contract(ContractError::ProfileUnavailable))));
        discovery.catalog.provider = ProviderId::new("kiro").unwrap();
        assert!(matches!(request(&binding, &role, identity, &discovery),
            Err(BackendError::Contract(ContractError::Mismatch("role native catalog")))));
    }

    #[test]
    fn policy_bindings_track_runtime_native_identity_not_a_release_receipt() {
        let mut native = NativeIdentity {
            version: "2.28.0".into(),
            harness: "v2".into(),
            ui: "tui".into(),
            platform: Platform::MacOs,
            executable_sha256: digest(b"old image"),
        };
        let candidate = digest(b"candidate");
        let provider = ProviderId::new("kiro").unwrap();
        let old = policy_evidence(&provider, &native, candidate).unwrap();
        assert!(!old.is_empty());
        native.version = "2.29.0".into();
        native.executable_sha256 = digest(b"updated image");
        let new = policy_evidence(&provider, &native, candidate).unwrap();
        assert_eq!(new.len(), old.len());
        for (old, new) in old.iter().zip(&new) {
            assert_eq!(new.native, native);
            assert_eq!(new.candidate_sha256, candidate);
            assert_eq!(new.policy_sha256, old.policy_sha256);
            assert_ne!(new.evidence_sha256, old.evidence_sha256);
        }
        native.harness = "v3".into();
        assert!(policy_evidence(&provider, &native, candidate).unwrap().is_empty());
    }

    // Field 2026-11-02 (tmux桌面端, parity-audit-team): macOS /usr/libexec/tmp_cleaner runs daily at
    // 00:00 and deletes every regular file under /tmp idle for >3 days. The leader was then trapped in
    // restart -> "requires explicit migration" -> migrate -> "file fingerprint: Io(NotFound)" -> doctor -> restart.
    fn purge_regular_files(root: &Path) {
        for entry in std::fs::read_dir(root).unwrap() {
            let path = entry.unwrap().path();
            let metadata = std::fs::symlink_metadata(&path).unwrap();
            if metadata.is_dir() { purge_regular_files(&path) } else if metadata.is_file() { std::fs::remove_file(&path).unwrap() }
        }
    }

    #[test]
    fn os_purged_transport_root_is_not_misdiagnosed_as_legacy_migration() {
        let mut fixture = RoleFixture::new();
        drop(fixture.canonical(SeatStatus::Ready, Phase::F4Commit, Outcome::Committed, None));
        let workspace = fixture.binding.workspace.path.clone();
        purge_regular_files(&fixture.binding.transport_root);
        assert!(!fixture.binding.transport_root.join("contract.db").exists());
        let error = Backend::open(&workspace, "test").err().expect("a purged authority cannot open as healthy");
        assert!(!error.to_string().contains("requires explicit migration"),
            "no legacy contract.db exists, so routing the leader to migrate is a false diagnosis: {error}");
        let advice = authority_recovery_action(&workspace, "test", "worker");
        assert!(!advice.contains("migrate"), "recovery advice must not route to migrate without a legacy store: {advice}");
        // CLI migrate reaches the importer only through this classification; the raw importer
        // would fingerprint the missing contract.db and leak Io(NotFound).
        let classified = authority_check(&workspace, "test").err().expect("purged authority must be classified");
        assert!(matches!(classified, BackendError::AuthorityLost { .. }), "{classified}");
        assert!(!classified.to_string().contains("Io(NotFound)"), "{classified}");
        let source = include_str!("backend.rs").split("#[cfg(test)]\nmod tests").next().unwrap();
        let migrate = source.split_once("pub fn migrate(").unwrap().1;
        let (before_import, _) = migrate.split_once("importer::contract::migrate(").unwrap();
        assert!(before_import.contains("check_authority(&binding)"), "migrate must classify authority before importing");
    }

    #[test]
    fn durable_contract_authority_is_not_created_in_os_purged_tmp() {
        let fixture = RoleFixture::new();
        let cwd = fixture.binding.workspace.clone();
        std::fs::remove_dir(root_for(&cwd.path, &fixture.binding.scope)).unwrap();
        let candidate = std::env::current_exe().unwrap().canonicalize().unwrap();
        let backend = Backend::create(cwd.clone(), "test", candidate, "/controlled/tmux".into()).unwrap();
        let transport = backend.binding.transport_root.clone();
        let store = backend.store.root().canonicalize().unwrap();
        drop(backend);
        let _ = std::fs::remove_dir_all(&transport);
        // Sockets may stay short under /tmp (sockaddr_un); durable projections may not, unless the
        // workspace itself was placed there by the caller.
        let purged = [Path::new("/tmp"), Path::new("/private/tmp")];
        assert!(store.starts_with(&cwd.path) || !purged.iter().any(|tmp| store.starts_with(tmp)),
            "durable contract authority {store:?} is placed where macOS tmp_cleaner deletes files idle >3 days");
    }
}
