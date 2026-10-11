//! Cursor native worker: explicit Node/entry identity, native chat allocation,
//! private project MCP, and first-business terminal prompt delivery. Declared
//! recipes are not substitutes for candidate-bound live acceptance evidence.
pub mod interaction;
pub mod native;

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::path::PathBuf;

use crate::contract::{
    descriptor::*, fork::*, hooks::*, plan::*, session::{CaptureOrigin, ResumeBinding, SessionStorage}, types::*,
};

pub const UNVERIFIED: Reason = Reason {
    code: "cursor-native-admission-missing",
    message: "Cursor recon does not establish this operation; no Kiro or legacy fallback is allowed",
};
const EFFORT: Reason = Reason {
    code: "cursor-effort-is-native-model-id",
    message: "Select an exact catalog model ID; a separate effort flag or suffix rewrite is not verified",
};
const MCP: Reason = Reason {
    code: "cursor-team-mcp-unverified",
    message: "Owned stdio handshake alone does not prove the full native Team-tool registry or round-trip consumption",
};

pub static CURSOR_DESCRIPTOR: ProviderDescriptor = ProviderDescriptor {
    identity: IdentityFacet {
        id: "cursor",
        display_name: "Cursor Agent (contract)",
        binary: "cursor-agent",
        // cursor_agent/agent remain legacy routes; this is not a state migration.
        aliases: &[],
        leader: Support::Unverified(UNVERIFIED),
    },
    model: ModelFacet {
        selection: Support::Supported(ModelSelection::ExactCatalogId),
        omitted: OmittedModel::Reject,
        catalog: Support::Supported(native::CATALOG_SOURCE),
    },
    effort: EffortFacet {
        admission: [const { EffortAdmission::Reject(EFFORT) }; 6],
        carrier: Support::Unverified(EFFORT),
        inherit_team_default: false,
        model_dependent: true,
    },
    auth: AuthFacet {
        subscription: Support::Supported(AuthMechanism::NativeExistingSession),
        official_api: Support::Unverified(UNVERIFIED),
        compatible_api: Support::Unverified(UNVERIFIED),
    },
    bypass: BypassFacet { intent: Support::Supported(BypassPolicy {
        enabled_arguments: &["--force"], requires_startup_consent: false,
    }) },
    prompt: PromptFacet { carrier: Support::Supported(PromptCarrier::RuntimeBridge) },
    mcp: McpFacet {
        carrier: Support::Supported(McpCarrier::ScopedConfig),
        scope: ResourceScope::WorkingDirectory,
        tools: &TEAM_TOOLS,
        // Runtime admission refuses an existing global MCP file; actual launch
        // requires a private cwd. This is not inference from --approve-mcps alone.
        excludes_ambient_configuration: true,
    },
    tool_names: ToolNameFacet { naming: Support::Supported(ToolNaming::StaticServer { key: "team" }) },
    session: SessionFacet {
        fresh: FreshSession::Preassigned,
        // Native allocation and populated print-process exit → exact-ID recall
        // are measured. Team TTY recovery remains a live gate; the binding origin
        // ExplicitResumeLaunch is deliberately not current-session TUI echo.
        resume: Support::Supported(ResumeMode::ExactId),
    },
    fork: ForkFacet {
        in_window: Support::Unverified(UNVERIFIED),
        full_snapshot: Support::Unverified(UNVERIFIED),
        native_new_seat: Support::Unverified(UNVERIFIED),
        allowed_auth: &[],
    },
    input: InputFacet { profiles: Support::Supported(&interaction::PROFILES) },
    startup: StartupFacet {
        interactive: true,
        authorized_consent: Support::Unverified(UNVERIFIED),
    },
    probe_sources: ProbeSourcesFacet {
        process: Support::Supported(ProbeSource::Host),
        pane: Support::Supported(ProbeSource::NativeSurface),
        server: Support::Supported(ProbeSource::ServerLifecycle),
        client_binding: Support::Unverified(MCP),
        round_trip: Support::Unverified(MCP),
        semantic: Support::Supported(ProbeSource::NativeSurface),
    },
    workspace: WorkspaceFacet {
        config_scope: ResourceScope::WorkingDirectory,
        shared_cwd: false,
        requires_materialization_lease: true,
    },
    teardown: TeardownFacet {
        resources: &[
            ResourcePolicy { kind: ResourceKind::McpConfig, disposition: ResourceDisposition::OwnedRemovable },
            ResourcePolicy { kind: ResourceKind::Prompt, disposition: ResourceDisposition::OwnedRemovable },
            ResourcePolicy { kind: ResourceKind::SessionBacking, disposition: ResourceDisposition::OwnedPreserved },
            ResourcePolicy { kind: ResourceKind::GlobalSettings, disposition: ResourceDisposition::Forbidden },
            ResourcePolicy { kind: ResourceKind::NativeDatabase, disposition: ResourceDisposition::Forbidden },
        ],
    },
};

// No Debug: candidate arguments/environment are not diagnostics.
pub struct McpStdio {
    pub executable: PathBuf,
    pub arguments: Vec<String>,
    pub environment: BTreeMap<String, String>,
}
pub struct CursorAdapter {
    mcp: McpStdio,
    runtime: Option<native::RuntimeInstallation>,
    project: Option<PathBuf>,
}
impl CursorAdapter {
    pub fn new(mcp: McpStdio) -> Result<Self, ContractError> {
        require_absolute(&mcp.executable, "Cursor MCP executable")?;
        if mcp.executable.to_str().is_none() || mcp.arguments.iter().any(|arg| arg.contains('\0')) {
            return Err(ContractError::Invalid("Cursor MCP arguments"));
        }
        EnvironmentDelta {
            remove: BTreeSet::new(),
            set: mcp.environment.iter().map(|(key, value)| (key.clone(), value.into())).collect(),
        }.validate()?;
        Ok(Self { mcp, runtime: None, project: None })
    }
    pub fn with_runtime(mut self, runtime: native::RuntimeInstallation) -> Self {
        self.runtime = Some(runtime);
        self
    }
    pub fn with_project(mut self, project: PathBuf) -> Result<Self, ContractError> {
        require_absolute(&project, "Cursor project directory")?;
        self.project = Some(project);
        Ok(self)
    }
    pub fn hooks(&self) -> ProviderHooks<'_> {
        ProviderHooks {
            catalog: HookBinding::Bound(self), plan: HookBinding::Bound(self),
            materialize: HookBinding::Bound(self), session: HookBinding::Bound(self),
            semantic: HookBinding::Bound(self), interaction: HookBinding::Bound(self),
            fork: HookBinding::Bound(self),
        }
    }
    /// Configuration intent only. The caller must grant the exact owned cwd/file;
    /// no overwrite/global merge, prompt carrier or native approval is implied.
    pub fn mcp_resource(
        &self,
        owner: InstanceIdentity,
        cwd: PathBuf,
    ) -> Result<OwnedResourceRequest, ContractError> {
        Ok(OwnedResourceRequest {
            owner,
            path: OwnedPath::new(cwd, ".cursor/mcp.json".into())?,
            kind: ResourceKind::McpConfig,
            contents: self.mcp_contents()?,
        })
    }
    fn mcp_contents(&self) -> Result<Vec<u8>, ContractError> {
        serde_json::to_vec(&serde_json::json!({"mcpServers":{"team":{
            "command":self.mcp.executable.to_str(),
            "args":self.mcp.arguments,"env":self.mcp.environment
        }}})).map_err(|_| ContractError::Invalid("Cursor MCP JSON"))
    }
}
fn refusal(field: &'static str) -> ContractError {
    ContractError::Unverified { field, reason: UNVERIFIED }
}
fn partial(error: ContractError, resources: Vec<OwnedResourceReceipt>) -> PartialFailure {
    PartialFailure { error, receipt: MaterializeReceipt { resources } }
}
impl PlanHook for CursorAdapter {
    fn plan(&self, resolved: &ResolvedLaunch) -> Result<LaunchPlan, ContractError> {
        let r = resolved.request();
        let runtime = self.runtime.as_ref().ok_or_else(|| refusal("Cursor runtime installation"))?;
        let project = self.project.as_ref().ok_or(ContractError::Invalid("Cursor project directory"))?;
        if resolved.provider().as_str() != "cursor" || r.native != runtime.native
            || r.paths.executable != runtime.engine || !interaction::PROFILES[0].matches_native(&r.native)
            || r.channel != Channel::Tmux || r.mode != LaunchMode::FullWorker
            || r.auth != AuthMode::NativeSubscription || self.mcp.executable != r.paths.candidate
            || !matches!(r.operation, Operation::Fresh | Operation::Resume)
        { return Err(ContractError::Mismatch("Cursor native launch identity")); }
        if !matches!(resolved.expected_session(), ExpectedSession::PendingAllocation)
            && r.paths.cwd.path == *project
        { return Err(ContractError::Invalid("Cursor private primary workspace required")); }
        let prompt = r.prompt.as_ref().ok_or(ContractError::Invalid("Cursor worker prompt"))?;
        let mcp = self.mcp_resource(r.identity.clone(), r.paths.cwd.path.clone())?;
        let prompt = OwnedResourceRequest {
            owner: r.identity.clone(),
            path: OwnedPath::new(r.paths.cwd.path.clone(), ".cursor/team-agent-prompt.txt".into())?,
            kind: ResourceKind::Prompt, contents: prompt.as_bytes().to_vec(),
        };
        let mut arguments: Vec<OsString> = vec!["--use-system-ca".into(), runtime.entry().as_os_str().to_owned()];
        let model_index = arguments.len();
        arguments.extend(["--model".into(), resolved.model().ok_or(ContractError::Invalid("Cursor exact model"))?.into()]);
        arguments.extend(["--workspace".into(), r.paths.cwd.path.as_os_str().to_owned(),
            "--add-dir".into(), project.as_os_str().to_owned(), "--approve-mcps".into(), "--trust".into()]);
        // --trust concerns our private primary directory. Native tool bypass is
        // independently explicit; false never adds --force or --sandbox disabled.
        let bypass = if r.bypass {
            let index = arguments.len(); arguments.push("--force".into());
            CarrierUse::Specified(CarrierRef::Arguments(vec![index]))
        } else { CarrierUse::NotRequested };
        let id = match resolved.expected_session() {
            ExpectedSession::Preassigned(id) => Some(id),
            ExpectedSession::Resume(binding) => Some(&binding.native_session),
            ExpectedSession::PendingAllocation => None, // pure preflight; host refuses to spawn
            _ => return Err(ContractError::Invalid("Cursor explicit native session required")),
        };
        if let Some(id) = id {
            native::parse_chat_id(id.as_str().as_bytes())?;
            arguments.extend(["--resume".into(), id.as_str().into()]);
        }
        if !matches!(resolved.effort(), EffortResolution::Unspecified) {
            return Err(ContractError::EffortRejected(EFFORT));
        }
        let cache = if r.native.platform == Platform::MacOs {
            runtime.home().join("Library/Caches/cursor-compile-cache")
        } else { runtime.home().join(".cache/cursor-compile-cache") };
        Ok(LaunchPlan {
            executable: runtime.engine.clone(), arguments,
            executable_inputs: vec![runtime.executable_input(1)],
            cwd: r.paths.cwd.path.clone(),
            environment: EnvironmentDelta {
                remove: BTreeSet::from(["CURSOR_API_KEY".into(), "CURSOR_API_ENDPOINT".into()]),
                set: BTreeMap::from([
                    ("HOME".into(), runtime.home().as_os_str().to_owned()),
                    ("CURSOR_INVOKED_AS".into(), "cursor-agent".into()),
                    ("NODE_COMPILE_CACHE".into(), cache.into_os_string()),
                ]),
            },
            expected_session: resolved.expected_session().clone(),
            carriers: CarrierReport {
                model: CarrierUse::Specified(CarrierRef::Arguments(vec![model_index, model_index + 1])),
                prompt: CarrierUse::Specified(CarrierRef::Resource(prompt.path.clone())),
                mcp: CarrierUse::Specified(CarrierRef::Resource(mcp.path.clone())),
                bypass, effort: CarrierUse::NotRequested,
            },
            materialization: vec![mcp, prompt],
        })
    }
}
impl MaterializeHook for CursorAdapter {
    fn materialize(
        &self,
        requests: &[OwnedResourceRequest],
        io: &mut dyn OwnedIo,
    ) -> Result<MaterializeReceipt, PartialFailure> {
        let expected = self.mcp_contents().map_err(|error| partial(error, vec![]))?;
        let valid = (1..=2).contains(&requests.len()) && requests[0].kind == ResourceKind::McpConfig
            && requests[0].path.relative() == std::path::Path::new(".cursor/mcp.json")
            && requests[0].contents == expected
            && requests.get(1).is_none_or(|prompt| {
                prompt.kind == ResourceKind::Prompt && prompt.owner == requests[0].owner
                    && prompt.path.root() == requests[0].path.root()
                    && prompt.path.relative() == std::path::Path::new(".cursor/team-agent-prompt.txt")
                    && std::str::from_utf8(&prompt.contents).is_ok_and(nonblank)
            });
        if !valid {
            return Err(partial(ContractError::Invalid("exact owned Cursor MCP/prompt config required"), vec![]));
        }
        let mut resources = Vec::new();
        for request in requests {
            // OwnedIo owns all lease/no-follow/exclusive effects; keep partial receipts.
            let receipt = match io.create_exclusive(request) {
                Ok(receipt) => receipt,
                Err(failure) => {
                    resources.extend(failure.receipt.resources);
                    return Err(partial(failure.error, resources));
                }
            };
            let valid = receipt.path == request.path && receipt.owner == request.owner
                && receipt.kind == request.kind && receipt.exclusive
                && receipt.disposition == ResourceDisposition::OwnedRemovable
                && matches!(receipt.write_effect, ResourceWriteEffect::Written { bytes_sha256 }
                    if bytes_sha256 == crate::host::digest(&request.contents));
            resources.push(receipt);
            if !valid { return Err(partial(ContractError::Mismatch("Cursor config receipt"), resources)); }
        }
        Ok(MaterializeReceipt { resources })
    }
}
impl SessionHook for CursorAdapter {
    fn bind(&self, evidence: &ScopedSessionEvidence) -> Result<ResumeBinding, ContractError> {
        if evidence.provider.as_str() != "cursor" || !interaction::PROFILES[0].matches_native(&evidence.native)
            || evidence.origin != CaptureOrigin::ExplicitResumeLaunch
            || evidence.evidence_sha256 != crate::host::digest(&evidence.record)
        { return Err(ContractError::Mismatch("Cursor explicit session provenance")); }
        Ok(ResumeBinding {
            provider: evidence.provider.clone(), native_session: native::parse_chat_id(&evidence.record)?,
            storage: SessionStorage::Local, cwd: evidence.cwd.clone(), native: evidence.native.clone(),
            source: evidence.scope.identity.clone(), origin: evidence.origin,
            evidence_kind: evidence.evidence_kind, evidence_sha256: evidence.evidence_sha256, backing: None,
        })
    }
}
impl ForkHook for CursorAdapter {
    fn stage(&self, _request: &ResolvedFork, _io: &mut dyn OwnedIo) -> Result<NativeForkPlan, PartialFailure> {
        Err(partial(refusal("Cursor native fork lineage"), vec![]))
    }
}
impl CatalogHook for CursorAdapter {
    fn discover(&self, request: &CatalogRequest, host: &mut dyn BoundedReadHost) -> Result<CatalogObservation, ReadFailure> {
        native::validate_catalog_request(request).map_err(ReadFailure::Error)?;
        let output = host.read_catalog(request)?;
        if output.elapsed > request.bounds.deadline {
            return Err(ReadFailure::TimedOut { elapsed: output.elapsed });
        }
        if output.stdout.len() > request.bounds.max_output_bytes {
            return Err(ReadFailure::OutputLimit { limit: request.bounds.max_output_bytes });
        }
        if output.exit_code != 0 {
            return Err(ReadFailure::Exit { code: output.exit_code });
        }
        Ok(CatalogObservation {
            provider: request.provider.clone(), native: request.native.clone(),
            schema: request.source.schema.into(),
            models: native::parse_catalog(&output.stdout).map_err(ReadFailure::Error)?,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExitClass { CommandSucceeded, Unknown }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExitClassification {
    pub class: ExitClass,
    pub native_effects_may_have_occurred: bool,
    pub delivery_floor: crate::contract::delivery::DeliveryEffect,
}
pub fn classify_exit(code: Option<i32>, prior: crate::contract::delivery::DeliveryEffect) -> ExitClassification {
    ExitClassification {
        class: if code == Some(0) { ExitClass::CommandSucceeded } else { ExitClass::Unknown },
        native_effects_may_have_occurred: true,
        delivery_floor: prior,
    }
}
