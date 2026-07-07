//! Team Agent — Rust 全量重写。
//!
//! 真相源: Python `team-agent-public` @ `v0.2.11` (commit `439bef8`),只读快照。
//! 见 `docs/phase0/snapshot-manifest.md`。
//!
//! # 现状: Phase 0
//! 本 crate 暂空(§13「第一步」: 骨架先分 module 不拆 crate)。实现尚未开始 ——
//! 先产出 Phase 0 五交付物并机械自审/cr 签字,才动实现(§4 铁律)。
//!
//! # 计划 module 地图(§5 单一真相源设计 / §6 15 步依赖序)
//! 实现按 §6 顺序逐个落地;每个 module 落地时对照 `contracts.yaml` 验。
//!
//! ```text
//!  step 2  model      typed id/status/envelope/spec/state/DB row/event structs
//!  step 3  db         SQLite schema/migration (rusqlite bundled, WAL+busy-timeout)
//!  step 4  event_log  events.jsonl append/tail/rotation + 稳定事件名
//!  step 5  state      state.json 原子写/锁/self-heal/team projection
//!  step 6  spec       TEAM.md/agents/profiles compile/validate
//!  step 7  message_store  messages/results/scheduled/tokens/health/watchers/...
//!  step 8  provider   命令生成/MCP config/transcript·status 解析/trust/session
//!  step 9  transport  tmux list/capture/inject/send-keys/readiness + 平台能力门
//!  step 10 leader     attach/claim/takeover/rebind/rediscover/inject 去重
//!  step 11 messaging  send/内部投递/retry/scheduled/collect·report_result/watchers
//!  step 12 coordinator  daemon lifecycle/tick/health/stuck·idle fallback/takeover
//!  step 13 lifecycle  quick-start/add·fork·reset·stop·remove/restart/display
//!  step 14 mcp+cli    stdio MCP/tool/clap 子命令/错误
//!  step 15 packaging  各平台 release/薄 installer shim/migration·repair
//! ```
//!
//! §10 守护进程不崩: `coordinator`/`lifecycle`/`daemon` module 落地时,各自顶加
//! `#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]`,tick 签名
//! `fn tick(..) -> Result<TickReport, TickError>`。
//!
//! §10 不隐藏消耗 token: provider 调用走 trait;`coordinator`/`scheduler` 不依赖任何
//! provider client crate,测试断言 mock 调用计数 = 0。

// step 2 (model) — typed bedrock。其余 step 落地时在此挂载。
pub mod model;

// step 3 (db) — team.db SQLite schema/migration(rusqlite bundled)。
pub mod db;

// step 4 (event_log) — events.jsonl 唯一审计流(append/tail/rotation + 稳定事件名)。
pub mod event_log;

// step 5 (state) — state.json 持久化/identity/owner-gate/projection(bug-084 韧性)。
pub mod state;

// step 6 (compiler) — TEAM.md + agents/*.md → 规范 team.spec dict(doc→spec 纯变换)。
pub mod compiler;

// step 7 (message_store) — team.db 上的核心消息生命周期(create/claim/mark/通知去重)。
// unit-10 (Stage 4) compat shim. Physical home is now `crate::db::message_store`.
pub use crate::db::message_store;

// step 8 (provider) — ProviderAdapter trait + typed provider/turn-state/liveness 等(ROUND-0 骨架;
// fn body unimplemented!(),P2 porter 落实现)。MUST-NOT-13:provider 调用全走 trait。
pub mod provider;
/// unit-6 (Stage 2) compat shim. Physical home is now
/// `crate::provider::session::capture`; this re-export keeps every
/// `crate::session_capture::*` caller working without modification.
pub use crate::provider::session::capture as session_capture;
pub(crate) mod os_probe;

// step 9 (transport) — Transport trait(控制面)+ Target/PaneId/InjectReport 等(ROUND-0 骨架;
// fn body unimplemented!(),P2 porter 落实现)。tmux/WezTerm/ConPTY 三后端。
pub mod transport;

// step 10-12 (leader/messaging/coordinator) — ROUND-0.5 类型+fn surface 骨架(fn body unimplemented!(),
// P2 porter 落实现)。leader=lease/owner-bind/idle-takeover;messaging=send/deliver/retry/watchers;
// coordinator=daemon tick(§10 no-panic,Result<TickReport,TickError>)。
pub mod leader;
pub mod messaging;
pub mod coordinator;
pub mod diagnose;

// step 13-15 (lifecycle/mcp_server/cli/packaging) — ROUND-0.5b behavioral-rich 骨架(entry-fn 签名 +
// 富返回类型,fn body unimplemented!(),P2 porter 落实现)。lifecycle=quick-start/restart/display;
// mcp_server=stdio MCP tool handlers;cli=clap 子命令;packaging=install/migrate/repair。
pub mod lifecycle;

// 0.5.x Windows portability Batch 0: platform abstraction layer.
// Truth sources:
// - Design:    `.team/artifacts/0.5.x-windows-portability-survey-design.md`
// - CR verdict: `.team/artifacts/0.5.x-windows-portability-cr-verdict.md`
//              (6 constraints anchored inside the module doc)
pub mod platform;
// 0.3.28 — unified adaptive layout manager (single source of truth for tmux
// topology decisions). See `.team/artifacts/adaptive-layout-full-architecture-locate.md`.
pub mod layout;
pub mod topology;
pub mod mcp_server;
pub mod cli;
pub mod packaging;

// fake-worker — subscription-free backing program for Provider::Fake; lets the real spawn path
// (launch dry_run=false → tmux window) be exercised with no real provider (port of fake_worker.py).
pub mod fake_worker;

// tmux_backend — concrete tmux Transport executor (Command::new tmux via a CommandRunner seam);
// the real spawn/capture/inject/has_session backend the daemon + launch use (step 9 shipped only
// the trait + argv-builders). Real subprocess execution is the #[ignore] real-machine boundary.
pub mod codex_app_server;
pub mod tmux_backend;

// 0.5.x Windows-native transport Phase 1: ConPTY backend + named-pipe
// protocol. `protocol` is portable (pure logic, tested on all hosts);
// `backend` compiles on all hosts and returns typed MuxUnavailable when
// no pipe client is wired (honest degradation, not silent success).
//
// Truth source:
// - Design: `.team/artifacts/0.5.x-windows-native-transport-design.md`
//   §Transport Boundary + §Named Pipe Control Protocol + §Phase 1
// - CR verdict: `.team/artifacts/0.5.x-windows-transport-cr-verdict.md`
//   (7 constraints; C-1/C-2/C-3/C-5 anchor into this module)
pub mod conpty;

// 0.5.x Phase 1d Batch 0: backend assembly factory.
// Truth sources:
// - Design:    `.team/artifacts/0.5.x-backend-assembly-factory-design.md`
// - CR verdict: `.team/artifacts/0.5.x-backend-factory-cr-verdict.md`
//              (6 constraints anchored inside the module doc)
pub mod transport_factory;

// 0.5.x Windows portability Batch 5: `app_server_test_support` is a
// Unix-domain-socket fake for the Codex app-server client. It is
// Unix-only because the code-under-test (`codex_app_server`) is
// Unix-only via cfg (Windows gets typed `SocketUnreachable`
// unsupported returns). Cfg-gating the module keeps `cargo check
// --tests --target x86_64-pc-windows-msvc` compilable.
#[cfg(all(test, unix))]
pub(crate) mod app_server_test_support;
