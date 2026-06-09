use std::path::Path;

use serde_json::Value;

use crate::model::ids::TeamKey;

use super::super::helpers::{ensure_object, object_fields, tool_runtime_error};
use super::super::{ToolOk, ToolResult};

pub(crate) fn update_state(
    workspace: &Path,
    owner_team: Option<&TeamKey>,
    note: &str,
) -> ToolResult {
    let selected = match crate::state::selector::resolve_active_team(
        workspace,
        owner_team.map(TeamKey::as_str),
        crate::state::selector::SelectorMode::RequireSpec,
    ) {
        Ok(selected) => selected,
        Err(err) if is_missing_active_spec(&err) => {
            return update_state_without_spec(workspace, owner_team, note);
        }
        Err(err) => return Err(tool_runtime_error(err)),
    };
    let mut state = selected.state;
    ensure_object(&mut state);
    append_note(&mut state, note);
    crate::state::projection::save_team_scoped_state(&selected.run_workspace, &state)
        .map_err(tool_runtime_error)?;
    let spec_path = selected
        .spec_path
        .ok_or_else(|| tool_runtime_error("active team spec not found for update_state"))?;
    let spec_workspace = spec_path.parent().ok_or_else(|| {
        tool_runtime_error(format!("active team spec has no parent: {}", spec_path.display()))
    })?;
    let spec_text = std::fs::read_to_string(&spec_path).map_err(tool_runtime_error)?;
    let spec = crate::model::yaml::loads(&spec_text).map_err(tool_runtime_error)?;
    let path = crate::lifecycle::restart::write_team_state(spec_workspace, &spec, &state)
        .map_err(tool_runtime_error)?;
    Ok(update_state_ok(path))
}

fn update_state_without_spec(
    workspace: &Path,
    owner_team: Option<&TeamKey>,
    note: &str,
) -> ToolResult {
    let selected = crate::state::selector::resolve_active_team(
        workspace,
        owner_team.map(TeamKey::as_str),
        crate::state::selector::SelectorMode::RuntimeOnly,
    )
    .map_err(tool_runtime_error)?;
    let mut state = selected.state;
    ensure_object(&mut state);
    seed_legacy_team_key(&mut state, &selected.run_workspace, &selected.team_key);
    append_note(&mut state, note);
    crate::state::projection::save_team_scoped_state(&selected.run_workspace, &state)
        .map_err(tool_runtime_error)?;
    let path = crate::lifecycle::restart::write_team_state(
        &selected.run_workspace,
        &crate::model::yaml::Value::Null,
        &state,
    )
    .map_err(tool_runtime_error)?;
    Ok(update_state_ok(path))
}

fn append_note(state: &mut Value, note: &str) {
    if let Some(obj) = state.as_object_mut() {
        let notes = obj
            .entry("notes".to_string())
            .or_insert_with(|| Value::Array(Vec::new()));
        if !notes.is_array() {
            *notes = Value::Array(Vec::new());
        }
        if let Some(items) = notes.as_array_mut() {
            items.push(Value::String(note.to_string()));
        }
    }
}

fn seed_legacy_team_key(state: &mut Value, run_workspace: &Path, team_key: &str) {
    if state.get("team_dir").and_then(Value::as_str).is_some()
        || state.get("spec_path").and_then(Value::as_str).is_some()
        || state.get("session_name").and_then(Value::as_str).is_some()
    {
        return;
    }
    if let Some(obj) = state.as_object_mut() {
        obj.insert(
            "team_dir".to_string(),
            Value::String(
                run_workspace
                    .join(".team")
                    .join(team_key)
                    .to_string_lossy()
                    .to_string(),
            ),
        );
    }
}

fn update_state_ok(path: std::path::PathBuf) -> ToolOk {
    let mut fields = serde_json::Map::new();
    fields.insert("ok".to_string(), Value::Bool(true));
    fields.insert(
        "state_file".to_string(),
        Value::String(path.to_string_lossy().to_string()),
    );
    ToolOk { fields }
}

fn is_missing_active_spec(err: &crate::state::StateError) -> bool {
    matches!(
        err,
        crate::state::StateError::TeamSelect(message)
            if message.starts_with("active team spec not found:")
    )
}

pub(crate) fn get_team_status(
    workspace: &Path,
    owner_team: Option<&TeamKey>,
) -> ToolResult {
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
