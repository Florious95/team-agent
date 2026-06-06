//! lifecycle::helpers —— runtime snapshot 原子写(bug-084)+ plan-state 路径/读取。

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use super::*;

/// `save_team_runtime_snapshot(workspace, state)`(`restart/snapshot.py:42`,**bug-084**)。
/// `os.replace` 原子写;`EACCES/EPERM/EBUSY` 退避重试,**绝不 unwrap**。
pub fn save_team_runtime_snapshot(
    workspace: &Path,
    state: &serde_json::Value,
) -> Result<PathBuf, LifecycleError> {
    let session_name = state
        .get("session_name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| LifecycleError::StatePersist("session_name is required".to_string()))?;
    let safe = safe_snapshot_name(session_name);
    // golden restart/snapshot.py:46-47 — team_runtime_snapshot_dir = runtime_dir(workspace)/teams/<safe>,
    // and paths.py:25-26 runtime_dir = workspace/.team/runtime. Use the crate path helper so the snapshot
    // lands at <ws>/.team/runtime/teams/<safe>, matching golden AND the rest of the crate (not the
    // ".team"-less <ws>/runtime/teams that the original port wrote).
    let dir = crate::model::paths::runtime_dir(workspace).join("teams").join(safe);
    fs::create_dir_all(&dir).map_err(|e| persist_err("create snapshot dir", &e))?;
    let path = dir.join("state.json");
    let tmp = dir.join("state.json.tmp");
    let data = serde_json::to_vec_pretty(state)
        .map_err(|e| LifecycleError::StatePersist(format!("serialize snapshot: {e}")))?;
    fs::write(&tmp, data).map_err(|e| persist_err("write snapshot temp", &e))?;
    fs::rename(&tmp, &path).map_err(|e| persist_err("replace snapshot", &e))?;
    Ok(path)
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

pub(crate) fn plan_state_path(workspace: &Path, plan_id: &PlanId) -> PathBuf {
    plan_state_dir(workspace).join(format!("plan-{}.state.json", plan_id.as_str()))
}

pub(crate) fn plan_lock_path(workspace: &Path, plan_id: &PlanId) -> PathBuf {
    plan_state_dir(workspace).join(format!("plan-{}.lock", plan_id.as_str()))
}

fn plan_state_dir(workspace: &Path) -> PathBuf {
    crate::model::paths::runtime_dir(workspace).join("orchestrator")
}

pub(crate) fn read_plan_state(path: &Path) -> Result<PlanState, LifecycleError> {
    let data = fs::read_to_string(path)
        .map_err(|e| LifecycleError::InvalidPlan(format!("plan not found: {e}")))?;
    serde_json::from_str(&data)
        .map_err(|e| LifecycleError::InvalidPlan(format!("invalid plan state: {e}")))
}

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
