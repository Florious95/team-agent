//! ---
//! purpose: 零上下文重读角色文件再起一席，不继承源会话
//! contract:
//!   provides:
//!     - name: clone_agent
//!       what: 用源席最新角色文件 add-agent，保留源工具集
//! boundary:
//!   - 不走 fork 的窗口内命令
//!   - 不把 clone 与 fork 合成一条路径
//! maturity: wired
//! ---
use crate::lifecycle::*;
use crate::model::ids::AgentId;
use crate::provider::SessionId;
use std::path::Path;

use super::*;

/// ---
/// purpose: 以源席最新角色文件另起一席，不继承源会话
/// params:
///   source_agent_id: 源席位，只借它的角色文件与工具集
///   as_agent_id: 新席位名
///   label: 新席位的角色标签
/// returns: clone 报告
/// errors: 选不到 team 返回 TeamSelect，源席不存在返回 RequirementUnmet，角色物化与加席错误透传
/// ---
pub fn clone_agent(
    workspace: &Path,
    source_agent_id: &AgentId,
    as_agent_id: &AgentId,
    label: Option<&str>,
    open_display: bool,
    team: Option<&str>,
) -> Result<CloneAgentReport, LifecycleError> {
    let selected = crate::state::selector::resolve_active_team(
        workspace,
        team,
        crate::state::selector::SelectorMode::RequireSpec,
    )
    .map_err(|error| LifecycleError::TeamSelect(error.to_string()))?;
    ensure_owner_allowed_for_state(&selected.state, Some(source_agent_id))?;
    let source_agent = selected
        .state
        .get("agents")
        .and_then(|agents| agents.get(source_agent_id.as_str()))
        .ok_or_else(|| {
            LifecycleError::RequirementUnmet(format!("unknown worker agent id: {source_agent_id}"))
        })?;
    let source_session_id = source_agent
        .get("session_id")
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.is_empty())
        .map(SessionId::new);
    let mut materialized = materialize_latest_role(
        &selected.run_workspace,
        &selected.team_dir,
        &selected.state,
        source_agent_id,
        as_agent_id,
        label,
    )?;
    // The materialized role carries the source role's DECLARED tools verbatim —
    // a clone must preserve the source seat's full tools set (add-agent does
    // the same; no leader-ceiling clamp). See role_source.rs for the removed
    // clamp_materialized_role_to_leader.
    let added = add_agent(
        &selected.run_workspace,
        as_agent_id,
        materialized.path(),
        open_display,
        Some(selected.team_key.as_str()),
    )?;
    let verified = read_agent_session(
        &selected.run_workspace,
        selected.team_key.as_str(),
        as_agent_id,
        source_session_id.as_ref(),
    );
    if let Some((session_id, backing_path)) = verified.as_ref() {
        persist_clone_session(
            &selected.run_workspace,
            selected.team_key.as_str(),
            as_agent_id,
            session_id,
            backing_path,
        )?;
    }
    let (session_id, backing_path, backing_state) = match verified {
        Some((session_id, backing_path)) => (
            Some(session_id),
            Some(backing_path),
            CloneBackingState::Verified,
        ),
        None => (None, None, CloneBackingState::PendingFirstTurn),
    };
    materialized.keep();
    Ok(CloneAgentReport {
        source_agent_id: source_agent_id.clone(),
        new_agent_id: as_agent_id.clone(),
        env: added.env,
        session_id,
        backing_path,
        backing_state,
    })
}

fn read_agent_session(
    workspace: &Path,
    team_key: &str,
    agent_id: &AgentId,
    source_session_id: Option<&SessionId>,
) -> Option<(SessionId, std::path::PathBuf)> {
    let state = crate::state::projection::select_runtime_state(workspace, Some(team_key)).ok()?;
    let agent = state
        .get("agents")
        .and_then(|agents| agents.get(agent_id.as_str()))?;
    let session = agent
        .get("session_id")
        .or_else(|| agent.get("_pending_session_id"))
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.is_empty())?;
    let backing = agent
        .get("rollout_path")
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.is_empty())
        .map(std::path::PathBuf::from)
        .or_else(|| find_claude_backing(agent, session))?;
    let distinct = source_session_id.is_none_or(|source| source.as_str() != session);
    (distinct && backing.is_file()).then(|| (SessionId::new(session), backing))
}

fn find_claude_backing(agent: &serde_json::Value, session: &str) -> Option<std::path::PathBuf> {
    let root = agent
        .get("claude_projects_root")
        .and_then(serde_json::Value::as_str)
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .map(|home| std::path::PathBuf::from(home).join(".claude/projects"))
        })?;
    let target = format!("{session}.jsonl");
    let mut pending = vec![root];
    while let Some(dir) = pending.pop() {
        let entries = std::fs::read_dir(&dir).ok()?;
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else if path.file_name().and_then(|name| name.to_str()) == Some(target.as_str()) {
                return Some(path);
            }
        }
    }
    None
}

fn persist_clone_session(
    workspace: &Path,
    team_key: &str,
    agent_id: &AgentId,
    session_id: &SessionId,
    backing_path: &Path,
) -> Result<(), LifecycleError> {
    let _lock = acquire_agent_lifecycle_lock(LifecycleLockRequest {
        workspace,
        operation: "clone-session-bind",
        team: Some(team_key),
        agent_id: Some(agent_id),
    })?;
    let mut state = crate::state::projection::select_runtime_state(workspace, Some(team_key))
        .map_err(|error| LifecycleError::TeamSelect(error.to_string()))?;
    let Some(agent) = state
        .get_mut("agents")
        .and_then(serde_json::Value::as_object_mut)
        .and_then(|agents| agents.get_mut(agent_id.as_str()))
        .and_then(serde_json::Value::as_object_mut)
    else {
        return Ok(());
    };
    agent.insert(
        "session_id".to_string(),
        serde_json::json!(session_id.as_str()),
    );
    agent.insert(
        "rollout_path".to_string(),
        serde_json::json!(backing_path.to_string_lossy().to_string()),
    );
    agent.insert("captured_via".to_string(), serde_json::json!("clone-agent"));
    crate::lifecycle::launch::save_launched_team_state_for_key(
        workspace,
        &state,
        Some(team_key),
        Some(agent_id.as_str()),
    )
}
