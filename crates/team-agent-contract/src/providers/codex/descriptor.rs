//! Codex CLI fifteen facets. Data only: argv, grammar and probes live in the
//! sibling modules; nothing here reads the environment or runs a process.
use crate::contract::{descriptor::*, types::*};

/// The canonical contract key compiled specs and state carry. Role documents
/// may declare the shared legacy key `codex` with `contract_runtime: true`.
pub const ID: &str = "codex_tui";
pub const UNVERIFIED: Reason = Reason { code: "codex-native-admission-missing", message: "This Codex capability lacks scoped native evidence; no legacy or other-provider fallback is allowed" };
const STARTUP: Reason = Reason { code: "codex-startup-modals-not-keyed", message: "Update checks are disabled by argv and the private non-Git cwd has no trust screen; no startup key is ever authorized" };
pub static CODEX_DESCRIPTOR: ProviderDescriptor = ProviderDescriptor {
    identity: IdentityFacet { id: ID, display_name: "Codex CLI (contract)", binary: "codex", aliases: &[], leader: Support::Unverified(UNVERIFIED) },
    model: ModelFacet { selection: Support::Supported(ModelSelection::ExactCatalogId), omitted: OmittedModel::Reject, catalog: Support::Supported(super::catalog::CATALOG_SOURCE) },
    // Each catalog model lists its own reasoning levels; `ultra` (client
    // delegation) passes verbatim where listed and is never clamped to max.
    effort: EffortFacet { admission: [const { EffortAdmission::Pass }; 6],
        carrier: Support::Supported(ValueCarrier::ConfigArgument { flag: "-c", key: "model_reasoning_effort" }),
        inherit_team_default: false, model_dependent: true },
    auth: AuthFacet { subscription: Support::Supported(AuthMechanism::NativeExistingSession), official_api: Support::Unverified(UNVERIFIED), compatible_api: Support::Unverified(UNVERIFIED) },
    bypass: BypassFacet { intent: Support::Supported(BypassPolicy { enabled_arguments: &["--dangerously-bypass-approvals-and-sandbox"], requires_startup_consent: false }) },
    prompt: PromptFacet { carrier: Support::Supported(PromptCarrier::RuntimeBridge) },
    // `-c mcp_servers.<key>.*` exists only for this invocation. Account/native
    // servers from the user's config coexist; only this generation's owned
    // server and same-connection facts confer Team identity.
    mcp: McpFacet { carrier: Support::Supported(McpCarrier::Argv), scope: ResourceScope::WorkingDirectory,
        tools: &TEAM_TOOLS, isolation: McpIsolation::OwnedServerIsolated },
    tool_names: ToolNameFacet { naming: Support::Supported(ToolNaming::RuntimeBound) },
    session: SessionFacet { fresh: FreshSession::CaptureAfterLaunch, resume: Support::Supported(ResumeMode::ExactId) },
    fork: ForkFacet { in_window: Support::Unverified(UNVERIFIED), full_snapshot: Support::Unverified(UNVERIFIED), native_new_seat: Support::Unverified(UNVERIFIED), allowed_auth: &[] },
    input: InputFacet { profiles: Support::Supported(super::interaction::PROFILES) },
    startup: StartupFacet { interactive: true, authorized_consent: Support::Unsupported(STARTUP) },
    probe_sources: ProbeSourcesFacet { process: Support::Supported(ProbeSource::Host), pane: Support::Supported(ProbeSource::NativeSurface), server: Support::Supported(ProbeSource::ServerLifecycle), client_binding: Support::Supported(ProbeSource::NativeRuntime), round_trip: Support::Supported(ProbeSource::NativeRuntime), semantic: Support::Supported(ProbeSource::NativeSurface) },
    workspace: WorkspaceFacet { config_scope: ResourceScope::WorkingDirectory, shared_cwd: false, requires_materialization_lease: true },
    teardown: TeardownFacet { resources: &[
        ResourcePolicy { kind: ResourceKind::Prompt, disposition: ResourceDisposition::OwnedRemovable },
        ResourcePolicy { kind: ResourceKind::SessionBacking, disposition: ResourceDisposition::OwnedPreserved },
        ResourcePolicy { kind: ResourceKind::GlobalSettings, disposition: ResourceDisposition::Forbidden },
        ResourcePolicy { kind: ResourceKind::NativeDatabase, disposition: ResourceDisposition::Forbidden },
    ] },
};
