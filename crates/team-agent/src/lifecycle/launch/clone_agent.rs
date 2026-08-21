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
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.is_empty())?;
    let backing = agent
        .get("rollout_path")
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.is_empty())
        .map(std::path::PathBuf::from)?;
    let distinct = source_session_id.is_none_or(|source| source.as_str() != session);
    (distinct && backing.is_file()).then(|| (SessionId::new(session), backing))
}
