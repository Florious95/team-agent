//! step 14a · mcp_server — stdio MCP server (`team_orchestrator`) over JSON-RPC 2.0.
//!
//! Card: `docs/phase0/subsystems/14-mcp_cli.md` (MCP half).
//! Truth source (READ-ONLY) `team-agent-public` @ v0.2.11 / `439bef8`:
//!   - `mcp_server/server.py`    — stdio loop + JSON-RPC route (`dispatch`/`handle_mcp`/`main`)
//!   - `mcp_server/tools.py`     — `TeamOrchestratorTools`: the 12 typed tool handlers
//!   - `mcp_server/contracts.py` — `TOOLS`: name/description/inputSchema (wire single-truth)
//!   - `mcp_server/normalize.py` — result envelope / compact-result regularization
//!   - `mcp_server/__init__.py`  — package re-export surface locked by boundary tests
//!
//! SCOPE — this is the TYPE + BEHAVIORAL-ENTRY-FN layer so RED contracts can both
//! NAME the wire/envelope types AND CALL real handlers and assert against rich
//! return values. It is "the thinnest shell": it owns the wire protocol shape, the
//! tool-regularization rules, the error envelope, and identity/scope anchoring —
//! everything durable is delegated to step 5/6/7/11/13. Bodies are
//! `unimplemented!("step14 port: …")`.
//!
//! REUSE (do NOT redefine):
//!   - [`MessageStore`] (step 7) — `request_human` creates the leader message row.
//!   - [`EventLog`] (step 4) — `mcp.scope_resolved` / `mcp.send_message_refused` /
//!     `mcp.identity_inference_failed` / `mcp.task_inference_failed` audit events.
//!   - [`load_runtime_state`] / [`save_runtime_state`] (step 5 persist) — `assign_task`
//!     / `update_state` read-modify-write; `get_visible_peers` reads team scope.
//!   - [`messaging`] (step 11) — `send_message` / `report_result` / `collect` /
//!     `stuck_list` / `stuck_cancel` delegated by the tool handlers.
//!   - [`crate::model::enums`] (step 2) — [`ResultStatus`] / [`ChangeKind`] /
//!     [`TestStatus`] / [`RiskSeverity`] are the normalized result-envelope value
//!     enums; this layer ONLY does string-alias regularization onto them.
//!   - [`AgentId`] / [`TaskId`] / [`TeamKey`] (step 2 ids) — identity/scope anchors.
//!
//! 铁律 (card §11, Rust 绝不重蹈 Python 坑):
//!   - **scope 锚 env, 禁候选扫描** (C13-C17/bug-064/082): sender identity =
//!     spawn-time `TEAM_AGENT_ID`; scope = `TEAM_AGENT_OWNER_TEAM_ID`. `to="*"`
//!     defaults to the sender team; worker-origin RPC arguments cannot widen
//!     that scope. A peer not in scope → typed [`ToolError`] with
//!     [`ToolErrorReason::PeerNotInScope`] — never leak other-team peer names.
//!   - **错误信封冗余键** (server.py:98-106): `reason == error_code` and
//!     `message == error` are byte-stable downstream contracts — preserved verbatim
//!     in [`ToolError`]'s serialization, NOT "cleaned up".
//!   - **notifications/* 不回包** (server.py:49-50): `notifications/*` → [`RpcMethod::
//!     Notification`] → [`handle_mcp`] returns `None`; the loop `continue`s. Emitting
//!     a frame here would corrupt the stdout JSON-RPC stream.
//!   - **stdout 是传输通道** (server.py:135): every error is surfaced ON stdout as a
//!     JSON-RPC frame; logs/warnings MUST go to stderr/file, never stdout.
//!   - **worker-recipient 异步 accepted** (tools.py:176-183): a worker recipient with
//!     a message_id → [`SendOutcome::WorkerAccepted`] carrying the byte-stable
//!     `poll_via = "team-agent inbox <id>"`; leader/`*` → [`SendOutcome::Direct`].
//!   - **兜底字符串字节级保留** (bug-085): `_infer_task_id` failure → `"manual"` (not
//!     None); `_infer_agent_id` failure → `None` → caller routes to `"unknown"`.
//!
//! §10 deny: this subsystem is NOT a daemon/coordinator path, so the MCP shell does
//! not force top-level `#![deny(unwrap/expect/panic)]` (leader decides at
//! integration; card §109 carves out only `diagnose::comms::evaluate_idle_behavior`
//! and `diagnose::orphan::*`, which live in the CLI/diagnose lane, not here). All
//! fallible handlers return `Result<_, McpError>` regardless.

// ROUND-0 skeleton: fn bodies are all unimplemented!() so imports/fields/methods are
// not yet exercised; P2 porter removes this when implementing.
#![allow(
    dead_code,
    unused_imports,
    unused_variables,
    clippy::result_large_err,
    clippy::doc_overindented_list_items,
    clippy::doc_lazy_continuation,
    clippy::io_other_error
)]
// §10:MCP stdio handlers 实现层禁 unwrap/expect/panic(unimplemented!() stub 不被拦);tests 子模块各自 allow。
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeSet;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

// ── REUSE: step 2 model (ids + normalized-envelope value enums) ─────────────
use crate::model::enums::{ChangeKind, ResultStatus, RiskSeverity, TestStatus};
use crate::model::ids::{AgentId, TaskId, TeamKey};

// ── REUSE: step 4 event_log / step 7 message_store ──────────────────────────
use crate::event_log::EventLog;
use crate::message_store::MessageStore;

// ── REUSE: step 5 state persist / projection ────────────────────────────────
use crate::state::persist::{load_runtime_state, save_runtime_state};

// ── REUSE: step 11 messaging delegate surface ───────────────────────────────
use crate::messaging::{self, DeliveryOutcome, MessageTarget, SendOptions, TrustedSender};

pub mod helpers;
pub(crate) mod lifecycle_tools;
pub mod normalize;
pub mod tools;
pub mod types;
pub mod wire;

// ── re-export: 保持 `crate::mcp_server::X` 与 test `super::X` 解析不变 ─────────
pub use helpers::*;
pub use normalize::*;
pub use tools::*;
pub use types::*;
pub use wire::*;

// pub(crate) 子项 (normalize 的 list helpers、wire 的 dispatch_tool 等) 经此再导出,
// 使 `#[cfg(test)] mod tests` 的 `use super::*` 与跨子模块引用解析不变。
pub(crate) use helpers::{
    delivery_outcome_value, ensure_object, enum_value, insert_array, latest_reportable_message_for,
    latest_task_for_assignee, non_empty_string, normalize_token, normalized_envelope_value,
    object_fields, text_field, text_of_value, tool_error_reason_wire, tool_runtime_error,
};
pub(crate) use normalize::{
    normalize_artifacts, normalize_changes, normalize_next_actions, normalize_risks,
    normalize_tests,
};
pub(crate) use wire::dispatch_tool;

#[cfg(test)]
mod tests;
