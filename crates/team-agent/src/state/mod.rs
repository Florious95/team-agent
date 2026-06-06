//! step 5 · state — state.json 持久化 / identity / owner-gate / projection(真相源 `state.py`)。
//!
//! §6 step 5:原子写/锁/self-heal(**bug-084** 血泪)/ team projection / leader receiver identity /
//! owner-gate(trust own-vs-foreign,**§11/bug-064/082**)。owner-gate 的 tmux-liveness 依赖经
//! **trait 注入**,真探测延 step 9。
//!
//! **字节关键**:state.json = `json.dumps(state, indent=2, ensure_ascii=False)` —— pretty 2-空格 +
//! 非 ASCII 字面 + **无 sort_keys(插入序 → 靠 serde_json preserve_order)**,与 event_log 的
//! sort_keys/compact 截然不同。
//!
//! 本 slice 落地:`persist`(bug-084 韧性)。owner-gate / projection / identity 后续 slice。
//!
//! §10:无 unwrap/expect/panic;bug-084 的 os.replace 崩溃经 `Result` + 退避 + self-heal 处理,
//! 绝不 in-place truncate,绝不让审计/重试失败拖垮可见的原 state。
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

pub mod identity;
pub mod owner_gate;
pub mod persist;
pub mod projection;
pub mod selector;

use serde_json::Value;
use thiserror::Error;

/// Python 真值语义(`null`/`false`/`0`/`""`/`[]`/`{}` → false),`state` 模块共用。
pub(crate) fn json_truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

#[derive(Debug, Error)]
pub enum StateError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("{0} is locked by another team-agent process; serialize team-agent {0} calls and retry")]
    Locked(String),
    /// self-heal 也失败(原 state 仍可见);携带最终错误。
    #[error("state save failed after self-heal: {0}")]
    SaveFailed(String),
    /// `select_runtime_state` team 选择失败(`team_agent.errors.RuntimeError` 等价):
    /// 歧义 / 未找到。携带的 `String` == Python `str(exc)`(供 `resolve_team_scoped_state`
    /// 透传为 `team_target_unresolved` 的 `error` 字段)。
    #[error("{0}")]
    TeamSelect(String),
    /// identity 派生失败(leader_session_uuid 输入含 NUL,`derive_leader_session_uuid`
    /// raise ValueError 等价)。文件系统拒绝 NUL,实践中不会触发。
    #[error("identity: {0}")]
    Identity(#[from] crate::model::errors::ModelError),
}
