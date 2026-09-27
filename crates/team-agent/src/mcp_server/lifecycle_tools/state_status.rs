//!
use std::path::Path;

use serde_json::Value;

use crate::model::ids::TeamKey;

use super::super::helpers::{object_fields, tool_runtime_error};
use super::super::{ToolOk, ToolResult};

pub(crate) fn get_team_status(workspace: &Path, owner_team: Option<&TeamKey>) -> ToolResult {
    let selected = crate::state::selector::resolve_active_team(
        workspace,
        owner_team.map(TeamKey::as_str),
        crate::state::selector::SelectorMode::RuntimeOnly,
    )
    .map_err(tool_runtime_error)?;
    let status = crate::cli::status_port::status_scoped(
        &selected.run_workspace,
        &selected.state,
        Some(selected.team_key.as_str()),
        true,
        false,
    )
    .map_err(tool_runtime_error)?;
    let mut fields = object_fields(status);
    fields
        .entry("teams".to_string())
        .or_insert_with(|| selected_team_only(&selected.state, &selected.team_key));
    Ok(ToolOk { fields })
}

fn selected_team_only(state: &Value, team_key: &str) -> Value {
    let mut teams = serde_json::Map::new();
    if let Some(team) = state
        .get("teams")
        .and_then(Value::as_object)
        .and_then(|all| all.get(team_key))
    {
        teams.insert(team_key.to_string(), team.clone());
    }
    Value::Object(teams)
}
