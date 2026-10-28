//! Pi coding agent fifteen facets. Data only: argv, grammar and probes live in
//! the sibling modules; nothing here reads the environment or runs a process.
use crate::contract::{descriptor::*, types::*};

/// The canonical contract key compiled specs and state carry. The legacy
/// `pi` key remains on the legacy runtime.
pub const ID: &str = "pi_tui";
pub const UNVERIFIED: Reason = Reason { code: "pi-native-admission-missing", message: "This Pi capability lacks scoped native evidence; no legacy or other-provider fallback is allowed" };
const EFFORT_DEFAULT: Reason = Reason { code: "pi-native-effort-default", message: "Pi catalog does not prove per-model thinking levels; leave thinking unspecified and use the native model default" };
const BYPASS: Reason = Reason { code: "pi-has-no-approval-gate", message: "Pi never asks before a tool call, so there is no approval to bypass; omit dangerously_skip_permissions" };
const STARTUP: Reason = Reason { code: "pi-startup-modals-not-keyed", message: "--no-approve decides project trust in argv; no startup key is ever authorized" };
pub static PI_DESCRIPTOR: ProviderDescriptor = ProviderDescriptor {
    identity: IdentityFacet { id: ID, display_name: "Pi (contract)", binary: "pi", aliases: &[], leader: Support::Unverified(UNVERIFIED) },
    model: ModelFacet { selection: Support::Supported(ModelSelection::ExactCatalogId), omitted: OmittedModel::Reject, catalog: Support::Supported(super::catalog::CATALOG_SOURCE) },
    // Reasoning yes/no does not prove which levels a model accepts. Preserve
    // Pi's native default rather than reject a valid model or silently clamp.
    effort: EffortFacet { admission: [const { EffortAdmission::IgnoreWithReason(EFFORT_DEFAULT) }; 6],
        carrier: Support::Supported(ValueCarrier::Flag("--thinking")), inherit_team_default: false, model_dependent: false },
    auth: AuthFacet { subscription: Support::Supported(AuthMechanism::NativeExistingSession), official_api: Support::Unverified(UNVERIFIED), compatible_api: Support::Unverified(UNVERIFIED) },
    bypass: BypassFacet { intent: Support::Unsupported(BYPASS) },
    prompt: PromptFacet { carrier: Support::Supported(PromptCarrier::ScopedFile) },
    // Pi has no per-invocation MCP argv and reads only trusted project config,
    // which a replacing MCP extension ignores. The owned runtime file is a Pi
    // extension loaded by absolute `-e` for this invocation; account/native servers
    // coexist, and only this generation's server and connection facts confer
    // Team identity.
    mcp: McpFacet { carrier: Support::Supported(McpCarrier::Argv), scope: ResourceScope::RuntimeRoot,
        tools: &TEAM_TOOLS, isolation: McpIsolation::OwnedServerIsolated },
    tool_names: ToolNameFacet { naming: Support::Supported(ToolNaming::RuntimeBound) },
    session: SessionFacet { fresh: FreshSession::CaptureAfterLaunch, resume: Support::Supported(ResumeMode::ExactId) },
    fork: ForkFacet { in_window: Support::Unverified(UNVERIFIED), full_snapshot: Support::Unverified(UNVERIFIED), native_new_seat: Support::Unverified(UNVERIFIED), allowed_auth: &[] },
    input: InputFacet { profiles: Support::Supported(super::interaction::PROFILES) },
    startup: StartupFacet { interactive: true, authorized_consent: Support::Unsupported(STARTUP) },
    probe_sources: ProbeSourcesFacet { process: Support::Supported(ProbeSource::Host), pane: Support::Supported(ProbeSource::NativeSurface), server: Support::Supported(ProbeSource::ServerLifecycle), client_binding: Support::Supported(ProbeSource::NativeRuntime), round_trip: Support::Supported(ProbeSource::NativeRuntime), semantic: Support::Supported(ProbeSource::NativeSurface) },
    workspace: WorkspaceFacet { config_scope: ResourceScope::RuntimeRoot, shared_cwd: true, requires_materialization_lease: true },
    teardown: TeardownFacet { resources: &[
        ResourcePolicy { kind: ResourceKind::McpConfig, disposition: ResourceDisposition::OwnedRemovable },
        ResourcePolicy { kind: ResourceKind::Prompt, disposition: ResourceDisposition::OwnedRemovable },
        ResourcePolicy { kind: ResourceKind::SessionBacking, disposition: ResourceDisposition::OwnedPreserved },
        ResourcePolicy { kind: ResourceKind::GlobalSettings, disposition: ResourceDisposition::Forbidden },
        ResourcePolicy { kind: ResourceKind::NativeDatabase, disposition: ResourceDisposition::Forbidden },
    ] },
};
