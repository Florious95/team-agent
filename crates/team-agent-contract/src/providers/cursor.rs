//! Cursor native recon: 40 observed, 3 unknown and 12 unrun cards, not a launch
//! admission. Catalog/stdio configuration/surface grammar are independent of
//! prompt, session, authentication and genuine Team-tool binding evidence.
pub mod interaction;
pub mod native;

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use crate::contract::{
    descriptor::*, fork::*, hooks::*, plan::*, session::ResumeBinding, types::*,
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
    message: "Local mcpServers and echo calls are observed; ambient exclusion and genuine Team tool binding are not",
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
        subscription: Support::Unverified(UNVERIFIED),
        official_api: Support::Unverified(UNVERIFIED),
        compatible_api: Support::Unverified(UNVERIFIED),
    },
    // The measured --force/--trust run does not prove bypass=false semantics.
    bypass: BypassFacet { intent: Support::Unverified(UNVERIFIED) },
    prompt: PromptFacet { carrier: Support::Unverified(UNVERIFIED) },
    mcp: McpFacet {
        carrier: Support::Unverified(MCP),
        scope: ResourceScope::WorkingDirectory,
        tools: &TEAM_TOOLS,
        excludes_ambient_configuration: false,
    },
    tool_names: ToolNameFacet { naming: Support::Unverified(MCP) },
    session: SessionFacet {
        fresh: FreshSession::CaptureAfterLaunch,
        resume: Support::Unverified(UNVERIFIED),
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
        Ok(Self { mcp })
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
    fn plan(&self, _request: &ResolvedLaunch) -> Result<LaunchPlan, ContractError> {
        // Launcher != Node image. An ancestor/inode check cannot establish the
        // missing prompt/session/auth/carrier contract or native package identity.
        Err(refusal("Cursor launch: prompt/session/native-image admission"))
    }
}
impl MaterializeHook for CursorAdapter {
    fn materialize(
        &self,
        requests: &[OwnedResourceRequest],
        io: &mut dyn OwnedIo,
    ) -> Result<MaterializeReceipt, PartialFailure> {
        let expected = self.mcp_contents().map_err(|error| partial(error, vec![]))?;
        if requests.len() != 1
            || requests[0].kind != ResourceKind::McpConfig
            || requests[0].path.relative() != std::path::Path::new(".cursor/mcp.json")
            || requests[0].contents != expected
        {
            return Err(partial(ContractError::Invalid("one exact owned Cursor MCP config required"), vec![]));
        }
        let request = &requests[0];
        // OwnedIo enforces lease, root identity, no-follow and exclusive create.
        let receipt = io.create_exclusive(request)?;
        if receipt.path != request.path || receipt.owner != request.owner
            || receipt.kind != request.kind || !receipt.exclusive
            || receipt.disposition != ResourceDisposition::OwnedRemovable
            || !matches!(receipt.write_effect, ResourceWriteEffect::Written { bytes_sha256 }
                if bytes_sha256 == crate::host::digest(&request.contents))
        {
            return Err(partial(ContractError::Mismatch("Cursor MCP receipt"), vec![receipt]));
        }
        Ok(MaterializeReceipt { resources: vec![receipt] })
    }
}
impl SessionHook for CursorAdapter {
    fn bind(&self, _evidence: &ScopedSessionEvidence) -> Result<ResumeBinding, ContractError> {
        // A UUID in the exit hint is not a running native session/backing tuple.
        Err(refusal("Cursor current-session binding"))
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
