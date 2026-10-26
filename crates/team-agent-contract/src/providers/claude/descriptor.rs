//! Claude Code fifteen facets. Data only: argv, grammar and probes live in the
//! sibling modules; nothing here reads the environment or runs a process.
use crate::contract::{descriptor::*, types::*};

/// The canonical contract key compiled specs and state carry. `claude` and
/// `claude_code` stay legacy Provider keys and never select this leaf.
pub const ID: &str = "claude_tui";
pub const UNVERIFIED: Reason = Reason { code: "claude-native-admission-missing", message: "This Claude Code capability lacks scoped native evidence; no legacy or other-provider fallback is allowed" };
const STARTUP: Reason = Reason { code: "claude-folder-trust-not-keyed", message: "Folder trust is the user's own global consent and its focus defaults to No, exit; the private primary must already sit under a folder the user trusts. No startup key is ever authorized" };
const EFFORT: Reason = Reason { code: "claude-effort-not-native", message: "Native Claude Code accepts low, medium, high, xhigh, max per model; never clamp or silently drop effort" };
pub static CLAUDE_DESCRIPTOR: ProviderDescriptor = ProviderDescriptor {
    identity: IdentityFacet { id: ID, display_name: "Claude Code (contract)", binary: "claude", aliases: &[], leader: Support::Unverified(UNVERIFIED) },
    model: ModelFacet { selection: Support::Supported(ModelSelection::ExactCatalogId), omitted: OmittedModel::Reject, catalog: Support::Supported(super::catalog::CATALOG_SOURCE) },
    // Each catalog model lists its own effort levels; `ultra` is not native.
    effort: EffortFacet { admission: [EffortAdmission::Pass, EffortAdmission::Pass, EffortAdmission::Pass, EffortAdmission::Pass, EffortAdmission::Pass, EffortAdmission::Reject(EFFORT)],
        carrier: Support::Supported(ValueCarrier::Flag("--effort")), inherit_team_default: false, model_dependent: true },
    auth: AuthFacet { subscription: Support::Supported(AuthMechanism::NativeExistingSession), official_api: Support::Unverified(UNVERIFIED), compatible_api: Support::Unverified(UNVERIFIED) },
    bypass: BypassFacet { intent: Support::Supported(BypassPolicy { enabled_arguments: &["--dangerously-skip-permissions"], requires_startup_consent: false }) },
    prompt: PromptFacet { carrier: Support::Supported(PromptCarrier::RuntimeBridge) },
    // `--mcp-config` exists only for this invocation. Account/native servers
    // from the user's config coexist; only this generation's owned server and
    // same-connection facts confer Team identity.
    mcp: McpFacet { carrier: Support::Supported(McpCarrier::Argv), scope: ResourceScope::WorkingDirectory,
        tools: &TEAM_TOOLS, isolation: McpIsolation::OwnedServerIsolated },
    tool_names: ToolNameFacet { naming: Support::Supported(ToolNaming::RuntimeBound) },
    // `--session-id <uuid>` names the fresh session; `--resume <uuid>` reopens it.
    session: SessionFacet { fresh: FreshSession::Preassigned, resume: Support::Supported(ResumeMode::ExactId) },
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
