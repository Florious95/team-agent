//! §19 散字符串态 → 穷尽 enum。每个 variant 与 Python 字符串**一一对应**(`#[serde]`
//! rename 到精确字符串),保证 spec/state/event 序列化字节不漂移(§7)。
//!
//! 值全部取自真相源 v0.2.11:`spec.py:13/116/156/172/174/176/229/255`、
//! `profiles/constants.py:6`、`task_graph.py:5-16`、`permissions.py:5-14/44-50`、
//! `display/backend.py:7-9`、`compiler.py:80`。

use serde::{Deserialize, Serialize};

/// provider(`SUPPORTED_PROVIDERS` `spec.py:13`)。**血泪 §3/陷阱 #4**:`claude` vs
/// `claude_code` 不能漏归一(原 Python 漏 → take-over 全死)。Rust 穷尽 match 防漏。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Provider {
    Claude,
    ClaudeCode,
    Codex,
    /// GitHub Copilot CLI(0.3.x 新增)。一期 subscription-only(已登录态),无 fork
    /// 能力(caps.fork=false → CapabilityUnsupported),system prompt 走 per-worker
    /// AGENTS.md + `COPILOT_CUSTOM_INSTRUCTIONS_DIRS` env(B2 灵魂件降级,§C2)。
    Copilot,
    GeminiCli,
    Fake,
}

/// 0.4.x Provider effort MVP: reasoning effort level passed to the provider.
/// Configuration sources (resolution order):
///   1. role doc front matter `effort: low|medium|high|xhigh|max`
///   2. TEAM.md front matter `provider_effort: low|medium|high|xhigh|max`
///   3. provider default (framework passes no flag)
///
/// Provider support:
///   - claude / claude_code: low|medium|high|xhigh|max → `--effort <level>`
///   - codex: low|medium|high|xhigh (NOT max) → `-c model_reasoning_effort=<level>`
///   - copilot / gemini_cli / fake: unsupported — warning event, no flag
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ProviderEffort {
    #[serde(rename = "low")]
    Low,
    #[serde(rename = "medium")]
    Medium,
    #[serde(rename = "high")]
    High,
    #[serde(rename = "xhigh")]
    XHigh,
    #[serde(rename = "max")]
    Max,
}

impl ProviderEffort {
    /// Parse from a wire string. Returns None on unknown literal.
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim() {
            "low" => Some(Self::Low),
            "medium" => Some(Self::Medium),
            "high" => Some(Self::High),
            "xhigh" => Some(Self::XHigh),
            "max" => Some(Self::Max),
            _ => None,
        }
    }

    /// Wire string used by both CLI argv and state serialization.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::XHigh => "xhigh",
            Self::Max => "max",
        }
    }

    /// Effort levels Claude-only (Codex / others must reject `max`).
    pub fn is_claude_only(self) -> bool {
        matches!(self, Self::Max)
    }

    /// True when the given provider supports this effort level. `max` is
    /// Claude-only; other levels are supported by Claude and Codex, ignored
    /// by Copilot/Gemini/Fake (warning emitted at runtime).
    pub fn is_supported_by(self, provider: Provider) -> bool {
        match provider {
            Provider::Claude | Provider::ClaudeCode => true,
            Provider::Codex => !self.is_claude_only(),
            Provider::Copilot | Provider::GeminiCli | Provider::Fake => false,
        }
    }
}

/// auth 模式(`AUTH_MODES` `profiles/constants.py:6`)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthMode {
    Subscription,
    OfficialApi,
    CompatibleApi,
}

/// task 状态(`TASK_STATUSES` `task_graph.py:5-16`)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Pending,
    Ready,
    Running,
    Blocked,
    NeedsRetry,
    Done,
    Failed,
    Cancelled,
}

impl TaskStatus {
    /// `TERMINAL_TASK_STATUSES = {done, failed, cancelled}`(`task_graph.py:16`)。
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Done | Self::Failed | Self::Cancelled)
    }
}

/// result-envelope 顶层 status(`spec.py:156`)。**与 `TaskStatus` 不同集合**(success≠done)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResultStatus {
    Success,
    Blocked,
    Failed,
    Partial,
}

/// envelope `changes[].kind`(`spec.py:172`)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    Created,
    Modified,
    Deleted,
    Observed,
}

/// envelope `tests[].status`(`spec.py:174`)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TestStatus {
    Passed,
    Failed,
    NotRun,
    Skipped,
}

/// `risks[].severity` + task.risk(`spec.py:176`)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RiskSeverity {
    Low,
    Medium,
    High,
}

/// team mode(`spec.py:116`)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TeamMode {
    SupervisorWorker,
    SwarmLimited,
}

/// comm protocol(`spec.py:229`)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommProtocol {
    McpInbox,
    FileBus,
}

/// terminal/session backend(`spec.py:255`)。§8:Windows 无原生 tmux → 能力门在 step 9,
/// enum 在此定义(Windows 原生经 WezTerm 后端,见 transport-backend-design.md)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Backend {
    Tmux,
    Pty,
}

/// display backend(`VALID_DISPLAY_BACKENDS` `display/backend.py:7-9`)。
/// **`adaptive` 在代码集里但不在 JSON schema enum** —— 以代码为准(陷阱 #5)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DisplayBackend {
    None,
    TmuxAttach,
    Iterm,
    Ghostty,
    GhosttyWindow,
    GhosttyWorkspace,
    Adaptive,
}

impl DisplayBackend {
    /// `DISPLAY_BACKENDS_WITH_WORKER_VIEWS = GHOSTTY_* | {adaptive}`(`display/backend.py:8`)。
    pub fn has_worker_views(self) -> bool {
        matches!(
            self,
            Self::Ghostty | Self::GhosttyWindow | Self::GhosttyWorkspace | Self::Adaptive
        )
    }
}

/// agent permission mode(schema `agent.permission_mode`;compiler 恒发 `restricted`)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionMode {
    Restricted,
    Ask,
    Trusted,
}

/// 规范化 tool(`CANONICAL_TOOLS` `permissions.py:5-14`)。别名(`fs_*`/`@builtin`/`*`)
/// 在 `expand_tools` 展开 —— 那是单独的 alias 输入枚举,不在此(step 2 后续)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tool {
    FsRead,
    FsWrite,
    FsList,
    ExecuteBash,
    GitDiff,
    Network,
    McpTeam,
    ProviderBuiltin,
}

/// provider×tool enforcement(`PROVIDER_ENFORCEMENT` `permissions.py:44-65`)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Enforcement {
    Hard,
    PromptOnly,
}

/// tmux pane 存活态(`state.py:336-341`)。**`Unknown` 既不可当 dead 也不可当 live**
/// (owner-gate `state.py:382` 用 `!= LIVE`)—— 穷尽 match,不 fallthrough(§11)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PaneLiveness {
    Live,
    Dead,
    Unknown,
}

/// context 策略 `receive_worker_outputs`。**已知三方 drift**(陷阱 #5):JSON schema 限
/// `{summary_only,structured_only,full_on_request}`,但 compiler 发
/// `business_messages_and_short_summaries`,且 `spec.py` 不校验该值。
/// → 保留 passthrough(`Other`),序列化回原字节,**不拒未知值**(§7)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "String", into = "String")]
pub enum ReceiveWorkerOutputs {
    SummaryOnly,
    StructuredOnly,
    FullOnRequest,
    /// passthrough:未知值原样保留(含 compiler 的 drift 默认值)。
    Other(String),
}

impl From<String> for ReceiveWorkerOutputs {
    fn from(s: String) -> Self {
        match s.as_str() {
            "summary_only" => Self::SummaryOnly,
            "structured_only" => Self::StructuredOnly,
            "full_on_request" => Self::FullOnRequest,
            _ => Self::Other(s),
        }
    }
}

impl From<ReceiveWorkerOutputs> for String {
    fn from(v: ReceiveWorkerOutputs) -> Self {
        match v {
            ReceiveWorkerOutputs::SummaryOnly => "summary_only".to_string(),
            ReceiveWorkerOutputs::StructuredOnly => "structured_only".to_string(),
            ReceiveWorkerOutputs::FullOnRequest => "full_on_request".to_string(),
            ReceiveWorkerOutputs::Other(s) => s,
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    /// 序列化字节 == Python 精确字符串(挑 rename 不平凡的 variant 逐一钉)。
    #[test]
    fn enums_serialize_to_exact_python_strings() {
        let cases: &[(String, &str)] = &[
            (
                serde_json::to_string(&Provider::ClaudeCode).unwrap(),
                "\"claude_code\"",
            ),
            (
                serde_json::to_string(&Provider::GeminiCli).unwrap(),
                "\"gemini_cli\"",
            ),
            (
                serde_json::to_string(&AuthMode::CompatibleApi).unwrap(),
                "\"compatible_api\"",
            ),
            (
                serde_json::to_string(&TaskStatus::NeedsRetry).unwrap(),
                "\"needs_retry\"",
            ),
            (
                serde_json::to_string(&TestStatus::NotRun).unwrap(),
                "\"not_run\"",
            ),
            (
                serde_json::to_string(&TeamMode::SupervisorWorker).unwrap(),
                "\"supervisor_worker\"",
            ),
            (
                serde_json::to_string(&CommProtocol::McpInbox).unwrap(),
                "\"mcp_inbox\"",
            ),
            (
                serde_json::to_string(&DisplayBackend::TmuxAttach).unwrap(),
                "\"tmux_attach\"",
            ),
            (
                serde_json::to_string(&DisplayBackend::GhosttyWorkspace).unwrap(),
                "\"ghostty_workspace\"",
            ),
            (
                serde_json::to_string(&DisplayBackend::Adaptive).unwrap(),
                "\"adaptive\"",
            ),
            (
                serde_json::to_string(&Tool::ExecuteBash).unwrap(),
                "\"execute_bash\"",
            ),
            (
                serde_json::to_string(&Tool::ProviderBuiltin).unwrap(),
                "\"provider_builtin\"",
            ),
            (
                serde_json::to_string(&Enforcement::PromptOnly).unwrap(),
                "\"prompt_only\"",
            ),
            (
                serde_json::to_string(&PaneLiveness::Unknown).unwrap(),
                "\"unknown\"",
            ),
            (serde_json::to_string(&Backend::Pty).unwrap(), "\"pty\""),
        ];
        for (got, want) in cases {
            assert_eq!(got, want);
        }
    }

    #[test]
    fn provider_all_five_round_trip() {
        for s in ["claude", "claude_code", "codex", "gemini_cli", "fake"] {
            let json = format!("\"{s}\"");
            let p: Provider = serde_json::from_str(&json).unwrap();
            assert_eq!(serde_json::to_string(&p).unwrap(), json);
        }
    }

    #[test]
    fn unknown_provider_string_is_rejected() {
        // 与 receive_worker_outputs 不同:Provider 是封闭集,未知值必须 Err(不 passthrough)。
        // NOTE: "copilot" 已 0.3.5 加入(design + cr verdict 全 APPROVE),改用一个
        // 仍未注册的串验"封闭集"语义。
        assert!(serde_json::from_str::<Provider>("\"gibberish\"").is_err());
    }

    #[test]
    fn task_status_terminal_set() {
        for s in [TaskStatus::Done, TaskStatus::Failed, TaskStatus::Cancelled] {
            assert!(s.is_terminal());
        }
        for s in [
            TaskStatus::Pending,
            TaskStatus::Ready,
            TaskStatus::Running,
            TaskStatus::Blocked,
            TaskStatus::NeedsRetry,
        ] {
            assert!(!s.is_terminal());
        }
    }

    #[test]
    fn display_backend_worker_views() {
        for b in [
            DisplayBackend::Ghostty,
            DisplayBackend::GhosttyWindow,
            DisplayBackend::GhosttyWorkspace,
            DisplayBackend::Adaptive,
        ] {
            assert!(b.has_worker_views());
        }
        for b in [
            DisplayBackend::None,
            DisplayBackend::TmuxAttach,
            DisplayBackend::Iterm,
        ] {
            assert!(!b.has_worker_views());
        }
    }

    #[test]
    fn receive_worker_outputs_passthrough_preserves_drift_value() {
        // compiler 发的 drift 值必须原样保留(byte 不丢),不被拒。
        let drift = "\"business_messages_and_short_summaries\"";
        let v: ReceiveWorkerOutputs = serde_json::from_str(drift).unwrap();
        assert_eq!(
            v,
            ReceiveWorkerOutputs::Other("business_messages_and_short_summaries".to_string())
        );
        assert_eq!(serde_json::to_string(&v).unwrap(), drift);
        // 已知值仍映射到 variant。
        assert_eq!(
            serde_json::from_str::<ReceiveWorkerOutputs>("\"structured_only\"").unwrap(),
            ReceiveWorkerOutputs::StructuredOnly
        );
        assert_eq!(
            serde_json::to_string(&ReceiveWorkerOutputs::SummaryOnly).unwrap(),
            "\"summary_only\""
        );
    }
}
