//! step 8 · provider — type + trait SKELETON (ROUND-0).
//!
//! Design doc: `docs/phase0/subsystems/08-provider.md`
//! (type-inventory table lines ~48-67, public API list ~71-81, module map ~128-137).
//! Truth source (READ-ONLY snapshot `team-agent-public` @ v0.2.11 / `439bef8`):
//!   - `provider_cli/base.py` · `adapter.py` · `claude.py` · `codex.py` · `gemini.py` · `fake.py`
//!   - `provider_cli/prompt.py` · `registry.py` · `unsupported.py` · `providers.py`
//!   - `idle_predicate.py`
//!   - `provider_state/common.py` (`decide_state`/`process_liveness`/`_CLOSING`)
//!   - `provider_state/{claude,codex,registry,__init__}.py`
//!   - `approvals/{constants,parsing,runtime_prompts,status}.py`
//!
//! 职责(doc §职责): per-provider 命令构造 / session 捕获 / JSONL→neutral turn-state
//! 翻译 / idle take-over predicate / runtime approval·trust prompt 检测应答。
//!
//! 铁律(doc §bug/陷阱):
//!   - **unknown 永不当 idle**:`TurnState` 穷尽 match,无 `_ => idle` 兜底。
//!   - **bug-085 None 漏穿**:`session_id`/`rollout_path`/`turn_id` 全 `Option<T>`,穷尽处理 `None`。
//!   - **MUST-NOT-13**:idle/classify/selftest/abnormal 路径零 provider client / network SDK。
//!   - §10:本子系统跑在 daemon tick 入口,fallible 方法返 `Result`,实现层禁 unwrap/expect/panic
//!     (`#![deny(...)]` 由 leader 在 integration 时加,本 ROUND-0 skeleton 不加)。
//!
//! ROUND-0:仅类型 + trait 签名。自由函数 / placeholder impl body 一律
//! `unimplemented!("step8 port: <what>")`。

// §10:daemon tick / idle / classify 路径,实现层禁 unwrap/expect/panic(unimplemented! 未实现 stub 不算)。
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

// REUSE — 不重定义(doc ADJUDICATION):`Provider` 单 `ClaudeCode` 变体归一 claude/claude_code;
// `AuthMode` 决定 fork 能力 / MCP config 路径 / claude compatible_api fallback 分支。
// 复用 model::enums 的 Provider/AuthMode,并 re-export(下游惯于 `provider::Provider` 取用)。
pub use crate::model::enums::{AuthMode, Provider};

pub mod adapter;
pub mod approvals;
pub mod classify;
pub mod faults;
pub mod startup_prompt;
pub mod types;
// helpers 全部原为模块私有(JSONL 解析 / 正则编译),非根可见 → 私有 mod,不参与 re-export。
mod helpers;

// RE-EXPORT INVARIANT:外部 `crate::provider::X` + 测试 `use super::*` 解析不变。
pub use adapter::*;
pub use approvals::parsing::*;
pub use approvals::runtime_prompts::*;
pub use classify::*;
pub use faults::*;
pub use startup_prompt::*;
pub use types::*;

#[cfg(test)]
mod tests;
