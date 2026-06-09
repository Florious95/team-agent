//! status/data enums + data structs + 有界重试常量 (§19 必变:散字符串态 → 穷尽 enum;
//! §3 ad-hoc dict → typed 边界)。

use serde::{Deserialize, Serialize};

use crate::model::enums::Provider;
use crate::model::ids::{LeaderSessionUuid, OwnerEpoch, TaskId, TeamKey};
use crate::transport::PaneId;

use super::helpers::MessageStatusShadow;

// ===========================================================================
// ENUMS (§19 必变:散字符串态 → 穷尽 enum,serde rename 到精确 Python 字符串)
// ===========================================================================

/// 投递层结果态 (**≠** `messages.status` 行态;card §41)。Python 把两套词表混在同一
/// dict 里漂移;Rust 拆开:本 enum 是「投递动作的结果」,行态另由 step 7 `MessageStatus` 表达。
/// 值取自 `delivery.py:181,259,307`、`send.py:261,299,418,489`、`leader.py:230,428`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryStatus {
    Delivered,
    Failed,
    /// busy → 延后不丢 (card §131:不 mark failed,留队列)。
    Queued,
    Blocked,
    Refused,
    RetryScheduled,
    TrustAutoAnswerExhausted,
    AlreadyDelivered,
    /// leader fallback inbox 审计:`ok=True` 但**非真投递成功** (bug-52,card §129)。
    /// 上游必须能区分,绝不当 `Submitted`。
    FallbackLog,
    BroadcastDelivered,
    BroadcastPartial,
    FanoutDelivered,
    FanoutPartial,
}

/// 投递/发件拒绝原因 (card §42)。Python 散裸字符串靠 `==` 比对易拼错;Rust 穷尽 enum。
/// 值散落 `send.py`/`delivery.py`/`leader.py`/`session_drift.py`/`owner_gate`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryRefusal {
    TargetNotInTeam,
    HumanConfirmationRequired,
    MissingPermissions,
    RecipientBusy,
    UnknownRecipient,
    TmuxTargetMissing,
    MessageAlreadyClaimed,
    LeaderNotAttached,
    NoCallerPane,
    TeamOwnerMismatch,
    Ambiguous,
    RecipientPaneInNonInputMode,
    SessionDrift,
    /// Caller supplied a `--message-id` that already exists in the store
    /// (CR-015/054 caller-key dedup; identical idempotent re-send is rejected, not duplicated).
    Duplicate,
    /// Send without a resolvable target/assignee (CR-061/N27): the prompt text
    /// is content, not a target. Distinct from `TargetNotInTeam` (where caller
    /// did pick a target but it's unknown).
    RoutingAmbiguous,
}

/// 注入失败阶段 (审计用;`delivery.py:309` injection.stage)。card §43。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryStage {
    TrustAutoAnswerDismissalWait,
    Inject,
    Submit,
    VisibleCheck,
}

/// scheduled_events.kind (step 7 定义,本步**穷尽消费**;card §44)。
/// `_fire_due_scheduled_events` 的 `else` 返回 `unknown scheduled event kind`
/// (`scheduler.py:100`) —— Rust 穷尽 match 让漏 kind 编不过,**无运行时 fallback**。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScheduledKind {
    Send,
    HealthPing,
    TrustRetry,
}

/// classifier 产物 status (card §48;消费 step 8 classifier)。
/// **致命铁律 (bug-071/077/085)**:`Uncertain` (= agent_health 的 Unknown) 在 ping /
/// take-over predicate 里**显式 block**,绝不 fallthrough 成 idle。穷尽 match,**无 `_ => Idle`**。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivityStatus {
    Working,
    Idle,
    Stuck,
    Uncertain,
}

impl ActivityStatus {
    /// take-over / ping predicate 闸门:仅非-`Uncertain` 且为 `Idle` 才放行。
    /// `Uncertain`/`Working`/`Stuck` 全 block (穷尽,无兜底)。
    pub fn allows_idle_takeover(self) -> bool {
        match self {
            ActivityStatus::Idle => true,
            ActivityStatus::Working | ActivityStatus::Stuck | ActivityStatus::Uncertain => false,
        }
    }
}

/// 告警类型 (card §49;`scheduler.py:38` `_ALERT_TYPES`)。`stuck_cancel` 还接 `all` (展开全集)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AlertType {
    Stuck,
    IdleFallback,
    CrossWorkerDeadlock,
}

impl AlertType {
    /// `stuck_cancel(alert_type="all")` → `sorted(_ALERT_TYPES)` 全集 (`scheduler.py:269`)。
    pub fn all() -> [AlertType; 3] {
        [AlertType::CrossWorkerDeadlock, AlertType::IdleFallback, AlertType::Stuck]
    }
}

/// selftest check 状态 (card §47;`diagnose/comms.py`)。§19 必变 enum。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckStatus {
    Pass,
    Fail,
    Deferred,
    NotImplemented,
    NotChallenged,
}

/// selftest check 验证项 (`verifies` 字段;`diagnose/comms.py:149`)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckKind {
    ReceiverBinding,
    ContractSuite,
    /// 机械门 (§84/MUST-NOT-13):`{anthropic,openai,httpx} == 0`。
    NoProviderSdkCalls,
}

/// leader_receiver.mode (card §51;`leader.py:103,166`)。§3/§41 必 enum。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReceiverMode {
    DirectTmux,
}

// ===========================================================================
// PaneWidth fail-safe (bug-064/082;`delivery.py:20-51` `_tmux_pane_width`)
// ===========================================================================

/// `_tmux_pane_width` 的 typed 结果 (card §124 / `delivery.py:20-51`)。**fail-safe 铁律**:
/// 查询失败时**绝不**返回默认宽度 —— matcher 退回精确相等,右边缘截断的 prompt 绝不靠猜测
/// 自动应答。故 `Failed` 携带 reason 但**不**携带任何 fallback 宽度。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaneWidthQuery {
    Ok { pane_width: u32 },
    /// 失败原因 (`tmux_query_failed:<exc>`/`tmux_query_nonzero`/`empty_output`/
    /// `unparseable_output`/`non_positive_width`)。**无默认宽度** (fail-safe)。
    Failed { error: String },
}

// ===========================================================================
// STRUCTS (§3/§19 必变:ad-hoc dict → typed 边界)
// ===========================================================================

/// 投递结果 (card §40;`delivery.py:180`/`send.py:347` 的 ad-hoc dict 的 typed 版)。
/// 每个 caller 自己拼/读 dict key,漂移高危 → §3 typed:`status` 必 enum。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeliveryOutcome {
    pub ok: bool,
    /// 投递层结果态 (≠ 行态)。
    pub status: DeliveryStatus,
    /// step 7 `messages.status` 行态原文 (shadow;leader 集成时收口为 step 7 enum)。
    pub message_status: MessageStatusShadow,
    pub message_id: Option<String>,
    /// 注入可见性验证 (transport InjectReport 的投影)。
    pub verification: Option<String>,
    pub stage: Option<DeliveryStage>,
    pub reason: Option<DeliveryRefusal>,
    /// fallback / broadcast / fanout 等通道标注 (`channel` 字段)。
    pub channel: Option<String>,
}

/// trust retry payload (card §45;`delivery.py:273` scheduled_events.payload_json 的 typed 视图)。
/// `attempt` 是 u8 有界 (≤ [`TRUST_RETRY_MAX_ATTEMPTS`])。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrustRetryPayload {
    pub message_id: String,
    pub attempt: u8,
    pub max_attempts: u8,
    /// 首投目标 pane (`first_target`)。
    pub first_target: PaneId,
}

/// send retry payload (card §46;`results.py:251`/`scheduler.py:134`)。report_result 排队给
/// leader 的 send 事件 payload;`max_attempts` = [`SEND_RETRY_MAX_ATTEMPTS`]。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)] // 无 Eq:含 f64 timeout
pub struct SendEventPayload {
    pub content: String,
    pub task_id: Option<TaskId>,
    pub sender: String,
    pub requires_ack: bool,
    pub wait_visible: bool,
    pub timeout: f64,
    pub max_attempts: u8,
    pub attempt: u8,
}

/// selftest 单 check (card §47;`diagnose/comms.py:119`)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelftestCheck {
    pub status: CheckStatus,
    pub verifies: CheckKind,
    /// 证据 (proof/calls 等;leader 集成时按 CheckKind 收口具体形状)。
    pub evidence: CheckEvidence,
}

/// selftest check 证据 (`proof`/`calls`/`mismatches` 的 typed union)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckEvidence {
    /// `no_provider_sdk_calls` 的机械证据 (§84):三 SDK 调用计数。
    ProviderSdkCalls(ProviderSdkCalls),
    /// binding 一致性比对结果 (mismatch 列表)。
    Binding { mismatches: Vec<String>, details: serde_json::Value },
    /// executable zero-token comms contract suite evidence.
    ContractSuite { checks: Vec<ContractSuiteCheck> },
}

/// One executable zero-token comms contract-suite subcheck.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContractSuiteCheck {
    pub name: String,
    pub status: CheckStatus,
    pub reason: Option<String>,
}

/// **机械门** (§84/MUST-NOT-13;`diagnose/comms.py:142`):selftest 路径 provider SDK 调用
/// 计数,断言 `{anthropic,openai,httpx} == 0`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ProviderSdkCalls {
    pub anthropic: u32,
    pub openai: u32,
    pub httpx: u32,
}

impl ProviderSdkCalls {
    /// `any(calls.values())` 取反:三者全 0 才 pass。
    pub fn is_zero(self) -> bool {
        self.anthropic == 0 && self.openai == 0 && self.httpx == 0
    }
}

/// comms selftest 顶层结果 (`run_comms_selftest` 返回;`diagnose/comms.py:40`)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelftestReport {
    pub ok: bool,
    pub status: CheckStatus,
    pub run_id: String,
    /// `scope = "binding_consistency"`。
    pub scope: String,
    pub boundary: String,
    pub receiver_binding: SelftestCheck,
    pub contract_suite: SelftestCheck,
    pub provider_sdk_calls: SelftestCheck,
}

/// idle 行为评估结果 (`evaluate_idle_behavior` 返回;`diagnose/comms.py:87`)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdleEvaluation {
    pub ok: bool,
    pub agent_id: String,
    pub claimed_status: String,
    pub token: String,
    pub status: CheckStatus,
    pub execution_ack: String,
}

/// 抑制快照 (card §50;`scheduler.py:383` `_agent_alert_snapshot`)。清除判据靠 snapshot diff。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AlertSnapshot {
    pub assigned_task_ids: Vec<TaskId>,
    pub delivered_message_ids: Vec<String>,
}

/// 告警抑制条目 (card §50;`scheduler.py:287`)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AlertSuppression {
    pub suppressed_at: String,
    pub suppressed_by: String,
    pub snapshot: AlertSnapshot,
    pub manual_acknowledge: Option<bool>,
    pub expires_at: Option<String>,
}

/// leader_receiver binding (card §51;step 5/10 主拥,本步消费/校验;`leader.py:103,166`)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeaderReceiver {
    pub mode: ReceiverMode,
    pub pane_id: PaneId,
    pub provider: Provider,
    pub leader_session_uuid: Option<LeaderSessionUuid>,
    pub owner_epoch: OwnerEpoch,
}

/// leader_notification_log dedup key (card §52;`leader_notification_log.py:30`)。
/// **陷阱**:dedup key = `(result_id, owner_team_id, owner_epoch)`,**不含**
/// `leader_session_uuid` (它退化为 nullable audit)。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LeaderNotificationKey {
    pub result_id: String,
    pub owner_team_id: TeamKey,
    pub owner_epoch: OwnerEpoch,
}

/// classifier 产物 (card §48;`activity_detector.py:107` 的 `{status,confidence,rationale}`)。
#[derive(Debug, Clone, PartialEq)]
pub struct AgentActivity {
    pub status: ActivityStatus,
    pub confidence: f64,
    pub rationale: String,
}

/// result watcher 通知记录 (card §53;`result_delivery.py` deliver/dedupe 返回的逐 watcher 结果)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WatcherNotice {
    pub watcher_id: String,
    pub result_id: Option<String>,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    /// dedupe / 投递 / 失败的 message_id (存活语义:requeue 不得清空,Gap 32)。
    pub notified_message_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub primary_watcher_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prior_state: Option<String>,
    pub error: Option<String>,
}

// ===========================================================================
// BOUNDED CONSTANTS (有界重试;穷尽计数器锁死,绝不死循环;card §125)
// ===========================================================================

/// `_TRUST_RETRY_MAX_ATTEMPTS = 4` (`delivery.py:61`)。第 4 次终态发
/// `leader_panes.trust_auto_answer_exhausted`,不死循环。
pub const TRUST_RETRY_MAX_ATTEMPTS: u8 = 4;

/// `_TRUST_RETRY_BACKOFF_SECONDS = {2:5, 3:15, 4:30}` (`delivery.py:60`)。
pub const TRUST_RETRY_BACKOFF_SECONDS: &[(u8, u32)] = &[(2, 5), (3, 15), (4, 30)];

/// send 重试 `max_attempts = 3` (`scheduler.py:134`/`results.py:251`)。
pub const SEND_RETRY_MAX_ATTEMPTS: u8 = 3;

/// result watcher 投递有界重试 max 5 (`result_delivery.py` notify max 5)。
pub const RESULT_DELIVERY_MAX_ATTEMPTS: u8 = 5;
