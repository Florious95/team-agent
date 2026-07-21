use crate::lifecycle::*;
use crate::model::ids::AgentId;
use crate::provider::SessionId;
use std::path::Path;

use super::*;

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
    let spec_path = selected
        .spec_path
        .as_ref()
        .ok_or_else(|| LifecycleError::TeamSelect("active team spec not found".to_string()))?;
    let spec = crate::model::yaml::loads(
        &std::fs::read_to_string(spec_path)
            .map_err(|error| LifecycleError::Compile(error.to_string()))?,
    )
    .map_err(|error| LifecycleError::Compile(error.to_string()))?;
    clamp_materialized_role_to_leader(materialized.path(), &spec)?;
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
