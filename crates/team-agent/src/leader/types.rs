//! leader 数据层 — lease/owner/incident 枚举 + struct + 错误 + cross-lane 占位/trait + 锁名常量。

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use crate::model::ids::{LeaderSessionUuid, OwnerEpoch, TeamKey};
use crate::provider::{Provider, RolloutPath, TurnState};
use crate::state::StateError;
use crate::transport::{PaneId, SessionName, WindowName};

// ===========================================================================
// ENUMS (§19 散字符串态 → 穷尽 enum;serde rename 到精确 Python 字符串,字节对齐)
// ===========================================================================

/// `receiver.mode`(card §22)。现只一种;留 enum 防 transport 扩展。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReceiverMode {
    DirectTmux,
}

/// `receiver.status`(card §23)。§19 散字符串 → enum。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReceiverStatus {
    Attached,
}

/// `discovery`(card §24)。§19 必须 enum;现 free-string。
/// 取自 `__init__.py` 写入点:`"attach_readopt"`(:534/550-552)、`"claim_leader"`(:876)
/// + `_resolve_leader_pane` 的发现路径(env / explicit / current_pane,step 9 transport)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Discovery {
    AttachReadopt,
    StaleRediscoveryOwnerIdentity,
    StaleRediscoveryUniqueCandidate,
    ClaimLeader,
    /// `$TEAM_AGENT_LEADER_PANE_ID` / `$TMUX_PANE` 直接命中(autobind / require_current)。
    EnvPane,
    /// 显式 `--pane` 参数指定。
    ExplicitPane,
    /// 当前 pane(`require_current=True`)。
    CurrentPane,
}

/// `claimed_via`(card §25)。§19 enum;`source` 也是 enum(见 [`LeaseSource`])。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ClaimedVia {
    /// `"claim-leader"`(`_claim_lease_no_incident` :694 / claim_leader :780)。
    ClaimLeader,
    /// `"attach-leader"`(`_try_readopt_leader_pane` :546)。
    AttachLeader,
}

/// `attach_leader_to_state(source=...)` / `claimed_via` 的 source 维度(card §25)。
/// `__init__.py` 的 `source` 取值:`"manual"`(attach_leader :34)、`"launch"`/`"quick_start"`
/// (first-time 门 :297)、`restart` orchestration、`autobind` 的传入值。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeaseSource {
    Launch,
    QuickStart,
    Restart,
    Manual,
}

/// lease refusal `reason`(card §30 / contract C22)。闭枚举 = `_LEASE_REASON_ENUM`
/// (`__init__.py:372-383`) **并集** 契约 C22 的 `caller_pane_missing`
/// (`leader_binding.bind_owner_from_caller_pane` 的拒绝 reason)。
/// 命门:这是闭集,序列化字节必须与 Python 字符串一致 → impl 时锁死测试。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeaseReason {
    VacantAcquired,
    PreviousOwnerPaneDead,
    PreviousOwnerAliveRefused,
    OwnerEpochAdvanced,
    ForceConfirmRequired,
    CallerNotLeaderShaped,
    CallerPaneNotLive,
    CallerCwdMismatch,
    NotInTmuxPane,
    /// binding 路径(`leader_binding.py`)的 `$TMUX_PANE` 缺席拒绝。
    CallerPaneMissing,
}

impl LeaseReason {
    /// `_LEASE_REBIND_REQUIRED_REASONS`(`__init__.py:384-386`):决定 refusal 事件名
    /// 是 `leader_receiver.rebind_required` 还是 `leader_receiver.claim_refused`。
    pub fn is_rebind_required(self) -> bool {
        matches!(
            self,
            Self::NotInTmuxPane
                | Self::CallerNotLeaderShaped
                | Self::CallerPaneNotLive
                | Self::CallerCwdMismatch
        )
    }
}

/// lease 结果 `status`(card §31)。§19 enum。
/// `"already_bound"`/`"claimed"`/`"refused"`/`"dry_run"`(+ ambiguous claim 的 refused 子原因)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeaseStatus {
    AlreadyBound,
    Claimed,
    Refused,
    DryRun,
}

/// `leader_session_uuid_source`(card §35)。env override vs derived vs env-inherited。
/// 复刻 `_leader_identity_context`(:206)的 `"override"`/`"derived"` 二值 + caller 身份
/// 的 `"env"`(state::identity `caller_identity_from_env` 已用 `"explicit-override"`/`"env"`/
/// `"derived"`)。**NOTE(cross-lane)**:identity lane 用字符串串 `"explicit-override"`,
/// 此处 leader plan 用 `"override"` —— 二者来源不同函数,leader 集成时复核统一。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeaderSessionUuidSource {
    Derived,
    Override,
    Env,
}

/// wake 重读决策 reason(card §36)。`wake.py:should_reread` 五值穷尽 enum。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RereadReason {
    NoFile,
    NeverClassified,
    FileChanged,
    QuiescentAlreadyClassified,
    Unchanged,
}

/// leader start plan 模式(`leader_start_plan` :109/111/116/123)。§19 散字符串 → enum。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeaderStartMode {
    /// `os.environ["TMUX"]` 已在 tmux 内 → exec provider in-place。
    ExecProvider,
    /// Managed launcher topology: provider pane lives in the team session's `leader` window;
    /// the invoking terminal is only a tmux client attached/switched to that pane.
    ManagedTmuxClient,
    /// 无 tmux session → 新建 tmux session。
    NewTmuxSession,
    /// session 已存在 / `--attach-session` → attach。
    AttachExisting,
}

/// Leader launcher tmux/socket selection. Managed launcher sessions must use the
/// workspace tmux socket, never the user's default tmux server.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeaderLaunchSocket {
    Workspace,
}

/// Execution status for a leader launch plan. `NotStarted` is intentionally
/// distinct so JSON callers cannot report `ok:true` for an unexecuted launcher.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeaderLaunchStatus {
    Exited,
    Detached,
    NotStarted,
}

/// 全部 leader 审计事件名(card §34)。§3 typed event kinds + §40 JSON 名与 Python
/// **字节级一致**。映射到 [`LeaderEvent::name`] 返回 `EventLog::write` 用的精确字符串。
/// (既有 `EventLog::write(&str, Value)` 仍吃裸字符串;此 enum 是 type-safe 名表,
/// impl 时所有 leader 审计点用它而非散字符串字面量。)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LeaderEvent {
    // leader_receiver.* — `__init__.py`
    ReceiverAttached,
    ReceiverRebindApplied,
    ReceiverClaimApplied,
    ReceiverClaimRefused,
    ReceiverRebindRequired,
    ReceiverAttachFailed,
    ReceiverStateDivergenceRepaired,
    ReceiverFirstTimeEnvSeeded,
    ReceiverAutobindSkipped,
    ReceiverRequeuedExhaustedWatchers,
    ReceiverAmbiguousCandidates,
    ReceiverClaimRequeue,
    ReceiverClaimLeaderNotification,
    // owner.* — `__init__.py` + `leader_binding.py`
    OwnerAdoptedOnRestart,
    OwnerBoundFromCallerPane,
    OwnerBindRefused,
    OwnerEpochAdvanced,
    // leader_session_uuid.* / leader.* / result_watcher.*
    LeaderSessionUuidOverride,
    LeaderStart,
    ResultWatcherRequeued,
    // idle_takeover.* — `idle_takeover.py` / `idle_takeover_wiring.py`
    IdleTakeoverClassify,
    IdleTakeoverPing,
    IdleTakeoverReminder,
    IdleTakeoverPushFailed,
}

impl LeaderEvent {
    /// 精确 Python 事件名(`EventLog.write` 第一参)。§40 字节级一致。
    pub fn name(self) -> &'static str {
        match self {
            Self::ReceiverAttached => "leader_receiver.attached",
            Self::ReceiverRebindApplied => "leader_receiver.rebind_applied",
            Self::ReceiverClaimApplied => "leader_receiver.claim_applied",
            Self::ReceiverClaimRefused => "leader_receiver.claim_refused",
            Self::ReceiverRebindRequired => "leader_receiver.rebind_required",
            Self::ReceiverAttachFailed => "leader_receiver.attach_failed",
            Self::ReceiverStateDivergenceRepaired => "leader_receiver.state_divergence_repaired",
            Self::ReceiverFirstTimeEnvSeeded => "leader_receiver.first_time_env_seeded",
            Self::ReceiverAutobindSkipped => "leader_receiver.autobind_skipped",
            Self::ReceiverRequeuedExhaustedWatchers => {
                "leader_receiver.requeued_exhausted_watchers"
            }
            Self::ReceiverAmbiguousCandidates => "leader_receiver.ambiguous_candidates",
            Self::ReceiverClaimRequeue => "leader_receiver.claim_requeue",
            Self::ReceiverClaimLeaderNotification => "leader_receiver.claim_leader_notification",
            Self::OwnerAdoptedOnRestart => "owner.adopted_on_restart",
            Self::OwnerBoundFromCallerPane => "owner.bound_from_caller_pane",
            Self::OwnerBindRefused => "owner.bind_refused",
            Self::OwnerEpochAdvanced => "owner_epoch_advanced",
            Self::LeaderSessionUuidOverride => "leader_session_uuid.override",
            Self::LeaderStart => "leader.start",
            Self::ResultWatcherRequeued => "result_watcher.requeued",
            Self::IdleTakeoverClassify => "idle_takeover.classify",
            Self::IdleTakeoverPing => "idle_takeover.ping",
            Self::IdleTakeoverReminder => "idle_takeover.reminder",
            Self::IdleTakeoverPushFailed => "idle_takeover.push_failed",
        }
    }
}

// ===========================================================================
// STRUCTS — §5 单一 typed binding(LeaderReceiver / TeamOwner 不再是 ad-hoc map)
// ===========================================================================

/// `state.json["leader_receiver"]`(card §20)。投递目标 + 身份。
/// **bug-085**:所有可选字段 `Option<T>`(半状态合法,缺字段不崩)。
/// 字段集对齐 `__init__.py` 写入的 receiver dict(:282-296 / :861-877)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeaderReceiver {
    pub mode: ReceiverMode,
    pub status: ReceiverStatus,
    pub provider: Provider,
    pub pane_id: PaneId,
    pub session_name: Option<SessionName>,
    pub window_index: Option<String>,
    pub window_name: Option<WindowName>,
    pub pane_index: Option<String>,
    pub pane_tty: Option<String>,
    pub pane_current_command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tmux_socket: Option<String>,
    /// `_target_fingerprint(pane_info)`。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fingerprint: Option<String>,
    /// owner 身份(C10/C12:carry 已记录 owner 的 uuid,不重派生)。fake provider 时可缺。
    pub leader_session_uuid: Option<LeaderSessionUuid>,
    pub owner_epoch: Option<OwnerEpoch>,
    pub attached_at: Option<String>,
    pub discovery: Option<Discovery>,
    /// requested vs inferred provider 不一致时记录(`receiver["requested_provider"]`)。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub requested_provider: Option<Provider>,
    /// 非致命校验告警(`validation["warning"]`)。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
}

/// `state.json["team_owner"]`(card §21)。租约权威记录。
/// 字段集对齐 `__init__.py` owner dict(:538-546 / :686-694)+ `leader_binding` owner(:129-136)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TeamOwner {
    pub pane_id: PaneId,
    pub provider: Provider,
    pub machine_fingerprint: String,
    pub leader_session_uuid: Option<LeaderSessionUuid>,
    pub owner_epoch: OwnerEpoch,
    pub claimed_at: String,
    pub claimed_via: ClaimedVia,
    /// `leader_binding.bind_owner_from_caller_pane` 还写 `os_user`(Family A owner)。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub os_user: Option<String>,
}

/// leader 身份上下文(card §35;`_leader_identity_context` :192-211 的 typed 版)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeaderIdentity {
    pub leader_session_uuid: LeaderSessionUuid,
    pub leader_session_uuid_source: LeaderSessionUuidSource,
    pub machine_fingerprint: String,
    pub workspace_abspath: PathBuf,
    pub os_user: String,
    pub team_id: TeamKey,
}

/// wake 重读决策(card §36;`wake.should_reread` 返回 dict 的 typed 版)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RereadDecision {
    pub reread: bool,
    pub reason: RereadReason,
}

/// `wake.on_file_changed` / `take_pending` 操作的 watch 状态(per-node pending set + mtimes)。
/// `wake.py` 用 `{pending: sorted list, mtimes: {node_id: mtime}}` dict;此为 typed 版。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct WakeWatchState {
    /// 文件变更待处理的 node(`sorted`)。
    pub pending: Vec<String>,
    /// 每 node 最近 mtime。
    pub mtimes: BTreeMap<String, f64>,
}

/// leader 启动计划(`leader_start_plan` 返回 dict 的 typed 版)。
/// `_run_leader_plan` 据此 exec / new-session / attach。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeaderStartPlan {
    pub mode: LeaderStartMode,
    pub provider: Provider,
    pub workspace: PathBuf,
    pub socket: LeaderLaunchSocket,
    pub session_name: Option<SessionName>,
    /// 要 exec 的 argv(provider argv 或 tmux argv)。
    pub argv: Vec<String>,
    /// For managed launches, the provider command argv spawned inside the leader pane.
    pub provider_argv: Vec<String>,
    /// For managed launches, the window that hosts the leader provider pane.
    pub leader_window: Option<WindowName>,
    /// True for external/current-pane leader compatibility paths.
    pub is_external_leader: bool,
    /// `new_tmux_session` / `exec_provider` 的 leader env 导出。
    pub leader_env: BTreeMap<String, String>,
    pub identity: Option<LeaderIdentity>,
    /// 非 tty 的 `new_tmux_session` → `-d` detached。
    pub detached: bool,
}

/// Result of executing a [`LeaderStartPlan`]. Interactive launches should carry
/// the provider/tmux process exit code; detached launches carry the managed
/// session when known.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeaderLaunchOutcome {
    pub status: LeaderLaunchStatus,
    pub exit_code: Option<i32>,
    pub session_name: Option<SessionName>,
    pub reason: Option<String>,
}

impl LeaderLaunchOutcome {
    pub fn not_started(reason: impl Into<String>) -> Self {
        Self {
            status: LeaderLaunchStatus::NotStarted,
            exit_code: None,
            session_name: None,
            reason: Some(reason.into()),
        }
    }
}

/// idle-takeover 的 node 分类行(`build_idle_nodes` / `_leader_node` 产物)。
/// **bug-085**:`state` 用 `TurnState`(穷尽,`Unknown` 不当 idle);`rollout_path` `Option`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdleNode {
    pub node_id: String,
    /// `"worker"` / `"leader"`(C13:leader 也是 provider node)。
    pub role: NodeRole,
    pub state: TurnState,
    pub turn_id: Option<String>,
    pub annotations: Vec<String>,
    pub provider: Option<Provider>,
    pub auth_mode: Option<String>,
    /// bug-085:`None` → 该 node 走 `Unknown` 分支(不猜 idle)。
    pub rollout_path: Option<RolloutPath>,
}

/// idle node 角色(`build_idle_nodes` role 字段)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeRole {
    Worker,
    Leader,
}

/// lease 变更结果(attach/claim/takeover/autobind/readopt 统一返回)。
/// 复刻 `__init__.py` 各路径返回 dict 的并集(成功 receiver/owner/epoch + 拒绝 reason/action)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeaseResult {
    pub ok: bool,
    pub status: LeaseStatus,
    pub receiver: Option<LeaderReceiver>,
    pub owner: Option<TeamOwner>,
    pub owner_epoch: Option<OwnerEpoch>,
    /// 成功路径:`vacant_acquired` / `previous_owner_pane_dead`;拒绝路径:闭枚举 reason。
    pub reason: Option<LeaseReason>,
    /// 拒绝时给操作者的 hint(`action` 字段)。
    pub action: Option<String>,
    /// dry-run / refused 时携带的 bound pane。
    pub bound_pane_id: Option<PaneId>,
}

/// Family A 正源 owner 绑定结果(`bind_owner_from_caller_pane` 返回 dict 的 typed 版)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnerBindResult {
    pub ok: bool,
    /// 成功时的 owner 记录。
    pub owner: Option<TeamOwner>,
    pub caller_pane_id: PaneId,
    /// 诊断元数据(`#{pane_current_command}` 的单次定向查询结果)。
    pub caller_current_command: String,
    pub team_id: TeamKey,
    /// 拒绝 reason(`caller_pane_missing`)+ hint。
    pub reason: Option<LeaseReason>,
    pub hint: Option<String>,
}

// ===========================================================================
// ERROR — LeaderError(lib 边界,thiserror)
// ===========================================================================

/// step 10 leader 错误。**ADJUDICATION**:不复用 `ModelError`(那是 spec/envelope 校验层)。
/// lease 变更路径被 launch/restart 同步调用 + 原子双写(bug-084 同源)→ daemon-path,
/// fallible 操作返 `Result<_, LeaderError>`(§10 实现层禁 unwrap/expect/panic)。
/// `RuntimeError`(`__init__.py` 抛的 leader pane 校验失败 / tmux scan 失败)→ `Validation`/`Tmux`。
#[derive(Debug, Error)]
pub enum LeaderError {
    /// leader pane 校验失败(`_strict_leader_validation_error` / `attach_failed`)。
    #[error("leader pane validation failed: {0}")]
    Validation(String),
    /// provider 命令未安装 / leader 启动前置失败(`leader_start_plan` raise RuntimeError)。
    #[error("leader start error: {0}")]
    Start(String),
    /// tmux target scan / set-environment / send 失败(transport 层冒泡)。
    #[error("tmux error: {0}")]
    Tmux(String),
    /// state 持久化 / 双写 / 锁失败(bug-084 同源,从 state 层冒泡)。
    #[error("state error: {0}")]
    State(#[from] StateError),
    /// 身份派生失败(leader_session_uuid 输入含 NUL)。
    #[error("identity error: {0}")]
    Identity(#[from] crate::model::errors::ModelError),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("event log: {0}")]
    EventLog(#[from] crate::event_log::EventLogError),
    #[error("message store: {0}")]
    MessageStore(#[from] crate::message_store::MessageStoreError),
    #[error("messaging: {0}")]
    Messaging(#[from] crate::messaging::MessagingError),
}

// ===========================================================================
// CROSS-LANE PLACEHOLDERS — 其他并行 lane 的 typed 依赖,leader 集成时 reconcile。
// (本骨架定义 MINIMAL 本地占位,不猜对方精确命名;见 cross_deps_or_placeholders。)
// ===========================================================================

/// 命名互斥锁句柄占位(step 11 messaging / runtime 的 `_runtime_lock(workspace, name)`)。
/// `LEADER_OWNERSHIP_LOCK = "send"`:attach/claim/takeover/autobind 共享同一临界区。
/// **PLACEHOLDER**:真锁类型(文件锁 / RAII guard)在 runtime/messaging lane 落地。
pub struct RuntimeLockGuard {
    pub _name: String,
}

/// MessageStore 占位引用(step 7 已有 `message_store::MessageStore`,但 requeue 方法
/// `requeue_delivery_exhausted_watchers` / `requeue_after_claim_leader` 属 step 11 messaging)。
/// **PLACEHOLDER**:claim/attach 后 requeue exhausted result watchers 的接口签名,
/// 由 messaging lane 落地。
pub struct RequeueOutcome {
    pub watcher_ids: Vec<String>,
}

/// `_resolve_leader_pane` 返回的 pane 信息占位(step 9 transport `PaneInfo` 的相邻产物)。
/// `__init__.py:276` 调 `runtime._resolve_leader_pane` 返 `(pane_info, discovery)`。
/// **PLACEHOLDER**:实际 pane_info 字段映射到 `transport::PaneInfo`,resolve 逻辑在 step 9/11。
pub struct ResolvedLeaderPane {
    pub pane_info: Value,
    pub discovery: Discovery,
}

// ===========================================================================
// 常量 / 锁名
// ===========================================================================

/// `LEADER_OWNERSHIP_LOCK = "send"`(`__init__.py:393`)。一把锁串行化所有 lease 变更
/// (takeover / claim-leader / attach-leader / autobind)+ 对 send mutator 串行
/// (旧 owner 并发 send 不能 race rebind)。
pub const LEADER_OWNERSHIP_LOCK: &str = "send";

// ===========================================================================
// CROSS-LANE TRAIT — turn-state 分类器(注入,MUST-NOT-13 零 provider client)
// ===========================================================================

/// `read_turn_state(provider, session_log_text)` 的注入接口(card §59 / §97)。
/// **MUST-NOT-13**:coordinator tick 经此 trait 分类,**不**依赖任何 provider client crate;
/// 测试注入 mock 并断言 provider 调用计数 = 0。
/// **CROSS-LANE**:真实现是 step 8 provider 的 `provider_state::read_turn_state`;此 trait 是
/// leader/coordinator 侧的依赖倒置接口,leader 集成时桥接到 step 8 registry。
pub trait TurnStateClassifier {
    /// 从 provider session-log 文本分类 turn 状态(**unknown ≠ idle**:返回 `TurnState`,
    /// `Unknown` 不当 idle)。
    fn classify(
        &self,
        provider: Provider,
        session_log_text: &str,
    ) -> Result<TurnClassification, LeaderError>;
}

/// `read_turn_state` 的分类结果(card §59;`provider_state` 返回 dict 的 typed 版)。
/// **CROSS-LANE PLACEHOLDER**:精确字段集对齐 step 8 provider_state lane,leader 集成时 reconcile。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnClassification {
    pub state: TurnState,
    pub turn_id: Option<String>,
    pub annotations: Vec<String>,
    /// unknown/abnormal 时的 reason(写 `idle_takeover.classify`)。
    pub reason: Option<String>,
}

/// `evaluate_takeover_reminder` 的结果(card §50/§51;`idle_predicate` 返回 dict 的 typed 版)。
/// **CROSS-LANE PLACEHOLDER**:精确字段集对齐 step 8 provider-neutral idle_predicate lane。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TakeoverReminderResult {
    pub should_ping: bool,
    /// `should_ping` 时投给 leader 的中性提醒消息体。
    pub message: Option<String>,
    pub interrupted_nodes: Vec<String>,
    pub reason: Option<String>,
}
