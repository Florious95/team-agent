use super::delivery::InputProfile;
use super::hooks::ProviderHooks;
use super::types::*;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IdentityFacet {
    pub id: &'static str,
    pub display_name: &'static str,
    pub binary: &'static str,
    pub aliases: &'static [&'static str],
    pub leader: Support<()>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModelSelection {
    NativeDefaultOrOpaqueId,
    ExactCatalogId,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OmittedModel {
    NativeDefault,
    Reject,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CatalogSource {
    pub arguments: &'static [&'static str],
    pub schema: &'static str,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelFacet {
    pub selection: Support<ModelSelection>,
    pub omitted: OmittedModel,
    pub catalog: Support<CatalogSource>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Effort {
    Low,
    Medium,
    High,
    Xhigh,
    Max,
    Ultra,
}

impl Effort {
    pub const ALL: [Self; 6] = [
        Self::Low,
        Self::Medium,
        Self::High,
        Self::Xhigh,
        Self::Max,
        Self::Ultra,
    ];
    pub fn parse(raw: &str) -> Result<Self, ContractError> {
        Self::ALL
            .into_iter()
            .find(|effort| effort.as_str() == raw.trim())
            .ok_or(ContractError::UnknownEffort)
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Xhigh => "xhigh",
            Self::Max => "max",
            Self::Ultra => "ultra",
        }
    }
    pub fn index(self) -> usize {
        self as usize
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EffortAdmission {
    Pass,
    IgnoreWithReason(Reason),
    Reject(Reason),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ValueCarrier {
    Flag(&'static str),
    ConfigArgument {
        flag: &'static str,
        key: &'static str,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EffortFacet {
    pub admission: [EffortAdmission; 6],
    pub carrier: Support<ValueCarrier>,
    pub inherit_team_default: bool,
    pub model_dependent: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuthMode {
    NativeSubscription,
    OfficialApi,
    CompatibleApi,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AuthMechanism {
    NativeExistingSession,
    ExplicitEnvironment(&'static [&'static str]),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthFacet {
    pub subscription: Support<AuthMechanism>,
    pub official_api: Support<AuthMechanism>,
    pub compatible_api: Support<AuthMechanism>,
}

impl AuthFacet {
    pub fn mechanism(&self, mode: AuthMode) -> &Support<AuthMechanism> {
        match mode {
            AuthMode::NativeSubscription => &self.subscription,
            AuthMode::OfficialApi => &self.official_api,
            AuthMode::CompatibleApi => &self.compatible_api,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BypassPolicy {
    pub enabled_arguments: &'static [&'static str],
    pub requires_startup_consent: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BypassFacet {
    pub intent: Support<BypassPolicy>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PromptCarrier {
    Argv,
    ScopedFile,
    ConfigField,
    RuntimeBridge,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PromptFacet {
    pub carrier: Support<PromptCarrier>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum McpCarrier {
    Argv,
    ScopedConfig,
    RuntimeBridge,
    GlobalInstaller,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResourceScope {
    RuntimeRoot,
    WorkingDirectory,
    Global,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct McpFacet {
    pub carrier: Support<McpCarrier>,
    pub scope: ResourceScope,
    pub tools: &'static [LogicalTool],
    pub excludes_ambient_configuration: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ToolNaming {
    RuntimeBound,
    StaticServer { key: &'static str },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolNameFacet {
    pub naming: Support<ToolNaming>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FreshSession {
    CaptureAfterLaunch,
    Preassigned,
    NotApplicable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResumeMode {
    ExactId,
    ExactPath,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionFacet {
    pub fresh: FreshSession,
    pub resume: Support<ResumeMode>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ForkFacet {
    pub in_window: Support<()>,
    pub full_snapshot: Support<()>,
    pub native_new_seat: Support<()>,
    pub allowed_auth: &'static [AuthMode],
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InputFacet {
    pub profiles: Support<&'static [InputProfile]>,
}

impl InputFacet {
    /// Terminal policy selection, not native acceptance. RPC is a separate, zero-key route.
    pub fn resolve(
        &self,
        native: &NativeIdentity,
        profile_id: &str,
        operation: Operation,
        channel: Channel,
    ) -> Result<&'static InputProfile, ContractError> {
        native.validate()?;
        if channel != Channel::Tmux {
            return Err(ContractError::Unsupported {
                field: "terminal channel",
                reason: Reason {
                    code: "not-a-tmux-channel",
                    message: "This terminal contract does not execute RPC or direct stdin",
                },
            });
        }
        let profiles = *self.profiles.require("input profiles")?;
        let mut matches = profiles.iter().filter(|profile| {
            profile.id == profile_id
                && profile.matches_native(native)
                && profile.channel == channel
                && profile.operations.contains(&operation)
        });
        let profile = matches.next().ok_or(ContractError::ProfileUnavailable)?;
        if matches.next().is_some() {
            return Err(ContractError::AmbiguousProfile);
        }
        profile.policy.require("input profile")?.validate()?;
        Ok(profile)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StartupFacet {
    pub interactive: bool,
    pub authorized_consent: Support<()>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProbeSource {
    Host,
    NativeSurface,
    NativeRuntime,
    ServerLifecycle,
    SessionRecords,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProbeSourcesFacet {
    pub process: Support<ProbeSource>,
    pub pane: Support<ProbeSource>,
    pub server: Support<ProbeSource>,
    pub client_binding: Support<ProbeSource>,
    pub round_trip: Support<ProbeSource>,
    pub semantic: Support<ProbeSource>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceFacet {
    pub config_scope: ResourceScope,
    pub shared_cwd: bool,
    pub requires_materialization_lease: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[derive(serde::Serialize, serde::Deserialize)]
pub enum ResourceKind {
    Prompt,
    AgentConfig,
    McpConfig,
    RuntimeBridge,
    SessionBacking,
    GlobalSettings,
    NativeDatabase,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[derive(serde::Serialize, serde::Deserialize)]
pub enum ResourceDisposition {
    OwnedRemovable,
    OwnedPreserved,
    Shared,
    Forbidden,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResourcePolicy {
    pub kind: ResourceKind,
    pub disposition: ResourceDisposition,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TeardownFacet {
    pub resources: &'static [ResourcePolicy],
}

/// Exactly fifteen mandatory data facets. There are no executable callbacks here.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderDescriptor {
    pub identity: IdentityFacet,
    pub model: ModelFacet,
    pub effort: EffortFacet,
    pub auth: AuthFacet,
    pub bypass: BypassFacet,
    pub prompt: PromptFacet,
    pub mcp: McpFacet,
    pub tool_names: ToolNameFacet,
    pub session: SessionFacet,
    pub fork: ForkFacet,
    pub input: InputFacet,
    pub startup: StartupFacet,
    pub probe_sources: ProbeSourcesFacet,
    pub workspace: WorkspaceFacet,
    pub teardown: TeardownFacet,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NameMode {
    CanonicalOnly,
    ExplicitAliases,
}

pub fn resolve_provider<'a>(
    registry: &'a [&'a ProviderDescriptor],
    raw: &str,
    mode: NameMode,
) -> Result<&'a ProviderDescriptor, ContractError> {
    let mut matches = registry.iter().copied().filter(|descriptor| {
        raw == descriptor.identity.id
            || (mode == NameMode::ExplicitAliases && descriptor.identity.aliases.contains(&raw))
    });
    let descriptor = matches.next().ok_or(ContractError::UnknownProvider)?;
    if matches.next().is_some() {
        return Err(ContractError::AmbiguousProvider);
    }
    Ok(descriptor)
}

pub fn validate_descriptor(
    descriptor: &ProviderDescriptor,
    hooks: &ProviderHooks<'_>,
    operation: Operation,
) -> Result<(), ContractError> {
    let d = descriptor;
    ProviderId::new(d.identity.id)?;
    if !nonblank(d.identity.display_name) || !nonblank(d.identity.binary) {
        return Err(ContractError::Invalid("identity facet"));
    }
    let mut aliases = Vec::new();
    for alias in d.identity.aliases {
        if !valid_id(alias) || *alias == d.identity.id || aliases.contains(alias) {
            return Err(ContractError::Invalid("provider aliases"));
        }
        aliases.push(*alias);
    }
    if let Support::Supported(source) = &d.model.catalog {
        if !nonblank(source.schema) || source.arguments.iter().any(|arg| arg.contains('\0')) {
            return Err(ContractError::Invalid("catalog source"));
        }
    }
    for admission in &d.effort.admission {
        if let EffortAdmission::IgnoreWithReason(reason) | EffortAdmission::Reject(reason) =
            admission
        {
            if !nonblank(reason.code) || !nonblank(reason.message) {
                return Err(ContractError::Invalid("effort disposition reason"));
            }
        }
    }
    for mechanism in [
        &d.auth.subscription,
        &d.auth.official_api,
        &d.auth.compatible_api,
    ] {
        if let Support::Supported(AuthMechanism::ExplicitEnvironment(keys)) = mechanism {
            if keys.is_empty() || keys.iter().any(|key| !valid_env_key(key)) {
                return Err(ContractError::Invalid("authentication environment mapping"));
            }
        }
    }
    if d.effort
        .admission
        .iter()
        .any(|a| matches!(a, EffortAdmission::Pass))
    {
        match d.effort.carrier.require("effort carrier")? {
            ValueCarrier::Flag(flag) if nonblank(flag) => {}
            ValueCarrier::ConfigArgument { flag, key } if nonblank(flag) && nonblank(key) => {}
            _ => return Err(ContractError::Invalid("effort carrier")),
        }
    }
    if d.mcp.carrier.is_supported() {
        if d.mcp.tools.len() != TEAM_TOOLS.len()
            || TEAM_TOOLS.iter().any(|t| !d.mcp.tools.contains(t))
        {
            return Err(ContractError::Invalid("three logical MCP tools"));
        }
        match d.tool_names.naming.require("tool naming")? {
            ToolNaming::StaticServer { key } if !nonblank(key) => {
                return Err(ContractError::Invalid("server key"))
            }
            _ => {}
        }
    }
    if d.workspace.shared_cwd
        && d.workspace.config_scope == ResourceScope::WorkingDirectory
        && !d.workspace.requires_materialization_lease
    {
        return Err(ContractError::Invalid(
            "shared cwd requires a materialization lease",
        ));
    }
    if let Support::Supported(profiles) = &d.input.profiles {
        if profiles.is_empty() {
            return Err(ContractError::Invalid("empty supported profiles"));
        }
        let mut ids = Vec::new();
        for profile in *profiles {
            if [&profile.id, &profile.version, &profile.harness, &profile.ui]
                .into_iter()
                .any(|s| !nonblank(s))
                || profile.operations.is_empty()
                || ids.contains(&profile.id)
                || profile.operations.iter().any(|operation| {
                    !matches!(
                        operation,
                        Operation::FirstBusiness
                            | Operation::OrdinarySend
                            | Operation::StartupBypassAck
                            | Operation::SessionInspect
                            | Operation::InWindowBranch
                            | Operation::Stop
                            | Operation::Shutdown
                    )
                })
            {
                return Err(ContractError::Invalid("input profile identity"));
            }
            ids.push(profile.id);
            if let Support::Supported(policy) = &profile.policy {
                if profile.channel != Channel::Tmux {
                    return Err(ContractError::Invalid("nonterminal submit profile"));
                }
                policy.validate()?;
            }
        }
    }
    let mut kinds = Vec::new();
    for resource in d.teardown.resources {
        if kinds.contains(&resource.kind)
            || (matches!(
                resource.kind,
                ResourceKind::GlobalSettings | ResourceKind::NativeDatabase
            ) && resource.disposition != ResourceDisposition::Forbidden)
            || (resource.disposition == ResourceDisposition::OwnedRemovable
                && matches!(
                    resource.kind,
                    ResourceKind::SessionBacking
                        | ResourceKind::GlobalSettings
                        | ResourceKind::NativeDatabase
                ))
        {
            return Err(ContractError::Invalid("resource disposition"));
        }
        kinds.push(resource.kind);
    }

    if operation == Operation::Catalog {
        d.model.catalog.require("catalog")?;
        hooks.catalog.require("H1 CatalogHook")?;
    }
    if matches!(
        operation,
        Operation::Fresh
            | Operation::Resume
            | Operation::NewSeatFullSnapshot
            | Operation::NativeNewSeat
    ) {
        hooks.plan.require("H2 PlanHook")?;
        if matches!(
            d.model.selection,
            Support::Supported(ModelSelection::ExactCatalogId)
        ) {
            d.model.catalog.require("catalog")?;
            hooks.catalog.require("H1 CatalogHook")?;
        }
        let materializes = matches!(
            d.prompt.carrier,
            Support::Supported(
                PromptCarrier::ScopedFile
                    | PromptCarrier::ConfigField
                    | PromptCarrier::RuntimeBridge
            )
        ) || matches!(
            d.mcp.carrier,
            Support::Supported(
                McpCarrier::ScopedConfig | McpCarrier::RuntimeBridge | McpCarrier::GlobalInstaller
            )
        );
        if materializes {
            hooks.materialize.require("H3 MaterializeHook")?;
        }
    }
    if matches!(
        operation,
        Operation::Resume
            | Operation::SessionInspect
            | Operation::InWindowBranch
            | Operation::NewSeatFullSnapshot
            | Operation::NativeNewSeat
    ) || (operation == Operation::Fresh && d.session.fresh == FreshSession::CaptureAfterLaunch)
    {
        hooks.session.require("H4 SessionHook")?;
    }
    if d.probe_sources.semantic.is_supported() {
        hooks.semantic.require("H5 SemanticReader")?;
    }
    if matches!(
        operation,
        Operation::FirstBusiness
            | Operation::OrdinarySend
            | Operation::StartupBypassAck
            | Operation::SessionInspect
            | Operation::InWindowBranch
    ) || (matches!(
        operation,
        Operation::Fresh
            | Operation::Resume
            | Operation::NewSeatFullSnapshot
            | Operation::NativeNewSeat
    ) && d.startup.interactive)
    {
        hooks.interaction.require("H6 InteractionHook")?;
    }
    let fork_capability = match operation {
        Operation::InWindowBranch => Some((&d.fork.in_window, "in-window branch")),
        Operation::NewSeatFullSnapshot => Some((&d.fork.full_snapshot, "full snapshot")),
        Operation::NativeNewSeat => Some((&d.fork.native_new_seat, "native new seat")),
        _ => None,
    };
    if let Some((capability, field)) = fork_capability {
        capability.require(field)?;
        hooks.fork.require("H7 ForkHook")?;
    }
    Ok(())
}
