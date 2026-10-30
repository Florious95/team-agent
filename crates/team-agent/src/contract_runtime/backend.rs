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
}

pub fn scope_for(workspace: &CwdIdentity, team: &str) -> Result<ScopeId, BackendError> {
    if team.trim().is_empty() || team.chars().any(char::is_control) {
        return Err(BackendError::Metadata);
    }
    let encoded = serde_json::to_vec(&(workspace, team)).map_err(|_| BackendError::Metadata)?;
    Ok(ScopeId::new(format!(
        "contract-{}",
        &digest_hex(digest(&encoded))[..32]
    ))?)
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
        let file_path = root.join("framework-binding.json");
        let metadata = std::fs::symlink_metadata(&file_path)?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(BackendError::Metadata);
        }
        let mut bytes = Vec::new();
        std::fs::File::open(&file_path)?
            .take(65537)
            .read_to_end(&mut bytes)?;
        if bytes.len() > 65536 {
            return Err(BackendError::Metadata);
        }
        let binding: Binding =
            serde_json::from_slice(&bytes).map_err(|_| BackendError::Metadata)?;
        if binding.version != 1
            || binding.workspace != workspace
            || binding.team != team
            || binding.scope != scope
            || binding.endpoint != endpoint(&scope)
        {
            return Err(BackendError::Metadata);
        }
        Ok(binding)
}

impl Backend {
    pub fn open(workspace: &Path, team: &str) -> Result<Self, BackendError> {
        Self::open_mode(workspace, team, false)
    }
    pub fn open_readonly(workspace: &Path, team: &str) -> Result<Self, BackendError> {
        Self::open_mode(workspace, team, true)
    }
    fn open_mode(workspace: &Path, team: &str, readonly: bool) -> Result<Self, BackendError> {
        let binding = read_binding(workspace, team)?;
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
        Ok(Self { store, binding })
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
        // OS entropy gives this transport parent a fresh incarnation even when
        // a logical team/name is reused. This is not a native session identity.
        let mut nonce = [0u8; 32];
        std::fs::File::open("/dev/urandom")?.read_exact(&mut nonce)?;
        let transport_root = Path::new("/tmp")
            .canonicalize()?
            .join(format!("ta-{}", &digest_hex(digest(&nonce))[..24]));
        // K3 and K2 share the same owned short root. Keeping only the pointer
        // under the project avoids macOS sockaddr_un limits without weakening
        // lifecycle's store-root fence or adopting an existing transport.
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
        Ok(Self { store, binding })
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
    Ok((SharedTmux { endpoint, socket, session_name, session }, transport, generation))
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

fn preflight_request(
    binding: &Binding,
    role: &RoleConfig,
    identity: InstanceIdentity,
    discovered: &Discovery,
) -> Result<LaunchRequest, BackendError> {
    let mut request = request(binding, role, identity, discovered)?;
    if matches!(registry::provider(role.provider.as_str())?, registry::NativeProvider::Codex) {
        // Pure H2 only: this is a proposed private path, not a filesystem receipt.
        request.paths.cwd = team_agent_contract::contract::session::CwdIdentity {
            path: binding.transport_root.join("workspaces").join(role.id.as_str()),
            identity: digest(b"effect-free-planned-private-cwd"),
        };
    }
    Ok(request)
}

/// Providers whose native config search would otherwise inherit a Git or
/// project configuration ancestor. Metadata only; no body is read.
pub(super) fn require_private_primary(
    selected: registry::NativeProvider,
    cwd: &team_agent_contract::contract::session::CwdIdentity,
) -> Result<(), BackendError> {
    match selected {
        registry::NativeProvider::Codex => team_agent_contract::providers::codex::launcher::require_private_primary(cwd)?,
        registry::NativeProvider::GrokBuild | registry::NativeProvider::Kiro | registry::NativeProvider::Cursor | registry::NativeProvider::Pi | registry::NativeProvider::Claude => {}
    }
    Ok(())
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
        tmux: discovery::executable("tmux")?,
        transport_root: "/tmp/preflight-no-write".into(),
    };
    for role in team.roles.iter().filter(|role| !role.paused) {
        let selected = registry::provider(role.provider.as_str())?;
        if !discoveries.contains_key(&role.provider) {
            discoveries.insert(
                role.provider.clone(),
                discovery::discover_runtime(&role.provider, &team.workspace.path)?,
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
        let request = preflight_request(&binding, role, identity, discovered)?;
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

pub(crate) fn require_candidate(binding: &Binding) -> Result<(), BackendError> {
    let candidate = std::env::current_exe()?.canonicalize()?;
    if candidate != binding.candidate || executing_candidate_sha()? != binding.candidate_sha256 { return Err(BackendError::Fence { reason: "candidate_path_or_fingerprint_mismatch" }); }
    Ok(())
}

/// Explicit discard-session may adopt an upgraded binary at the SAME installed
/// path, but only while every native seat is stopped and all operations ended.
/// Ordinary start/resume/MCP calls retain the exact candidate fingerprint fence.
pub(super) fn refresh_quiescent_candidate(workspace: &Path, team: &str, seat: &SeatId) -> Result<(), BackendError> {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    let mut backend = Backend::open(workspace, team)?;
    let candidate = std::env::current_exe()?.canonicalize()?;
    if candidate != backend.binding.candidate { return Err(BackendError::Fence { reason: "candidate_rebind_path_mismatch" }); }
    let fingerprint = fingerprint_file(&candidate, 1024 * 1024 * 1024, Duration::from_secs(10))?;
    if fingerprint == backend.binding.candidate_sha256 { return Ok(()); }
    let root = root_for(workspace, &backend.binding.scope);
    if root.canonicalize()? != root { return Err(BackendError::Fence { reason: "candidate_rebind_root_alias" }); }
    let directory = std::fs::File::open(&root)?;
    if !crate::platform::file_lock::try_lock_once_nonblocking(&directory)? { return Err(Error::Conflict.into()); }
    if !backend.store.unfinished()?.is_empty() { return Err(Error::Conflict.into()); }
    for current in backend.store.seats()? {
        if current.status != SeatStatus::Stopped || backend.store.leased(&current.identity.seat)?
            || current.physical.as_ref().is_some_and(|target| !matches!(
                team_agent_contract::host::process::sample_process(&target.process),
                team_agent_contract::host::process::ProcessState::Exited | team_agent_contract::host::process::ProcessState::Replaced)) {
            return Err(Error::Invalid("stop the native cohort before upgrading its candidate binding").into());
        }
    }
    let path = root.join("framework-binding.json");
    let metadata = std::fs::symlink_metadata(&path)?;
    if !metadata.is_file() || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.nlink() != 1 || metadata.mode() & 0o022 != 0 { return Err(BackendError::Fence { reason: "candidate_rebind_file_ownership" }); }
    let before = std::fs::read(&path)?;
    backend.binding.candidate_sha256 = fingerprint;
    let temporary = root.join(format!("binding-{}.tmp", backend.store.propose_identity(seat)?.instance.as_str()));
    let mut file = std::fs::OpenOptions::new().write(true).create_new(true).mode(metadata.mode() & 0o777).open(&temporary)?;
    let result = (|| -> Result<(), BackendError> {
        file.write_all(&serde_json::to_vec(&backend.binding).map_err(|_| BackendError::Metadata)?)?;
        file.sync_all()?;
        if std::fs::read(&path)? != before { return Err(BackendError::Fence { reason: "candidate_rebind_concurrent_write" }); }
        std::fs::rename(&temporary, &path)?;
        directory.sync_all()?;
        Ok(())
    })();
    if result.is_err() { let _ = std::fs::remove_file(&temporary); }
    result
}

pub(crate) fn validate_delivery_target(binding: &Binding, seat: &SeatRecord, budget: Duration) -> Result<(), BackendError> {
    let target = seat.physical_target()?;
    if target.candidate_sha256 != binding.candidate_sha256
        || seat.evidence_kind != EvidenceKind::Native || !matches!(seat.status, SeatStatus::Starting | SeatStatus::Ready) {
        return Err(BackendError::Fence { reason: "delivery_target_not_current_native_owner" });
    }
    let clock = RealClock::new();
    let mut host = TmuxHost::restore_owned(target.clone(), binding.tmux.clone(),
        settings(binding, registry::provider(seat.provider.as_str())?).limits,
        RealCommandRunner::default(), &clock, budget)?;
    host.validate_target(target, &clock, budget)?;
    Ok(())
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

fn receipt_error(record: OperationRecord) -> BackendError {
    let failure = match (record.failure, record.recovery_failure) {
        (Some(first), Some(recovery)) => Some(format!("{first}; recovery_failure={recovery}")),
        (None, Some(recovery)) => Some(format!("recovery_failure={recovery}")),
        (first, None) => first,
    };
    BackendError::Shutdown { operation: record.id, phase: record.phase,
        outcome: record.outcome, failure: failure.or(record.pending) }
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
    require_candidate(&backend.binding)?;
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
    require_candidate(&backend.binding)?;
    match existing_start(&backend, &role.id)? {
        Some(ExistingStart::Committed(record))
            if backend.store.assert_current(&record.target.identity)?.physical.as_ref().is_some_and(|target|
                target.candidate_sha256 != backend.binding.candidate_sha256 || team_agent_contract::host::process::sample_process(&target.process) == team_agent_contract::host::process::ProcessState::Exited) => {
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

/// No creation receipt, unfinished lease or seat exists for this ID.
pub(super) fn seat_unclaimed(backend: &Backend, seat: &SeatId) -> Result<bool, BackendError> {
    Ok(existing_start(backend, seat)?.is_none())
}

/// Team-level forwarding peers (legacy members and the leader) that every new
/// native seat's framework route depends on. Shared by fresh start and fork.
pub(super) fn publish_framework_routes(backend: &mut Backend, members: &[MemberRoute]) -> Result<(), BackendError> {
    let route = backend.binding.framework().route()?;
    let mut peers: Vec<_> = members
        .iter()
        .filter(|member| member.runtime == RuntimeFamily::Legacy)
        .map(
            |member| team_agent_contract::orchestration::forward::FrameworkPeer {
                recipient: member.id.clone(),
                route: route.clone(),
                provider: member.provider.clone(),
            },
        )
        .collect();
    peers.push(team_agent_contract::orchestration::forward::FrameworkPeer {
        recipient: "leader".into(),
        route: route.clone(),
        provider: "framework".into(),
    });
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
    if let Some(registry::NativeRuntime::Cursor(runtime)) = &discovered.runtime {
        request.preassigned_session = Some(
            team_agent_contract::providers::cursor::native::allocate_chat(
                runtime, &request.paths.cwd, &mut RealCommandRunner::default(),
                team_agent_contract::contract::hooks::ReadBounds {
                    deadline: Duration::from_secs(90), max_output_bytes: 1024 * 1024,
                },
            ).map_err(|error| BackendError::Discovery(DiscoveryError::Catalog(error)))?,
        );
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
    let _ownership = native_seat_lock(workspace, team, &role.id, false)?;
    let cwd = resolve_cwd(workspace)?;
    let scope = scope_for(&cwd, team)?;
    if root_for(&cwd.path, &scope).exists() {
        let mut backend = Backend::open(&cwd.path, team)?;
        require_candidate(&backend.binding)?;
        reconcile_start(&mut backend, &role.id)?;
        match existing_start(&backend, &role.id)? {
            Some(ExistingStart::Committed(record)) => {
                let current = backend.store.assert_current(&record.target.identity)?;
                if current.physical.as_ref().is_some_and(|target|
                    target.candidate_sha256 != backend.binding.candidate_sha256 || team_agent_contract::host::process::sample_process(&target.process) == team_agent_contract::host::process::ProcessState::Exited) {
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
    let discovered = discovery::discover_runtime(&role.provider, workspace)?;
    let cwd = resolve_cwd(workspace)?;
    let scope = scope_for(&cwd, team)?;
    let candidate = std::env::current_exe()?.canonicalize()?;
    let tmux = discovery::executable("tmux")?;
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
    let preflight = preflight_request(&provisional, role, provisional_identity, &discovered)?;
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
    if backend.binding.candidate != provisional.candidate
        || backend.binding.candidate_sha256 != candidate_sha256
    {
        return Err(BackendError::Fence { reason: "startup_candidate_changed" });
    }
    if existing_start(&backend, &role.id)?.is_some() {
        return Err(Error::Conflict.into());
    }
    publish_framework_routes(&mut backend, members)?;
    let identity = backend.store.propose_identity(&role.id)?;
    let server_key = selected.server_key_for(&identity)?;
    write_role(&backend.binding, &identity, role)?;
    let adapter = launch_adapter(&backend.binding, &identity, &role.provider, &discovered)?;
    let hooks = adapter.hooks();
    let mut request = request(&backend.binding, role, identity, &discovered)?;
    if !descriptor.workspace.shared_cwd {
        request.paths.cwd = super::workspace::prepare(&backend.binding, &role.id)?;
    }
    require_private_primary(selected, &request.paths.cwd)?;
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
    let record = Lifecycle {
        store: &mut backend.store,
        host: &mut physical,
        io: &mut io,
    }
    .startup(
        &Adapter {
            descriptor,
            hooks: &hooks,
            catalog: Some(&discovered.catalog),
        },
        &request,
        Routing {
            pane: "unpublished".into(),
            binding_key: format!("binding-{}", request.identity.instance.as_str()),
            server_key: server_key.into(),
        },
        operation,
    )?;
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

pub fn prepare_resume(
    workspace: &Path,
    team: &str,
    seat: &SeatId,
    role: Option<&RoleConfig>,
) -> Result<PreparedResume, BackendError> {
    let backend = Backend::open_readonly(workspace, team)?;
    require_candidate(&backend.binding)?;
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
    if resolve_cwd(&parent.cwd.path)? != parent.cwd {
        return Err(ContractError::Mismatch("resume cwd incarnation").into());
    }
    let candidate = std::env::current_exe()?.canonicalize()?;
    let candidate_sha256 = fingerprint_file(&candidate, 1024 * 1024 * 1024, Duration::from_secs(10))?;
    if backend.binding.candidate != candidate || backend.binding.candidate_sha256 != candidate_sha256 {
        return Err(BackendError::Fence { reason: "resume_candidate_changed" });
    }
    let discovered = discovery::discover_runtime(&role.provider, workspace)?;
    let identity = backend.store.propose_identity(seat)?;
    let adapter = launch_adapter(&backend.binding, &identity, &role.provider, &discovered)?;
    let hooks = adapter.hooks();
    let source = resumable_session(&parent, discovered.runtime.as_ref().and_then(|runtime| match runtime {
        registry::NativeRuntime::Claude(runtime) => Some(runtime.home()), _ => None,
    }))?;
    let mut request = request(&backend.binding, &role, identity, &discovered)?;
    request.paths.cwd = parent.cwd.clone();
    if let Some(source) = source {
        request.operation = Operation::Resume;
        request.resume = Some(ResumeRequest { binding: source, expected_source: parent.identity.clone() });
    } else if matches!(&discovered.runtime, Some(registry::NativeRuntime::Claude(_))) {
        // Pure identity-derived allocation, as in normal Fresh startup. Never run
        // Cursor's native allocation command during this read-only preflight.
        preassign_session(&mut request, &discovered)?;
    }
    let selected = registry::provider(role.provider.as_str())?;
    require_private_primary(selected, &request.paths.cwd)?;
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
    if resumable_session(&parent, prepared.discovered.runtime.as_ref().and_then(|runtime| match runtime {
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
            if before.endpoint != target.endpoint || before.session.as_deref() != Some(target.session.as_str()) {
                return Err(ContractError::Mismatch("resume workspace session").into());
            }
            let stopped = stop(&backend.binding.workspace.path, &backend.binding.team, &prepared.role.id)?;
            if stopped.outcome != Outcome::Committed {
                return Err(receipt_error(stopped));
            }
        }
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

pub fn migrate_if_required(workspace: &Path, team: &str) -> Result<bool, BackendError> {
    let binding = match read_binding(workspace, team) {
        Ok(binding) => binding,
        Err(BackendError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    };
    if !binding.transport_root.join("contract.json").exists() {
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
    let mut binding = read_binding(workspace, team)?;
    let root = root_for(&binding.workspace.path, &binding.scope);
    if root.canonicalize()? != root { return Err(BackendError::Fence { reason: "migration_root_alias" }); }
    let directory = std::fs::File::open(&root)?;
    directory.lock()?;
    // Re-read after acquiring the cutover lock so a concurrent invocation is
    // idempotent and cannot restore an older candidate binding.
    binding = read_binding(workspace, team)?;
    let mut result = crate::importer::contract::migrate(&binding.transport_root, binding.scope.clone(), &binding.endpoint)
        .map_err(|error| BackendError::Migration(error.to_string()))?;
    let candidate = std::env::current_exe()?.canonicalize()?;
    let fingerprint = fingerprint_file(&candidate, 1024 * 1024 * 1024, Duration::from_secs(10))?;
    if binding.candidate != candidate || binding.candidate_sha256 != fingerprint {
        if !binding.transport_root.join("legacy-import.json").is_file() {
            return Err(BackendError::Fence { reason: "migration_requires_legacy_provenance_for_candidate_rebind" });
        }
        let path = root.join("framework-binding.json");
        let metadata = std::fs::symlink_metadata(&path)?;
        if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.uid() != unsafe { libc::geteuid() }
            || metadata.nlink() != 1 || metadata.mode() & 0o022 != 0 { return Err(BackendError::Fence { reason: "migration_binding_ownership" }); }
        let before = std::fs::read(&path)?;
        binding.candidate = candidate;
        binding.candidate_sha256 = fingerprint;
        let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_err(|_| BackendError::Metadata)?.as_nanos();
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
    let authority = Backend::open_readonly(&binding.workspace.path, team)?;
    result["seat_status"] = serde_json::json!(authority.store.seats()?.into_iter().map(|seat| {
        let foreign = seat.physical.as_ref().is_some_and(|target| target.candidate_sha256 != binding.candidate_sha256);
        let live_foreign = foreign && seat.physical.as_ref().is_some_and(|target|
            team_agent_contract::host::process::sample_process(&target.process) == team_agent_contract::host::process::ProcessState::Alive);
        serde_json::json!({"seat":seat.identity.seat,"candidate":if foreign { "foreign" } else { "current" },
            "live_foreign":live_foreign,"next_command":if foreign { Some(restart_command(&binding.workspace.path, team, seat.identity.seat.as_str())) } else { None }})
    }).collect::<Vec<_>>());
    result["team"] = serde_json::json!(team);
    result["candidate"] = serde_json::json!(binding.candidate);
    result["maintenance"] = serde_json::json!("Use only this release for subsequent lifecycle operations");
    Ok(result)
}

pub(crate) fn restart_command(workspace: &Path, team: &str, seat: &str) -> String {
    format!("team-agent restart {} --workspace {} --team {}", crate::cli::shell_quote(seat),
        crate::cli::shell_quote(&workspace.to_string_lossy()), crate::cli::shell_quote(team))
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

        fn save_operation(&self, operation: &OperationRecord) {
            let db = rusqlite::Connection::open(self.binding.transport_root.join("contract.db")).unwrap();
            db.execute("UPDATE contract_operations SET record=?1 WHERE id=?2", rusqlite::params![
                serde_json::to_string(operation).unwrap(), operation.id.as_str()]).unwrap();
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
            let store = ContractStore::create(&self.binding.transport_root, self.binding.scope.clone(), &self.binding.endpoint).unwrap();
            let target = SeatRecord { identity: self.identity.clone(), provider: self.role.provider.clone(),
                native: NativeIdentity { version: "fixture".into(), harness: "fixture".into(), ui: "controlled".into(),
                    platform: Platform::MacOs, executable_sha256: digest(b"fixture") },
                cwd: self.binding.workspace.clone(), endpoint: self.binding.endpoint.clone(), pane: "%1".into(),
                binding_key: "controlled".into(), profile: "fixture".into(), server_key: "fixture".into(),
                evidence_kind: EvidenceKind::Fixture, status, process: None, physical: None, session: None,
                resources: vec![], workspace_resources: vec![], bootstrap_used: false };
            let operation = OperationRecord { id: OperationId::new("controlled-start").unwrap(), kind: TransactionKind::Startup,
                target: target.clone(), parent: None, phase, pending: pending.map(str::to_owned), spawn_observation: None, native_launch: None, resources: vec![],
                effect: team_agent_contract::contract::delivery::DeliveryEffect::NoEffect, outcome, preserved: vec![], failure: None };
            let db = rusqlite::Connection::open(self.binding.transport_root.join("contract.db")).unwrap();
            db.execute("INSERT INTO contract_seats VALUES(?1,?2)", rusqlite::params![self.identity.seat.as_str(), serde_json::to_string(&target).unwrap()]).unwrap();
            db.execute("INSERT INTO contract_generations VALUES(?1,?2)", rusqlite::params![self.identity.seat.as_str(), self.identity.generation.0]).unwrap();
            db.execute("INSERT INTO contract_instances VALUES(?1,?2,?3)", rusqlite::params![self.identity.instance.as_str(), self.identity.seat.as_str(), self.identity.generation.0]).unwrap();
            db.execute("INSERT INTO contract_operations VALUES(?1,?2)", rusqlite::params![operation.id.as_str(), serde_json::to_string(&operation).unwrap()]).unwrap();
            if matches!(outcome, Outcome::Running | Outcome::NeedsRecovery) {
                db.execute("INSERT INTO contract_leases VALUES(?1,?2)", rusqlite::params![self.identity.seat.as_str(), operation.id.as_str()]).unwrap();
            }
            std::fs::write(root_for(&self.binding.workspace.path, &self.binding.scope).join("framework-binding.json"),
                serde_json::to_vec(&self.binding).unwrap()).unwrap();
            drop(db); drop(store);
            Backend::open_readonly(&self.binding.workspace.path, "test").unwrap()
        }
    }
    impl Drop for RoleFixture { fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.root); } }

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
        let db = rusqlite::Connection::open(fixture.binding.transport_root.join("contract.db")).unwrap();
        db.execute("INSERT INTO contract_seats VALUES(?1,?2)", rusqlite::params![parent.identity.seat.as_str(), serde_json::to_string(&parent).unwrap()]).unwrap();
        db.execute("INSERT INTO contract_leases VALUES(?1,?2)", rusqlite::params![parent.identity.seat.as_str(), operation.id.as_str()]).unwrap();
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
        let db = rusqlite::Connection::open(fixture.binding.transport_root.join("contract.db")).unwrap();
        db.execute("INSERT INTO contract_seats VALUES(?1,?2)", rusqlite::params![
            sibling.identity.seat.as_str(), serde_json::to_string(&sibling).unwrap()]).unwrap();
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
        let db = rusqlite::Connection::open(fixture.binding.transport_root.join("contract.db")).unwrap();
        db.execute("INSERT INTO contract_operations VALUES(?1,?2)", rusqlite::params![interrupted.id.as_str(), serde_json::to_string(&interrupted).unwrap()]).unwrap();
        assert!(matches!(existing_start(&backend, &fixture.identity.seat).unwrap(),
            Some(ExistingStart::Blocked(record)) if record == interrupted));
        assert_eq!(backend.store.seat(&fixture.identity.seat).unwrap().unwrap().identity, fixture.identity);
    }

    #[test]
    fn startup_receipt_missing_committed_seat_refuses_but_compensated_allows_fresh_proposal() {
        for outcome in [Outcome::Committed, Outcome::Compensated] {
            let mut fixture = RoleFixture::new();
            let backend = fixture.canonical(SeatStatus::Stopped, Phase::F5Receipt, outcome, None);
            let db = rusqlite::Connection::open(fixture.binding.transport_root.join("contract.db")).unwrap();
            db.execute("DELETE FROM contract_seats", []).unwrap();
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
            let store = ContractStore::create(&fixture.binding.transport_root, fixture.binding.scope.clone(), &fixture.binding.endpoint).unwrap();
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
            let db = rusqlite::Connection::open(fixture.binding.transport_root.join("contract.db")).unwrap();
            db.execute("INSERT INTO contract_seats(seat,record) VALUES(?1,?2)",
                rusqlite::params![fixture.identity.seat.as_str(), serde_json::to_string(&seat).unwrap()]).unwrap();
            db.execute("INSERT INTO contract_generations(seat,generation) VALUES(?1,1)", [fixture.identity.seat.as_str()]).unwrap();
            drop(db);
            std::fs::write(root_for(&fixture.binding.workspace.path, &fixture.binding.scope).join("framework-binding.json"),
                serde_json::to_vec(&fixture.binding).unwrap()).unwrap();
            let stale_running = serde_json::json!({"agents":{"worker":{"provider":provider,"status":"running"}}});
            assert!(prepare_restart(&fixture.root, "test", &directory, &stale_running).unwrap().is_empty());
            assert_eq!(store.seat(&fixture.identity.seat).unwrap(), Some(seat));
            assert_eq!(store.propose_identity(&fixture.identity.seat).unwrap().generation, Generation(2));
        }
    }

    #[test]
    fn explicit_discard_can_refresh_only_a_quiescent_same_path_candidate() {
        for status in [SeatStatus::Ready, SeatStatus::Stopped] {
            let mut fixture = RoleFixture::new();
            fixture.binding.candidate = std::env::current_exe().unwrap().canonicalize().unwrap();
            fixture.binding.candidate_sha256 = Digest([0; 32]);
            let _backend = fixture.canonical(status, Phase::F5Receipt, Outcome::Committed, None);
            let path = root_for(&fixture.root, &fixture.binding.scope).join("framework-binding.json");
            let before = std::fs::read(&path).unwrap();
            assert!(require_candidate(&fixture.binding).is_err());
            let result = refresh_quiescent_candidate(&fixture.root, "test", &fixture.identity.seat);
            if status == SeatStatus::Ready {
                assert!(result.is_err());
                assert_eq!(std::fs::read(&path).unwrap(), before);
            } else {
                result.unwrap();
                require_candidate(&Backend::open_readonly(&fixture.root, "test").unwrap().binding).unwrap();
                fixture.binding.candidate = fixture.root.join("foreign-candidate");
                std::fs::write(&path, serde_json::to_vec(&fixture.binding).unwrap()).unwrap();
                assert!(refresh_quiescent_candidate(&fixture.root, "test", &fixture.identity.seat).is_err());
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
        let db = rusqlite::Connection::open(fixture.binding.transport_root.join("contract.db")).unwrap();
        let outbox: i64 = db.query_row("select count(*) from contract_outbox", [], |row| row.get(0)).unwrap();
        assert_eq!(outbox, 0, "no synthetic task or message-delivery ledger row");
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
        let db = rusqlite::Connection::open(fixture.binding.transport_root.join("contract.db")).unwrap();
        db.execute("update contract_operations set record=json_set(record, '$.failure', ?1)",
            ["session observation deferred: capture-failed; tmux capture: Deadline"]).unwrap();
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
}
