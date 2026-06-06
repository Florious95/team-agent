//! Active team workspace selector.

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::model::paths::{canonical_run_workspace, team_workspace};
use crate::state::persist::{load_runtime_state, runtime_state_path};
use crate::state::projection::{select_runtime_state, team_state_key};
use crate::state::StateError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectorMode {
    RuntimeOnly,
    RequireSpec,
}

#[derive(Debug, Clone)]
pub struct SelectedTeam {
    pub run_workspace: PathBuf,
    pub team_key: String,
    pub state: Value,
    pub spec_workspace: Option<PathBuf>,
    pub spec_path: Option<PathBuf>,
}

pub fn resolve_active_team(
    input: &Path,
    team: Option<&str>,
    mode: SelectorMode,
) -> Result<SelectedTeam, StateError> {
    let explicit_spec = input.join("team.spec.yaml");
    let (run_workspace, state, spec_workspace) = if explicit_spec.exists() {
        let team_run = team_workspace(input).map_err(|e| StateError::TeamSelect(e.to_string()))?;
        let run = if runtime_state_path(input).exists() || !runtime_state_path(&team_run).exists() {
            input.to_path_buf()
        } else {
            team_run
        };
        let state = select_runtime_state(&run, team).or_else(|_| load_runtime_state(&run))?;
        (run, state, Some(input.to_path_buf()))
    } else {
        let run = canonical_run_workspace(input)
            .map_err(|e| StateError::TeamSelect(e.to_string()))?;
        if !input.exists()
            && !runtime_state_path(&run).exists()
            && !run.join(".team").exists()
            && !run.join("team.spec.yaml").exists()
        {
            return Err(StateError::TeamSelect(format!(
                "invalid workspace: {}",
                input.display()
            )));
        }
        let state = select_runtime_state(&run, team).or_else(|_| load_runtime_state(&run))?;
        let spec_workspace = spec_workspace_from_state(&state)
            .or_else(|| run.join("team.spec.yaml").exists().then(|| run.clone()));
        (run, state, spec_workspace)
    };

    let spec_path = spec_workspace.as_ref().map(|workspace| workspace.join("team.spec.yaml"));
    if matches!(mode, SelectorMode::RequireSpec) && !spec_path.as_ref().is_some_and(|path| path.exists()) {
        let expected = spec_path
            .as_ref()
            .cloned()
            .unwrap_or_else(|| run_workspace.join("team.spec.yaml"));
        return Err(StateError::TeamSelect(format!(
            "active team spec not found: input_workspace={} run_workspace={} team_key={} expected_spec_path={} hint=run quick-start or pass --team/--workspace <teamdir>",
            input.display(),
            run_workspace.display(),
            selected_team_key(&state, team),
            expected.display()
        )));
    }

    Ok(SelectedTeam {
        run_workspace,
        team_key: selected_team_key(&state, team),
        state,
        spec_workspace,
        spec_path,
    })
}

fn spec_workspace_from_state(state: &Value) -> Option<PathBuf> {
    state
        .get("spec_path")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .and_then(|s| Path::new(s).parent().map(Path::to_path_buf))
        .or_else(|| {
            state
                .get("team_dir")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(PathBuf::from)
        })
}

fn selected_team_key(state: &Value, team: Option<&str>) -> String {
    team.filter(|s| !s.is_empty())
        .map(ToString::to_string)
        .or_else(|| state.get("active_team_key").and_then(Value::as_str).filter(|s| !s.is_empty()).map(ToString::to_string))
        .unwrap_or_else(|| team_state_key(state))
}
