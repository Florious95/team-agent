use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::lifecycle::{ResetAgentOutcome, ResetRefusal};
use crate::model::ids::{AgentId, TeamKey};
use crate::model::yaml::{self, Value as YamlValue};

use super::super::helpers::{enum_value, object_fields, tool_runtime_error};
use super::super::{ToolOk, ToolResult};

pub(crate) fn stop_agent(
    workspace: &Path,
    owner_team: Option<&TeamKey>,
    agent_id: &str,
) -> ToolResult {
    let lifecycle_workspace = lifecycle_workspace(workspace, owner_team, true)?;
    let report = crate::lifecycle::stop_agent(
        &lifecycle_workspace,
        &AgentId::new(agent_id),
        owner_team.map(TeamKey::as_str),
    )
    .map_err(tool_runtime_error)?;
    Ok(ToolOk {
        fields: object_fields(serde_json::json!({
            "ok": true,
            "agent_id": report.agent_id.as_str(),
            "status": "stopped",
            "stopped": report.stopped,
            "target": report.target,
            "display_closed": report.display_closed,
            "state_file": report.state_file.to_string_lossy().to_string(),
        })),
    })
}

pub(crate) fn reset_agent(
    workspace: &Path,
    owner_team: Option<&TeamKey>,
    agent_id: &str,
    discard_session: bool,
) -> ToolResult {
    if !discard_session {
        return Ok(ToolOk {
            fields: object_fields(serde_json::json!({
                "ok": false,
                "agent_id": agent_id,
                "status": "refused",
                "reason": "discard_session_required",
            })),
        });
    }
    emit_newer_daemon_preserved_if_present(workspace)?;
    let lifecycle_workspace = lifecycle_workspace(workspace, owner_team, true)?;
    match crate::lifecycle::reset_agent(
        &lifecycle_workspace,
        &AgentId::new(agent_id),
        discard_session,
        false,
        owner_team.map(TeamKey::as_str),
    )
    .map_err(tool_runtime_error)?
    {
        ResetAgentOutcome::Reset {
            env,
            start_mode,
            discarded_session_id,
            session_id,
            new_session_id,
            capture_state,
            reset_proof,
            weak_reset_warning,
        } => Ok(ToolOk {
            fields: object_fields(serde_json::json!({
                "ok": true,
                "agent_id": env.agent_id.as_str(),
                "status": "reset",
                "state_file": env.state_file.to_string_lossy().to_string(),
                "coordinator_started": env.coordinator_started,
                "start_mode": enum_value(start_mode),
                "discarded_session_id": discarded_session_id.as_ref().map(|id| id.as_str()),
                "session_id": session_id.as_ref().map(|id| id.as_str()),
                "new_session_id": new_session_id.as_ref().map(|id| id.as_str()),
                "capture_state": match capture_state.as_str() {
                    "captured" => "captured",
                    "attribution_ambiguous" => "attribution_ambiguous",
                    _ => "transcript_missing",
                },
                "reset_proof": if reset_proof == "weak" { "weak" } else { "strong" },
                "weak_reset_warning": weak_reset_warning,
            })),
        }),
        ResetAgentOutcome::Refused { reason } => Ok(ToolOk {
            fields: object_fields(serde_json::json!({
                "ok": false,
                "agent_id": agent_id,
                "status": "refused",
                "reason": reset_refusal_reason(reason),
            })),
        }),
    }
}

pub(crate) fn fork_agent(
    workspace: &Path,
    owner_team: Option<&TeamKey>,
    source_agent_id: &str,
    as_agent_id: &str,
    label: Option<&str>,
) -> ToolResult {
    emit_newer_daemon_preserved_if_present(workspace)?;
    let lifecycle_workspace = lifecycle_workspace(workspace, owner_team, false)?;
    // operations.py:315 — the label becomes the forked agent's role.
    let report = crate::lifecycle::launch::fork_agent(
        &lifecycle_workspace,
        &AgentId::new(source_agent_id),
        &AgentId::new(as_agent_id),
        label,
        false,
        owner_team.map(TeamKey::as_str),
    )
    .map_err(tool_runtime_error)?;
    Ok(ToolOk {
        fields: object_fields(serde_json::json!({
            "ok": true,
            "status": "forked",
            "source_agent_id": report.source_agent_id.as_str(),
            "agent_id": report.new_agent_id.as_str(),
            "new_agent_id": report.new_agent_id.as_str(),
            "state_file": report.env.state_file.to_string_lossy().to_string(),
            "coordinator_started": report.env.coordinator_started,
            "session_id": report.session_id.as_ref().map(|session| session.as_str()),
        })),
    })
}

pub(crate) fn clone_agent(
    workspace: &Path,
    owner_team: Option<&TeamKey>,
    source_agent_id: &str,
    as_agent_id: &str,
    label: Option<&str>,
) -> ToolResult {
    emit_newer_daemon_preserved_if_present(workspace)?;
    let lifecycle_workspace = lifecycle_workspace(workspace, owner_team, false)?;
    let report = crate::lifecycle::launch::clone_agent(
        &lifecycle_workspace,
        &AgentId::new(source_agent_id),
        &AgentId::new(as_agent_id),
        label,
        false,
        owner_team.map(TeamKey::as_str),
    )
    .map_err(tool_runtime_error)?;
    Ok(ToolOk {
        fields: object_fields(serde_json::json!({
            "ok": true,
            "status": "cloned",
            "source_agent_id": report.source_agent_id.as_str(),
            "agent_id": report.new_agent_id.as_str(),
            "new_agent_id": report.new_agent_id.as_str(),
            "state_file": report.env.state_file.to_string_lossy().to_string(),
            "coordinator_started": report.env.coordinator_started,
            "session_id": report.session_id.as_str(),
            "new_session_id": report.session_id.as_str(),
            "backing_path": report.backing_path.to_string_lossy().to_string(),
        })),
    })
}

fn reset_refusal_reason(reason: ResetRefusal) -> Value {
    match reason {
        ResetRefusal::DiscardSessionRequired => {
            Value::String("discard_session_required".to_string())
        }
    }
}

fn emit_newer_daemon_preserved_if_present(workspace: &Path) -> Result<(), super::super::ToolError> {
    let workspace_path = crate::coordinator::WorkspacePath::new(workspace.to_path_buf());
    let health = crate::coordinator::coordinator_health(&workspace_path);
    if !(health.service_available
        && !health.binary_identity_ok
        && matches!(
            health.binary_identity_relation,
            crate::coordinator::CoordinatorBinaryIdentityRelation::DaemonNewerThanCaller
        ))
    {
        return Ok(());
    }
    crate::event_log::EventLog::new(workspace)
        .write(
            "coordinator.newer_daemon_preserved",
            serde_json::json!({
                "pid": health.pid.map(|pid| pid.get()),
                "binary_identity_relation": health.binary_identity_relation.as_str(),
                "reason": "daemon_newer_than_caller",
                "daemon_binary_path": health
                    .metadata
                    .as_ref()
                    .and_then(|metadata| metadata.binary_path.clone()),
                "daemon_binary_version": health
                    .metadata
                    .as_ref()
                    .and_then(|metadata| metadata.binary_version.clone()),
                "caller_binary_path": health.current_binary_identity.binary_path,
                "caller_binary_version": health.current_binary_identity.binary_version,
            }),
        )
        .map(|_| ())
        .map_err(tool_runtime_error)
}

fn lifecycle_workspace(
    workspace: &Path,
    owner_team: Option<&TeamKey>,
    prepare_state: bool,
) -> Result<PathBuf, super::super::ToolError> {
    let team = owner_team.map(TeamKey::as_str);
    if let Ok(raw_state) = load_local_runtime_state(workspace) {
        if let Some(path) = state_spec_workspace(&raw_state, team) {
            return Ok(path);
        }
        if raw_state.get("agents").is_some() || raw_state.get("teams").is_some() {
            return materialize_mcp_lifecycle_spec(workspace, raw_state, team, prepare_state);
        }
    }
    if let Ok(selected) = crate::state::selector::resolve_active_team(
        workspace,
        team,
        crate::state::selector::SelectorMode::RequireSpec,
    ) {
        if let Some(path) = selected.spec_workspace {
            return Ok(path);
        }
    }
    let state = crate::state::projection::select_runtime_state(workspace, team)
        .map_err(tool_runtime_error)?;
    if let Some(path) = state_spec_workspace(&state, team) {
        return Ok(path);
    }
    Ok(workspace.to_path_buf())
}

fn state_spec_workspace(state: &Value, team: Option<&str>) -> Option<PathBuf> {
    if let Some(team) = team {
        if let Some(entry) = state
            .get("teams")
            .and_then(Value::as_object)
            .and_then(|teams| teams.get(team))
        {
            if let Some(path) = state_spec_workspace_from_entry(entry) {
                return Some(path);
            }
        }
    }
    state_spec_workspace_from_entry(state).or_else(|| {
        let teams = state.get("teams").and_then(Value::as_object)?;
        if teams.len() == 1 {
            teams
                .values()
                .next()
                .and_then(state_spec_workspace_from_entry)
        } else {
            None
        }
    })
}

fn state_spec_workspace_from_entry(state: &Value) -> Option<PathBuf> {
    state
        .get("spec_path")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .and_then(|s| Path::new(s).parent().map(Path::to_path_buf))
        .filter(|p| p.join("team.spec.yaml").exists())
        .or_else(|| {
            state
                .get("team_dir")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(PathBuf::from)
                .filter(|p| p.join("team.spec.yaml").exists())
        })
}

fn load_local_runtime_state(workspace: &Path) -> Result<Value, super::super::ToolError> {
    let path = crate::state::persist::runtime_state_path(workspace);
    match crate::state::repository::StateRepository::new(workspace)
        .load_workspace_if_exists_without_migrations()
    {
        Ok(Some(state)) => Ok(state),
        Ok(None) => Err(tool_runtime_error(format!(
            "read local runtime state {}: {}",
            path.display(),
            std::io::Error::from(std::io::ErrorKind::NotFound)
        ))),
        Err(crate::state::StateError::Json(error)) => Err(tool_runtime_error(format!(
            "parse local runtime state {}: {error}",
            path.display()
        ))),
        Err(crate::state::StateError::Io(error)) => Err(tool_runtime_error(format!(
            "read local runtime state {}: {error}",
            path.display()
        ))),
        Err(error) => Err(tool_runtime_error(format!(
            "read local runtime state {}: {error}",
            path.display()
        ))),
    }
}

fn materialize_mcp_lifecycle_spec(
    workspace: &Path,
    mut state: Value,
    team: Option<&str>,
    prepare_state: bool,
) -> Result<PathBuf, super::super::ToolError> {
    let team_name = team
        .filter(|s| !s.is_empty())
        .or_else(|| state.get("active_team_key").and_then(Value::as_str))
        .unwrap_or("team");
    let team_name = team_name.to_string();
    let Some(agents) = selected_agents(&state, team) else {
        return Ok(workspace.to_path_buf());
    };
    let mut agent_items = Vec::new();
    let top_agents = state.get("agents").and_then(Value::as_object);
    for (agent_id, agent_state) in agents {
        let provider = agent_state
            .get("provider")
            .and_then(Value::as_str)
            .or_else(|| {
                top_agents
                    .and_then(|all| all.get(agent_id))
                    .and_then(|agent| agent.get("provider"))
                    .and_then(Value::as_str)
            })
            .unwrap_or("fake");
        agent_items.push(YamlValue::Map(vec![
            ("id".to_string(), YamlValue::Str(agent_id.clone())),
            ("provider".to_string(), YamlValue::Str(provider.to_string())),
            ("role".to_string(), YamlValue::Str("Worker".to_string())),
        ]));
    }
    if agent_items.is_empty() {
        return Ok(workspace.to_path_buf());
    }
    let spec = YamlValue::Map(vec![
        (
            "team".to_string(),
            YamlValue::Map(vec![
                ("name".to_string(), YamlValue::Str(team_name.clone())),
                (
                    "objective".to_string(),
                    YamlValue::Str("MCP lifecycle state-backed team".to_string()),
                ),
            ]),
        ),
        (
            "leader".to_string(),
            YamlValue::Map(vec![(
                "provider".to_string(),
                YamlValue::Str("codex".to_string()),
            )]),
        ),
        ("agents".to_string(), YamlValue::List(agent_items)),
    ]);
    let spec_workspace = workspace.join(".team").join(&team_name);
    std::fs::create_dir_all(&spec_workspace).map_err(|e| {
        tool_runtime_error(format!(
            "create MCP lifecycle spec dir {}: {e}",
            spec_workspace.display()
        ))
    })?;
    let spec_path = spec_workspace.join("team.spec.yaml");
    std::fs::write(&spec_path, yaml::dumps(&spec)).map_err(|e| {
        tool_runtime_error(format!(
            "write MCP lifecycle spec {}: {e}",
            spec_path.display()
        ))
    })?;
    if prepare_state {
        prepare_selected_team_state(
            workspace,
            &mut state,
            &team_name,
            &spec_workspace,
            &spec_path,
        )?;
    }
    Ok(spec_workspace)
}

fn selected_agents<'a>(
    state: &'a Value,
    team: Option<&str>,
) -> Option<&'a serde_json::Map<String, Value>> {
    team.and_then(|team| {
        state
            .get("teams")
            .and_then(Value::as_object)
            .and_then(|teams| teams.get(team))
            .and_then(|entry| entry.get("agents"))
            .and_then(Value::as_object)
    })
    .or_else(|| state.get("agents").and_then(Value::as_object))
}

fn prepare_selected_team_state(
    workspace: &Path,
    state: &mut Value,
    team: &str,
    spec_workspace: &Path,
    spec_path: &Path,
) -> Result<(), super::super::ToolError> {
    let Some(root) = state.as_object_mut() else {
        return Ok(());
    };
    let top_session_name = root.get("session_name").cloned();
    let top_leader_receiver = root.get("leader_receiver").cloned();
    let top_agents = root.get("agents").and_then(Value::as_object).cloned();
    let teams = root
        .entry("teams".to_string())
        .or_insert_with(|| Value::Object(serde_json::Map::new()));
    let Some(teams) = teams.as_object_mut() else {
        return Ok(());
    };
    let entry = teams
        .entry(team.to_string())
        .or_insert_with(|| Value::Object(serde_json::Map::new()));
    let Some(entry) = entry.as_object_mut() else {
        return Ok(());
    };
    if let Some(value) = top_session_name {
        entry.entry("session_name".to_string()).or_insert(value);
    }
    if let Some(value) = top_leader_receiver {
        entry.entry("leader_receiver".to_string()).or_insert(value);
    }
    entry.insert(
        "team_dir".to_string(),
        Value::String(spec_workspace.to_string_lossy().to_string()),
    );
    entry.insert(
        "spec_path".to_string(),
        Value::String(spec_path.to_string_lossy().to_string()),
    );
    if let Some(top_agents) = top_agents {
        if let Some(team_agents) = entry.get_mut("agents").and_then(Value::as_object_mut) {
            for (agent_id, top_agent) in top_agents {
                let Some(team_agent) = team_agents
                    .get_mut(&agent_id)
                    .and_then(Value::as_object_mut)
                else {
                    continue;
                };
                let Some(top_agent) = top_agent.as_object() else {
                    continue;
                };
                for (key, value) in top_agent {
                    team_agent
                        .entry(key.clone())
                        .or_insert_with(|| value.clone());
                }
            }
        }
    }
    crate::state::repository::StateRepository::new(workspace)
        .save(
            crate::state::repository::StateWriteIntent::McpLifecycleAgentOps {
                team_key: Some(team),
            },
            state,
        )
        .map_err(|e| {
            tool_runtime_error(format!(
                "save MCP lifecycle scoped state {}: {e}",
                workspace.display()
            ))
        })
}
