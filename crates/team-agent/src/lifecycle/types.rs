//! lifecycle 数据类型:newtype / enum / data struct / error / outcome-report 集中定义。

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::model::ids::AgentId;
use crate::provider::{RolloutPath, SessionId};
use crate::transport::{PaneId, SessionName, WindowName};

use super::DisplayBackend;

// ===========================================================================
// CROSS-LANE PLACEHOLDERS(13/14/15 兄弟 lane 尚未交付;leader 集成时 reconcile)
// ===========================================================================

/// step8 provider 还未暴露的 launch/resume/fork 命令构造门面。lifecycle 经它构造
/// provider 命令字符串(`shell_command_for_agent`/`shell_resume_command_for_agent`/
/// `shell_fork_command_for_agent`,`runtime` 自由函数)—— provider lane 落地后替换为
/// 真实 `provider::shell_*` API。**PLACEHOLDER**:仅占位 argv,字段映射由 provider lane 定。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellCommand {
    pub argv: Vec<String>,
}

/// `EventLog` 已在 step4 落地,但 lifecycle 发射的 typed `EventKind` 名集(`lifecycle.*`/
/// `restart.*`/`display.*`)是 step4 owned 的稳定事件名(JSON 名与 Python 一致)。
/// **PLACEHOLDER**:此处仅命名 lifecycle 关心的事件名常量集,leader 集成时映射到
/// step4 `event_log::EventLog::write(name, fields)`。
pub mod event_names {
    // lifecycle 步进事件(Gap 15/16:发射顺序被测试锁死)。
    pub const ADD_STEP_COMPLETED: &str = "lifecycle.add_step_completed";
    pub const ADD_STEP_ROLLED_BACK: &str = "lifecycle.add_step_rolled_back";
    pub const ADD_FAILED: &str = "lifecycle.add_failed";
    pub const REMOVE_STEP_COMPLETED: &str = "lifecycle.remove_step_completed";
    pub const REMOVE_ROLLED_BACK: &str = "lifecycle.remove_rolled_back";
    // restart 决策事件(Route B audit 契约必发)。
    pub const RESTART_RESUME_DECISION: &str = "restart.resume_decision";
    pub const RESTART_ATOMIC_REFUSAL: &str = "restart.atomic_refusal";
    pub const RESTART_FIRST_SEND_AT_INVALID: &str = "restart.first_send_at_invalid";
    pub const RESTART_FRESH_SPAWN: &str = "restart.fresh_spawn";
    pub const RESTART_ROLLBACK_SESSION: &str = "restart.rollback_session";
    // display 事件(C15/C16:每次降级非静默)。
    pub const DISPLAY_BACKEND_RESOLVED: &str = "display.backend_resolved";
    pub const DISPLAY_ADAPTIVE_OPENED: &str = "display.adaptive_opened";
    pub const DISPLAY_ADAPTIVE_BLOCKED: &str = "display.adaptive_blocked";
    pub const DISPLAY_ADAPTIVE_REBUILT: &str = "display.adaptive_rebuilt";
    pub const DISPLAY_ADAPTIVE_CLOSED: &str = "display.adaptive_closed";
}

// ===========================================================================
// NEWTYPES(card §3:散 str → newtype。`AgentId`/`SessionName` 复用既有;
// 此处新增 plan id / 派生 session 名)
// ===========================================================================

/// `PlanId`(`_PLAN_ID_RE = ^[A-Za-z0-9][A-Za-z0-9_.-]{0,63}$`,`orchestrator/state.py:11`)。
/// **必须 newtype**:防路径穿越(无 `/`、空格);`sanitize_plan_id` 拒绝则 `InvalidPlanId`。
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PlanId(String);

impl PlanId {
    /// `sanitize_plan_id`(`orchestrator/state.py:18`):正则校验,失败 → `Err`。
    pub fn parse(raw: &str) -> Result<Self, LifecycleError> {
        let mut chars = raw.chars();
        let first_ok = chars
            .next()
            .map(|c| c.is_ascii_alphanumeric())
            .unwrap_or(false);
        let rest_ok = chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'));
        if first_ok && rest_ok && raw.len() <= 64 {
            Ok(Self(raw.to_string()))
        } else {
            Err(LifecycleError::InvalidPlanId(format!(
                "{raw:?} does not match ^[A-Za-z0-9][A-Za-z0-9_.-]{{0,63}}$; no slashes, spaces, or path-traversal segments are allowed"
            )))
        }
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for PlanId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// ghostty 派生的唯一 linked-session 名(`ghostty_display_session_name`,sha1 派生,
/// `display/ghostty.py`)。与 worker 的 `SessionName` 类型上区分,防混传(card §57)。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DisplaySessionName(pub String);

impl DisplaySessionName {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

// ===========================================================================
// ENUM(card §3/§11:散字符串 → 穷尽 enum)
// ===========================================================================

/// `start_mode` / `restart_mode`(`start.py:179`,`orchestration.py:208`)。
/// **必须 enum**:start/restart 全程穷尽分支;`noop`(窗口已存在且非 force)只在
/// `start_agent` 出现。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StartMode {
    Resumed,
    Fresh,
    FreshAfterMissingRollout,
    Noop,
}

/// Route B resume 决策(`orchestration.py:498-505`)。每非 paused worker 发一条
/// `restart.resume_decision`;`Refuse` 是 atomic refusal 的唯一触发。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResumeDecision {
    Resume,
    FreshStart,
    Refuse,
}

/// `first_send_at` 严格分类(`_classify_first_send_at`,`orchestration.py:399-426`)。
/// **必须 enum + 严格解析**:显式拒空串 / `0` / `False` / `"null"` / 非 ISO;
/// garbage **hard refuse**,绝不靠 truthiness 把 `""` 当 absent 漏过去(陷阱)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FirstSendAtState {
    /// `null` / 缺失 —— worker 从未交互,可丢弃 fresh。
    Absent,
    /// 合法 ISO-8601 UTC 串。
    Valid,
    /// `""` / `0` / `False` / `"null"` / 非 ISO —— state.json 损坏,决策前 hard refuse。
    Corrupt,
}

/// adaptive 阻塞 reason(`ADAPTIVE_BLOCK_REASONS`,6 个封闭值,`display/adaptive.py:21`)。
/// **必须 enum**(契约 C16 封闭集);`adaptive_blocked` 对越界 reason 兜底成
/// `AggregatorRebuildFailed`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdaptiveBlockReason {
    LeaderNotInTmux,
    SplitFailed,
    WindowCreateFailed,
    WorkerSessionMissing,
    NotImplementedThisPlatform,
    AggregatorRebuildFailed,
}

/// display `status`(`agent_state["display"]["status"]`,`start.py:123`)。
/// **应 enum**:决定 `start_agent` 是否重开显示。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DisplayStatus {
    Opened,
    Blocked,
    Stopped,
}

/// plan `status`(`orchestrator/__init__.py`)。**应 enum**:`start_plan` 对已
/// running/halted/completed 幂等返回。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanStatus {
    Running,
    Halted,
    Completed,
}

/// 危险审批继承来源(`config.py:16` `dangerous_auto_approve_source`)。**应 enum**:
/// launch 在 `inherited=false` 且无 `--yes` 时 raise(`core.py:120`)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DangerousApprovalSource {
    RuntimeConfig,
    LeaderProcess,
    Disabled,
}

// ===========================================================================
// DATA STRUCT(每 worker display / restart 候选 / plan state / 危险审批)
// ===========================================================================

/// adaptive 能力探测结果(`probe_display_capabilities`,`display/adaptive.py:31`)。
/// **C13 一等公民**:分支只看 probe,不看 `cfg!(target_os)`;Windows/WSL →
/// `NotImplementedThisPlatform`。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DisplayProbe {
    pub in_tmux: bool,
    pub platform: String,
    pub leader_session: Option<SessionName>,
    pub leader_pane: Option<PaneId>,
    pub caps: CapsFlags,
    /// 探测后的 adaptive 状态(可开 / 已封闭+reason)。
    pub adaptive_status: DisplayStatus,
    /// 封闭时填 reason(C16 封闭集)。
    pub reason: Option<AdaptiveBlockReason>,
}

/// 平台能力位(`caps{tmux_append_windows, adaptive_display}`)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CapsFlags {
    pub tmux_append_windows: bool,
    pub adaptive_display: bool,
}

/// 每 worker display state(写进 `state.agents.<id>.display`)。adaptive vs ghostty_window
/// vs ghostty_workspace 字段不同 → enum 区分(card §50)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "backend", rename_all = "snake_case")]
pub enum WorkerDisplay {
    Adaptive {
        status: DisplayStatus,
        window: Option<WindowName>,
        workspace_window: Option<WindowName>,
        pane_id: Option<PaneId>,
        pane_title: Option<String>,
        target: Option<String>,
        target_worker_session: Option<String>,
        linked_session: Option<String>,
        leader_session: Option<SessionName>,
        display_session: Option<SessionName>,
        fallback: Option<String>,
    },
    GhosttyWindow {
        status: DisplayStatus,
        linked_session: DisplaySessionName,
        display_session: DisplaySessionName,
    },
    GhosttyWorkspace {
        status: DisplayStatus,
        display_session: DisplaySessionName,
    },
    Blocked {
        reason: AdaptiveBlockReason,
    },
}

/// `RestartCandidate`(`restart/selection.py:27`)。`select_restart_state` 多 team 选择;
/// `has_context` 是 resume 可行性粗判。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RestartCandidate {
    pub session_name: SessionName,
    pub team_name: String,
    pub state_path: PathBuf,
    pub spec_path: PathBuf,
    pub agents: Vec<AgentId>,
    pub has_context: bool,
}

/// 危险审批继承态(`config.py:16` `dangerous_auto_approve{_source,_inherited,_provider,_flag}`)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DangerousApproval {
    pub enabled: bool,
    pub source: DangerousApprovalSource,
    pub inherited: bool,
    pub provider: Option<String>,
    pub flag: Option<String>,
    pub worker_capability_above_leader: bool,
    pub ancestry_binary_name: Option<String>,
    pub unexpected_binary: bool,
}

/// `PlanState`(`orchestrator/__init__.py:51`)。plan 多 stage 状态机持久态。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanState {
    pub plan_id: PlanId,
    pub plan_path: PathBuf,
    pub team: Option<String>,
    pub current_stage: i64,
    pub started_at: String,
    pub completed_stages: Vec<String>,
    pub status: PlanStatus,
    pub halt_reason: Option<String>,
    pub halt_artifact: Option<PathBuf>,
    pub stages: Vec<PlanStage>,
    pub current_dispatch: Option<String>,
}

/// 单 stage(`orchestrator/plan.py` stage mapping)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanStage {
    pub id: String,
    pub assignee: Option<AgentId>,
    pub prompt: Option<String>,
    pub on_result: Option<PlanCondition>,
    pub status: Option<String>,
}

/// plan 推进条件(封闭文法 `_CONDITION_RE`,`orchestrator/plan.py:9`)。
/// **必须 typed**:`any` | `report_result.<field> == '<value>'`;越界 → `InvalidPlan`。
/// 不做成自由表达式。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PlanCondition {
    /// 无条件推进。
    Any,
    /// `report_result.<field> == '<value>'`。
    FieldEq { field: String, value: String },
}

impl PlanCondition {
    /// 解析封闭条件文法。越界 raise `InvalidPlan`(`orchestrator/plan.py:_is_supported_condition`)。
    pub fn parse(expr: &str) -> Result<Self, LifecycleError> {
        let trimmed = expr.trim();
        if trimmed.eq_ignore_ascii_case("any") {
            return Ok(Self::Any);
        }

        let Some(rest) = trimmed.strip_prefix("report_result.") else {
            return Err(LifecycleError::InvalidPlan(format!(
                "unsupported condition: {expr}"
            )));
        };
        let Some((field, value_expr)) = rest.split_once("==") else {
            return Err(LifecycleError::InvalidPlan(format!(
                "unsupported condition: {expr}"
            )));
        };
        let field = field.trim();
        if field.is_empty() || !field.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return Err(LifecycleError::InvalidPlan(format!(
                "unsupported condition: {expr}"
            )));
        }
        let value_expr = value_expr.trim();
        let value = if let Some(inner) = value_expr
            .strip_prefix('\'')
            .and_then(|v| v.strip_suffix('\''))
        {
            inner
        } else if let Some(inner) = value_expr
            .strip_prefix('"')
            .and_then(|v| v.strip_suffix('"'))
        {
            inner
        } else {
            return Err(LifecycleError::InvalidPlan(format!(
                "unsupported condition: {expr}"
            )));
        };
        Ok(Self::FieldEq {
            field: field.to_string(),
            value: value.to_string(),
        })
    }
}

// ===========================================================================
// ERROR(card §10:fallible 边界;daemon/CLI 入口返 rich Result<Report, Error>)
// ===========================================================================

/// lifecycle 子系统错误。能力性降级(adaptive blocked)**不**走这里 —— 那是 typed
/// outcome(`DisplayStatus::Blocked` / `AdaptiveBlockReason`)。这里只装真失败:
/// owner-gate 拒绝、session 冲突、state 写崩(bug-084)、provider/transport I/O、回滚失败。
#[derive(Debug, Error)]
pub enum LifecycleError {
    /// owner-gate 拒绝(foreign owner;`check_team_owner` 失败 —— lifecycle 第一道门)。
    #[error("owner gate refused: {0}")]
    OwnerRefused(String),
    /// 同名 tmux session 已存在 —— **拒绝而非 kill**(`core.py:127`,`orchestration.py:79`)。
    #[error("tmux session conflict: {0}")]
    SessionConflict(String),
    /// 危险自动审批未显式确认(`core.py:120`:inherited=false 且无 --yes)。
    #[error("dangerous auto-approve requires explicit --yes: {0}")]
    DangerousApprovalRequired(String),
    /// 启动前门失败(`ensure_agent_start_requirements`:provider/profile/model check)。
    #[error("agent start requirement unmet: {0}")]
    RequirementUnmet(String),
    /// state 持久化失败(bug-084:`os.replace` EACCES/EPERM/EBUSY 退避后仍败)。
    #[error("state persistence failed: {0}")]
    StatePersist(String),
    /// 编译 spec / role doc 失败(`compile_team`/`compile_role_doc_agent`)。
    #[error("spec compile failed: {0}")]
    Compile(String),
    /// provider 命令构造 / resume 不可用(`ResumeUnavailable`)。
    #[error("provider error: {0}")]
    Provider(String),
    /// transport I/O(tmux new-session / new-window / kill 等子进程失败)。
    #[error("transport error: {0}")]
    Transport(String),
    /// 原子动作中途失败且**回滚也失败**(字节级回滚写回失败 —— Gap 15/16 最坏臂)。
    #[error("rollback failed after {step}: {detail}")]
    RollbackFailed { step: String, detail: String },
    /// plan id 非法(`sanitize_plan_id` 拒绝;防路径穿越)。
    #[error("invalid plan id: {0}")]
    InvalidPlanId(String),
    /// plan YAML 校验失败(`InvalidPlanError`)。
    #[error("invalid plan: {0}")]
    InvalidPlan(String),
    /// 多 team 选择歧义 / 未找到(`select_restart_state`)。
    #[error("team select: {0}")]
    TeamSelect(String),
}

// ===========================================================================
// OUTCOME / REPORT(rich return — 契约断言这些 carry 的值)
// ===========================================================================

/// `launch(...)` 报告(`launch/core.py:294` 的 typed dict)。dry-run 时 `agents` 空、
/// `safety` 填充。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchReport {
    pub session_name: SessionName,
    /// 实际起的 worker(冷启;dry-run 时空)。
    pub started: Vec<StartedAgent>,
    /// 是否 dry-run(只解析路由/权限,不起进程)。
    pub dry_run: bool,
    /// 路由决策(每 task 一条;`routing.decision` 事件)。
    pub routes: Vec<RoutingDecision>,
    /// 权限摘要(每 agent 一条)。
    pub permissions: Vec<PermissionSummary>,
    /// 危险审批安全态(dry-run 报告里的 `safety`)。
    pub safety: DangerousApproval,
    /// leader receiver(attach 成功时;经 step10 leader::attach_leader_to_state)。
    pub leader_receiver_attached: bool,
    pub session_capture_incomplete_agents: Vec<String>,
}

/// 单个已起 worker(`launch` 的 `agents[]` / `started`)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartedAgent {
    pub agent_id: AgentId,
    pub start_mode: StartMode,
    pub target: String,
    pub session_id: Option<SessionId>,
    pub rollout_path: Option<RolloutPath>,
    pub pending_session_id: Option<SessionId>,
    pub claude_config_dir: Option<PathBuf>,
    pub provider_projects_root: Option<PathBuf>,
    pub managed_mcp_config: bool,
    pub display: WorkerDisplay,
}

/// 路由决策(`routing.decision` 事件 / launch `routes[]`)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoutingDecision {
    pub task_id: Option<String>,
    pub selected_agent: AgentId,
    pub reason: String,
    pub manual_override: bool,
}

/// 权限摘要(`resolve_permissions(agent)` 的 typed 版)。**PLACEHOLDER 字段**:
/// 实际形状由 step6 compiler 的 `resolve_permissions` 决定,集成时映射。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionSummary {
    pub agent_id: AgentId,
    pub raw: serde_json::Value,
}

/// BUG-7 (0.3.1): quick-start cannot honestly report "ready" before the workers'
/// MCP tool sets have actually loaded — provider-side schema rejections (codex
/// invalid_function_parameters etc.) happen AFTER spawn and silently disable the
/// worker. The report must therefore carry a readiness verdict so the CLI surface
/// never emits bare "ready" while worker capability is unverified or already known
/// to be degraded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QuickStartReadiness {
    /// At least one agent already failed to materialize a live tmux window (BUG-2
    /// observable). The team is *not* ready; user must inspect / restart.
    Degraded { unhealthy_agents: Vec<String> },
    /// All spawned agents have live windows but their MCP tool set load has NOT
    /// been verified yet — provider-side schema/auth failures could still leave
    /// the worker unable to call team_orchestrator tools. CLI must label this
    /// `pending` / `unverified`, NOT bare `ready`.
    PendingToolLoad,
}

/// `quick_start(...)` 报告(`diagnose/quick_start.py:103` typed 版)。`Refused` 区分
/// existing-context(需 restart 或 --fresh)与 preflight 失败。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QuickStartReport {
    /// 起队成功 + wait_ready 就绪。
    Ready {
        session_name: SessionName,
        launch: Box<LaunchReport>,
        next_actions: Vec<String>,
        attach_commands: Vec<String>,
        display_backend: String,
        /// BUG-7: real readiness verdict. `Ready` ⇒ the wrapper completed AND the
        /// caller already verified tool-set availability; the framework itself
        /// never emits this without an external observable confirming worker
        /// tool calls succeeded. quick_start_with_transport always defaults to
        /// [`QuickStartReadiness::PendingToolLoad`] (or `Degraded` if any agent
        /// failed to spawn) so the CLI surface cannot lie about availability.
        worker_readiness: QuickStartReadiness,
    },
    /// 已有 runtime state,非 --fresh → 引导用 restart(`quick_start.py:42`)。
    ExistingRuntime {
        team: Option<String>,
        session_name: Option<SessionName>,
        state_path: Option<PathBuf>,
        next_actions: Vec<String>,
    },
    /// preflight 阻塞(`quick_start.py:59`)。
    PreflightBlocked {
        summary: String,
        blockers: Vec<String>,
        next_actions: Vec<String>,
    },
}

/// 单 worker 动作通用 envelope(`{ok, agent_id, status, ...}`,`operations.py`/`agents.py`/
/// `start.py`)。status 的语义随动作不同,故各动作有专属 outcome enum 收口;此 struct
/// 是它们共享的字段载体。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentActionEnvelope {
    pub agent_id: AgentId,
    pub state_file: PathBuf,
    /// coordinator 起后的报告(launch/start/fork/restart 末尾起 coordinator)。
    pub coordinator_started: bool,
}

/// `start_agent(...)` 结果(`start.py:101/134/353`)。穷尽 start 路径。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StartAgentOutcome {
    /// 起/复活成功,carry start_mode(resumed/fresh/fresh_after_missing_rollout)。
    Running {
        env: AgentActionEnvelope,
        start_mode: StartMode,
        target: String,
        session_id: Option<SessionId>,
        rollout_path: Option<RolloutPath>,
    },
    /// 窗口已存在且非 force → noop(`start.py:134`)。
    Noop {
        env: AgentActionEnvelope,
        target: String,
    },
    /// agent paused → 不起(`start.py:101`,reason=agent_paused)。
    Paused { agent_id: AgentId },
}

/// `stop_agent(...)` 结果(`operations.py:99`)。同时关显示(`test_stop_agent_display`)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StopAgentReport {
    pub agent_id: AgentId,
    pub target: String,
    /// 实际 kill 了的 window(已停则 false)。
    pub stopped: bool,
    pub display_closed: bool,
    pub state_file: PathBuf,
}

/// `reset_agent(...)` 结果(`operations.py:104/133`)。**必须 `discard_session=true`**,
/// 否则 `Refused{ DiscardSessionRequired }`(`test_reset_agent_discard`)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResetAgentOutcome {
    /// discard + 重起成功。
    Reset {
        env: AgentActionEnvelope,
        start_mode: StartMode,
    },
    /// 未传 discard_session → 拒绝(不丢上下文的误用保护)。
    Refused { reason: ResetRefusal },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResetRefusal {
    DiscardSessionRequired,
}

/// `add_agent(...)` 结果(`operations.py:272`)。动态 role doc 编译进 spec + 起 worker;
/// 失败字节级回滚 spec_yaml / workspace_state / **team_state.md** / role_file(Gap 15.11)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddAgentReport {
    pub env: AgentActionEnvelope,
    pub start_mode: StartMode,
    /// 写入的动态 role file 路径。
    pub role_file: PathBuf,
}

/// `fork_agent(...)` 结果(`operations.py:402`)。native session fork(provider 须
/// supports_session_fork ∧ auth_mode!=compatible_api)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForkAgentReport {
    pub source_agent_id: AgentId,
    pub new_agent_id: AgentId,
    pub env: AgentActionEnvelope,
    pub session_id: Option<SessionId>,
}

/// `remove_agent(...)` 结果(`agents.py:54/56/150`)。`_RemoveRollback` 快照
/// spec/state/team_state/role_file/agent_health 字节级回滚(Gap 16)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoveAgentOutcome {
    /// 原子摘除成功(spec/state/team_state/role-file/agent_health 全删)。
    Removed {
        agent_id: AgentId,
        state_file: PathBuf,
        /// GC 掉的 agent_health 行(`test_remove_agent_health_gc`)。
        agent_health_deleted: bool,
    },
    /// 未传 from_spec 确认(`agents.py:54`)。
    RefusedFromSpecConfirm { agent_id: AgentId },
    /// 运行中未传 force(`agents.py:56`)。
    RefusedForceRequired { agent_id: AgentId },
}

/// `restart(...)` 结果(`orchestration.py:114/142/387`)。Route B:**先全量验证**
/// (resume 决策 + first_send_at 校验)**再**破坏性 teardown。refuse 时 nothing created。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RestartReport {
    /// 整队重建成功。每 worker 已发 `restart.resume_decision`。
    Restarted {
        session_name: SessionName,
        agents: Vec<RestartedAgent>,
        coordinator_started: bool,
        next_actions: Vec<String>,
        attach_commands: Vec<String>,
    },
    /// atomic refusal(`reason=resume_atomicity`):某 interacted worker 不可 resume
    /// 且非 allow_fresh。**nothing created yet**,无需回滚。
    RefusedResumeAtomicity {
        unresumable: Vec<UnresumableWorker>,
        allow_fresh: bool,
        error: String,
    },
    /// session capture did not converge before destructive restart. No teardown/spawn occurred.
    RefusedResumeNotReady {
        missing: Vec<AgentId>,
        allow_fresh: bool,
        deadline: std::time::Duration,
        elapsed: std::time::Duration,
        error: String,
    },
    /// first_send_at 损坏(`reason=invalid_first_send_at`):决策前 hard refuse。
    RefusedInvalidFirstSendAt {
        invalid: Vec<CorruptFirstSendAt>,
        allow_fresh: bool,
        error: String,
    },
}

/// 单个重建后的 worker(carry restart_mode)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestartedAgent {
    pub agent_id: AgentId,
    pub restart_mode: StartMode,
    pub decision: ResumeDecision,
    pub session_id: Option<SessionId>,
}

/// atomic refusal 里的不可重建 worker(`orchestration.py:517`)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnresumableWorker {
    pub agent_id: AgentId,
    /// `no_persisted_session_id` | `session_unresumable`。
    pub reason: String,
    pub session_id: Option<SessionId>,
    pub first_send_at: Option<String>,
}

/// first_send_at 损坏条目(`restart.first_send_at_invalid` 事件 payload)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CorruptFirstSendAt {
    pub worker_id: AgentId,
    pub raw_first_send_at: serde_json::Value,
    pub raw_first_send_at_type: String,
}

/// Route B 全量验证产物(`_emit_resume_decisions` + `_collect_corrupt_first_send_at`,
/// `orchestration.py:430/467`)。restart() **先**算它(纯计算,无副作用),corrupt 非空则
/// hard-refuse;否则按 decisions/unresumable 决定 teardown。把"每非 paused worker 发一条
/// `restart.resume_decision`" 与 "python type().__name__ 映射" 从 start_agent 的整条
/// lock+spawn 路径里**分离出来**,使 Route B audit 契约可在 fixture state 上单元级断言
/// (gate gap:restart() 只返回终态 RestartReport,缺隔离测试面)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestartPlan {
    /// 每非 paused worker 一条决策(顺序 = restart_agents 顺序);Route B audit 契约必发。
    pub decisions: Vec<RestartedAgent>,
    /// first_send_at 损坏条目(决策前 hard-refuse 的依据;carry python type-name)。
    pub corrupt_entries: Vec<CorruptFirstSendAt>,
    /// allow_fresh=false 且 interacted-but-unresumable 的 worker(atomic_refusal 触发集)。
    pub unresumable: Vec<UnresumableWorker>,
}

/// display 解析结果(`resolve_display_backend`,`display/backend.py`)。非默认时非静默
/// 发 `display.backend_resolved`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedBackend {
    pub backend: DisplayBackend,
    /// 是否非默认(默认 adaptive;非默认非静默发事件)。
    pub non_default: bool,
}

/// `open_worker_displays` 结果(`display/worker_window.py`)。每 worker 一个
/// `WorkerDisplay`,失败不阻塞 team readiness(C14)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenDisplaysReport {
    pub backend: DisplayBackend,
    pub displays: BTreeMap<String, WorkerDisplay>,
}

/// `close_team_display_backends` 结果(`display/close.py`,C9 close-by-recorded-backend)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloseDisplaysReport {
    /// 按 state 记录的后端关掉的窗口/会话标识。
    pub closed: Vec<String>,
    /// orphan 清理(adaptive 只删带 team tag 的窗口,C2 leader pane 安全)。
    pub orphans_cleaned: Vec<String>,
}

/// plan 状态机推进结果(`orchestrator/__init__.py` start_plan / handle_report_result /
/// halt_plan 的 typed 版)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanProgress {
    /// 仍运行,当前 stage。
    Running {
        plan_id: PlanId,
        current_stage: i64,
        state_path: PathBuf,
    },
    /// 全 stage 完成。
    Completed { plan_id: PlanId },
    /// halted(carry reason + artifact)。
    Halted {
        plan_id: PlanId,
        reason: String,
        artifact: Option<PathBuf>,
    },
    /// report_result 不匹配任何 stage 条件 → no-op(`__init__.py:96`)。
    NoMatch,
}
