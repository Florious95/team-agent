//! Pi coding agent fifteen facets. Data only: argv, grammar and probes live in
//! the sibling modules; nothing here reads the environment or runs a process.
use crate::contract::{descriptor::*, types::*};

/// The canonical contract key compiled specs and state carry. Role documents
/// declare it as `pi`; a persisted pre-upgrade `pi` state row keeps its key and
/// is never reclassified onto this leaf.
pub const ID: &str = "pi_tui";
pub const UNVERIFIED: Reason = Reason { code: "pi-native-admission-missing", message: "This Pi capability lacks scoped native evidence; no legacy or other-provider fallback is allowed" };
const LEVEL: Reason = Reason { code: "pi-effort-level-unproven", message: "Pi's catalog lists reasoning yes/no only and Pi clamps a level the model lacks; xhigh/max are refused, never silently lowered" };
const ULTRA: Reason = Reason { code: "pi-effort-ultra-not-native", message: "Pi has no ultra thinking level; never clamp or silently drop effort" };
const BYPASS: Reason = Reason { code: "pi-has-no-approval-gate", message: "Pi never asks before a tool call, so there is no approval to bypass; omit dangerously_skip_permissions" };
const STARTUP: Reason = Reason { code: "pi-startup-modals-not-keyed", message: "--no-approve decides project trust in argv; no startup key is ever authorized" };
pub static PI_DESCRIPTOR: ProviderDescriptor = ProviderDescriptor {
    identity: IdentityFacet { id: ID, display_name: "Pi (contract)", binary: "pi", aliases: &[], leader: Support::Unverified(UNVERIFIED) },
    model: ModelFacet { selection: Support::Supported(ModelSelection::ExactCatalogId), omitted: OmittedModel::Reject, catalog: Support::Supported(super::catalog::CATALOG_SOURCE) },
    // Each catalog row proves only whether the model reasons at all.
    effort: EffortFacet { admission: [EffortAdmission::Pass, EffortAdmission::Pass, EffortAdmission::Pass,
            EffortAdmission::Reject(LEVEL), EffortAdmission::Reject(LEVEL), EffortAdmission::Reject(ULTRA)],
        carrier: Support::Supported(ValueCarrier::Flag("--thinking")), inherit_team_default: false, model_dependent: true },
    auth: AuthFacet { subscription: Support::Supported(AuthMechanism::NativeExistingSession), official_api: Support::Unverified(UNVERIFIED), compatible_api: Support::Unverified(UNVERIFIED) },
    bypass: BypassFacet { intent: Support::Unsupported(BYPASS) },
    prompt: PromptFacet { carrier: Support::Supported(PromptCarrier::RuntimeBridge) },
    // Pi has no per-invocation MCP argv and reads only trusted project config,
    // which a replacing MCP extension ignores. The owned cwd file is a Pi
    // extension loaded by `-e` for this invocation; account/native servers
    // coexist, and only this generation's server and connection facts confer
    // Team identity.
    mcp: McpFacet { carrier: Support::Supported(McpCarrier::ScopedConfig), scope: ResourceScope::WorkingDirectory,
        tools: &TEAM_TOOLS, isolation: McpIsolation::OwnedServerIsolated },
    tool_names: ToolNameFacet { naming: Support::Supported(ToolNaming::RuntimeBound) },
    session: SessionFacet { fresh: FreshSession::CaptureAfterLaunch, resume: Support::Supported(ResumeMode::ExactId) },
    // A new seat forks natively with `--fork <parent uuid>` (1.4.0: resolved
    // from another cwd; the child lands in the new cwd's store with header
    // `parentSession`, new ID, full body; the parent is untouched).
    fork: ForkFacet { in_window: Support::Unverified(UNVERIFIED), full_snapshot: Support::Unverified(UNVERIFIED), native_new_seat: Support::Supported(()), allowed_auth: &[AuthMode::NativeSubscription] },
    input: InputFacet { profiles: Support::Supported(super::interaction::PROFILES) },
    startup: StartupFacet { interactive: true, authorized_consent: Support::Unsupported(STARTUP) },
    probe_sources: ProbeSourcesFacet { process: Support::Supported(ProbeSource::Host), pane: Support::Supported(ProbeSource::NativeSurface), server: Support::Supported(ProbeSource::ServerLifecycle), client_binding: Support::Supported(ProbeSource::NativeRuntime), round_trip: Support::Supported(ProbeSource::NativeRuntime), semantic: Support::Supported(ProbeSource::NativeSurface) },
    workspace: WorkspaceFacet { config_scope: ResourceScope::WorkingDirectory, shared_cwd: false, requires_materialization_lease: true },
    teardown: TeardownFacet { resources: &[
        ResourcePolicy { kind: ResourceKind::McpConfig, disposition: ResourceDisposition::OwnedRemovable },
        ResourcePolicy { kind: ResourceKind::Prompt, disposition: ResourceDisposition::OwnedRemovable },
        ResourcePolicy { kind: ResourceKind::SessionBacking, disposition: ResourceDisposition::OwnedPreserved },
        ResourcePolicy { kind: ResourceKind::GlobalSettings, disposition: ResourceDisposition::Forbidden },
        ResourcePolicy { kind: ResourceKind::NativeDatabase, disposition: ResourceDisposition::Forbidden },
    ] },
};
