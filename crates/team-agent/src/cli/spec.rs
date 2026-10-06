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
    AllowPeerTalk,
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
    DispatchKind::AllowPeerTalk,
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
    CommandSpec { name: "quick-start", tier: CommandTier::Core, category: CommandCategory::Start, kind: CommandKind::Dispatch(DispatchKind::QuickStart), summary: "从角色文件启动队伍，或查看已有队伍的连接方式", usage: "team-agent quick-start [TEAMDIR] [--workspace WORKSPACE] [--team TEAM] [--name NAME] [--team-id TEAM] [--yes] [--backend tmux|conpty] [--json] [--detail]", default_help: true, command_help: true, suggestion_index: true, token_usage: TokenUsage::Conditional, sunset: None, action: None, governance: None },
    CommandSpec { name: "send", tier: CommandTier::Core, category: CommandCategory::Start, kind: CommandKind::Dispatch(DispatchKind::Send), summary: "向指定队友派发任务", usage: "team-agent send <agent> MESSAGE... [--workspace WORKSPACE] [--team TEAM] [--mailbox] [--json]", default_help: true, command_help: true, suggestion_index: true, token_usage: TokenUsage::Conditional, sunset: Some("后续兼容版本"), action: Some("使用 team-agent send <agent> '任务内容'，用 --workspace 和 --team 选择队伍"), governance: None },
    CommandSpec { name: "status", tier: CommandTier::Core, category: CommandCategory::Start, kind: CommandKind::Dispatch(DispatchKind::Status), summary: "查看队友状态，不读取队友的对话内容", usage: "team-agent status [<agent>] [--workspace WORKSPACE] [--team TEAM] [--json] [--summary|--detail]", default_help: true, command_help: true, suggestion_index: true, token_usage: TokenUsage::No, sunset: None, action: None, governance: None },
    CommandSpec { name: "models", tier: CommandTier::Core, category: CommandCategory::Start, kind: CommandKind::Dispatch(DispatchKind::Models), summary: "查找工具实际支持的模型名称", usage: "team-agent models [--provider pi|cursor_agent|codex|claude|claude_code|grok] [QUERY|--search TEXT] [--json]", default_help: true, command_help: true, suggestion_index: true, token_usage: TokenUsage::No, sunset: None, action: None, governance: None },
    CommandSpec { name: "inbox", tier: CommandTier::Core, category: CommandCategory::Start, kind: CommandKind::Dispatch(DispatchKind::Inbox), summary: "查看队友回复", usage: "team-agent inbox <agent> [-n N|--limit N] [--workspace WORKSPACE] [--team TEAM] [--json]", default_help: true, command_help: true, suggestion_index: true, token_usage: TokenUsage::No, sunset: None, action: None, governance: None },
    CommandSpec { name: "restart", tier: CommandTier::Core, category: CommandCategory::TeamLifecycle, kind: CommandKind::Dispatch(DispatchKind::Restart), summary: "恢复已有队伍，优先沿用已保存的会话", usage: "team-agent restart [WORKSPACE] [--team TEAM] [--allow-fresh] [--session-converge-deadline SECONDS] [--json] [--detail]", default_help: true, command_help: true, suggestion_index: true, token_usage: TokenUsage::Conditional, sunset: None, action: None, governance: None },
    CommandSpec { name: "shutdown", tier: CommandTier::Core, category: CommandCategory::TeamLifecycle, kind: CommandKind::Dispatch(DispatchKind::Shutdown), summary: "关闭指定队伍，并查看是否仍有残留资源", usage: "team-agent shutdown [--workspace WORKSPACE] [--team TEAM] [--keep-logs] [--json]", default_help: true, command_help: true, suggestion_index: true, token_usage: TokenUsage::No, sunset: None, action: None, governance: None },
    CommandSpec { name: "add-agent", tier: CommandTier::Core, category: CommandCategory::WorkerLifecycle, kind: CommandKind::Dispatch(DispatchKind::AddAgent), summary: "新增并启动一位队友", usage: "team-agent add-agent <agent> [--role-file FILE] [--provider TOOL] [--model MODEL] [--effort LEVEL] [--bypass true|false] [--prompt TEXT] [--profile NAME] [--workspace WORKSPACE] [--team TEAM] [--json]", default_help: true, command_help: true, suggestion_index: true, token_usage: TokenUsage::Conditional, sunset: None, action: None, governance: None },
    CommandSpec { name: "start-agent", tier: CommandTier::Core, category: CommandCategory::WorkerLifecycle, kind: CommandKind::Dispatch(DispatchKind::StartAgent), summary: "启动队伍中已有的队友", usage: "team-agent start-agent <agent> [--provider TOOL] [--model MODEL] [--effort LEVEL] [--bypass true|false] [--prompt TEXT] [--profile NAME] [--workspace WORKSPACE] [--team TEAM] [--allow-fresh] [--json]", default_help: true, command_help: true, suggestion_index: true, token_usage: TokenUsage::Conditional, sunset: None, action: None, governance: None },
    CommandSpec { name: "stop-agent", tier: CommandTier::Core, category: CommandCategory::WorkerLifecycle, kind: CommandKind::Dispatch(DispatchKind::StopAgent), summary: "暂停一位队友，保留其配置和会话记录", usage: "team-agent stop-agent <agent> [--workspace WORKSPACE] [--team TEAM] [--json]", default_help: true, command_help: true, suggestion_index: true, token_usage: TokenUsage::No, sunset: None, action: None, governance: None },
    CommandSpec { name: "reset-agent", tier: CommandTier::Core, category: CommandCategory::WorkerLifecycle, kind: CommandKind::Dispatch(DispatchKind::ResetAgent), summary: "清除队友保存的会话关联", usage: "team-agent reset-agent <agent> --discard-session [--workspace WORKSPACE] [--team TEAM] [--json]", default_help: true, command_help: true, suggestion_index: true, token_usage: TokenUsage::No, sunset: None, action: None, governance: None },
    CommandSpec { name: "clone-agent", tier: CommandTier::Core, category: CommandCategory::WorkerLifecycle, kind: CommandKind::Dispatch(DispatchKind::CloneAgent), summary: "复制队友配置，创建新队友", usage: "team-agent clone-agent <agent> --as NEW_AGENT [--label LABEL] [--workspace WORKSPACE] [--team TEAM] [--json]", default_help: true, command_help: true, suggestion_index: true, token_usage: TokenUsage::Conditional, sunset: None, action: None, governance: None },
    CommandSpec { name: "fork-agent", tier: CommandTier::Core, category: CommandCategory::WorkerLifecycle, kind: CommandKind::Dispatch(DispatchKind::ForkAgent), summary: "在工具支持的条件下从队友会话分出新队友", usage: "team-agent fork-agent <agent> --as NEW_AGENT [--label LABEL] [--workspace WORKSPACE] [--team TEAM] [--json]", default_help: true, command_help: true, suggestion_index: true, token_usage: TokenUsage::Conditional, sunset: None, action: None, governance: None },
    CommandSpec { name: "remove-agent", tier: CommandTier::Core, category: CommandCategory::WorkerLifecycle, kind: CommandKind::Dispatch(DispatchKind::RemoveAgent), summary: "确认后移除一位队友", usage: "team-agent remove-agent <agent> --confirm [--workspace WORKSPACE] [--team TEAM] [--from-spec] [--force] [--json]", default_help: true, command_help: true, suggestion_index: true, token_usage: TokenUsage::No, sunset: None, action: None, governance: None },
    CommandSpec { name: "leaders", tier: CommandTier::Core, category: CommandCategory::Observe, kind: CommandKind::Dispatch(DispatchKind::Leaders), summary: "查看本机可用的主控及其所属队伍", usage: "team-agent leaders [QUERY|--search TEXT] [--all|--stale] [--json] | --prune [--dry-run] [--json]", default_help: true, command_help: true, suggestion_index: true, token_usage: TokenUsage::No, sunset: None, action: None, governance: None },
    CommandSpec { name: "doctor", tier: CommandTier::Core, category: CommandCategory::Observe, kind: CommandKind::Dispatch(DispatchKind::Doctor), summary: "检查队伍并给出恢复建议，默认不修复", usage: "team-agent doctor [SPEC] [--workspace WORKSPACE] [--team TEAM] [--comms] [--gate orphans|comms] [--fix] [--fix-schema] [--cleanup-orphans] [--confirm] [--json]", default_help: true, command_help: true, suggestion_index: true, token_usage: TokenUsage::No, sunset: None, action: None, governance: None },
    CommandSpec { name: "approvals", tier: CommandTier::Core, category: CommandCategory::Observe, kind: CommandKind::Dispatch(DispatchKind::Approvals), summary: "查看队友正在等待的权限确认", usage: "team-agent approvals [<agent>] [--workspace WORKSPACE] [--team TEAM] [--json]", default_help: true, command_help: true, suggestion_index: true, token_usage: TokenUsage::No, sunset: None, action: None, governance: None },
    CommandSpec { name: "allow-peer-talk", tier: CommandTier::Core, category: CommandCategory::Observe, kind: CommandKind::Dispatch(DispatchKind::AllowPeerTalk), summary: "允许两位队友直接交流", usage: "team-agent allow-peer-talk <agent> OTHER_AGENT [--workspace WORKSPACE] [--json]", default_help: true, command_help: true, suggestion_index: true, token_usage: TokenUsage::No, sunset: None, action: None, governance: None },
    CommandSpec { name: "route", tier: CommandTier::Core, category: CommandCategory::Setup, kind: CommandKind::Dispatch(DispatchKind::Route), summary: "设置工具启动时额外使用的命令行参数", usage: "team-agent route [status|enable|disable|show [TOOL]|set TOOL -- ARG...|add TOOL -- ARG...|clear TOOL] [--json]", default_help: true, command_help: true, suggestion_index: true, token_usage: TokenUsage::No, sunset: None, action: None, governance: None },
    CommandSpec { name: "profile", tier: CommandTier::Core, category: CommandCategory::Setup, kind: CommandKind::Dispatch(DispatchKind::Profile), summary: "管理登录方式和代理设置", usage: "team-agent profile init|doctor|show NAME [--workspace WORKSPACE] [--team TEAM] [--auth-mode MODE] [--proxy-mode direct|inherit] [--json]", default_help: true, command_help: true, suggestion_index: true, token_usage: TokenUsage::No, sunset: None, action: None, governance: None },
    CommandSpec { name: "install-skill", tier: CommandTier::Core, category: CommandCategory::Setup, kind: CommandKind::Dispatch(DispatchKind::InstallSkill), summary: "可选：安装已有的公开操作指南", usage: "team-agent install-skill (--source DIR|--uninstall) [--target codex|claude|copilot|all] [--dest DIR] [--dry-run] [--json]", default_help: true, command_help: true, suggestion_index: true, token_usage: TokenUsage::No, sunset: None, action: None, governance: None },
    CommandSpec { name: "claim-leader", tier: CommandTier::Guided, category: CommandCategory::GuidedRecovery, kind: CommandKind::Dispatch(DispatchKind::ClaimLeader), summary: "仅按体检提示，将当前终端登记为主控", usage: "team-agent claim-leader [--workspace WORKSPACE] [--team TEAM] [--confirm] [--json] [--detail]", default_help: true, command_help: true, suggestion_index: true, token_usage: TokenUsage::No, sunset: None, action: None, governance: None },
    CommandSpec { name: "takeover", tier: CommandTier::Guided, category: CommandCategory::GuidedRecovery, kind: CommandKind::Dispatch(DispatchKind::Takeover), summary: "仅按体检提示，确认后接管现有队伍", usage: "team-agent takeover [--workspace WORKSPACE] [--team TEAM] [--confirm] [--json]", default_help: true, command_help: true, suggestion_index: true, token_usage: TokenUsage::No, sunset: None, action: None, governance: None },
    CommandSpec { name: "attach-leader", tier: CommandTier::Guided, category: CommandCategory::GuidedRecovery, kind: CommandKind::Dispatch(DispatchKind::AttachLeader), summary: "仅按体检提示，将已核验的终端连接为主控", usage: "team-agent attach-leader [--workspace WORKSPACE] [--team TEAM] --pane PANE --provider TOOL [--confirm] [--json]", default_help: true, command_help: true, suggestion_index: true, token_usage: TokenUsage::No, sunset: None, action: None, governance: None },
    CommandSpec { name: "pi", tier: CommandTier::Core, category: CommandCategory::Start, kind: CommandKind::LeaderPassthrough { provider: "pi" }, summary: "用 Pi 打开主控对话", usage: "team-agent pi [--attach-existing] [--confirm] [--attach-session SESSION] [--external-leader] [--allow-nested-attach] [--json] [-- TOOL_ARGS...]", default_help: true, command_help: true, suggestion_index: true, token_usage: TokenUsage::Conditional, sunset: None, action: None, governance: None },
    CommandSpec { name: "codex", tier: CommandTier::Core, category: CommandCategory::Start, kind: CommandKind::LeaderPassthrough { provider: "codex" }, summary: "用 Codex 打开主控对话", usage: "team-agent codex [--attach-existing] [--confirm] [--attach-session SESSION] [--external-leader] [--allow-nested-attach] [--json] [-- TOOL_ARGS...]", default_help: true, command_help: true, suggestion_index: true, token_usage: TokenUsage::Conditional, sunset: None, action: None, governance: None },
    CommandSpec { name: "claude", tier: CommandTier::Core, category: CommandCategory::Start, kind: CommandKind::LeaderPassthrough { provider: "claude" }, summary: "用 Claude Code 打开主控对话", usage: "team-agent claude [--attach-existing] [--confirm] [--attach-session SESSION] [--external-leader] [--allow-nested-attach] [--json] [-- TOOL_ARGS...]", default_help: true, command_help: true, suggestion_index: true, token_usage: TokenUsage::Conditional, sunset: None, action: None, governance: None },
    CommandSpec { name: "copilot", tier: CommandTier::Core, category: CommandCategory::Start, kind: CommandKind::LeaderPassthrough { provider: "copilot" }, summary: "用 Copilot 打开主控对话", usage: "team-agent copilot [--attach-existing] [--confirm] [--attach-session SESSION] [--external-leader] [--allow-nested-attach] [--json] [-- TOOL_ARGS...]", default_help: true, command_help: true, suggestion_index: true, token_usage: TokenUsage::Conditional, sunset: None, action: None, governance: None },
    CommandSpec { name: "grok", tier: CommandTier::Core, category: CommandCategory::Start, kind: CommandKind::LeaderPassthrough { provider: "grok" }, summary: "用 Grok 打开主控对话", usage: "team-agent grok [--attach-existing] [--confirm] [--attach-session SESSION] [--external-leader] [--allow-nested-attach] [--json] [-- TOOL_ARGS...]", default_help: true, command_help: true, suggestion_index: true, token_usage: TokenUsage::Conditional, sunset: None, action: None, governance: None },
    CommandSpec { name: "cursor", tier: CommandTier::Core, category: CommandCategory::Start, kind: CommandKind::LeaderPassthrough { provider: "cursor_agent" }, summary: "用 Cursor 的 agent 工具打开主控对话", usage: "team-agent cursor [--attach-existing] [--confirm] [--attach-session SESSION] [--external-leader] [--allow-nested-attach] [--json] [-- TOOL_ARGS...]", default_help: true, command_help: true, suggestion_index: true, token_usage: TokenUsage::Conditional, sunset: None, action: None, governance: None },
];

pub(crate) fn command_spec(name: &str) -> Option<&'static CommandSpec> {
    COMMAND_SPECS.iter().find(|spec| spec.name == name)
}
