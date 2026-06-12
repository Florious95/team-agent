//! step 11 · messaging — delivery / scheduler / results / collect / report_result /
//! watchers / leader-receiver / selftest 编排状态机 (ROUND-0 SKELETON).
//!
//! Card: `docs/phase0/subsystems/11-messaging.md` (type table §38-53, public API §60-75,
//! bug/trap §118-132, module map §134-149).
//! Truth source (READ-ONLY) `team-agent-public` @ v0.2.11 / `439bef8`:
//!   - `messaging/send.py` · `delivery.py` · `internal_delivery.py` · `scheduler.py`
//!   - `messaging/results.py` · `result_delivery.py` · `leader.py` · `leader_panes.py`
//!   - `messaging/idle_alerts.py` · `activity_detector.py` · `trust_auto_answer.py`
//!   - `messaging/session_drift.py` · `owner_bypass.py` · `routing.py`
//!   - `diagnose/comms.py` (`run_comms_selftest` / `evaluate_idle_behavior`)
//!
//! SCOPE — this is the type/interface LAYER (so RED contracts can NAME these and
//! compile), NOT the implementation. It mirrors only the CONTRACT-NAMED public
//! surface; the 18 Python files' internal helpers are deferred (see
//! `cross_deps_or_placeholders`). Bodies are `unimplemented!("step11 port: …")`.
//!
//! REUSE (do NOT redefine): [`MessageStore`] + [`NotificationClaimParams`] (step 7);
//! [`Transport`] / [`Target`] / [`PaneId`] (step 9); [`Provider`] / [`MessageStatus`]
//! shadow (step 2 enums); [`worker_sender_bypasses_owner_gate`] (step 5 owner-gate);
//! [`RouteResult`] / [`route_task`] (step 2 routing); [`ProviderError`] / classifier
//! types (step 8); [`EventLog`] (step 4).
//!
//! 铁律 (card §118-132, Rust 绝不重蹈):
//!   - **unknown ≠ idle** (bug-071/077/085): [`ActivityStatus`] 穷尽 match,`Uncertain`
//!     独立分支显式 block,**无 `_ => Idle` 兜底**。
//!   - **dedup key 不含 leader_session_uuid** (Stage 12): 唯一去重原语 =
//!     [`MessageStore::claim_leader_notification_delivery`] 的 `INSERT OR IGNORE`;
//!     `peek` 只读快路径,非去重本身。`notified_message_id` requeue 必须存活 (Gap 32)。
//!   - **trust 自动应答 fail-safe** (bug-064/082): pane 宽度查询失败时**绝不**返回默认
//!     宽度 ([`PaneWidthQuery::Failed`]),matcher 退回精确相等;trust 应答只对自己工作
//!     目录 realpath 全等。重试有界 ([`TRUST_RETRY_MAX_ATTEMPTS`]),终态显式。
//!   - **scheduled kind 穷尽**: [`ScheduledKind`] 穷尽 match,漏 kind 编不过,无运行时 fallback。
//!   - **busy → 延后不丢**: [`DeliveryStatus::Queued`] 不 mark failed,留队列等下次 tick。
//!   - **selftest 零 provider SDK** (§84/MUST-NOT-13): [`run_comms_selftest`] 走 trait
//!     mock,断言 `{anthropic,openai,httpx} == 0`,见 [`ProviderSdkCalls`]。
//!
//! §10:scheduler / result_delivery / leader_receiver / delivery 子路径被 coordinator
//! daemon (step 12) tick 直接调用 (bug-084 崩溃面) → 所有投递/调度/IO 返
//! `Result<_, MessagingError>` (thiserror)。`#![deny(unwrap/expect/panic)]` 由 leader
//! 集成时统一加,本 ROUND-0 skeleton 不加 (与 provider.rs/transport.rs 一致)。

// ROUND-0 skeleton:fn body 全 unimplemented!() → import/field/method 暂未被用;P2 porter 落实现时移除。
#![allow(dead_code, unused_imports)]
// §10:scheduler/result_delivery/leader_receiver/delivery 子路径被 coordinator daemon tick 直接调用
// (bug-084 崩溃面)→ 实现层禁 unwrap/expect/panic(unimplemented!() stub 不被拦);tests 各自 allow。
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use thiserror::Error;

// ── REUSE: step 2 model ────────────────────────────────────────────────────
// 这些模块根级 `use` 保持子模块测试 `use super::*` 的名字解析不变 (TeamKey / PaneId / …)。
use crate::model::enums::Provider;
use crate::model::ids::{LeaderSessionUuid, OwnerEpoch, TaskId, TeamKey};
// route_task 已在 step 2 落地 (model::routing),send 子例程 REUSE,不重定义。
pub use crate::model::routing::{route_task, RouteResult};

// ── REUSE: step 4 event_log / step 7 message_store / step 9 transport ───────
use crate::event_log::EventLog;
use crate::message_store::MessageStore;
use crate::transport::{PaneId, Target, Transport};

pub mod activity;
pub mod delivery;
pub mod helpers;
pub mod leader_receiver;
pub mod peers;
pub mod results;
pub mod scheduler;
pub mod selftest;
pub mod send;
pub mod trust;
pub mod types;
pub mod watchers;

// ── re-export: 保持 `crate::messaging::X` 与 test `super::X` 解析不变 ──────────
pub use activity::{classify_agent_activity, detect_cross_worker_deadlocks, detect_idle_fallbacks};
pub use delivery::{
    deliver_pending_message, deliver_pending_messages, deliver_stored_message, execute_trust_retry,
    handle_trust_retry_needed, record_turn_open_if_leader_to_worker,
    retry_injection_after_trust_auto_answer, stamp_first_send_at_if_leader_to_worker,
    tmux_pane_width,
};
pub use helpers::fail_leader_delivery;
pub use leader_receiver::{
    claim_leader_receiver, deliver_to_leader_fallback_pane, mirror_peer_message_to_leader,
    send_to_leader_receiver, send_to_leader_receiver_with_message_id,
};
pub use peers::allow_peer_talk;
pub use results::{
    collect, collect_for_team, collect_results_and_notify_watchers, report_result,
    report_result_for_owner_team, report_result_for_owner_team_with_primary_error,
};
pub use scheduler::{detect_stuck_agents, fire_due_scheduled_events, stuck_cancel, stuck_list};
pub use selftest::{evaluate_idle_behavior, run_comms_selftest, CommsSelftestDriver};
pub use send::{
    apply_worker_sender_bypass, send_message, session_drift_refusal, MessageTarget, SendOptions,
};
pub use trust::{attempt_trust_auto_answer, TrustAnswerOutcome};
pub use types::{
    ActivityStatus, AgentActivity, AlertSnapshot, AlertSuppression, AlertType, CheckEvidence,
    CheckKind, CheckStatus, ContractSuiteCheck, DeliveryOutcome, DeliveryRefusal, DeliveryStage,
    DeliveryStatus, IdleEvaluation, LeaderNotificationKey, LeaderReceiver, PaneWidthQuery,
    ProviderSdkCalls, ReceiverMode, ScheduledKind, SelftestCheck, SelftestReport, SendEventPayload,
    TrustRetryPayload, WatcherNotice, RESULT_DELIVERY_MAX_ATTEMPTS, SEND_RETRY_MAX_ATTEMPTS,
    TRUST_RETRY_BACKOFF_SECONDS, TRUST_RETRY_MAX_ATTEMPTS,
};
pub use watchers::{
    delivered_result_message, format_result_watcher_notification, notify_result_watchers,
    requeue_after_claim_leader, requeue_delivery_exhausted_watchers, result_id_from_text,
    retry_result_deliveries,
};

// `MessageStatusShadow` 是 [`DeliveryOutcome::message_status`] 的公有字段类型,原在模块根
// 可见 (`crate::messaging::MessageStatusShadow`),故 `pub` 再导出保持解析不变。
pub use helpers::MessageStatusShadow;

// ===========================================================================
// ERROR (thiserror, lib 边界;daemon tick 经此 Result 退避,§10/bug-084)
// ===========================================================================

/// messaging 子系统错误。投递/调度/IO 失败一律经此 `Result` 上抛,由 step 12 主循环
/// catch + 退避;**绝不**在调度/投递循环 panic (bug-084)。
#[derive(Debug, Error)]
pub enum MessagingError {
    #[error("db: {0}")]
    Db(#[from] crate::db::DbError),
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("message store: {0}")]
    Store(#[from] crate::message_store::MessageStoreError),
    #[error("transport: {0}")]
    Transport(#[from] crate::transport::TransportError),
    #[error("state: {0}")]
    State(#[from] crate::state::StateError),
    #[error("event log: {0}")]
    EventLog(#[from] crate::event_log::EventLogError),
    #[error("provider: {0}")]
    Provider(String),
    /// envelope / 输入校验 (validate_result_envelope 等)。
    #[error("validation: {0}")]
    Validation(String),
    /// team 解析 / 路由 / owner-gate 拒绝 (非 IO,语义性失败)。
    #[error("routing: {0}")]
    Routing(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
}

#[cfg(test)]
mod tests;
