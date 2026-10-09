//! Human command catalog: help, spelling suggestions, and public dispatch guards.
#![allow(dead_code)]

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CommandTier {
    Core,
    Guided,
    Secondary,
    DevInternal,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CommandCategory {
    Start,
    Daily,
    TeamLifecycle,
    WorkerLifecycle,
    GuidedRecovery,
    Setup,
    Observe,
    Dev,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CommandKind {
    Dispatch(DispatchKind),
    LeaderPassthrough { provider: &'static str },
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TokenUsage {
    No,
    Yes,
    Conditional,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DispatchKind {
    QuickStart,
    Send,
    Status,
    Models,
    Inbox,
    Restart,
    Shutdown,
    AddAgent,
    StartAgent,
    StopAgent,
    ResetAgent,
    CloneAgent,
    ForkAgent,
    RemoveAgent,
    Leaders,
    Doctor,
    Approvals,
    Route,
    Profile,
    InstallSkill,
    ClaimLeader,
    Takeover,
    AttachLeader,
}
pub(crate) const ALL_DISPATCH_KINDS: &[DispatchKind] = &[
    DispatchKind::QuickStart,
    DispatchKind::Send,
    DispatchKind::Status,
    DispatchKind::Models,
    DispatchKind::Inbox,
    DispatchKind::Restart,
    DispatchKind::Shutdown,
    DispatchKind::AddAgent,
    DispatchKind::StartAgent,
    DispatchKind::StopAgent,
    DispatchKind::ResetAgent,
    DispatchKind::CloneAgent,
    DispatchKind::ForkAgent,
    DispatchKind::RemoveAgent,
    DispatchKind::Leaders,
    DispatchKind::Doctor,
    DispatchKind::Approvals,
    DispatchKind::Route,
    DispatchKind::Profile,
    DispatchKind::InstallSkill,
    DispatchKind::ClaimLeader,
    DispatchKind::Takeover,
    DispatchKind::AttachLeader,
];
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct GovernanceNote {
    pub(crate) decision: &'static str,
    pub(crate) reason: &'static str,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct CommandSpec {
    pub(crate) name: &'static str,
    pub(crate) tier: CommandTier,
    pub(crate) category: CommandCategory,
    pub(crate) kind: CommandKind,
    pub(crate) summary: &'static str,
    pub(crate) usage: &'static str,
    pub(crate) default_help: bool,
    pub(crate) command_help: bool,
    pub(crate) suggestion_index: bool,
    pub(crate) token_usage: TokenUsage,
    pub(crate) sunset: Option<&'static str>,
    pub(crate) action: Option<&'static str>,
    pub(crate) governance: Option<GovernanceNote>,
}

// Only human operations belong here. Compatibility commands are private in emit.rs.
#[rustfmt::skip]
pub(crate) const COMMAND_SPECS: &[CommandSpec] = &[
    CommandSpec { name: "quick-start", tier: CommandTier::Core, category: CommandCategory::Start, kind: CommandKind::Dispatch(DispatchKind::QuickStart), summary: "Create a team from role files; use restart for an initialized team", usage: "team-agent quick-start [TEAMDIR] [--workspace WORKSPACE] [--team TEAM] [--name NAME] [--team-id TEAM] [--yes] [--backend tmux|conpty] [--json] [--detail]", default_help: true, command_help: true, suggestion_index: true, token_usage: TokenUsage::Conditional, sunset: None, action: None, governance: None },
    CommandSpec { name: "send", tier: CommandTier::Core, category: CommandCategory::Start, kind: CommandKind::Dispatch(DispatchKind::Send), summary: "Send a task to a specified agent", usage: "team-agent send <agent> MESSAGE... [--workspace WORKSPACE] [--team TEAM] [--mailbox] [--json]", default_help: true, command_help: true, suggestion_index: true, token_usage: TokenUsage::Conditional, sunset: Some("a future compatibility release"), action: Some("Use team-agent send <agent> 'task message'; select scope with --workspace and --team"), governance: None },
    CommandSpec { name: "status", tier: CommandTier::Core, category: CommandCategory::Start, kind: CommandKind::Dispatch(DispatchKind::Status), summary: "Check agent status without reading conversation content", usage: "team-agent status [<agent>] [--workspace WORKSPACE] [--team TEAM] [--json] [--summary|--detail]", default_help: true, command_help: true, suggestion_index: true, token_usage: TokenUsage::No, sunset: None, action: None, governance: None },
    CommandSpec { name: "models", tier: CommandTier::Core, category: CommandCategory::Start, kind: CommandKind::Dispatch(DispatchKind::Models), summary: "Discover model names supported by the native provider", usage: "team-agent models [--provider pi|cursor_agent|codex|claude|claude_code|grok] [QUERY|--search TEXT] [--json]", default_help: true, command_help: true, suggestion_index: true, token_usage: TokenUsage::No, sunset: None, action: None, governance: None },
    CommandSpec { name: "inbox", tier: CommandTier::Core, category: CommandCategory::Start, kind: CommandKind::Dispatch(DispatchKind::Inbox), summary: "Read agent replies", usage: "team-agent inbox <agent> [-n N|--limit N] [--workspace WORKSPACE] [--team TEAM] [--json]", default_help: true, command_help: true, suggestion_index: true, token_usage: TokenUsage::No, sunset: None, action: None, governance: None },
    CommandSpec { name: "restart", tier: CommandTier::Core, category: CommandCategory::TeamLifecycle, kind: CommandKind::Dispatch(DispatchKind::Restart), summary: "Restart an existing team using saved sessions when available", usage: "team-agent restart [WORKSPACE] [--team TEAM] [--allow-fresh] [--session-converge-deadline SECONDS] [--json] [--detail]", default_help: true, command_help: true, suggestion_index: true, token_usage: TokenUsage::Conditional, sunset: None, action: None, governance: None },
    CommandSpec { name: "shutdown", tier: CommandTier::Core, category: CommandCategory::TeamLifecycle, kind: CommandKind::Dispatch(DispatchKind::Shutdown), summary: "Shut down the selected team and report residual resources", usage: "team-agent shutdown [--workspace WORKSPACE] [--team TEAM] [--keep-logs] [--json]", default_help: true, command_help: true, suggestion_index: true, token_usage: TokenUsage::No, sunset: None, action: None, governance: None },
    CommandSpec { name: "add-agent", tier: CommandTier::Core, category: CommandCategory::WorkerLifecycle, kind: CommandKind::Dispatch(DispatchKind::AddAgent), summary: "Add and start an agent", usage: "team-agent add-agent <agent> [--role-file FILE] [--provider TOOL] [--model MODEL] [--effort LEVEL] [--bypass true|false] [--prompt TEXT] [--profile NAME] [--workspace WORKSPACE] [--team TEAM] [--json]", default_help: true, command_help: true, suggestion_index: true, token_usage: TokenUsage::Conditional, sunset: None, action: None, governance: None },
    CommandSpec { name: "start-agent", tier: CommandTier::Core, category: CommandCategory::WorkerLifecycle, kind: CommandKind::Dispatch(DispatchKind::StartAgent), summary: "Start an existing team agent", usage: "team-agent start-agent <agent> [--provider TOOL] [--model MODEL] [--effort LEVEL] [--bypass true|false] [--prompt TEXT] [--profile NAME] [--workspace WORKSPACE] [--team TEAM] [--allow-fresh] [--json]", default_help: true, command_help: true, suggestion_index: true, token_usage: TokenUsage::Conditional, sunset: None, action: None, governance: None },
    CommandSpec { name: "stop-agent", tier: CommandTier::Core, category: CommandCategory::WorkerLifecycle, kind: CommandKind::Dispatch(DispatchKind::StopAgent), summary: "Stop an agent while retaining configuration and session history", usage: "team-agent stop-agent <agent> [--workspace WORKSPACE] [--team TEAM] [--json]", default_help: true, command_help: true, suggestion_index: true, token_usage: TokenUsage::No, sunset: None, action: None, governance: None },
    CommandSpec { name: "reset-agent", tier: CommandTier::Core, category: CommandCategory::WorkerLifecycle, kind: CommandKind::Dispatch(DispatchKind::ResetAgent), summary: "Discard an agent's saved session association", usage: "team-agent reset-agent <agent> --discard-session [--workspace WORKSPACE] [--team TEAM] [--json]", default_help: true, command_help: true, suggestion_index: true, token_usage: TokenUsage::No, sunset: None, action: None, governance: None },
    CommandSpec { name: "clone-agent", tier: CommandTier::Core, category: CommandCategory::WorkerLifecycle, kind: CommandKind::Dispatch(DispatchKind::CloneAgent), summary: "Clone agent configuration into a new agent", usage: "team-agent clone-agent <agent> --as NEW_AGENT [--label LABEL] [--workspace WORKSPACE] [--team TEAM] [--json]", default_help: true, command_help: true, suggestion_index: true, token_usage: TokenUsage::Conditional, sunset: None, action: None, governance: None },
    CommandSpec { name: "fork-agent", tier: CommandTier::Core, category: CommandCategory::WorkerLifecycle, kind: CommandKind::Dispatch(DispatchKind::ForkAgent), summary: "Fork an agent session where supported by the provider", usage: "team-agent fork-agent <agent> --as NEW_AGENT [--label LABEL] [--workspace WORKSPACE] [--team TEAM] [--json]", default_help: true, command_help: true, suggestion_index: true, token_usage: TokenUsage::Conditional, sunset: None, action: None, governance: None },
    CommandSpec { name: "remove-agent", tier: CommandTier::Core, category: CommandCategory::WorkerLifecycle, kind: CommandKind::Dispatch(DispatchKind::RemoveAgent), summary: "Remove an agent after confirmation", usage: "team-agent remove-agent <agent> --confirm [--workspace WORKSPACE] [--team TEAM] [--from-spec] [--force] [--json]", default_help: true, command_help: true, suggestion_index: true, token_usage: TokenUsage::No, sunset: None, action: None, governance: None },
    CommandSpec { name: "leaders", tier: CommandTier::Core, category: CommandCategory::Observe, kind: CommandKind::Dispatch(DispatchKind::Leaders), summary: "List local leaders and their team scopes", usage: "team-agent leaders [QUERY|--search TEXT] [--all|--stale] [--json] | --prune [--dry-run] [--json]", default_help: true, command_help: true, suggestion_index: true, token_usage: TokenUsage::No, sunset: None, action: None, governance: None },
    CommandSpec { name: "doctor", tier: CommandTier::Core, category: CommandCategory::Observe, kind: CommandKind::Dispatch(DispatchKind::Doctor), summary: "Diagnose a team and suggest recovery; read-only by default", usage: "team-agent doctor [SPEC] [--workspace WORKSPACE] [--team TEAM] [--comms] [--gate orphans|comms] [--fix] [--fix-schema] [--cleanup-orphans] [--confirm] [--json]", default_help: true, command_help: true, suggestion_index: true, token_usage: TokenUsage::No, sunset: None, action: None, governance: None },
    CommandSpec { name: "approvals", tier: CommandTier::Core, category: CommandCategory::Observe, kind: CommandKind::Dispatch(DispatchKind::Approvals), summary: "List pending agent approvals", usage: "team-agent approvals [<agent>] [--workspace WORKSPACE] [--team TEAM] [--json]", default_help: true, command_help: true, suggestion_index: true, token_usage: TokenUsage::No, sunset: None, action: None, governance: None },
    CommandSpec { name: "route", tier: CommandTier::Core, category: CommandCategory::Setup, kind: CommandKind::Dispatch(DispatchKind::Route), summary: "Configure additional provider launch arguments", usage: "team-agent route [status|enable|disable|show [TOOL]|set TOOL -- ARG...|add TOOL -- ARG...|clear TOOL] [--json]", default_help: true, command_help: true, suggestion_index: true, token_usage: TokenUsage::No, sunset: None, action: None, governance: None },
    CommandSpec { name: "profile", tier: CommandTier::Core, category: CommandCategory::Setup, kind: CommandKind::Dispatch(DispatchKind::Profile), summary: "Manage authentication and proxy profiles", usage: "team-agent profile init|doctor|show NAME [--workspace WORKSPACE] [--team TEAM] [--auth-mode MODE] [--proxy-mode direct|inherit] [--json]", default_help: true, command_help: true, suggestion_index: true, token_usage: TokenUsage::No, sunset: None, action: None, governance: None },
    CommandSpec { name: "install-skill", tier: CommandTier::Core, category: CommandCategory::Setup, kind: CommandKind::Dispatch(DispatchKind::InstallSkill), summary: "Optionally install an existing public operation skill", usage: "team-agent install-skill (--source DIR|--uninstall) [--target codex|claude|copilot|all] [--dest DIR] [--dry-run] [--json]", default_help: true, command_help: true, suggestion_index: true, token_usage: TokenUsage::No, sunset: None, action: None, governance: None },
    CommandSpec { name: "claim-leader", tier: CommandTier::Guided, category: CommandCategory::GuidedRecovery, kind: CommandKind::Dispatch(DispatchKind::ClaimLeader), summary: "Register the current terminal as leader only when diagnosis recommends it", usage: "team-agent claim-leader [--workspace WORKSPACE] [--team TEAM] [--confirm] [--json] [--detail]", default_help: true, command_help: true, suggestion_index: true, token_usage: TokenUsage::No, sunset: None, action: None, governance: None },
    CommandSpec { name: "takeover", tier: CommandTier::Guided, category: CommandCategory::GuidedRecovery, kind: CommandKind::Dispatch(DispatchKind::Takeover), summary: "Take over an existing team only after diagnosis and confirmation", usage: "team-agent takeover [--workspace WORKSPACE] [--team TEAM] [--confirm] [--json]", default_help: true, command_help: true, suggestion_index: true, token_usage: TokenUsage::No, sunset: None, action: None, governance: None },
    CommandSpec { name: "attach-leader", tier: CommandTier::Guided, category: CommandCategory::GuidedRecovery, kind: CommandKind::Dispatch(DispatchKind::AttachLeader), summary: "Attach a verified leader terminal only when diagnosis recommends it", usage: "team-agent attach-leader [--workspace WORKSPACE] [--team TEAM] --pane PANE --provider TOOL [--confirm] [--json]", default_help: true, command_help: true, suggestion_index: true, token_usage: TokenUsage::No, sunset: None, action: None, governance: None },
    CommandSpec { name: "pi", tier: CommandTier::Core, category: CommandCategory::Start, kind: CommandKind::LeaderPassthrough { provider: "pi" }, summary: "Open a leader session with Pi", usage: "team-agent pi [--attach-existing] [--confirm] [--attach-session SESSION] [--external-leader] [--allow-nested-attach] [--json] [-- TOOL_ARGS...]", default_help: true, command_help: true, suggestion_index: true, token_usage: TokenUsage::Conditional, sunset: None, action: None, governance: None },
    CommandSpec { name: "codex", tier: CommandTier::Core, category: CommandCategory::Start, kind: CommandKind::LeaderPassthrough { provider: "codex" }, summary: "Open a leader session with Codex", usage: "team-agent codex [--attach-existing] [--confirm] [--attach-session SESSION] [--external-leader] [--allow-nested-attach] [--json] [-- TOOL_ARGS...]", default_help: true, command_help: true, suggestion_index: true, token_usage: TokenUsage::Conditional, sunset: None, action: None, governance: None },
    CommandSpec { name: "claude", tier: CommandTier::Core, category: CommandCategory::Start, kind: CommandKind::LeaderPassthrough { provider: "claude" }, summary: "Open a leader session with Claude Code", usage: "team-agent claude [--attach-existing] [--confirm] [--attach-session SESSION] [--external-leader] [--allow-nested-attach] [--json] [-- TOOL_ARGS...]", default_help: true, command_help: true, suggestion_index: true, token_usage: TokenUsage::Conditional, sunset: None, action: None, governance: None },
    CommandSpec { name: "copilot", tier: CommandTier::Core, category: CommandCategory::Start, kind: CommandKind::LeaderPassthrough { provider: "copilot" }, summary: "Open a leader session with Copilot", usage: "team-agent copilot [--attach-existing] [--confirm] [--attach-session SESSION] [--external-leader] [--allow-nested-attach] [--json] [-- TOOL_ARGS...]", default_help: true, command_help: true, suggestion_index: true, token_usage: TokenUsage::Conditional, sunset: None, action: None, governance: None },
    CommandSpec { name: "grok", tier: CommandTier::Core, category: CommandCategory::Start, kind: CommandKind::LeaderPassthrough { provider: "grok" }, summary: "Open a leader session with Grok", usage: "team-agent grok [--attach-existing] [--confirm] [--attach-session SESSION] [--external-leader] [--allow-nested-attach] [--json] [-- TOOL_ARGS...]", default_help: true, command_help: true, suggestion_index: true, token_usage: TokenUsage::Conditional, sunset: None, action: None, governance: None },
    CommandSpec { name: "cursor", tier: CommandTier::Core, category: CommandCategory::Start, kind: CommandKind::LeaderPassthrough { provider: "cursor_agent" }, summary: "Open a leader session with Cursor agent", usage: "team-agent cursor [--attach-existing] [--confirm] [--attach-session SESSION] [--external-leader] [--allow-nested-attach] [--json] [-- TOOL_ARGS...]", default_help: true, command_help: true, suggestion_index: true, token_usage: TokenUsage::Conditional, sunset: None, action: None, governance: None },
];

pub(crate) fn command_spec(name: &str) -> Option<&'static CommandSpec> {
    COMMAND_SPECS.iter().find(|spec| spec.name == name)
}
