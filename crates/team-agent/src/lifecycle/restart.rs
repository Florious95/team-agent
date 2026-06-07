//! lifecycle::restart —— 单 worker 起/停/重置/删 + 整队 Route B 重建 + plan halt/status。

use std::path::Path;
use std::collections::BTreeMap;

use crate::model::enums::{AuthMode, Provider};
use crate::model::ids::AgentId;
use crate::model::yaml::{self, Value as YamlValue};
use crate::provider::{RolloutPath, SessionId};
use crate::transport::{SessionName, Target, WindowName};

use super::*;

// ── lifecycle::agent —— 单 worker 起/停/重置/增/fork/删(全部 owner-gate 优先 + 回滚)─

struct LifecyclePaths {
    run_workspace: std::path::PathBuf,
    spec_workspace: std::path::PathBuf,
}

struct LifecyclePathRefs<'a> {
    run_workspace: &'a Path,
    spec_workspace: &'a Path,
}

mod agent;
mod common;
mod orchestrator;
mod rebuild;
mod remove;
mod selection;
mod team_state;

pub use agent::{reset_agent, reset_agent_with_transport, start_agent, start_agent_with_transport, stop_agent, stop_agent_with_transport};
pub(crate) use agent::start_agent_at_paths;
pub(crate) use common::refresh_missing_provider_sessions;
pub use orchestrator::{halt_plan, plan_status};
pub use rebuild::{restart, restart_candidates, restart_with_transport, select_restart_state};
pub use remove::{remove_agent, remove_agent_with_transport};
pub use selection::{classify_first_send_at, classify_restart_plan, decide_start_mode, python_type_name};
pub(crate) use team_state::write_team_state;

pub(crate) fn lifecycle_run_workspace(workspace: &Path) -> Result<std::path::PathBuf, LifecycleError> {
    crate::model::paths::canonical_run_workspace(workspace)
        .map_err(|e| LifecycleError::StatePersist(e.to_string()))
}

fn lifecycle_paths(workspace: &Path, team: Option<&str>) -> Result<LifecyclePaths, LifecycleError> {
    if input_has_no_local_team_context(workspace) {
        return Err(LifecycleError::TeamSelect(format!(
            "active team spec not found: input_workspace={} expected_spec_path={}",
            workspace.display(),
            workspace.join("team.spec.yaml").display()
        )));
    }
    let selected = crate::state::selector::resolve_active_team(
        workspace,
        team,
        crate::state::selector::SelectorMode::RequireSpec,
    )
    .map_err(|e| LifecycleError::TeamSelect(e.to_string()))?;
    let spec_workspace = selected_state_spec_workspace(&selected.state)
        .or(selected.spec_workspace)
        .ok_or_else(|| LifecycleError::TeamSelect("active team spec workspace not found".to_string()))?;
    Ok(LifecyclePaths {
        run_workspace: selected.run_workspace,
        spec_workspace,
    })
}

pub(crate) fn input_has_no_local_team_context(workspace: &Path) -> bool {
    !workspace.join("team.spec.yaml").exists()
        && !workspace.join(".team").exists()
        && !crate::state::persist::runtime_state_path(workspace).exists()
        && workspace.file_name().and_then(|s| s.to_str()) != Some(".team")
        && workspace
            .parent()
            .and_then(|p| p.file_name())
            .and_then(|s| s.to_str())
            != Some(".team")
}

fn selected_state_spec_workspace(state: &serde_json::Value) -> Option<std::path::PathBuf> {
    state
        .get("spec_path")
        .and_then(serde_json::Value::as_str)
        .filter(|s| !s.is_empty())
        .and_then(|s| Path::new(s).parent().map(Path::to_path_buf))
        .or_else(|| {
            state
                .get("team_dir")
                .and_then(serde_json::Value::as_str)
                .filter(|s| !s.is_empty())
                .map(std::path::PathBuf::from)
        })
}
