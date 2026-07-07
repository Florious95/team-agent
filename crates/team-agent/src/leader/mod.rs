//! step 10 · leader — ownership lease / leader-session binding / idle-takeover wiring
//! (ROUND-0 SKELETON: types + fn/method SURFACE only, all bodies `unimplemented!()`).
//!
//! Card: `docs/phase0/subsystems/10-leader.md`
//! (owned data/types table §16-37, public API §41-53, locks/rules §95-99).
//! Truth source (READ-ONLY snapshot `team-agent-public` @ v0.2.11 / `439bef8`):
//!   - `leader/__init__.py` (926 lines) — the five lease paths
//!     (attach / start / claim / autobind / readopt) + leader identity context
//!     + dual-state write + audit events.
//!   - `leader_binding.py` (183) — Family A positive-source owner binding
//!     (`bind_owner_from_caller_pane` / `derive_leader_session_uuid` / `emit_owner_bound_event`).
//!   - `idle_takeover.py` (facade re-export) / `idle_takeover_wiring.py`
//!     (`build_idle_nodes` / `push_idle_reminder`) / `wake.py`
//!     (`should_reread` / `on_file_changed` / `take_pending`).
//!   - `messaging/leader_panes.py` (`_leader_command_looks_usable` /
//!     provider attribution / `_target_leader_session_uuid`).
//!
//! 职责(card §职责):拥有「谁是 leader、消息投到哪个 pane」这一身份事实,当成租约(lease)
//! 管理。pane id 即权威路由/授权身份,`owner_epoch` 做 CAS/去重,确定派生的
//! `leader_session_uuid` + 注入 env 作兼容/审计元数据。统一 attach/claim/takeover/autobind/
//! readopt 五条路径到同一把锁(`LEADER_OWNERSHIP_LOCK = "send"`)同一组安全门,双写
//! workspace-level + team-level 两份 state 不分叉,每个 acquire/rebind/refusal 写结构化审计。
//!
//! 铁律(card §bug/陷阱 §80-89):
//!   - **unknown ≠ idle**:turn state 用 `provider::TurnState` 穷尽 match,`Unknown` 显式
//!     block ping,**绝不** `_ => idle`。
//!   - **bug-085 None 漏穿**:`rollout_path` 用 `Option<RolloutPath>`,`None` 走 `Unknown`
//!     分支并记 diagnostics;`_leader_node` path/provider 缺则省略 leader 节点而非猜 idle。
//!   - **pane 即身份(C2/C10)**:授权是 pane id 等值,owner liveness 要求候选 pane 携带 owner
//!     的 `leader_session_uuid`,不只看「leader-looking 命令名」。cwd 比对两边 realpath 后子树
//!     包含,禁 basename/startswith/子串/反推。
//!   - **死 owner 不锁活 caller(C4)**:liveness 探针失败 = 未确认活 → fail-safe 不据 stale
//!     record 拒新 caller;模糊信号(命令错/cwd 错但 pane 在)要 `--confirm`。
//!   - **TOCTOU epoch race(C3/C15)**:precheck 后在锁内 revalidate epoch + liveness,真 CAS。
//!   - **双写不分叉(C17/C18)**:同一锁内写 workspace state.json + team/<session> snapshot;
//!     发现分叉写 `state_divergence_repaired`。多候选 claim 分支也必须双写。
//!   - **claude_code 归一**:`provider::Provider` 单一归一,穷尽 match,编译期堵死。
//!   - **wake 不轮询/不解析**:`wake` 层纯决策何时重读,provider-neutral。
//!
//! ROUND-0:仅类型 + fn/method 签名;fallible 操作返 `Result`/`Option`(§10 实现层禁
//! unwrap/expect/panic,故签名先 fallible)。daemon-path(tick/idle push)返 `Result`。
//! `#![deny(...)]` 由 leader 在 integration 时统一加,本骨架不加。

// ROUND-0 skeleton:fn body 全 unimplemented!() → import/field/method 暂未被用;P2 porter 落实现时移除。
#![allow(dead_code, unused_imports)]
// §10:lease / owner-bind / idle-takeover / tick 路径,实现层禁 unwrap/expect/panic(未 port 的
// unimplemented! stub 不算 —— 那些待各自 contract+port)。
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use thiserror::Error;

// ── REUSE: 既有 typed 地基(不重定义)──────────────────────────────────────────
// model::ids — leader 身份/owner epoch newtypes(§3)。
use crate::model::ids::{LeaderSessionUuid, OwnerEpoch, TeamKey};
// provider — turn state 穷尽枚举(unknown≠idle 命门)+ provider 归一 + bug-085 None 漏穿。
use crate::provider::{Provider, RolloutPath, TurnState};
// transport — pane 寻址 / 后端无关 PaneInfo(身份/rebind 地基)。
use crate::transport::{PaneId, SessionName, Target, WindowName};
// state — owner-gate caller 身份 + first-time binding + identity 派生 + projection。
use crate::state::owner_gate::CallerIdentity;
use crate::state::StateError;

// ── REUSE-by-reference(签名引用,实际复用既有自由函数,不在此重声明)──────────────
// state::identity::{apply_first_time_leader_binding, leader_env_exports,
//                   validate_leader_uuid_from_targets, caller_identity_from_env,
//                   identity_machine_fingerprint, identity_os_user}
// state::projection::{team_state_key, select_runtime_state}
// state::owner_gate::{check_team_owner, workspace_paths_match, PaneLivenessProbe}
// model::ids::LeaderSessionUuid::derive  (== Python derive_leader_session_uuid)
// event_log::EventLog (write/tail)

// ── submodules(by responsibility) ──────────────────────────────────────────
mod helpers;
pub mod inject;
pub mod lease;
pub mod owner_bind;
pub mod provider_attribution;
pub mod rediscover;
pub mod registry;
pub mod start;
pub mod takeover;
pub mod types;

// ── RE-EXPORT INVARIANT:每个先前 root-visible 项原路径不变 ────────────────────
pub use inject::*;
pub use lease::*;
pub use owner_bind::*;
pub(crate) use provider_attribution::*;
pub use rediscover::*;
pub use start::*;
pub use takeover::*;
pub use types::*;
// 私有 helper(原 module-private,现 pub(crate) 跨子模块复用)按原可达性再导出。
pub(crate) use helpers::*;

#[cfg(test)]
mod tests;
