//! provider 共享类型:enums / newtypes / ProviderCaps / ProviderError / 捕获+classify payload / 占位结构。

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use thiserror::Error;

// ===========================================================================
// ENUMS (穷尽 + serde rename 到精确 Python 字符串)
// ===========================================================================

/// node 分类结果(`provider_state/common.py` / `idle_takeover.py`)。
/// **doc §49 铁律:`Unknown` 必须显式 block ping,绝不 fallthrough 成 idle**
/// (`idle_predicate.py:46-49` 任何非 `{idle, idle_interrupted}` 立刻 block)。
/// `idle_takeover_contract.md` 列为稳定 contract enum。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnState {
    Idle,
    Working,
    IdleInterrupted,
    BlockedOnHuman,
    Abnormal,
    Unknown,
}

impl TurnState {
    /// take-over predicate 仅对 `{Idle, IdleInterrupted}` 放行 ping
    /// (`idle_predicate.py:46-49`;C12 interrupted 算 idle 带注解)。其余一律 block。
    pub fn is_idle_for_takeover(self) -> bool {
        matches!(self, Self::Idle | Self::IdleInterrupted)
    }
}

/// reader 输出的归一 lifecycle 事件类型(`provider_state/common.py:15-20`)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FactKind {
    TurnOpen,
    TurnComplete,
    Interrupted,
    Failed,
    Approval,
    Error,
}

impl FactKind {
    /// `_CLOSING = {complete, interrupted, failed}`(`common.py:22`)——闭合一个 open turn。
    pub fn is_closing(self) -> bool {
        matches!(self, Self::TurnComplete | Self::Interrupted | Self::Failed)
    }
}

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

/// agent runtime status(`approvals/status.py:175` / `refresh_agent_runtime_statuses`)。
/// §19 散字符串态 → enum。归 step 5 state 但 step 8 写。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentRuntimeStatus {
    Running,
    Busy,
    Error,
    Missing,
    Paused,
    Stopped,
    AwaitingTrustPrompt,
}

/// agent health label(`approvals/status.py:114-124,98`)。message-store(step 7)写入。
/// **注意:Python 此处是大写串**(`"RUNNING"/.../"AWAITING_APPROVAL"`),serde SCREAMING_SNAKE_CASE
/// (多词变体 `AwaitingApproval` → `"AWAITING_APPROVAL"` 带下划线,非 UPPERCASE 的 "AWAITINGAPPROVAL")。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum HealthStatus {
    Running,
    Idle,
    Working,
    Blocked,
    Error,
    Done,
    Stuck,
    Uncertain,
    AwaitingApproval,
}

pub fn agent_health_status(status: &str) -> HealthStatus {
    match status.to_ascii_lowercase().as_str() {
        "busy" => HealthStatus::Running,
        "running" => HealthStatus::Idle,
        "working" => HealthStatus::Working,
        "paused" | "blocked" | "awaiting_approval" | "awaiting_trust_prompt" => {
            HealthStatus::Blocked
        }
        "error" | "missing" | "interrupted" => HealthStatus::Error,
        "stopped" | "done" => HealthStatus::Done,
        "stuck" => HealthStatus::Stuck,
        "uncertain" => HealthStatus::Uncertain,
        _ => HealthStatus::Idle,
    }
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthHintStatus {
    Present,
    Missing,
    MissingOrUnknown,
    Unknown,
}

/// take-over reason / event 名(`idle_predicate.py` `_result` 的 `reason` 字段)。
/// §5 EventLog:JSON 事件名 Rust 必须字节一致。固定串变体用 serde rename;
/// `Node(TurnState)` 承载 `"node_<state>"` 动态形态(`idle_predicate.py:49`)。
///
/// **铁律(BLOOD-LINE PIN, bug-071/077/085)**:任何非 `{Idle, IdleInterrupted}` node
/// 必须命中 `Node(state)` 并 block ping;`Unknown` 渲染 `"node_unknown"`,绝不 fallthrough idle。
/// `reason_str()` 给出 Python `read_turn_state`/`evaluate_takeover_reminder` 的精确 reason 串。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoPingReason {
    /// worker 从未 open 过 turn(`idle_predicate.py:61`)。
    NotArmedNoWorkerTurn,
    /// suppress flag 置位(acknowledge-idle 后,`idle_predicate.py:63`)。
    Acknowledged,
    /// armed 但 elapsed < debounce(`idle_predicate.py:65`)。
    DebounceActive,
    /// 本 episode 已 ping 过一次(`idle_predicate.py:67`)。
    AlreadyPingedThisEpisode,
    /// 全 idle + armed + debounce 到期 → **ping 侧 reason**(`idle_predicate.py:72`,should_ping=True)。
    AllIdleDebounceElapsed,
    /// 无 node(`idle_predicate.py:52`)。
    NoNodes,
    /// node 处于非 idle 态被 block,渲染 `"node_<state>"`(`idle_predicate.py:49`)。
    /// `Unknown` → `"node_unknown"`(missing-state 同样归 unknown)。
    Node(TurnState),
}

impl NoPingReason {
    /// Python `_result` 的 `reason` 字段精确串(`idle_predicate.py`)。
    /// 固定变体直接给串;`Node(state)` 拼成 `"node_<state-wire>"`。
    pub fn reason_str(&self) -> String {
        match self {
            Self::NotArmedNoWorkerTurn => "not_armed_no_worker_turn".to_string(),
            Self::Acknowledged => "acknowledged".to_string(),
            Self::DebounceActive => "debounce_active".to_string(),
            Self::AlreadyPingedThisEpisode => "already_pinged_this_episode".to_string(),
            Self::AllIdleDebounceElapsed => "all_idle_debounce_elapsed".to_string(),
            Self::NoNodes => "no_nodes".to_string(),
            Self::Node(state) => {
                let wire = match state {
                    TurnState::Idle => "idle",
                    TurnState::Working => "working",
                    TurnState::IdleInterrupted => "idle_interrupted",
                    TurnState::BlockedOnHuman => "blocked_on_human",
                    TurnState::Abnormal => "abnormal",
                    TurnState::Unknown => "unknown",
                };
                format!("node_{wire}")
            }
        }
    }
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

/// turn id(claude `requestId`/`uuid`;codex `turn_id`)。abnormal dedup key 一半
/// `(signature, turn_id)`(C8,`claude.py:54-62`)。`None` 合法 → `Option<TurnId>`。
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TurnId(pub String);

impl TurnId {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }
    pub fn as_str(&self) -> &str {
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

/// abnormal/fault fact 签名(`claude.py:51/57` `"signature"`、`codex.py:65/80`)。
/// C8 dedup key 的另一半 `(Signature, Option<TurnId>)`。固定取值:
/// `api_error` / `tool_result_is_error` / `turn_failed` / `approval_required`。
/// transparent String 与 hex newtype 风格一致,序列化字节 == Python 串。
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Signature(pub String);

impl Signature {
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
    /// session 捕获失败 / 超时。
    #[error("session capture failed: {0}")]
    CaptureFailed(String),
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

/// classify 结果来源(`provider_state/common.py:_result` 的 `source` 字段)。
/// `session_file`:verdict 出自日志 lifecycle fact;`process_guard`:open turn 被
/// process-identity 判定 demote;`registry`:未知 provider 兜底。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClassifySource {
    SessionFile,
    ProcessGuard,
    Registry,
}

/// 中性 classify 结果(`provider_state.common.decide_state`/`_result` 的 6 键 dict)。
/// **铁律**:`state == Unknown` 时 `reason` 必为 `unreadable_or_empty` /
/// `no_turn_lifecycle_fact` / `process_identity_unverified` / `unknown_provider`,
/// 且 `is_idle_for_takeover() == false`(穷尽,无 `_ => idle`)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClassifyResult {
    pub state: TurnState,
    pub turn_id: Option<TurnId>,
    pub reason: String,
    pub source: ClassifySource,
    pub annotations: Vec<String>,
    pub diagnostics: Vec<serde_json::Value>,
}

/// abnormal track 消费的 fault/approval fact(`provider_state.read_fault_facts`)。
/// C8 dedup key = `(signature, turn_id)`。`turn_id` 可 `None`(`api_error` 无 ids)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FaultFact {
    pub signature: Signature,
    pub turn_id: Option<TurnId>,
    pub kind: FactKind,
}

/// take-over reminder 判定结果(`idle_predicate.evaluate_takeover_reminder` `_result`)。
/// `interrupted_nodes`:C12 idle_interrupted node 的 id 列表(注解穿透进 ping 结果)。
/// `message`:should_ping 时携带的中性提醒文案,否则 `None`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemindResult {
    pub should_ping: bool,
    pub reason: NoPingReason,
    pub interrupted_nodes: Vec<String>,
    pub message: Option<String>,
}

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

/// pane→status 识别正则集(`provider_cli/claude.py:225`/`codex.py:140`
/// `status_patterns()` 返回的 idle/processing/error 三正则)。
/// claude:`idle=r"[>❯]\s"` `processing=r"[✶✢✽✻✳·].*…"` `error="Error|Traceback"`;
/// codex:`idle=r"(›|❯|codex>)"` `processing=r"•.*esc to interrupt"` `error="Error|Traceback|panic"`。
#[derive(Debug, Clone)]
pub struct StatusPatterns {
    pub idle: regex::Regex,
    pub processing: regex::Regex,
    pub error: regex::Regex,
}
