//! coordinator 共享数据面:常量 / newtype / 穷尽 enum / metadata / schema health /
//! report 结构 / CoordinatorEvent / abnormal-track 数据型 / WatchCursor / cross-dep 占位。

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

// ── REUSE — committed model types(不重定义)──────────────────────────────────
use crate::model::enums::{AuthMode, Provider};
// AuthMode/PaneLiveness 经 provider/transport 间接复用;此处只点名直接用到的。

// ── REUSE — step 8 provider(经 trait,MUST-NOT-13:绝不依赖 provider client crate)──
use crate::provider::{ProcessLiveness, ProviderAdapter, RolloutPath, TurnId, TurnState};

// ===========================================================================
// CONSTANTS(metadata.py:13 / __main__.py:101)
// ===========================================================================

/// `COORDINATOR_PROTOCOL_VERSION = 2`(`metadata.py:13`)。bump 即触发
/// `coordinator.restart_incompatible`。健康判定的三元之一。
pub const PROTOCOL_VERSION: u32 = 2;

/// `DEFAULT_TICK_INTERVAL_SEC = 5.0`(`__main__.py:101`;Gap 36c 从 2.0 提到 5.0,2.5x 省 CPU)。
pub const DEFAULT_TICK_INTERVAL_SEC: f64 = 5.0;

/// 指数退避封顶(`__main__.py:65` `min(.., 60.0)`)。bug-084 崩溃循环防护。
pub const BACKOFF_MAX_SEC: f64 = 60.0;

/// watch log rotation marker(`watch.py:22`,字节级一致 —— 测试钉死)。
pub const ROTATION_MARKER: &str =
    "[watch] log rotated; archived segment events.jsonl.1 not replayed — historical replay deferred to a future --replay flag";

// ===========================================================================
// NEWTYPES(§3:Pid / WorkspacePath 不与裸 int/PathBuf 混传)
// ===========================================================================

/// OS pid(`metadata.py` / `pid_is_running` / stop)。避免与其他 int id 混传。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Pid(pub u32);

impl Pid {
    pub fn new(pid: u32) -> Self {
        Self(pid)
    }
    pub fn get(self) -> u32 {
        self.0
    }
}

impl std::fmt::Display for Pid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// resolve()'d workspace 路径(`Path(args.workspace).resolve()`,`__main__.py:31`)。
/// tick/health/start/stop 全以它为 key —— 不与裸 `PathBuf` 混。
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct WorkspacePath(pub PathBuf);

impl WorkspacePath {
    pub fn new(p: impl Into<PathBuf>) -> Self {
        Self(p.into())
    }
    pub fn as_path(&self) -> &Path {
        &self.0
    }
}

// ===========================================================================
// §19 散字符串态 → 穷尽 enum
// ===========================================================================

/// metadata `source`(`__main__.py:34` boot / `lifecycle.py:119` start)。仅两调用点 → enum。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MetadataSource {
    Boot,
    Start,
}

/// health dict `status`(`lifecycle.py:30,34,38-46`)。
/// **ADJUDICATION**:与 `crate::provider::HealthStatus`(agent health label,大写)**不同概念** ——
/// 那是 worker agent 健康标签,本 enum 是 *coordinator daemon* 进程健康态 → 命名 `CoordinatorHealthStatus` 避撞。
/// `ok` 由 `running ∧ metadata_ok ∧ schema_ok` 三者合取得出(`lifecycle.py:38`),不在此 enum 内。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CoordinatorHealthStatus {
    /// pid 文件不存在(`lifecycle.py:30`)。
    Missing,
    /// pid 文件存在但非整数(`lifecycle.py:34`)。
    InvalidPid,
    /// pid 存活(`lifecycle.py:41`)。
    Running,
    /// pid 文件存在但进程已死(`lifecycle.py:41`)。
    Stale,
}

/// `start_coordinator` 结果 status(`lifecycle.py:54,73,89,121`)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StartOutcome {
    /// 已健康 → no-op(`lifecycle.py:54`)。
    AlreadyRunning,
    /// metadata 不兼容,先 stop 但 stop 失败(`lifecycle.py:73`)。
    RestartIncompatibleStopFailed,
    /// schema 不兼容,拒启并给修复 hint(`lifecycle.py:89`)。
    SchemaIncompatible,
    /// 正常 spawn(`lifecycle.py:121`)。
    Started,
}

/// `stop_coordinator` 结果 status(`lifecycle.py:232,238,243,247`)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopOutcome {
    /// pid 文件不存在(`lifecycle.py:232`)。
    Missing,
    /// pid 非整数 → 清文件返回(`lifecycle.py:238`)。
    InvalidPidRemoved,
    /// SIGTERM 失败(`lifecycle.py:243`)。
    KillFailed,
    /// SIGTERM + 清 pid/meta 成功(`lifecycle.py:247`)。
    Stopped,
}

/// tick degraded/stop 的 `reason`(`lifecycle.py:279,357`)。§19 散字符串 → enum。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TickStopReason {
    /// tmux session 不在 → `stop:true` 触发主循环退出(`lifecycle.py:279`)。
    TmuxSessionMissing,
    /// bug-084:tick-end save_runtime_state 失败,`stop:false`(`lifecycle.py:357`)。
    PersistenceDegraded,
}

/// 孤儿分类 reason(`orphan_cleanup.py:87-100`)。携带 hint 数据 → enum(非散字符串)。
/// **ADJUDICATION**:orphan_cleanup 的完整 SIGTERM/SIGKILL 升级逻辑归 step 14 诊断面;
/// 本 step 拥有 reason 分类 enum 是因为 daemon 自终止(Gap 37b)与诊断共享它(card §35)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum OrphanReason {
    /// workspace 目录已不存在(`orphan_cleanup.py`)。
    WorkspacePathMissing,
    /// 临时目录模式命中(携带 hint,如 `/var/folders/...team-agent-watcher-dedupe-*`)。
    EphemeralTempdirPattern { hint: String },
    /// workspace 仍在 → 不是孤儿。
    WorkspaceAlive,
    /// 无法解析 cmdline → 无法判定 workspace。
    CmdlineUnparsed,
    /// workspace 仍存在,但 coordinator metadata 不指向该 pid/schema。
    MetadataMismatch,
    /// ps 列表仍有命令残留,但 pid 已不再存活。
    PidNotRunning,
}

// ===========================================================================
// CoordinatorMetadata(metadata.py:50-57)
// ===========================================================================

/// `coordinator.json` 内容(`metadata.py:50-57`)。serde 字段名锁死稳定 JSON 契约。
/// 健康判定需三元全等:`pid == 实际 ∧ protocol_version == PROTOCOL_VERSION ∧
/// message_store_schema_version == SCHEMA_VERSION`(`metadata.py:37-43`)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoordinatorMetadata {
    pub pid: Pid,
    pub protocol_version: u32,
    pub message_store_schema_version: i64,
    pub source: MetadataSource,
    /// ISO8601(`datetime.now(timezone.utc).isoformat()`,`metadata.py:56`)。
    pub updated_at: String,
}

// ===========================================================================
// SchemaHealth(lifecycle.py:197-226)
// ===========================================================================

/// message-store schema 兼容门结果(`message_store_schema_health`,`lifecycle.py:197`)。
/// 用穷尽 enum 表 mismatch(card 表:`SchemaHealth { ok, error: Option<SchemaError> }`)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaHealth {
    /// `schema_ok`(`lifecycle.py:222/201`)。
    pub ok: bool,
    /// 当前 `MessageStore::SCHEMA_VERSION`(`lifecycle.py:198`)。
    pub schema_version: i64,
    /// `None` ⇔ `ok == true`。
    pub error: Option<SchemaError>,
    /// 修复 hint(`_SCHEMA_ACTION_HINT`,`lifecycle.py:131-135`);仅 error 时填。
    pub action: Option<String>,
}

/// schema mismatch 的穷尽分类(`_diagnose_schema_mismatch`,`lifecycle.py:164-194`)。
/// 区分「pre-init 必需列缺失」(拒启)vs「migratable 列缺失」(可迁移)——card §89 铁律。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SchemaError {
    /// pragma table_info vs expected 列集不符(`lifecycle.py:185-191`)。携带定位字段。
    SchemaMismatch {
        table: String,
        expected_columns: Vec<String>,
        actual_columns: Vec<String>,
        missing_columns: Vec<String>,
        /// pre-init 必需列(true)vs migratable(false)——决定拒启 vs 可迁移(card §89)。
        pre_init_required: bool,
    },
    /// `MessageStore(workspace)` 构造抛异常(`lifecycle.py:213`),携带原文。
    InitFailed { message: String },
}

// ===========================================================================
// health / start / stop report 结构(lifecycle.py:39-247 typed 版)
// ===========================================================================

/// `start_coordinator` 错误(spawn 子进程 / ensure dirs 失败)。lib 边界 thiserror(§12)。
#[derive(Debug, Error)]
pub enum StartError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("event log: {0}")]
    EventLog(#[from] crate::event_log::EventLogError),
    #[error("message store: {0}")]
    MessageStore(#[from] crate::message_store::MessageStoreError),
}

/// `stop_coordinator` 错误。
#[derive(Debug, Error)]
pub enum StopError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("event log: {0}")]
    EventLog(#[from] crate::event_log::EventLogError),
}

/// `coordinator_health` 结果(`lifecycle.py:39-46` typed 版)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HealthReport {
    /// `running ∧ metadata_ok ∧ schema_ok`(`lifecycle.py:38`)。
    pub ok: bool,
    pub status: CoordinatorHealthStatus,
    pub pid: Option<Pid>,
    pub metadata: Option<CoordinatorMetadata>,
    /// 三元全等(`coordinator_metadata_ok`,`metadata.py:37-43`)。
    pub metadata_ok: bool,
    pub schema: SchemaHealth,
}

/// `start_coordinator` 结果(`lifecycle.py:54/86/121` typed 版)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartReport {
    pub ok: bool,
    pub pid: Option<Pid>,
    pub status: StartOutcome,
    /// coordinator.log 路径(成功路径,`lifecycle.py:54/121`)。
    pub log: Option<PathBuf>,
    /// schema_incompatible 时的修复 hint / 失败原因。
    pub schema_error: Option<SchemaError>,
    pub action: Option<String>,
}

/// `stop_coordinator` 结果(`lifecycle.py:232-247` typed 版)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StopReport {
    pub ok: bool,
    pub status: StopOutcome,
    pub pid: Option<Pid>,
}

// ===========================================================================
// CoordinatorEvent(§3/§22:typed event enum,serde tag 与 Python 字节级一致)
// ===========================================================================

/// 本 step 发出的事件(card 表整列;事件名稳定契约由 step 4 EventLog 拥有,本卡只点名
/// coordinator 发的那批)。serde `tag = "event"` + 精确 rename 锁死字节(与 events.jsonl 一致)。
/// 渲染(`watch::render_event_line`)是它的 `Display` 投影。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)] // 无 Eq:某变体含 f64
#[serde(tag = "event")]
pub enum CoordinatorEvent {
    #[serde(rename = "coordinator.boot")]
    Boot { workspace: String, once: bool },
    #[serde(rename = "coordinator.started")]
    Started { pid: Pid, log: String },
    #[serde(rename = "coordinator.stopped")]
    Stopped { pid: Pid },
    #[serde(rename = "coordinator.exit")]
    Exit { stop: bool },
    #[serde(rename = "coordinator.session_missing")]
    SessionMissing { session: String },
    #[serde(rename = "coordinator.orphan_self_terminate")]
    OrphanSelfTerminate {
        initial_ppid: u32,
        current_ppid: u32,
        workspace: String,
    },
    #[serde(rename = "coordinator.tick_error")]
    TickError {
        error: String,
        exc_type: String,
        consecutive_failures: u32,
        next_sleep_sec: f64,
    },
    #[serde(rename = "coordinator.tick_error.suppressed")]
    TickErrorSuppressed {
        consecutive_failures: u32,
        next_sleep_sec: f64,
    },
    #[serde(rename = "coordinator.tick_recovered")]
    TickRecovered { consecutive_failures: u32 },
    #[serde(rename = "coordinator.restart_incompatible")]
    RestartIncompatible {
        pid: Option<Pid>,
        expected_protocol: u32,
        expected_schema: i64,
    },
    #[serde(rename = "coordinator.restart_incompatible_stop_failed")]
    RestartIncompatibleStopFailed { pid: Option<Pid> },
    #[serde(rename = "coordinator.schema_incompatible")]
    SchemaIncompatible {
        table: Option<String>,
        missing_columns: Vec<String>,
    },
    #[serde(rename = "idle_takeover.unknown_persistent")]
    IdleTakeoverUnknownPersistent {
        node_id: String,
        provider: Option<Provider>,
        auth_mode: Option<AuthMode>,
        consecutive_ticks: u32,
        rollout_path: Option<RolloutPath>,
    },
    #[serde(rename = "abnormal.notify")]
    AbnormalNotify {
        signature: String,
        turn_id: Option<TurnId>,
        decision: AbnormalDecision,
    },
    #[serde(rename = "worker.abnormal_exit")]
    WorkerAbnormalExit {
        team_id: String,
        agent_id: String,
        provider: Provider,
        path: String,
        provider_process_dead: bool,
        latest_explicit_error: bool,
        signature: String,
        turn_id: Option<TurnId>,
        process_liveness: ProcessLiveness,
    },
    #[serde(rename = "worker.abnormal_exit.check")]
    WorkerAbnormalExitCheck {
        team_id: String,
        agent_id: String,
        provider: Provider,
        path: String,
        provider_process_dead: bool,
        latest_explicit_error: bool,
        notification: bool,
        suppressed_reason: Option<String>,
    },
    #[serde(rename = "abnormal_exit.single_signal_suppressed")]
    AbnormalExitSingleSignalSuppressed {
        team_id: String,
        agent_id: String,
        provider: Provider,
        path: String,
        reason: String,
        provider_process_dead: bool,
        dead_process: bool,
        latest_explicit_error: bool,
    },
    #[serde(rename = "abnormal.whole_team_gone")]
    AbnormalWholeTeamGone { classification: WholeTeamGoneClass },
    #[serde(rename = "leader_notification.log_pruned")]
    LeaderNotificationLogPruned { removed: u64 },
    #[serde(rename = "leader_notification.prune_failed")]
    LeaderNotificationPruneFailed { error: String },
    #[serde(rename = "runtime.state.save_failed")]
    RuntimeStateSaveFailed {
        phase: String,
        error: String,
        exc_type: String,
    },
}

// ===========================================================================
// 异常轨 abnormal_track(abnormal_track.py)—— provider-neutral 数据型
// ===========================================================================

/// `_classify` 决策(`abnormal_track.py:198-204`)。whitelist > blacklist > default(C9 catch-bias)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AbnormalDecision {
    /// whitelist 命中 → 不通知(`abnormal_track.py:201`)。
    Skip,
    /// blacklist 命中 → 通知(`abnormal_track.py:203`)。
    NotifyBlacklist,
    /// 结构化 fault 默认通知(C9 catch-bias,`abnormal_track.py:204`)。
    NotifyDefault,
}

/// `detect_whole_team_gone` 分类(`abnormal_track.py:130/132/143`)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WholeTeamGoneClass {
    Alive,
    /// clean shutdown → 静默(`abnormal_track.py:130`)。
    CleanShutdown,
    /// restart 进行中 → 静默(`abnormal_track.py:132`)。
    RestartInProgress,
    /// 闪退 → durable marker + deferred escalation(`abnormal_track.py:143`)。
    UnexpectedExit,
}

/// `process_abnormal_records` 单条通知(`abnormal_track.py:70-79`)。
/// dedup key = `(signature, turn_id|fingerprint)`(C8,`abnormal_track.py:64-66`):turn_id 缺失退化为
/// per-record content fingerprint 桶,**绝不**把不同 fault 折叠进一个全局桶。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AbnormalNotification {
    pub signature: String,
    pub turn_id: Option<TurnId>,
    /// `kind == "approval"` → blocked_on_human,否则 abnormal(`abnormal_track.py:74`)。
    pub state: TurnState,
    pub decision: AbnormalDecision,
    pub provider: Option<Provider>,
    /// 原始 fault fact(provider reader 产出的结构化记录;本 module 不读屏不命名 provider)。
    pub raw: Value,
}

/// `process_abnormal_records` 返回(`abnormal_track.py:83-88` typed 版)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AbnormalProcessOutput {
    pub notifications: Vec<AbnormalNotification>,
    /// 更新后的去重状态(`seen` 集合,`abnormal_track.py:82`)——caller 回存。
    pub notification_state: AbnormalNotificationState,
}

/// abnormal 去重状态(`abnormal_track.py:31-32,82`)。`seen` = `signature\0bucket` key 集合。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AbnormalNotificationState {
    pub seen: BTreeSet<String>,
}

/// `detect_whole_team_gone` 返回(`abnormal_track.py:140-147` typed 版)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WholeTeamGoneReport {
    pub whole_team_gone: bool,
    pub classification: WholeTeamGoneClass,
    pub notify: bool,
    /// 仅 unexpected exit 时 true(`abnormal_track.py:145`):延迟到下条 leader 命令再 escalate。
    pub escalate_user_on_next_leader_command: bool,
    pub marker_written: bool,
}

/// 整队存活快照(`detect_whole_team_gone` 的 snapshot 入参,`abnormal_track.py:105-116`)。
/// coordinator-independent:不依赖 coordinator 活着也能判。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TeamPresenceSnapshot {
    pub coordinator_alive: bool,
    pub leader_alive: bool,
    /// 每 worker 进程存活(`provider_state` liveness;`ProcessLiveness::Alive` 才算活)。
    pub provider_processes_alive: Vec<bool>,
    pub tmux_sessions_present: bool,
    pub clean_shutdown: bool,
    pub restart_in_progress: bool,
}

/// abnormal track 错误(provider reader 翻译 fault facts 失败)。
#[derive(Debug, Error)]
pub enum AbnormalError {
    #[error("provider: {0}")]
    Provider(#[from] crate::provider::ProviderError),
}

// ===========================================================================
// WatchCursor(watch/__init__.py)
// ===========================================================================

/// watch tail 游标(`watch.py:14-19`)。rotation 检测靠 `archive_signature` 变化 + `offset > size`
/// (`watch.py:73-83`)。`archive_signature = Some((size, mtime_ns))`。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WatchCursor {
    pub event_offset: u64,
    pub seen_result_ids: BTreeSet<String>,
    pub initialized: bool,
    /// `(size, mtime_ns)`;`mtime_ns: i128`(防溢出)。
    pub archive_signature: Option<(u64, i128)>,
}

// ===========================================================================
// CROSS-DEP placeholders(其他并行 lane 拥有真名;leader 集成时 reconcile)
// ===========================================================================
// 下列类型/trait 是 tick 编排与异常轨命名到的、由 OTHER 10-15 子系统(step 8 provider、
// step 11 messaging、step 5 state projection)拥有的面。本 lane 给最小本地占位让 coordinator
// 的签名编得过;leader 集成时把它们换成真名(见 cross_deps_or_placeholders 清单)。

/// agent id(step 2/5 拥有 newtype;tick stuck 列表 / idle node 用)。
/// **PLACEHOLDER**:与 `crate::model::ids::AgentId` 同名同义,但 ids.rs 已定义 —— 集成时直接
/// `use crate::model::ids::AgentId` 替换本地别名。此处用本地 type alias 避免重定义冲突。
pub use crate::model::ids::AgentId;

/// provider adapter 解析器(`get_provider_registry` / `ADAPTERS` map 的 trait 版,step 8 拥有真名)。
/// **PLACEHOLDER**:step 8 provider lane 会给真正的 registry 类型;本 trait 是 coordinator 注入点
/// (MUST-NOT-13:经 trait 调,绝不依赖 provider client crate)。
pub trait ProviderRegistry {
    /// 拿某 provider 的 adapter(`providers.get_adapter`)。
    fn adapter_for(&self, provider: Provider) -> Box<dyn ProviderAdapter>;
    /// 该 provider 的 error 黑白名单(`get_provider_registry(provider)["error_lists"]`,abnormal_track 用)。
    fn error_lists(&self, provider: Provider) -> ErrorLists;
}

/// provider error 黑白名单(`abnormal_track.py:184-195`)。**PLACEHOLDER**(step 8 拥有)。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ErrorLists {
    pub whitelist: Vec<String>,
    pub blacklist: Vec<String>,
}

/// idle take-over 候选 node(`idle_takeover_wiring.build_idle_nodes`,step 8/11 拥有真名)。
/// **PLACEHOLDER**:bug-085 关键 —— `state` 是穷尽 `TurnState`,`rollout_path` 是 `Option`
/// (None 漏穿曾误判 unknown→idle)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdleNode {
    pub node_id: AgentId,
    pub state: TurnState,
    pub provider: Option<Provider>,
    pub rollout_path: Option<RolloutPath>,
}

/// idle take-over 提醒(`lifecycle.py:304-307`)。**PLACEHOLDER**(step 11 messaging 拥有 alert 形状)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdleAlert {
    pub message: Option<String>,
    pub reason: Option<String>,
    pub interrupted: Vec<AgentId>,
}

/// `_deliver_pending_messages` 投递记录(`delivery.py:484`,step 11 拥有)。**PLACEHOLDER**。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeliveredMessage {
    pub message_id: String,
}

/// `_fire_due_scheduled_events` 触发记录(`scheduler.py:41`,step 11 拥有)。**PLACEHOLDER**。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FiredScheduledEvent {
    pub id: i64,
}

/// `detect_cross_worker_deadlocks` 告警(`messaging/idle_alerts.py`,step 11 拥有)。**PLACEHOLDER**。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeadlockAlert {
    pub raw: Value,
}

/// `detect_compaction_degradation` 结果(`messaging/activity_detector.py`,step 11 拥有)。**PLACEHOLDER**。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactionResult {
    pub agent_id: AgentId,
    pub provider: Option<Provider>,
    pub observed: bool,
    pub reason: Option<String>,
    pub recommendation: Option<String>,
}

/// `detect_session_drift` 结果(`messaging/session_drift.py`,step 11 拥有)。**PLACEHOLDER**。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionDriftResult {
    pub agent_id: AgentId,
    pub stored_session_id: Option<String>,
    pub observed_session_id: Option<String>,
    pub status: String,
}

/// `detect_leader_api_errors` 结果(`messaging/leader_api_errors.py`,step 11 拥有)。**PLACEHOLDER**。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeaderApiError {
    pub provider: Option<Provider>,
    pub pane_id: Option<String>,
    pub fingerprint: String,
    pub message: String,
}

/// `_collect_results_and_notify_watchers` 结果(`results.py:430`,step 11 拥有)。**PLACEHOLDER**。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CollectedResult {
    pub result_id: String,
}

/// durable marker store(`detect_whole_team_gone` 的 marker_store 入参,`abnormal_track.py:226-239`)。
/// **PLACEHOLDER**:step 5 state / step 7 message_store 可能拥有真正的 marker 存储面;
/// 本 trait 是 abnormal track 的注入点(写 `whole_team_gone` durable marker)。
pub trait MarkerStore {
    /// 写一个命名 marker;返回是否落盘成功(`abnormal_track.py:232-238`)。
    fn set_marker(&mut self, name: &str, value: Value) -> bool;
}
