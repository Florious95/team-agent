//! ---
//! purpose: MCP update_state selection, runtime persistence, and team_state rendering
//! contract:
//!   provides:
//!     - name: update_state
//!       what: appends a note and commits runtime state with its rendered state artifact
//!   depends:
//!     - crate::state::selector
//!     - crate::state::repository
//!     - crate::lifecycle::restart::write_team_state
//! boundary:
//!   - error context reports raw workspace, resolved workspace, state path, and OS cause
//!   - success reads back both artifacts; rejection preserves their previous bytes
//! maturity: wired
//! ---
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::Value;

use crate::model::ids::TeamKey;

use super::super::helpers::{ensure_object, object_fields, tool_runtime_error};
use super::super::{ToolOk, ToolResult};

static RENDER_SEQ: AtomicU64 = AtomicU64::new(0);

/// ---
/// purpose: append an MCP note to runtime state and render team_state.md
/// params: workspace and owner team select the canonical run workspace
/// returns: raw success fields including the rendered state_file path
/// errors: path-rich persistence errors preserve the raw and resolved targets
/// ---
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
    let spec_path = selected
        .spec_path
        .ok_or_else(|| tool_runtime_error("active team spec not found for update_state"))?;
    let spec_workspace = spec_path.parent().ok_or_else(|| {
        tool_runtime_error(format!(
            "active team spec has no parent: {}",
            spec_path.display()
        ))
    })?;
    let spec_text = std::fs::read_to_string(&spec_path).map_err(tool_runtime_error)?;
    let spec = crate::model::yaml::loads(&spec_text).map_err(tool_runtime_error)?;
    commit_update_state(
        workspace,
        &selected.run_workspace,
        spec_workspace,
        &selected.team_key,
        &spec,
        note,
        false,
    )
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
    commit_update_state(
        workspace,
        &selected.run_workspace,
        &selected.run_workspace,
        &selected.team_key,
        &crate::model::yaml::Value::Null,
        note,
        true,
    )
}

fn commit_update_state(
    raw_workspace: &Path,
    runtime_workspace: &Path,
    resolved_workspace: &Path,
    team_key: &str,
    spec: &crate::model::yaml::Value,
    note: &str,
    seed_legacy: bool,
) -> ToolResult {
    let state_file = state_file_path(resolved_workspace, spec);
    validate_state_file_target(raw_workspace, resolved_workspace, &state_file)?;
    crate::state::repository::StateRepository::new(runtime_workspace)
        .commit_update_state_artifact(
            crate::state::repository::StateWriteIntent::McpUpdateStateNote {
                team_key: Some(team_key),
            },
            &state_file,
            |state| mutate_update_note(state, runtime_workspace, team_key, note, seed_legacy),
            |state| {
                let projected = selected_state_for_render(state, team_key);
                render_state_file(runtime_workspace, spec, &projected)
            },
        )
        .map_err(|error| {
            state_write_error(raw_workspace, resolved_workspace, &state_file, error)
        })?;
    Ok(update_state_ok(state_file))
}

fn state_file_path(resolved_workspace: &Path, spec: &crate::model::yaml::Value) -> PathBuf {
    let relative = spec
        .get("context")
        .and_then(|value| value.get("state_file"))
        .and_then(crate::model::yaml::Value::as_str)
        .unwrap_or("team_state.md");
    resolved_workspace.join(relative)
}

fn validate_state_file_target(
    raw_workspace: &Path,
    resolved_workspace: &Path,
    state_file: &Path,
) -> Result<(), super::super::types::ToolError> {
    let relative = state_file
        .strip_prefix(resolved_workspace)
        .map_err(|error| state_write_error(raw_workspace, resolved_workspace, state_file, error))?;
    if relative.components().any(|component| {
        matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    }) {
        return Err(state_write_error(
            raw_workspace,
            resolved_workspace,
            state_file,
            "state file escapes resolved workspace",
        ));
    }
    let canonical_workspace = std::fs::canonicalize(resolved_workspace)
        .map_err(|error| state_write_error(raw_workspace, resolved_workspace, state_file, error))?;
    let mut ancestor = state_file.parent().ok_or_else(|| {
        state_write_error(
            raw_workspace,
            resolved_workspace,
            state_file,
            "state file has no parent",
        )
    })?;
    while !ancestor.exists() {
        ancestor = ancestor.parent().ok_or_else(|| {
            state_write_error(
                raw_workspace,
                resolved_workspace,
                state_file,
                "state file parent has no existing ancestor",
            )
        })?;
    }
    let canonical_ancestor = std::fs::canonicalize(ancestor)
        .map_err(|error| state_write_error(raw_workspace, resolved_workspace, state_file, error))?;
    if !canonical_ancestor.starts_with(&canonical_workspace) {
        return Err(state_write_error(
            raw_workspace,
            resolved_workspace,
            state_file,
            "state file parent escapes resolved workspace",
        ));
    }
    if state_file.exists() {
        let canonical_state = std::fs::canonicalize(state_file).map_err(|error| {
            state_write_error(raw_workspace, resolved_workspace, state_file, error)
        })?;
        if !canonical_state.starts_with(&canonical_workspace) {
            return Err(state_write_error(
                raw_workspace,
                resolved_workspace,
                state_file,
                "state file escapes resolved workspace",
            ));
        }
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(state_file)
            .map_err(|error| {
                state_write_error(raw_workspace, resolved_workspace, state_file, error)
            })?;
        #[cfg(test)]
        if std::env::var_os("TEAM_AGENT_TEST_UPDATE_STATE_FAIL_PREFLIGHT_AFTER_OPEN").is_some() {
            return Err(state_write_error(
                raw_workspace,
                resolved_workspace,
                state_file,
                std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "injected preflight failure after open",
                ),
            ));
        }
    }
    Ok(())
}

fn render_state_file(
    runtime_workspace: &Path,
    spec: &crate::model::yaml::Value,
    state: &Value,
) -> Result<Vec<u8>, crate::state::StateError> {
    let seq = RENDER_SEQ.fetch_add(1, Ordering::Relaxed);
    let staging_root = crate::model::paths::runtime_dir(runtime_workspace)
        .join(format!(".update-state-render-{}-{seq}", std::process::id()));
    std::fs::create_dir_all(&staging_root)?;
    let result = crate::lifecycle::restart::write_team_state(&staging_root, spec, state)
        .map_err(|error| crate::state::StateError::SaveFailed(error.to_string()))
        .and_then(|path| std::fs::read(path).map_err(crate::state::StateError::from));
    let _ = std::fs::remove_dir_all(&staging_root);
    result
}

fn mutate_update_note(
    state: &mut Value,
    runtime_workspace: &Path,
    team_key: &str,
    note: &str,
    seed_legacy: bool,
) {
    ensure_object(state);
    let has_team = state
        .get("teams")
        .and_then(Value::as_object)
        .is_some_and(|teams| teams.contains_key(team_key));
    if has_team {
        let root_is_target = crate::state::projection::team_state_key(state) == team_key;
        if root_is_target {
            append_note(state, note);
        }
        if let Some(team) = state
            .get_mut("teams")
            .and_then(Value::as_object_mut)
            .and_then(|teams| teams.get_mut(team_key))
        {
            ensure_object(team);
            append_note(team, note);
        }
    } else {
        if seed_legacy {
            seed_legacy_team_key(state, runtime_workspace, team_key);
        }
        append_note(state, note);
    }
}

fn selected_state_for_render(state: &Value, team_key: &str) -> Value {
    if state
        .get("teams")
        .and_then(Value::as_object)
        .is_some_and(|teams| teams.contains_key(team_key))
    {
        crate::state::projection::project_top_level_view(state, team_key)
    } else {
        state.clone()
    }
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

fn state_write_error(
    raw_workspace: &Path,
    resolved_workspace: &Path,
    state_file: &Path,
    cause: impl std::fmt::Display,
) -> super::super::types::ToolError {
    tool_runtime_error(format!(
        "raw={} resolved={} state={} cause={cause}",
        raw_workspace.display(),
        resolved_workspace.display(),
        state_file.display(),
    ))
}

fn update_state_ok(path: PathBuf) -> ToolOk {
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

/// ---
/// purpose: return status for the selected team in the canonical run workspace
/// params: workspace and owner team constrain runtime-state selection
/// returns: scoped status fields for the selected team
/// errors: selection and status failures are returned as tool errors
/// ---
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
