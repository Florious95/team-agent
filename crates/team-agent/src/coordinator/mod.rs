//! step 12 · coordinator — daemon lifecycle / single-tick orchestration SKELETON (ROUND-0).
//!
//! Card: `docs/phase0/subsystems/12-coordinator.md`.
//! Truth source (READ-ONLY snapshot `team-agent-public` @ v0.2.11 / `439bef8`):
//!   - `coordinator/__init__.py`     (public re-export face; lazy `main` to break import cycle)
//!   - `coordinator/__main__.py`     (daemon main loop: pid/meta write, SIGTERM→STOP,
//!     orphan self-detect, catch-all + exponential backoff 5→60s, tick_error dedupe/suppress,
//!     tick_recovered)
//!   - `coordinator/lifecycle.py`    (health / start / stop / tick orchestration + schema health)
//!   - `coordinator/metadata.py`     (COORDINATOR_PROTOCOL_VERSION=2, pid_is_running, read/write/ok meta)
//!   - `coordinator/paths.py`        (coordinator.pid / coordinator.json / coordinator.log paths)
//!   - `watch/__init__.py`           (run_watch / collect_watch_lines / render_event_line / WatchCursor)
//!   - `abnormal_track.py`           (Gap 32 §4 provider-neutral abnormal track:
//!     process_abnormal_records / detect_whole_team_gone)
//!
//! 职责(card §职责):per-workspace daemon 生命周期 + 单次 tick 编排。tick 按固定顺序把
//! step 8-11 的原子操作串成一个只读 + 投递既定 obligation 的回路 —— **绝不**在无 pending
//! obligation 时注入探索性 prompt(§10 MUST-NOT-13 / §84)。
//!
//! 铁律(card §bug/陷阱):
//!   - **bug-084**:tick-end `save_runtime_state` 失败 → degraded `TickReport{ok:false,
//!     reason:PersistenceDegraded, persisted:false}` + `runtime.state.save_failed` 事件,**绝不 panic**;
//!     主循环 catch + 指数退避 5→10→20→40→60→60s + 去重/抑制 tick_error。
//!   - **bug-085 / unknown≠idle**:`TurnState::Unknown` 显式 block ping(穷尽 match,无 fallthrough);
//!     `rollout_path` 用 `Option<RolloutPath>`;长期 unknown 第 60 tick 起每 12 tick
//!     发 `idle_takeover.unknown_persistent`。
//!   - **take-over arm 来自真实投递**:监视器只能由真实 leader→worker 投递的 turn-open edge arm,
//!     绝不凭空 arm;无投递的队 → `not_armed_no_worker_turn`,绝不 ping。
//!   - **schema 兼容门**:metadata 三元(pid/protocol_version/message_store_schema_version)任一不匹配
//!     → restart_incompatible 先 stop 再起,**不可静默继续**用旧 schema 写库;区分 pre-init 必需列
//!     (拒启)vs migratable 列(可迁移)。
//!   - **孤儿不退 SIGTERM**:必须 SIGKILL 升级,优先按 pgid 杀整组。
//!   - **孤儿自终止**:仅 `current_ppid != initial_ppid ∧ current_ppid == 1 ∧ workspace 不存在`
//!     三者同时成立才自杀。
//!   - **§84 零注入**:无 pending obligation + event 时绝不注入探索性 prompt;compaction/drift/
//!     api-error/idle 探测都是只读分类;唯一会投递的 `push_idle_reminder` 也只在 `should_ping` 时
//!     发一条中立 ack 提示。
//!   - **整队消失区分 clean vs unexpected**:clean_shutdown/restart_in_progress 静默,
//!     仅 unexpected exit 写 durable marker + 延迟到下条 leader 命令再 escalate。
//!   - **abnormal track 不读屏/不命名 provider**:只消费结构化 fault fact + 进程身份;
//!     `(signature, turn_id)` 去重,turn_id 缺失退化为 per-record fingerprint 桶。
//!   - **watch rotation**:archive_signature 变化或 offset>size 即插 ROTATION_MARKER 并重置 offset,
//!     不重放历史段。
//!
//! ROUND-0:仅类型 + struct/enum + fn/trait/method 签名。所有 body =
//! `unimplemented!("step12 port: <what>")`。fallible 路径返 `Result`/`Option`(§10 实现层禁
//! unwrap/expect/panic,签名先做成 fallible)。daemon-path `tick(..) -> Result<TickReport, TickError>`。
//! `#![deny(...)]` 由 leader 在集成时统一加,本骨架不加。

// ROUND-0 skeleton:fn body 全 unimplemented!() → import/field/method 暂未被用;P2 porter 落实现时移除。
#![allow(dead_code, unused_imports)]
// §10:daemon-path(tick/abnormal/health)实现层禁 unwrap/expect/panic(unimplemented!() stub 不被拦);
// tests 子模块各自 allow。bug-084:tick-end persist 失败走 degraded TickReport,绝不 panic。
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

// ── module-root REUSE imports ─────────────────────────────────────────────
// decompose 前这些 `use` 在 coordinator.rs 顶层,测试 `mod tests` 经 `use super::*`
// 继承它们。拆分后保留在 mod.rs(私有 use,子模块 tests 仍经 `use super::*` 解析)。
use crate::message_store::MessageStore;
use crate::model::enums::Provider;
use crate::provider::{ProviderAdapter, TurnId, TurnState};
use serde_json::Value;

pub mod backoff;
pub mod health;
pub mod orphan;
pub mod runtime_detectors;
pub mod runtime_observation;
pub mod tick;
pub mod types;

// ── Re-export the full module-root surface (RE-EXPORT INVARIANT) ──────────────
// 这些 `pub use`/`pub(crate) use` 把每个曾在 module 根可见的 item 重新 surface 到
// `crate::coordinator::X`,使外部 crate 路径 + 测试 `use super::*` 解析保持不变。
pub use types::*;
pub use tick::*;
pub use backoff::*;
pub use orphan::*;
pub use health::*;
pub use runtime_detectors::*;
pub use runtime_observation::*;

#[cfg(test)]
mod tests;
