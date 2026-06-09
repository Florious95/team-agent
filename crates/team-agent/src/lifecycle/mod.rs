//! step 13 · lifecycle — team 进程级生命周期编排器(ROUND-0.5 骨架)。
//!
//! 真相源 `team-agent-public` @ v0.2.11 (439bef8),只读。Card:
//! `docs/phase0/subsystems/13-lifecycle.md`。
//!
//! Python 源:
//! - `launch/core.py`(`launch`)、`launch/bootstrap.py`/`config.py`/`requirements.py`。
//! - `lifecycle/start.py`(`start_agent`)、`lifecycle/operations.py`
//!   (`stop_agent`/`reset_agent`/`add_agent`/`fork_agent`)、`lifecycle/agents.py`
//!   (`remove_agent` + `_RemoveRollback`)、`lifecycle/paste_buffer_hygiene.py`。
//! - `restart/orchestration.py`(`restart` Route B)、`restart/selection.py`、`restart/snapshot.py`。
//! - `display/backend.py`/`adaptive.py`/`tiling.py`/`workspace.py`/`worker_window.py`/
//!   `ghostty.py`/`close.py`/`rebuild.py`。
//! - `orchestrator/__init__.py`/`plan.py`/`state.py`(plan 多 stage 状态机)。
//! - `diagnose/quick_start.py`(`quick_start`/`prepare_quick_start_team`/`wait_ready`)。
//!
//! 价值:把下层原语(transport step9 / provider step8 / leader step10 / messaging step11 /
//! state step5 / coordinator step12 / compiler step6)编排成**原子的、可回滚的、可审计的**
//! 用户级动作。本 module 不拥有底层原语,只**调用**它们。
//!
//! § 锁(机械化,leader 集成时上 `#![deny(unwrap/expect/panic)]`):本 module 调
//! `save_runtime_state`/`save_team_runtime_snapshot`/`save_plan_state`(`os.replace` 路径,
//! bug-084 高危)—— 所有写路径强制 `Result`,`EACCES/EPERM/EBUSY` 退避重试,绝不 unwrap。
//! § lifecycle **构造** provider 命令字符串经 step8 `ProviderAdapter` trait,**绝不**链接
//! provider client crate(anthropic/openai SDK)。
//!
//! ROUND-0.5:数据类型 + 行为入口 fn 签名齐备,body = `unimplemented!("step13 port: ...")`。
//! contracts blitz 可 NAME 这些类型并 CALL 这些 fn、断言其 rich return。

// ROUND-0 skeleton:fn body 全 unimplemented!() → import/field/param/大 Err 暂未落地;P2 porter 实现时移除。
#![allow(dead_code, unused_imports, unused_variables, clippy::result_large_err, clippy::doc_overindented_list_items, clippy::doc_lazy_continuation, clippy::io_other_error)]
// §10:lifecycle 写路径(save_runtime_state/snapshot/plan_state,bug-084 高危)实现层禁 unwrap/expect/panic
// (unimplemented!() stub 不被拦);tests 子模块各自 allow。
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

pub mod display;
pub mod helpers;
pub mod launch;
pub(crate) mod profile_launch;
pub(crate) mod profile_smoke;
pub mod restart;
pub mod types;

use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::model::ids::AgentId;
use crate::provider::{RolloutPath, SessionId};
use crate::transport::{PaneId, SessionName, WindowName};

// 复用既有 enum(model::enums)。`DisplayBackend` 已在 step2 定义并带 `has_worker_views()`。
pub use crate::model::enums::DisplayBackend;

pub use types::*;

pub use display::*;
pub use launch::*;
pub use restart::*;

pub use helpers::save_team_runtime_snapshot;
pub(crate) use helpers::{plan_lock_path, plan_state_path, read_plan_state, save_plan_state};

#[cfg(test)]
mod tests;
