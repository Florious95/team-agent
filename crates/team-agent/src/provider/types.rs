//!
//! provider 共享类型:enums / newtypes / ProviderCaps / ProviderError / 捕获+classify payload / 占位结构。

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::AuthMode;

// ===========================================================================
// ENUMS (穷尽 + serde rename 到精确 Python 字符串)
// ===========================================================================





/// process identity guard 三值判定(`provider_state/common.py:109`)。
/// **ADJUDICATION**:doc 原名 `Liveness`,与既有 `model::enums::PaneLiveness`
/// (tmux pane 存活,**不同概念**)同名冲突 → 本 enum 命名 `ProcessLiveness` 避撞。
/// C4:`Unverifiable ≠ Alive`,绝不乐观读成 working。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessLiveness {
    Alive,
    Dead,
    Unverifiable,
}




/// approval prompt kind(`approvals/parsing.py:30/55/65`)。
/// 自动 approve 只放行 `McpTool` ∩ 白名单。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalKind {
    McpTool,
    Command,
    Unknown,
}

/// session 捕获来源(`claude.py:101/371`、`codex.py:84`)。golden fixture 字段,
/// §5 EventLog 名稳定一致。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureVia {
    FsWatch,
    FsMtimeFallback,
    FsRepair,
    SessionScan,
    ForkSnapshot,
}

/// attribution confidence(doc §56)。bug-085 fallback 固定 `Low`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Confidence {
    High,
    Medium,
    Low,
}

/// auth_hint 状态(`adapter.py:38` 等)。doctor 用。
///
/// `PresentWeak`(C-A-5 cr verdict v2):provider 无 auth-status 子命令(如 copilot
/// — main-help Commands 节仅 completion/help/init/login/mcp/plugin/update/version,
/// **无 status**)时,framework 只能弱检测(命令在 PATH + ~/.copilot/config.json
/// 存在 等表层信号)。诚实 surface(MUST-NOT-13)— 不假报"已登录",doctor 文案
/// 标"weak / no auth-status command available"。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthHintStatus {
    Present,
    PresentWeak,
    Missing,
    MissingOrUnknown,
    Unknown,
}



// ===========================================================================
// NEWTYPES (透明 String/PathBuf 包装 — id 混传根因,§3)
// ===========================================================================

/// provider session id(uuid)。**bug-085:`None` 合法**(compatible_api fallback,
/// `claude.py:366`)——调用面用 `Option<SessionId>`,穷尽 match `None` 不崩。
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SessionId(pub String);

impl SessionId {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for SessionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// provider session 日志路径(claude transcript / codex rollout)。
/// **bug-085:`None` → node 留 `Unknown`,不得当 idle**(doc §53)。
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RolloutPath(pub PathBuf);

impl RolloutPath {
    pub fn new(p: impl Into<PathBuf>) -> Self {
        Self(p.into())
    }
    pub fn as_path(&self) -> &std::path::Path {
        &self.0
    }
}



/// approval prompt fingerprint = sha256[:16] hex(`approvals/parsing.py:138`)。
/// 幂等 dedup key。doc §62 给两选项(`[u8;8]` / `String`)——选 transparent `String`
/// 与既有 hex newtype 风格(`LeaderSessionUuid`)一致,且序列化字节 == Python hex 串。
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ApprovalFingerprint(pub String);

impl ApprovalFingerprint {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}




// ===========================================================================
// STRUCT
// ===========================================================================

/// provider 能力位(doc §59)。`supports_session_fork` 还依赖
/// `auth_mode != compatible_api`(`claude.py:54`/`codex.py:45`)——`fork` 的运行期
/// 真值由 `ProviderAdapter::fork` + auth_mode 共同决定,此 struct 是静态声明。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ProviderCaps {
    pub resume: bool,
    pub fork: bool,
    pub native_mcp_config: bool,
    pub writes_global_settings: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ProviderCommandOverrides {
    pub model: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ProviderProfileLaunch {
    pub env_overlay: BTreeMap<String, String>,
    pub env_unset: BTreeSet<String>,
    pub command_overrides: ProviderCommandOverrides,
    pub claude_config_dir: Option<PathBuf>,
    pub claude_projects_root: Option<PathBuf>,
    pub managed_mcp_config: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct ProviderCommandContext<'a> {
    pub auth_mode: AuthMode,
    pub mcp_config: Option<&'a McpConfig>,
    pub system_prompt: Option<&'a str>,
    pub model: Option<&'a str>,
    pub dangerously_skip_permissions: bool,
    pub profile_launch: Option<&'a ProviderProfileLaunch>,
    /// Optional agent id, used as a launch-time display-name hint for
    /// providers that accept `--name <name>` (Claude, Copilot). Codex has
    /// no `--name` CLI flag (probe verdict 2026-06-22) and ignores this
    /// field. None = legacy callers (the field is purely additive).
    pub agent_id_hint: Option<&'a str>,
    /// 0.4.x provider effort MVP step 4: reasoning effort level resolved
    /// from role doc / TEAM.md / provider default. `None` means the
    /// framework passes no effort flag (provider default).
    ///   - Claude / ClaudeCode: low|medium|high|xhigh|max → `--effort <level>`
    ///   - Codex: low|medium|high|xhigh|max|ultra → `-c model_reasoning_effort=<level>`
    ///     Ultra enables Codex client delegation on top of max reasoning.
    ///   - Pi: low|medium|high|xhigh|max → `--thinking <level>`; ultra rejected
    ///   - Grok: low|medium|high|xhigh → `--effort <level>`; max/ultra rejected
    ///   - Copilot / Gemini / Fake: low..xhigh ignored with a caller warning;
    ///     max/ultra rejected.
    pub effort: Option<crate::model::enums::ProviderEffort>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandPlan {
    pub argv: Vec<String>,
    pub expected_session_id: Option<SessionId>,
    pub provider_projects_root: Option<PathBuf>,
    pub managed_mcp_config: bool,
}

impl CommandPlan {
    pub fn argv_only(argv: Vec<String>) -> Self {
        Self {
            argv,
            expected_session_id: None,
            provider_projects_root: None,
            managed_mcp_config: false,
        }
    }
}

// ===========================================================================
// ERROR
// ===========================================================================

/// step 8 provider 错误。**ADJUDICATION**:不复用 `model::errors::ModelError`
/// (那是 spec/envelope 校验层);provider 层有自己的失败语义:
///   - `CapabilityUnsupported`:占位 provider(copilot/opencode)调用即拒
///     (`unsupported.py:31` `ProviderCapabilityError`)——doc §126「绝不静默返回空命令」。
///   - `ResumeUnavailable`:bug-085 / 不可 resume 时**干净 raise 不崩**(`adapter.py` `ResumeUnavailable`)。
///   - `Io`:tmux capture / send-keys / 文件读写子进程失败(daemon tick 必须返 Result 不 panic)。
///
/// 实现层补充变体时保持 fallible 边界;§10 deny-lock 由 leader 加。
#[derive(Debug, Error)]
pub enum ProviderError {
    /// 占位 provider 能力未实现(copilot/opencode)。
    #[error("provider capability unsupported: {0}")]
    CapabilityUnsupported(String),
    /// resume 不可用(bug-085 compatible_api `session_id=None` 等)。
    #[error("resume unavailable: {0}")]
    ResumeUnavailable(String),
    /// 命令构造 / model 校验失败。
    #[error("provider command error: {0}")]
    Command(String),
    /// tmux / 文件 / 子进程 I/O 失败(返 Result 不 panic,株连 bug-084 是禁区)。
    #[error("provider io error: {0}")]
    Io(String),
}

// ===========================================================================
// 捕获返回 payload (doc §73:capture_session_id 返 6 键 dict 的 typed 版)
// ===========================================================================

/// `capture_session_id` 成功返回(`claude.py:73`/`codex.py:62` 的 typed dict)。
/// bug-085:`session_id` 可为 `None`,`rollout_path` 可为 `None`(半状态合法)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapturedSession {
    pub session_id: Option<SessionId>,
    pub rollout_path: Option<RolloutPath>,
    pub captured_via: CaptureVia,
    pub attribution_confidence: Confidence,
    pub spawn_cwd: PathBuf,
}

// ===========================================================================
// CLASSIFY 结果 (provider_state.common._result 的 typed 版 — doc §73)
// ===========================================================================






// ===========================================================================
// 占位结构 (impl 阶段填充;ROUND-0 仅命名让 trait 签名编得过)
// ===========================================================================

/// MCP server 配置(step 6 spec compiler 产出 / 本子系统消费)。
/// ROUND-0 占位:字段在 step 8 impl 时对照 `ensure_compatible_claude_mcp_config` 填充。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpConfig {
    /// 占位:实际 server map / transport / env 注入在 impl 时补。
    pub raw: serde_json::Value,
}
