//! C1 command registry: one catalog for help visibility, known-command gates,
//! suggestion candidates, and command-tier governance.

#![allow(dead_code)]

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CommandTier {
    Core,
    Guided,
    Secondary,
    DevInternal,
    CompatHidden,
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
    Compat,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CommandKind {
    Dispatch(DispatchKind),
    LeaderPassthrough { provider: &'static str },
    SpecOnlyAlias { alias_of: &'static str },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TokenUsage {
    No,
    Yes,
    Conditional,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DispatchKind {
    Init,
    QuickStart,
    Compile,
    Send,
    AllowPeerTalk,
    Status,
    Stop,
    Shutdown,
    Restart,
    RestartAgent,
    StartAgent,
    StopAgent,
    ResetAgent,
    AddAgent,
    ForkAgent,
    RemoveAgent,
    StuckList,
    StuckCancel,
    AcknowledgeIdle,
    Takeover,
    ClaimLeader,
    AttachLeader,
    AttachAppServerLeader,
    Identity,
    Approvals,
    Inbox,
    Doctor,
    Watch,
    Sessions,
    Leaders,
    Validate,
    InstallSkill,
    Profile,
    Collect,
    Diagnose,
    Preflight,
    WaitReady,
    E2e,
    Peek,
    Coordinator,
}

pub(crate) const ALL_DISPATCH_KINDS: &[DispatchKind] = &[
    DispatchKind::Init,
    DispatchKind::QuickStart,
    DispatchKind::Compile,
    DispatchKind::Send,
    DispatchKind::AllowPeerTalk,
    DispatchKind::Status,
    DispatchKind::Stop,
    DispatchKind::Shutdown,
    DispatchKind::Restart,
    DispatchKind::RestartAgent,
    DispatchKind::StartAgent,
    DispatchKind::StopAgent,
    DispatchKind::ResetAgent,
    DispatchKind::AddAgent,
    DispatchKind::ForkAgent,
    DispatchKind::RemoveAgent,
    DispatchKind::StuckList,
    DispatchKind::StuckCancel,
    DispatchKind::AcknowledgeIdle,
    DispatchKind::Takeover,
    DispatchKind::ClaimLeader,
    DispatchKind::AttachLeader,
    DispatchKind::AttachAppServerLeader,
    DispatchKind::Identity,
    DispatchKind::Approvals,
    DispatchKind::Inbox,
    DispatchKind::Doctor,
    DispatchKind::Watch,
    DispatchKind::Sessions,
    DispatchKind::Leaders,
    DispatchKind::Validate,
    DispatchKind::InstallSkill,
    DispatchKind::Profile,
    DispatchKind::Collect,
    DispatchKind::Diagnose,
    DispatchKind::Preflight,
    DispatchKind::WaitReady,
    DispatchKind::E2e,
    DispatchKind::Peek,
    DispatchKind::Coordinator,
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
    pub(crate) alias_of: Option<&'static str>,
    pub(crate) sunset: Option<&'static str>,
    pub(crate) action: Option<&'static str>,
    pub(crate) governance: Option<GovernanceNote>,
}

#[rustfmt::skip]
pub(crate) const COMMAND_SPECS: &[CommandSpec] = &[
    CommandSpec { name: "quick-start", tier: CommandTier::Core, category: CommandCategory::Start, kind: CommandKind::Dispatch(DispatchKind::QuickStart), summary: "start or attach a team from TEAM.md", usage: "usage: team-agent quick-start [TEAMDIR] [--workspace WORKSPACE] [--name NAME] [--team-id TEAM|--team TEAM] [--yes] [--no-display] [--backend tmux|conpty] [--json]", default_help: true, command_help: true, suggestion_index: true, token_usage: TokenUsage::Conditional, alias_of: None, sunset: None, action: None, governance: None },
    CommandSpec { name: "send", tier: CommandTier::Core, category: CommandCategory::Daily, kind: CommandKind::Dispatch(DispatchKind::Send), summary: "persist a message for a logical recipient", usage: "usage: team-agent send TO MESSAGE... [--workspace WORKSPACE] [--team TEAM] [--json]", default_help: true, command_help: true, suggestion_index: true, token_usage: TokenUsage::Conditional, alias_of: None, sunset: Some("next compatibility release"), action: Some("use positional logical TO and the returned message id"), governance: None },
    CommandSpec { name: "status", tier: CommandTier::Core, category: CommandCategory::Daily, kind: CommandKind::Dispatch(DispatchKind::Status), summary: "show current team status", usage: "usage: team-agent status [AGENT] [--workspace WORKSPACE] [--team TEAM] [--summary|--json] [--detail]", default_help: true, command_help: true, suggestion_index: true, token_usage: TokenUsage::No, alias_of: None, sunset: None, action: None, governance: None },
    CommandSpec { name: "collect", tier: CommandTier::Core, category: CommandCategory::Daily, kind: CommandKind::Dispatch(DispatchKind::Collect), summary: "collect reported results", usage: "usage: team-agent collect [--workspace WORKSPACE] [--team TEAM] [--result-file FILE] [--json]", default_help: true, command_help: true, suggestion_index: true, token_usage: TokenUsage::No, alias_of: None, sunset: None, action: None, governance: None },
    CommandSpec { name: "restart", tier: CommandTier::Core, category: CommandCategory::TeamLifecycle, kind: CommandKind::Dispatch(DispatchKind::Restart), summary: "restart the selected team", usage: "usage: team-agent restart [WORKSPACE] [--team TEAM] [--allow-fresh] [--session-converge-deadline SECONDS] [--json]", default_help: true, command_help: true, suggestion_index: true, token_usage: TokenUsage::Conditional, alias_of: None, sunset: None, action: None, governance: None },
    CommandSpec { name: "shutdown", tier: CommandTier::Core, category: CommandCategory::TeamLifecycle, kind: CommandKind::Dispatch(DispatchKind::Shutdown), summary: "stop the selected team", usage: "usage: team-agent shutdown [--workspace WORKSPACE] [--team TEAM] [--keep-logs] [--json]", default_help: true, command_help: true, suggestion_index: true, token_usage: TokenUsage::No, alias_of: None, sunset: None, action: None, governance: None },
    CommandSpec { name: "add-agent", tier: CommandTier::Core, category: CommandCategory::WorkerLifecycle, kind: CommandKind::Dispatch(DispatchKind::AddAgent), summary: "add a worker", usage: "usage: team-agent add-agent AGENT --role-file FILE [--workspace WORKSPACE] [--team TEAM] [--no-display] [--json]", default_help: true, command_help: true, suggestion_index: true, token_usage: TokenUsage::Conditional, alias_of: None, sunset: None, action: None, governance: None },
    CommandSpec { name: "start-agent", tier: CommandTier::Core, category: CommandCategory::WorkerLifecycle, kind: CommandKind::Dispatch(DispatchKind::StartAgent), summary: "start an existing worker", usage: "usage: team-agent start-agent AGENT [--workspace WORKSPACE] [--team TEAM] [--force] [--allow-fresh] [--no-display] [--json]", default_help: true, command_help: true, suggestion_index: true, token_usage: TokenUsage::Conditional, alias_of: None, sunset: None, action: None, governance: None },
    CommandSpec { name: "stop-agent", tier: CommandTier::Core, category: CommandCategory::WorkerLifecycle, kind: CommandKind::Dispatch(DispatchKind::StopAgent), summary: "stop a worker", usage: "usage: team-agent stop-agent AGENT [--workspace WORKSPACE] [--team TEAM] [--json]", default_help: true, command_help: true, suggestion_index: true, token_usage: TokenUsage::No, alias_of: None, sunset: None, action: None, governance: None },
    CommandSpec { name: "reset-agent", tier: CommandTier::Core, category: CommandCategory::WorkerLifecycle, kind: CommandKind::Dispatch(DispatchKind::ResetAgent), summary: "reset a worker session", usage: "usage: team-agent reset-agent AGENT [--workspace WORKSPACE] [--team TEAM] [--discard-session] [--no-display] [--json]", default_help: true, command_help: true, suggestion_index: true, token_usage: TokenUsage::Conditional, alias_of: None, sunset: None, action: None, governance: None },
    CommandSpec { name: "diagnose", tier: CommandTier::Core, category: CommandCategory::Observe, kind: CommandKind::Dispatch(DispatchKind::Diagnose), summary: "inspect this workspace runtime and suggested repairs", usage: "usage: team-agent diagnose [--workspace WORKSPACE] [--team TEAM] [--json]", default_help: true, command_help: true, suggestion_index: true, token_usage: TokenUsage::No, alias_of: None, sunset: None, action: None, governance: None },
    CommandSpec { name: "claim-leader", tier: CommandTier::Guided, category: CommandCategory::GuidedRecovery, kind: CommandKind::Dispatch(DispatchKind::ClaimLeader), summary: "bind this pane when diagnose reports rebind_required", usage: "usage: team-agent claim-leader [--workspace WORKSPACE] [--team TEAM] [--confirm] [--json]", default_help: true, command_help: true, suggestion_index: true, token_usage: TokenUsage::No, alias_of: None, sunset: None, action: None, governance: None },
    CommandSpec { name: "takeover", tier: CommandTier::Guided, category: CommandCategory::GuidedRecovery, kind: CommandKind::Dispatch(DispatchKind::Takeover), summary: "replace a live leader binding with explicit consent", usage: "usage: team-agent takeover [--workspace WORKSPACE] [--team TEAM] [--confirm] [--json]", default_help: true, command_help: true, suggestion_index: true, token_usage: TokenUsage::No, alias_of: None, sunset: None, action: None, governance: None },
    CommandSpec { name: "attach-leader", tier: CommandTier::Guided, category: CommandCategory::GuidedRecovery, kind: CommandKind::Dispatch(DispatchKind::AttachLeader), summary: "bind an external leader pane", usage: "usage: team-agent attach-leader [--workspace WORKSPACE] [--team TEAM] [--pane PANE] [--provider PROVIDER] [--confirm] [--json]", default_help: true, command_help: true, suggestion_index: true, token_usage: TokenUsage::No, alias_of: None, sunset: None, action: None, governance: None },

    CommandSpec { name: "codex", tier: CommandTier::Core, category: CommandCategory::Start, kind: CommandKind::LeaderPassthrough { provider: "codex" }, summary: "launch a Codex leader", usage: "usage: team-agent codex [provider args...]", default_help: false, command_help: true, suggestion_index: true, token_usage: TokenUsage::Yes, alias_of: None, sunset: None, action: None, governance: None },
    CommandSpec { name: "claude", tier: CommandTier::Core, category: CommandCategory::Start, kind: CommandKind::LeaderPassthrough { provider: "claude" }, summary: "launch a Claude leader", usage: "usage: team-agent claude [provider args...]", default_help: false, command_help: true, suggestion_index: true, token_usage: TokenUsage::Yes, alias_of: None, sunset: None, action: None, governance: None },
    CommandSpec { name: "copilot", tier: CommandTier::Core, category: CommandCategory::Start, kind: CommandKind::LeaderPassthrough { provider: "copilot" }, summary: "launch a Copilot leader", usage: "usage: team-agent copilot [provider args...]", default_help: false, command_help: true, suggestion_index: true, token_usage: TokenUsage::Yes, alias_of: None, sunset: None, action: None, governance: None },

    CommandSpec { name: "init", tier: CommandTier::CompatHidden, category: CommandCategory::Compat, kind: CommandKind::Dispatch(DispatchKind::Init), summary: "legacy bootstrap command", usage: "usage: team-agent init [--workspace WORKSPACE] [--force] [--json]", default_help: false, command_help: true, suggestion_index: false, token_usage: TokenUsage::No, alias_of: Some("quick-start"), sunset: Some("C2"), action: Some("use `team-agent quick-start`"), governance: None },
    CommandSpec { name: "start", tier: CommandTier::CompatHidden, category: CommandCategory::Compat, kind: CommandKind::SpecOnlyAlias { alias_of: "quick-start" }, summary: "legacy start alias", usage: "usage: team-agent start [TEAMDIR] [--yes] [--fresh] [--json]", default_help: false, command_help: true, suggestion_index: false, token_usage: TokenUsage::Conditional, alias_of: Some("quick-start"), sunset: Some("C2"), action: Some("use `team-agent quick-start` or `team-agent restart`"), governance: None },
    CommandSpec { name: "stop", tier: CommandTier::CompatHidden, category: CommandCategory::Compat, kind: CommandKind::Dispatch(DispatchKind::Stop), summary: "legacy shutdown alias", usage: "usage: team-agent stop [--workspace WORKSPACE] [--team TEAM] [--keep-logs] [--json]", default_help: false, command_help: true, suggestion_index: false, token_usage: TokenUsage::No, alias_of: Some("shutdown"), sunset: Some("C2"), action: Some("use `team-agent shutdown`"), governance: None },
    CommandSpec { name: "restart-agent", tier: CommandTier::CompatHidden, category: CommandCategory::Compat, kind: CommandKind::Dispatch(DispatchKind::RestartAgent), summary: "legacy reset-agent alias", usage: "usage: team-agent restart-agent AGENT [--workspace WORKSPACE] [--team TEAM] [--discard-session] [--no-display] [--json]", default_help: false, command_help: true, suggestion_index: false, token_usage: TokenUsage::Conditional, alias_of: Some("reset-agent"), sunset: Some("C2"), action: Some("use `team-agent reset-agent`"), governance: None },
    CommandSpec { name: "stuck-list", tier: CommandTier::CompatHidden, category: CommandCategory::Compat, kind: CommandKind::Dispatch(DispatchKind::StuckList), summary: "legacy stuck alert listing", usage: "usage: team-agent stuck-list [--workspace WORKSPACE] [--team TEAM] [--json]", default_help: false, command_help: true, suggestion_index: false, token_usage: TokenUsage::No, alias_of: None, sunset: Some("C2"), action: Some("use `team-agent diagnose`"), governance: None },
    CommandSpec { name: "stuck-cancel", tier: CommandTier::CompatHidden, category: CommandCategory::Compat, kind: CommandKind::Dispatch(DispatchKind::StuckCancel), summary: "legacy stuck alert suppression", usage: "usage: team-agent stuck-cancel AGENT [--workspace WORKSPACE] [--alert-type stuck|idle_fallback|cross_worker_deadlock|all] [--json]", default_help: false, command_help: true, suggestion_index: false, token_usage: TokenUsage::No, alias_of: None, sunset: Some("C2"), action: Some("use the concrete notification action or `team-agent diagnose`"), governance: None },
    CommandSpec { name: "acknowledge-idle", tier: CommandTier::CompatHidden, category: CommandCategory::Compat, kind: CommandKind::Dispatch(DispatchKind::AcknowledgeIdle), summary: "legacy idle acknowledgement", usage: "usage: team-agent acknowledge-idle [--workspace WORKSPACE] [--team TEAM] [--json]", default_help: false, command_help: true, suggestion_index: false, token_usage: TokenUsage::No, alias_of: None, sunset: Some("C2"), action: Some("use the concrete idle notification action or `team-agent diagnose`"), governance: None },
    CommandSpec { name: "leaders", tier: CommandTier::Secondary, category: CommandCategory::Observe, kind: CommandKind::Dispatch(DispatchKind::Leaders), summary: "inspect registered host leaders", usage: "usage: team-agent leaders [--json]", default_help: false, command_help: true, suggestion_index: false, token_usage: TokenUsage::No, alias_of: None, sunset: None, action: None, governance: None },
    CommandSpec { name: "doctor", tier: CommandTier::Secondary, category: CommandCategory::Setup, kind: CommandKind::Dispatch(DispatchKind::Doctor), summary: "run host maintenance checks", usage: "usage: team-agent doctor [SPEC] [--workspace WORKSPACE] [--team TEAM] [--gate orphans|comms] [--comms] [--fix] [--fix-schema] [--cleanup-orphans] [--confirm] [--json]", default_help: false, command_help: true, suggestion_index: false, token_usage: TokenUsage::No, alias_of: None, sunset: None, action: None, governance: None },
    CommandSpec { name: "attach-app-server-leader", tier: CommandTier::Guided, category: CommandCategory::GuidedRecovery, kind: CommandKind::Dispatch(DispatchKind::AttachAppServerLeader), summary: "bind an app-server leader receiver", usage: "usage: team-agent attach-app-server-leader [--workspace WORKSPACE] [--team TEAM] --socket unix:///path.sock --thread-id THREAD_ID [--json]", default_help: false, command_help: true, suggestion_index: false, token_usage: TokenUsage::No, alias_of: None, sunset: None, action: None, governance: None },
    CommandSpec { name: "remove-agent", tier: CommandTier::Secondary, category: CommandCategory::WorkerLifecycle, kind: CommandKind::Dispatch(DispatchKind::RemoveAgent), summary: "remove a worker", usage: "usage: team-agent remove-agent AGENT [--workspace WORKSPACE] [--team TEAM] [--from-spec] [--confirm] [--force] [--json]", default_help: false, command_help: true, suggestion_index: false, token_usage: TokenUsage::No, alias_of: None, sunset: None, action: None, governance: None },
    CommandSpec { name: "fork-agent", tier: CommandTier::Secondary, category: CommandCategory::WorkerLifecycle, kind: CommandKind::Dispatch(DispatchKind::ForkAgent), summary: "fork a worker", usage: "usage: team-agent fork-agent SOURCE_AGENT --as AGENT [--label LABEL] [--workspace WORKSPACE] [--team TEAM] [--no-display] [--json]", default_help: false, command_help: true, suggestion_index: false, token_usage: TokenUsage::Conditional, alias_of: None, sunset: None, action: None, governance: None },
    CommandSpec { name: "allow-peer-talk", tier: CommandTier::Secondary, category: CommandCategory::Setup, kind: CommandKind::Dispatch(DispatchKind::AllowPeerTalk), summary: "allow direct worker peer talk", usage: "usage: team-agent allow-peer-talk A B [--workspace WORKSPACE] [--json]", default_help: false, command_help: true, suggestion_index: false, token_usage: TokenUsage::No, alias_of: None, sunset: None, action: None, governance: Some(GovernanceNote { decision: "secondary", reason: "capability/config surface" }) },
    CommandSpec { name: "approvals", tier: CommandTier::Secondary, category: CommandCategory::Observe, kind: CommandKind::Dispatch(DispatchKind::Approvals), summary: "inspect provider approval state", usage: "usage: team-agent approvals [AGENT] [--workspace WORKSPACE] [--team TEAM] [--json]", default_help: false, command_help: true, suggestion_index: false, token_usage: TokenUsage::No, alias_of: None, sunset: None, action: None, governance: Some(GovernanceNote { decision: "secondary", reason: "approval diagnostics" }) },
    CommandSpec { name: "profile", tier: CommandTier::Secondary, category: CommandCategory::Setup, kind: CommandKind::Dispatch(DispatchKind::Profile), summary: "manage provider profiles", usage: "usage: team-agent profile COMMAND NAME [--workspace WORKSPACE] [--team TEAM] [--auth-mode MODE] [--json]", default_help: false, command_help: true, suggestion_index: false, token_usage: TokenUsage::No, alias_of: None, sunset: None, action: None, governance: Some(GovernanceNote { decision: "secondary", reason: "provider/profile setup" }) },
    CommandSpec { name: "install-skill", tier: CommandTier::Secondary, category: CommandCategory::Setup, kind: CommandKind::Dispatch(DispatchKind::InstallSkill), summary: "install or uninstall Team Agent skills", usage: "usage: team-agent install-skill (--source DIR | --uninstall) [--target codex|claude|copilot|all] [--dest DIR] [--dry-run] [--json]", default_help: false, command_help: true, suggestion_index: false, token_usage: TokenUsage::No, alias_of: None, sunset: None, action: None, governance: Some(GovernanceNote { decision: "secondary", reason: "capability setup" }) },
    CommandSpec { name: "inbox", tier: CommandTier::Secondary, category: CommandCategory::Observe, kind: CommandKind::Dispatch(DispatchKind::Inbox), summary: "inspect queued messages", usage: "usage: team-agent inbox AGENT [--workspace WORKSPACE] [--team TEAM] [--limit N] [--since CURSOR] [--json]", default_help: false, command_help: true, suggestion_index: false, token_usage: TokenUsage::No, alias_of: None, sunset: None, action: None, governance: None },

    CommandSpec { name: "identity", tier: CommandTier::DevInternal, category: CommandCategory::Dev, kind: CommandKind::Dispatch(DispatchKind::Identity), summary: "inspect identity metadata", usage: "usage: team-agent identity [--workspace WORKSPACE] [--team TEAM] [--json]", default_help: false, command_help: true, suggestion_index: false, token_usage: TokenUsage::No, alias_of: None, sunset: None, action: None, governance: None },
    CommandSpec { name: "watch", tier: CommandTier::DevInternal, category: CommandCategory::Dev, kind: CommandKind::Dispatch(DispatchKind::Watch), summary: "watch event logs", usage: "usage: team-agent watch [--workspace WORKSPACE] [--team TEAM]", default_help: false, command_help: true, suggestion_index: false, token_usage: TokenUsage::No, alias_of: None, sunset: None, action: None, governance: None },
    CommandSpec { name: "sessions", tier: CommandTier::DevInternal, category: CommandCategory::Dev, kind: CommandKind::Dispatch(DispatchKind::Sessions), summary: "inspect session inventory", usage: "usage: team-agent sessions [--workspace WORKSPACE] [--team TEAM] [--json]", default_help: false, command_help: true, suggestion_index: false, token_usage: TokenUsage::No, alias_of: None, sunset: None, action: None, governance: None },
    CommandSpec { name: "compile", tier: CommandTier::DevInternal, category: CommandCategory::Dev, kind: CommandKind::Dispatch(DispatchKind::Compile), summary: "compile a team spec", usage: "usage: team-agent compile --team TEAM [--out FILE] [--json]", default_help: false, command_help: true, suggestion_index: false, token_usage: TokenUsage::No, alias_of: None, sunset: None, action: None, governance: None },
    CommandSpec { name: "validate", tier: CommandTier::DevInternal, category: CommandCategory::Dev, kind: CommandKind::Dispatch(DispatchKind::Validate), summary: "validate a team spec", usage: "usage: team-agent validate [SPEC] [--json]", default_help: false, command_help: true, suggestion_index: false, token_usage: TokenUsage::No, alias_of: None, sunset: None, action: None, governance: None },
    CommandSpec { name: "preflight", tier: CommandTier::DevInternal, category: CommandCategory::Dev, kind: CommandKind::Dispatch(DispatchKind::Preflight), summary: "run preflight checks", usage: "usage: team-agent preflight [TEAMDIR] [--json]", default_help: false, command_help: true, suggestion_index: false, token_usage: TokenUsage::No, alias_of: None, sunset: None, action: None, governance: None },
    CommandSpec { name: "wait-ready", tier: CommandTier::DevInternal, category: CommandCategory::Dev, kind: CommandKind::Dispatch(DispatchKind::WaitReady), summary: "wait for runtime readiness", usage: "usage: team-agent wait-ready [--workspace WORKSPACE] [--team TEAM] [--timeout SECONDS] [--json]", default_help: false, command_help: true, suggestion_index: false, token_usage: TokenUsage::No, alias_of: None, sunset: None, action: None, governance: None },
    CommandSpec { name: "e2e", tier: CommandTier::DevInternal, category: CommandCategory::Dev, kind: CommandKind::Dispatch(DispatchKind::E2e), summary: "run e2e harness", usage: "usage: team-agent e2e [--workspace WORKSPACE] [--providers LIST] [--real] [--json]", default_help: false, command_help: true, suggestion_index: false, token_usage: TokenUsage::Yes, alias_of: None, sunset: None, action: None, governance: None },
    CommandSpec { name: "peek", tier: CommandTier::DevInternal, category: CommandCategory::Dev, kind: CommandKind::Dispatch(DispatchKind::Peek), summary: "capture low-level terminal output", usage: "usage: team-agent peek AGENT [--workspace WORKSPACE] [--tail N|--head N] [--search TEXT] [--allow-raw-screen] [--json]", default_help: false, command_help: true, suggestion_index: false, token_usage: TokenUsage::No, alias_of: None, sunset: None, action: None, governance: None },
    CommandSpec { name: "coordinator", tier: CommandTier::DevInternal, category: CommandCategory::Dev, kind: CommandKind::Dispatch(DispatchKind::Coordinator), summary: "run the coordinator daemon", usage: "usage: team-agent coordinator [--workspace WORKSPACE] [--once] [--tick-interval SECONDS]", default_help: false, command_help: true, suggestion_index: false, token_usage: TokenUsage::No, alias_of: None, sunset: None, action: None, governance: None },
];

pub(crate) fn command_spec(name: &str) -> Option<&'static CommandSpec> {
    COMMAND_SPECS.iter().find(|spec| spec.name == name)
}
