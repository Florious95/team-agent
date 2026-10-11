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
use team_agent_contract::orchestration::{lifecycle::*, physical::*, store::*, Error};
use thiserror::Error as ThisError;

use super::{
    config::{MemberRoute, RoleConfig, RuntimeFamily},
    discovery::{self, Discovery, DiscoveryError},
    forward::FrameworkContext,
    registry::{self, NativeAdapter},
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

/// Team-wide native discovery/admission before the shared launcher creates any
/// seats (including legacy peers). Bounded read-only native commands run; no
/// runtime store, worker config, private socket, or worker process is created.
pub fn preflight(team: &super::config::TeamConfig) -> Result<(), BackendError> {
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
    for role in &team.roles {
        let selected = registry::provider(role.provider.as_str())?;
        if !discoveries.contains_key(&role.provider) {
            discoveries.insert(
                role.provider.clone(),
                discovery::discover(&role.provider, &team.workspace.path)?,
            );
        }
        let discovered = discoveries.get(&role.provider).ok_or(BackendError::Metadata)?;
        let identity = InstanceIdentity {
            scope: team.scope.clone(),
            seat: role.id.clone(),
            instance: InstanceId::new("preflight")?,
            generation: Generation(1),
        };
        let adapter = adapter(&binding, &identity, &role.provider)?;
        let hooks = adapter.hooks();
        let request = request(&binding, role, identity, discovered)?;
        let resolved = resolve_launch(
            selected.descriptor(),
            &hooks,
            &request,
            Some(&discovered.catalog),
        )?;
        validate_launch_plan(selected.descriptor(), &resolved, &adapter.plan(&resolved)?)?;
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
    let selected = registry::provider(role.provider.as_str())?;
    let descriptor = selected.descriptor();
    let discovered = discovery::discover(&role.provider, workspace)?;
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
    let probe_adapter = adapter(&provisional, &provisional_identity, &role.provider)?;
    let probe_hooks = probe_adapter.hooks();
    let preflight = request(&provisional, role, provisional_identity, &discovered)?;
    let preflight = resolve_launch(
        descriptor,
        &probe_hooks,
        &preflight,
        Some(&discovered.catalog),
    )?;
    let preflight_plan = probe_adapter.plan(&preflight)?;
    validate_launch_plan(descriptor, &preflight, &preflight_plan)?;
    let server_key = selected.server_key()?;
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
    let adapter = adapter(&backend.binding, &identity, &role.provider)?;
    let hooks = adapter.hooks();
    let request = request(&backend.binding, role, identity, &discovered)?;
    let resolved = resolve_launch(
        descriptor,
        &hooks,
        &request,
        Some(&discovered.catalog),
    )?;
    let plan = adapter.plan(&resolved)?;
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
        },
        settings(&backend.binding),
        operation.clone(),
        &clock,
    )?;
    physical.evidence = policy_evidence(&role.provider, &request.native, candidate_sha256)?;
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
    fn cursor_metadata_and_unverified_recipe_never_mint_native_evidence() {
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
        assert!(matches!(policy_evidence(&provider, &native, candidate),
            Err(ContractError::Unverified { field: "native submit policy", .. })));
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
            bypass: true, body: "role".into(),
            prompt: super::super::prompt::Prompt { text: "prompt".into(), sha256: digest(b"prompt"), sources: vec![] },
        };
        let identity = InstanceIdentity { scope, seat, instance: InstanceId::new("preflight").unwrap(), generation: Generation(1) };
        let native = NativeIdentity {
            version: "2026.09.29-a1b2c3".into(), harness: "cursor-launcher".into(),
            ui: "metadata".into(), platform: Platform::MacOs, executable_sha256: digest(b"launcher"),
        };
        let mut discovery = Discovery {
            engine: "/controlled/cursor-agent".into(), native: native.clone(),
            catalog: CatalogObservation { provider, native,
                schema: team_agent_contract::providers::cursor::native::CATALOG_SOURCE.schema.into(), models: vec![] },
        };
        assert!(matches!(request(&binding, &role, identity.clone(), &discovery),
            Err(BackendError::Contract(ContractError::Unverified { field: "native subscription launch", .. }))));
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
