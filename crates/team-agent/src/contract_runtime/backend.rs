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
    pub store: ContractStore,
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

impl Backend {
    pub fn open(workspace: &Path, team: &str) -> Result<Self, BackendError> {
        Self::open_mode(workspace, team, false)
    }
    pub fn open_readonly(workspace: &Path, team: &str) -> Result<Self, BackendError> {
        Self::open_mode(workspace, team, true)
    }
    fn open_mode(workspace: &Path, team: &str, readonly: bool) -> Result<Self, BackendError> {
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
        let store = if readonly {
            ContractStore::open_readonly(&binding.transport_root, scope, &binding.endpoint)?
        } else {
            ContractStore::open(&binding.transport_root, scope, &binding.endpoint)?
        };
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
        let store = ContractStore::create(&transport_root, scope.clone(), &endpoint(&scope))?;
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
fn workspace_tmux(
    binding: &Binding,
    seat: &SeatId,
    claim: Option<&team_agent_contract::host::transport::TargetReceipt>,
) -> Result<(SharedTmux, crate::tmux_backend::TmuxBackend, String), BackendError> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt};
    let selected = crate::state::selector::resolve_active_team_readonly(
        &binding.workspace.path, Some(&binding.team), crate::state::selector::SelectorMode::RuntimeOnly,
    ).map_err(|_| ContractError::Mismatch("workspace topology scope"))?;
    if selected.run_workspace != binding.workspace.path || selected.team_key != binding.team {
        return Err(Error::Fence.into());
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
                    if session.replace(id.to_owned()).is_some() { return Err(Error::Fence.into()); }
                }
            }
        } else if !(result.end == CommandEnd::Exited && result.exit_code == Some(1) && result.child_reaped
            && result.stdout.is_empty() && result.stderr == format!("no server running on {}\n", endpoint.display()).as_bytes())
        { return Err(ContractError::Mismatch("workspace server observation unavailable").into()); }
        if workspace_socket(&endpoint)? != socket { return Err(Error::Fence.into()); }
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

/// Projection is saved first by the caller. Only an exact current target that
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
    { return Err(Error::Fence.into()); }
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
fn launch_adapter(
    binding: &Binding,
    identity: &InstanceIdentity,
    provider: &ProviderId,
    discovered: &Discovery,
) -> Result<NativeAdapter, BackendError> {
    Ok(adapter(binding, identity, provider)?.with_launch_context(
        binding.workspace.path.clone(), discovered.runtime.clone(),
    )?)
}
fn request(
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
    if registry::provider(role.provider.as_str())? == registry::NativeProvider::GrokBuild {
        // Pure H2 only: this is a proposed private path, not a filesystem receipt.
        request.paths.cwd = team_agent_contract::contract::session::CwdIdentity {
            path: binding.transport_root.join("workspaces").join(role.id.as_str()),
            identity: digest(b"effect-free-planned-private-cwd"),
        };
    }
    Ok(request)
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
    let unfinished: Vec<_> = backend.store.unfinished()?.into_iter().filter(|record| {
        &record.target.identity.seat == seat
            || record.parent.as_ref().is_some_and(|parent| &parent.identity.seat == seat)
    }).collect();
    if unfinished.len() > 1 { return Err(Error::Fence.into()); }
    if let Some(record) = unfinished.into_iter().next() {
        if record.target.identity.scope != backend.binding.scope { return Err(Error::Fence.into()); }
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
            Some(_) => Err(Error::Fence.into()),
        };
    };
    if current.identity.scope != backend.binding.scope { return Err(Error::Fence.into()); }
    let record = backend.store.creation_receipt(seat, Some(&current.identity))?.ok_or(Error::Fence)?;
    if record.outcome != Outcome::Committed || record.phase != Phase::F5Receipt || record.pending.is_some() {
        return Ok(Some(ExistingStart::Blocked(record)));
    }
    Ok(Some(if current.status == SeatStatus::Stopped { ExistingStart::Resume(current) }
        else { ExistingStart::Committed(record) }))
}

fn require_candidate(binding: &Binding) -> Result<(), BackendError> {
    let candidate = std::env::current_exe()?.canonicalize()?;
    if candidate != binding.candidate || fingerprint_file(&candidate, 1024 * 1024 * 1024,
        Duration::from_secs(10))? != binding.candidate_sha256 { return Err(Error::Fence.into()); }
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
    BackendError::Shutdown { operation: record.id, phase: record.phase,
        outcome: record.outcome, failure: record.failure.or(record.pending) }
}

/// Plan only against this selected store and exact row (or an unbound seed).
/// A stale parent can be repaired only by its actual creation receipt, never
/// by selecting the greatest generation or adopting a discovered process.
pub(crate) fn start_needs_reconciliation(workspace: &Path, team: &str, seat: &SeatId, row: &serde_json::Value) -> Result<bool, BackendError> {
    let backend = Backend::open_readonly(workspace, team)?;
    require_candidate(&backend.binding)?;
    let class = existing_start(&backend, seat)?;
    let current = backend.store.seat(seat)?;
    if current.as_ref().is_some_and(|seat| row.get("provider").and_then(serde_json::Value::as_str) != Some(seat.provider.as_str())) {
        return Err(Error::Fence.into());
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
        _ => None,
    };
    if !seed && !current.as_ref().is_some_and(|seat| matches(&seat.identity))
        && !record.and_then(|record| record.parent.as_ref()).is_some_and(|parent| matches(&parent.identity)
            && row.get("session_id").and_then(serde_json::Value::as_str)
                == parent.session.as_ref().map(|session| session.native_session.as_str()))
    { return Err(Error::Fence.into()); }
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
                team_agent_contract::host::process::sample_process(&target.process) == team_agent_contract::host::process::ProcessState::Exited) => {
            prepare_resume(workspace, team, &role.id, Some(role))?;
        },
        Some(ExistingStart::Committed(record) | ExistingStart::FinishReceipt(record)) => { validate_existing(&backend, role, &record)?; },
        Some(ExistingStart::Resume(_)) => { prepare_resume(workspace, team, &role.id, Some(role))?; },
        Some(ExistingStart::Pending(record) | ExistingStart::Blocked(record)) => return Err(receipt_error(record)),
        None => return Ok(true), // Caller must run full-team fresh preflight before writes.
    }
    Ok(false)
}

fn finish_receipt(backend: &mut Backend, record: OperationRecord) -> Result<OperationRecord, BackendError> {
    let selected = registry::provider(record.target.provider.as_str())?;
    let adapter = adapter(&backend.binding, &record.target.identity, &record.target.provider)?;
    let hooks = adapter.hooks();
    let clock = RealClock::new();
    let mut host = PhysicalRuntime::new(Registration { descriptor: selected.descriptor(), hooks: &hooks,
        controls: selected.controls(), startup: selected.startup(), session_panel: selected.session_panel() },
        settings(&backend.binding, selected), record.id.clone(), &clock)?;
    Ok(Lifecycle { store: &mut backend.store, host: &mut host, io: &mut NoNewIo }.recover(&record.id)?)
}

/// Fresh startup is distinct from resume. An existing generation is never
/// silently replaced with a new native session.
pub fn start(
    workspace: &Path,
    team: &str,
    role: &RoleConfig,
    members: &[MemberRoute],
) -> Result<(Binding, OperationRecord), BackendError> {
    let cwd = resolve_cwd(workspace)?;
    let scope = scope_for(&cwd, team)?;
    if root_for(&cwd.path, &scope).exists() {
        let mut backend = Backend::open(&cwd.path, team)?;
        require_candidate(&backend.binding)?;
        match existing_start(&backend, &role.id)? {
            Some(ExistingStart::Committed(record)) => {
                let current = backend.store.assert_current(&record.target.identity)?;
                if current.physical.as_ref().is_some_and(|target|
                    team_agent_contract::host::process::sample_process(&target.process) == team_agent_contract::host::process::ProcessState::Exited) {
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
                if current.provider != role.provider { return Err(Error::Fence.into()); }
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
        return Err(Error::Fence.into());
    }
    if existing_start(&backend, &role.id)?.is_some() {
        return Err(Error::Conflict.into());
    }
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
    let identity = backend.store.propose_identity(&role.id)?;
    let server_key = selected.server_key_for(&identity)?;
    write_role(&backend.binding, &identity, role)?;
    let adapter = launch_adapter(&backend.binding, &identity, &role.provider, &discovered)?;
    let hooks = adapter.hooks();
    let mut request = request(&backend.binding, role, identity, &discovered)?;
    if !descriptor.workspace.shared_cwd {
        request.paths.cwd = super::workspace::prepare(&backend.binding, &role.id)?;
    }
    if selected == registry::NativeProvider::GrokBuild {
        team_agent_contract::providers::grok_build::native::require_private_primary(&request.paths.cwd)?;
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
    let resolved = resolve_launch(
        descriptor,
        &hooks,
        &request,
        Some(&discovered.catalog),
    )?;
    let plan = adapter.plan(&resolved)?;
    validate_launch_plan(descriptor, &resolved, &plan)?;
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
    Ok((backend.binding, record))
}

/// A fully admitted resume proposal. It is still effect-free: no parent stop,
/// session allocation, snapshot write or config replacement has occurred.
pub struct PreparedResume {
    binding: Binding,
    role: RoleConfig,
    discovered: Discovery,
    request: LaunchRequest,
}

impl PreparedResume {
    pub(crate) fn seat(&self) -> &SeatId { &self.request.identity.seat }
    pub(crate) fn session(&self) -> Option<&str> {
        self.request.resume.as_ref().map(|resume| resume.binding.native_session.as_str())
    }
}

pub(crate) fn prepare_restart(
    workspace: &Path,
    team: &str,
    directory: &Path,
    state: &serde_json::Value,
) -> Result<Vec<PreparedResume>, BackendError> {
    let config = super::framework::team_config(workspace, directory, team)?.ok_or(BackendError::Metadata)?;
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

pub fn prepare_resume(
    workspace: &Path,
    team: &str,
    seat: &SeatId,
    role: Option<&RoleConfig>,
) -> Result<PreparedResume, BackendError> {
    let backend = Backend::open_readonly(workspace, team)?;
    let parent = backend.store.seat(seat)?.ok_or(BackendError::Metadata)?;
    if parent.status == SeatStatus::Unknown {
        return Err(Error::Fence.into());
    }
    let role = match role {
        Some(role) => role.clone(),
        None => read_role(&backend.binding, &parent.identity)?,
    };
    if role.id != parent.identity.seat || role.provider != parent.provider {
        return Err(ContractError::Mismatch("resume role provider").into());
    }
    let source = parent.session.clone().ok_or(ContractError::Invalid("native resume binding required"))?;
    if resolve_cwd(&source.cwd.path)? != source.cwd {
        return Err(ContractError::Mismatch("resume cwd incarnation").into());
    }
    let candidate = std::env::current_exe()?.canonicalize()?;
    let candidate_sha256 = fingerprint_file(&candidate, 1024 * 1024 * 1024, Duration::from_secs(10))?;
    if backend.binding.candidate != candidate || backend.binding.candidate_sha256 != candidate_sha256 {
        return Err(Error::Fence.into());
    }
    let discovered = discovery::discover_runtime(&role.provider, workspace)?;
    let identity = backend.store.propose_identity(seat)?;
    let adapter = launch_adapter(&backend.binding, &identity, &role.provider, &discovered)?;
    let hooks = adapter.hooks();
    let mut request = request(&backend.binding, &role, identity, &discovered)?;
    request.operation = Operation::Resume;
    request.paths.cwd = source.cwd.clone();
    request.resume = Some(ResumeRequest {
        binding: source, expected_source: parent.identity,
    });
    let selected = registry::provider(role.provider.as_str())?;
    if selected == registry::NativeProvider::GrokBuild {
        team_agent_contract::providers::grok_build::native::require_private_primary(&request.paths.cwd)?;
    }
    let resolved = resolve_launch(selected.descriptor(), &hooks, &request, Some(&discovered.catalog))?;
    validate_launch_plan(selected.descriptor(), &resolved, &adapter.plan(&resolved)?)?;
    Ok(PreparedResume { binding: backend.binding, role, discovered, request })
}

pub fn resume(prepared: PreparedResume) -> Result<OperationRecord, BackendError> {
    let mut backend = Backend::open(&prepared.binding.workspace.path, &prepared.binding.team)?;
    if serde_json::to_vec(&backend.binding).map_err(|_| BackendError::Metadata)?
        != serde_json::to_vec(&prepared.binding).map_err(|_| BackendError::Metadata)?
    {
        return Err(Error::Fence.into());
    }
    let source = prepared.request.resume.as_ref().ok_or(BackendError::Metadata)?;
    let parent = backend.store.assert_current(&source.expected_source)?;
    if parent.session.as_ref() != Some(&source.binding) {
        return Err(Error::Fence.into());
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
                return Err(BackendError::Shutdown { operation: stopped.id, phase: stopped.phase,
                    outcome: stopped.outcome, failure: stopped.failure });
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
    physical.evidence = policy_evidence(&prepared.role.provider, &prepared.request.native, backend.binding.candidate_sha256)?;
    physical.workspace_tmux = workspace_topology;
    let record = Lifecycle { store: &mut backend.store, host: &mut physical, io: &mut io }.startup(
        &Adapter { descriptor, hooks: &hooks, catalog: Some(&prepared.discovered.catalog) },
        &prepared.request,
        Routing {
            pane: "unpublished".into(),
            binding_key: format!("binding-{}", prepared.request.identity.instance.as_str()),
            server_key: selected.server_key_for(&prepared.request.identity)?,
        },
        operation,
    )?;
    Ok(record)
}

fn role_path(binding: &Binding, identity: &InstanceIdentity) -> PathBuf {
    root_for(&binding.workspace.path, &binding.scope).join(format!(
        "role-{}-{}.json", identity.seat.as_str(), identity.instance.as_str(),
    ))
}
fn write_role(binding: &Binding, identity: &InstanceIdentity, role: &RoleConfig) -> Result<(), BackendError> {
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

pub fn stop(workspace: &Path, team: &str, seat: &SeatId) -> Result<OperationRecord, BackendError> {
    let mut backend = Backend::open(workspace, team)?;
    let target = backend
        .store
        .seat(seat)?
        .ok_or(Error::Invalid("unknown native seat"))?;
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
    physical.evidence = policy_evidence(&target.provider, &target.native, backend.binding.candidate_sha256)?;
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
            let root = std::env::temp_dir().join(format!("contract-role-{}-{}", std::process::id(),
                std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
            std::fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
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
                target: target.clone(), parent: None, phase, pending: pending.map(str::to_owned), resources: vec![],
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
