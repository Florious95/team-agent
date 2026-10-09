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
    tmux::HostLimits,
};
use team_agent_contract::kiro::{KiroAdapter, McpStdio, KIRO_DESCRIPTOR};
use team_agent_contract::orchestration::{lifecycle::*, physical::*, store::*, Error};
use thiserror::Error as ThisError;

use super::{
    config::{MemberRoute, RoleConfig, RuntimeFamily},
    discovery::{self, Discovery, DiscoveryError},
    forward::FrameworkContext,
};

#[derive(Debug, ThisError)]
pub enum BackendError {
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

pub(super) fn settings(binding: &Binding) -> PhysicalSettings {
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
        action_budget: Duration::from_secs(30),
        freshness: Duration::from_secs(2),
        journal_bytes: 2 * 1024 * 1024,
    }
}
pub(super) fn adapter(
    binding: &Binding,
    identity: &InstanceIdentity,
) -> Result<KiroAdapter, BackendError> {
    Ok(KiroAdapter::new(McpStdio {
        executable: binding.candidate.clone(),
        arguments: vec![
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
        environment: BTreeMap::new(),
    })?)
}
fn request(
    binding: &Binding,
    role: &RoleConfig,
    identity: InstanceIdentity,
    discovered: &Discovery,
) -> Result<LaunchRequest, BackendError> {
    let profiles = KIRO_DESCRIPTOR
        .input
        .profiles
        .require("Kiro input profiles")?;
    let mut matches = profiles.iter().filter(|profile| {
        profile.matches_native(&discovered.native)
            && profile.channel == Channel::Tmux
            && profile.operations.contains(&Operation::FirstBusiness)
    });
    let profile = matches.next().ok_or(ContractError::ProfileUnavailable)?;
    if matches.next().is_some() {
        return Err(ContractError::AmbiguousProfile.into());
    }
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

/// Team-wide native discovery/admission before the shared launcher creates any
/// seats (including legacy peers). Bounded read-only native commands run; no
/// runtime store, worker config, private socket, or worker process is created.
pub fn preflight(team: &super::config::TeamConfig) -> Result<(), BackendError> {
    let discovered = discovery::discover(&team.workspace.path)?;
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
    for role in &team.roles {
        let identity = InstanceIdentity {
            scope: team.scope.clone(),
            seat: role.id.clone(),
            instance: InstanceId::new("preflight")?,
            generation: Generation(1),
        };
        let adapter = adapter(&binding, &identity)?;
        let hooks = adapter.hooks();
        let request = request(&binding, role, identity, &discovered)?;
        let resolved = resolve_launch(
            &KIRO_DESCRIPTOR,
            &hooks,
            &request,
            Some(&discovered.catalog),
        )?;
        validate_launch_plan(&KIRO_DESCRIPTOR, &resolved, &adapter.plan(&resolved)?)?;
    }
    Ok(())
}

/// This first wiring is fresh-only. Resume/fork must use their typed contracts;
/// missing evidence cannot be bypassed by silently launching another fresh seat.
pub fn start(
    workspace: &Path,
    team: &str,
    role: &RoleConfig,
    members: &[MemberRoute],
) -> Result<(Binding, OperationRecord), BackendError> {
    let discovered = discovery::discover(workspace)?;
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
    let probe_adapter = adapter(&provisional, &provisional_identity)?;
    let probe_hooks = probe_adapter.hooks();
    let preflight = request(&provisional, role, provisional_identity, &discovered)?;
    let preflight = resolve_launch(
        &KIRO_DESCRIPTOR,
        &probe_hooks,
        &preflight,
        Some(&discovered.catalog),
    )?;
    probe_adapter.plan(&preflight)?;
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
    if backend.store.seat(&role.id)?.is_some() {
        return Err(Error::Conflict.into());
    }
    // A private immutable source snapshot binds model/effort and all four exact
    // instruction layers to this startup. Never print its prompt body.
    use std::os::unix::fs::OpenOptionsExt;
    let snapshot = serde_json::to_vec(role).map_err(|_| BackendError::Metadata)?;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(
            root_for(&backend.binding.workspace.path, &backend.binding.scope)
                .join(format!("role-{}.json", role.id.as_str())),
        )?;
    file.write_all(&snapshot)?;
    file.sync_all()?;
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
    let adapter = adapter(&backend.binding, &identity)?;
    let hooks = adapter.hooks();
    let request = request(&backend.binding, role, identity, &discovered)?;
    let resolved = resolve_launch(
        &KIRO_DESCRIPTOR,
        &hooks,
        &request,
        Some(&discovered.catalog),
    )?;
    let plan = adapter.plan(&resolved)?;
    let operation = OperationId::new(format!("start-{}", request.identity.instance.as_str()))?;
    let mut io = ScopedMaterializer::new(
        &KIRO_DESCRIPTOR,
        &resolved,
        &plan,
        operation.clone(),
        RealCommandRunner::default(),
    )?;
    let clock = RealClock::new();
    let mut physical = PhysicalRuntime::new(
        Registration {
            descriptor: &KIRO_DESCRIPTOR,
            hooks: &hooks,
            controls: team_agent_contract::kiro::interaction::CONTROLS,
        },
        settings(&backend.binding),
        operation.clone(),
        &clock,
    )?;
    physical.evidence = policy_evidence(&request.native, candidate_sha256);
    let record = Lifecycle {
        store: &mut backend.store,
        host: &mut physical,
        io: &mut io,
    }
    .startup(
        &Adapter {
            descriptor: &KIRO_DESCRIPTOR,
            hooks: &hooks,
            catalog: Some(&discovered.catalog),
        },
        &request,
        Routing {
            pane: "unpublished".into(),
            binding_key: format!("binding-{}", request.identity.instance.as_str()),
            server_key: "team".into(),
        },
        operation,
    )?;
    Ok((backend.binding, record))
}

pub(super) fn read_role(binding: &Binding, id: &SeatId) -> Result<RoleConfig, BackendError> {
    let path = root_for(&binding.workspace.path, &binding.scope)
        .join(format!("role-{}.json", id.as_str()));
    let metadata = std::fs::symlink_metadata(&path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(BackendError::Metadata);
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(4 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 4 * 1024 * 1024 {
        return Err(BackendError::Metadata);
    }
    let role: RoleConfig = serde_json::from_slice(&bytes).map_err(|_| BackendError::Metadata)?;
    if &role.id != id
        || role.provider.as_str() != "kiro"
        || digest(role.prompt.text.as_bytes()) != role.prompt.sha256
    {
        return Err(BackendError::Metadata);
    }
    Ok(role)
}

pub(super) fn policy_evidence(
    native: &NativeIdentity,
    candidate_sha256: Digest,
) -> Vec<CapabilityEvidence> {
    team_agent_contract::kiro::interaction::PROFILES
        .iter()
        .filter(|profile| profile.matches_native(native))
        .flat_map(|profile| {
            // Bind this observed executable and candidate to the stable recipe.
            // This is not the historical R0 receipt, nor a business/MCP PASS.
            let binding_sha256 = digest(format!(
                "kiro-runtime-policy-v1\n{native:?}\n{}\n{:?}\n{candidate_sha256:?}",
                profile.id, profile.policy_sha256,
            ).as_bytes());
            profile
                .operations
                .iter()
                .map(move |operation| CapabilityEvidence {
                    provider: ProviderId::new("kiro").expect("static provider"),
                    native: native.clone(),
                    profile_id: profile.id.into(),
                    policy_sha256: profile.policy_sha256,
                    channel: profile.channel,
                    operation: *operation,
                    kind: EvidenceKind::Native,
                    candidate_sha256,
                    evidence_sha256: binding_sha256,
                })
        })
        .collect()
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
    let adapter = adapter(&backend.binding, &target.identity)?;
    let hooks = adapter.hooks();
    let clock = RealClock::new();
    let mut physical = PhysicalRuntime::new(
        Registration {
            descriptor: &KIRO_DESCRIPTOR,
            hooks: &hooks,
            controls: &[],
        },
        settings(&backend.binding),
        operation.clone(),
        &clock,
    )?;
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
    fn policy_bindings_track_runtime_native_identity_not_a_release_receipt() {
        let mut native = NativeIdentity {
            version: "2.28.0".into(),
            harness: "v2".into(),
            ui: "tui".into(),
            platform: Platform::MacOs,
            executable_sha256: digest(b"old image"),
        };
        let candidate = digest(b"candidate");
        let old = policy_evidence(&native, candidate);
        assert!(!old.is_empty());
        native.version = "2.29.0".into();
        native.executable_sha256 = digest(b"updated image");
        let new = policy_evidence(&native, candidate);
        assert_eq!(new.len(), old.len());
        for (old, new) in old.iter().zip(&new) {
            assert_eq!(new.native, native);
            assert_eq!(new.candidate_sha256, candidate);
            assert_eq!(new.policy_sha256, old.policy_sha256);
            assert_ne!(new.evidence_sha256, old.evidence_sha256);
        }
        native.harness = "v3".into();
        assert!(policy_evidence(&native, candidate).is_empty());
    }
}
