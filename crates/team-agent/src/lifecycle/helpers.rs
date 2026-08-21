//! ---
//! purpose: runtime 快照的诊断性落盘，以及 plan 状态文件的路径与读写
//! contract:
//!   provides:
//!     - name: save_team_runtime_snapshot
//!       what: 把 runtime state 以带 not_authoritative 标记的诊断副本写到 legacy 每 session 路径
//!     - name: team_snapshot_path
//!       what: 给出某 session 的诊断快照路径
//!     - name: plan_state_path
//!       what: 给出某 plan 的状态文件路径
//!     - name: plan_lock_path
//!       what: 给出某 plan 的锁文件路径
//!     - name: read_plan_state
//!       what: 读取并反序列化 plan 状态
//!     - name: save_plan_state
//!       what: 序列化 plan 状态并经临时文件 rename 落盘
//!   depends:
//!     - crate::state::persist
//!     - crate::model::paths
//!     - std::fs
//! boundary:
//!   - 快照文件不是权威 runtime state，产品写路径不得调用它
//!   - plan 锁文件只被创建，本模块不对它加锁，也不提供并发互斥
//!   - 不解释 plan 语义，只做路径、序列化与落盘
//! maturity: wired
//! ---
//!
//! lifecycle::helpers —— runtime snapshot 原子写(bug-084)+ plan-state 路径/读取。

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use super::*;

/// ---
/// purpose: 把 runtime state 写成带诊断标记的每 session 快照副本
/// params:
///   state: 待快照的 runtime state，必须含 session_name 字段
/// returns: 写出的快照文件路径
/// errors: 缺 session_name、建目录、写临时文件或 rename 失败时返回 StatePersist
/// ---
///
/// `save_team_runtime_snapshot(workspace, state)` — Foundation-0 F0-2:
/// this is now a **diagnostic-only** legacy per-session snapshot writer.
/// Root/projection `.team/runtime/state.json` is the sole product
/// authority; the file this writes carries `_not_authoritative:true`
/// and pointers back to the canonical path so no reader can mistake it
/// for the real runtime state. See
/// `.team/artifacts/foundation-0-slice-design.md` §§4-5 F0-2.
///
/// Product save paths (attach/claim/start-agent/restart/stop/remove)
/// must NOT call this — the RED2 grep guard enforces that. Retained
/// callers are diagnostic/migration/test paths only.
///
/// Preserves the legacy path shape (`.team/runtime/teams/<session>/state.json`)
/// so 0.5.x operators inspecting the file see the diagnostic marker
/// rather than being surprised by a missing file. The B3 canonical
/// team-key layout will live under a different path
/// (`teams/<team_key>/...`), so this file also cannot be mistaken for
/// the future canonical shape.
pub(crate) fn save_team_runtime_snapshot(
    workspace: &Path,
    state: &serde_json::Value,
) -> Result<PathBuf, LifecycleError> {
    let session_name = state
        .get("session_name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| LifecycleError::StatePersist("session_name is required".to_string()))?;
    let path = team_snapshot_path(workspace, session_name);
    let dir = path
        .parent()
        .ok_or_else(|| LifecycleError::StatePersist("snapshot path has no parent".to_string()))?;
    fs::create_dir_all(&dir).map_err(|e| persist_err("create snapshot dir", &e))?;
    let canonical_path = crate::state::persist::runtime_state_path(workspace);
    let generated_at = chrono::Utc::now().to_rfc3339();
    // F0-2 RED1: annotate the snapshot payload with diagnostic-only
    // metadata so any reader can immediately see it is derived, not
    // authoritative. `_canonical_state_path` points at the real
    // runtime authority; `_derived_from` names the code path that
    // produced this file; `_generated_at` records freshness so stale
    // snapshots are easy to identify.
    let mut annotated = state.clone();
    if let Some(obj) = annotated.as_object_mut() {
        obj.insert("_not_authoritative".to_string(), serde_json::json!(true));
        obj.insert(
            "_canonical_state_path".to_string(),
            serde_json::json!(canonical_path.to_string_lossy()),
        );
        obj.insert(
            "_derived_from".to_string(),
            serde_json::json!("lifecycle::save_team_runtime_snapshot"),
        );
        obj.insert("_generated_at".to_string(), serde_json::json!(generated_at));
    }
    let tmp = dir.join("state.json.tmp");
    let data = serde_json::to_vec_pretty(&annotated)
        .map_err(|e| LifecycleError::StatePersist(format!("serialize snapshot: {e}")))?;
    fs::write(&tmp, data).map_err(|e| persist_err("write snapshot temp", &e))?;
    fs::rename(&tmp, &path).map_err(|e| persist_err("replace snapshot", &e))?;
    Ok(path)
}

/// ---
/// purpose: 给出某 session 的诊断快照文件路径
/// params:
///   session_name: 原始 session 名，非字母数字与下划线点横线的字符会被替换
/// returns: runtime 目录下 teams 子目录中的 state.json 路径
/// ---
pub fn team_snapshot_path(workspace: &Path, session_name: &str) -> PathBuf {
    crate::model::paths::runtime_dir(workspace)
        .join("teams")
        .join(safe_snapshot_name(session_name))
        .join("state.json")
}

fn persist_err(action: &str, err: &io::Error) -> LifecycleError {
    LifecycleError::StatePersist(format!("{action}: {err}"))
}

fn safe_snapshot_name(raw: &str) -> String {
    let replaced: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let trimmed = replaced.trim_matches(|c| matches!(c, '.' | '_' | '-'));
    if trimmed.is_empty() {
        "team".to_string()
    } else {
        trimmed.to_string()
    }
}

/// ---
/// purpose: 给出某 plan 的状态文件路径
/// returns: runtime 目录下 orchestrator 子目录中的 plan state 文件路径
/// ---
pub(crate) fn plan_state_path(workspace: &Path, plan_id: &PlanId) -> PathBuf {
    plan_state_dir(workspace).join(format!("plan-{}.state.json", plan_id.as_str()))
}

/// ---
/// purpose: 给出某 plan 的锁文件路径
/// returns: runtime 目录下 orchestrator 子目录中的 plan lock 文件路径
/// ---
pub(crate) fn plan_lock_path(workspace: &Path, plan_id: &PlanId) -> PathBuf {
    plan_state_dir(workspace).join(format!("plan-{}.lock", plan_id.as_str()))
}

fn plan_state_dir(workspace: &Path) -> PathBuf {
    crate::model::paths::runtime_dir(workspace).join("orchestrator")
}

/// ---
/// purpose: 从文件读取并反序列化 plan 状态
/// returns: 解析出的 PlanState
/// errors: 文件读不到或 JSON 不合法时都返回 InvalidPlan
/// ---
pub(crate) fn read_plan_state(path: &Path) -> Result<PlanState, LifecycleError> {
    let data = fs::read_to_string(path)
        .map_err(|e| LifecycleError::InvalidPlan(format!("plan not found: {e}")))?;
    serde_json::from_str(&data)
        .map_err(|e| LifecycleError::InvalidPlan(format!("invalid plan state: {e}")))
}

/// ---
/// purpose: 序列化 plan 状态，经同目录临时文件 rename 落盘，并确保锁文件存在
/// returns: 写出的 plan 状态文件路径
/// errors: 建目录、创建锁文件、写临时文件或 rename 失败时返回 StatePersist
/// ---
pub(crate) fn save_plan_state(
    workspace: &Path,
    state: &PlanState,
) -> Result<PathBuf, LifecycleError> {
    let path = plan_state_path(workspace, &state.plan_id);
    let parent = path
        .parent()
        .ok_or_else(|| LifecycleError::StatePersist("plan state path has no parent".to_string()))?;
    fs::create_dir_all(parent).map_err(|e| persist_err("create plan state dir", &e))?;
    let lock = plan_lock_path(workspace, &state.plan_id);
    if !lock.exists() {
        fs::write(&lock, b"").map_err(|e| persist_err("write plan lock", &e))?;
    }
    let tmp = parent.join("state.json.tmp");
    let data = serde_json::to_vec_pretty(&plan_state_json(state))
        .map_err(|e| LifecycleError::StatePersist(format!("serialize plan state: {e}")))?;
    fs::write(&tmp, data).map_err(|e| persist_err("write plan state temp", &e))?;
    fs::rename(&tmp, &path).map_err(|e| persist_err("replace plan state", &e))?;
    Ok(path)
}

fn plan_state_json(state: &PlanState) -> serde_json::Value {
    serde_json::json!({
        "completed_stages": state.completed_stages,
        "current_dispatch": state.current_dispatch,
        "current_stage": state.current_stage,
        "halt_artifact": state.halt_artifact,
        "halt_reason": state.halt_reason,
        "plan_id": state.plan_id,
        "plan_path": state.plan_path,
        "stages": state.stages,
        "started_at": state.started_at,
        "status": state.status,
        "team": state.team,
    })
}
