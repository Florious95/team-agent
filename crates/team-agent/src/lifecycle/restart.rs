//! lifecycle::restart —— 单 worker 起/停/重置/删 + 整队 Route B 重建 + plan halt/status。

use std::collections::BTreeMap;
use std::path::Path;

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
pub mod preflight;
mod rebuild;
mod remove;
mod selection;
mod team_state;

pub(crate) use agent::start_agent_at_paths;
pub use agent::{
    reset_agent, reset_agent_with_transport, start_agent, start_agent_with_transport, stop_agent,
    stop_agent_with_transport,
};
pub(crate) use common::refresh_missing_provider_sessions;
pub(crate) use common::restart_required_missing_session_agent_ids;
pub(crate) use common::session_identity_probe_for_agent;
// 0.3.24 add-agent socket drift fix: state-aware tmux resolver shared with
// `lifecycle::launch::add_agent` / `fork_agent` so all three (restart / add / fork)
// route to the SAME tmux socket the live team uses.
pub(crate) use common::lifecycle_worker_tmux_backend_for_selected_state;
pub use orchestrator::{halt_plan, plan_status};
pub(crate) use rebuild::restart_with_transport_with_session_convergence_deadline;
pub use rebuild::{
    restart, restart_candidates, restart_with_session_convergence_deadline, restart_with_transport,
    restart_with_transport_with_readiness_deadline, select_restart_state,
};
pub use remove::{remove_agent, remove_agent_with_transport};
pub use selection::{
    classify_first_send_at, classify_restart_plan, decide_start_mode, python_type_name,
};
// Layer 2 (leader follow-up 2026-06-22): test-visible workspace-aware
// classification so lifecycle/tests/restart.rs can exercise the
// SessionBackingStoreMissing + checked_paths + RecoveryHint path
// end-to-end without spinning up a full restart.
pub(crate) use selection::classify_restart_plan_with_resume_validation;
pub(crate) use team_state::write_team_state;

pub(crate) fn lifecycle_run_workspace(
    workspace: &Path,
) -> Result<std::path::PathBuf, LifecycleError> {
    crate::model::paths::canonical_run_workspace(workspace)
        .map_err(|e| LifecycleError::StatePersist(e.to_string()))
}

fn lifecycle_paths(workspace: &Path, team: Option<&str>) -> Result<LifecyclePaths, LifecycleError> {
    // RED-2-STILL(P0):入口门在 canonical_run_workspace 解析后的路径上判(quick-start 的 .team 落
    // team_dir 父目录,raw team_dir 必 miss)。期望路径报解析后 runtime 落点,不指 raw team_dir。
    let resolved_ws = crate::model::paths::canonical_run_workspace(workspace)
        .map_err(|e| LifecycleError::StatePersist(e.to_string()))?;
    if input_has_no_local_team_context(&resolved_ws) {
        return Err(LifecycleError::TeamSelect(format!(
            "active team spec not found: input_workspace={} expected_runtime_dir={}",
            workspace.display(),
            crate::model::paths::runtime_dir(&resolved_ws).display()
        )));
    }
    let selected = crate::state::selector::resolve_active_team(
        workspace,
        team,
        crate::state::selector::SelectorMode::RequireSpec,
    )
    .map_err(|e| LifecycleError::TeamSelect(e.to_string()))?;
    // E42 (0.3.24 P0, double-spec deadlock): canonical-first. The selector at
    // state/selector.rs:71-74 already sets `selected.spec_workspace` to the
    // canonical runtime spec parent (.team/runtime/<key>/) whenever the
    // runtime spec exists; only when the runtime spec is missing does it fall
    // back to the legacy user-dir spec_workspace. Honoring that first stops
    // remove-agent / stop-agent / reset-agent / fork-agent from reading a
    // stale `state.spec_path` (legacy `.team/<key>/team.spec.yaml`) that
    // disagrees with what add-agent writes to canonical
    // (lifecycle/launch.rs:3576-3577). Pre-fix order let the legacy stale
    // spec win and remove-agent couldn't see agents add-agent had just
    // added — the macmini e46-probe deadlock truth source.
    let spec_workspace = selected
        .spec_workspace
        .clone()
        .or_else(|| selected_state_spec_workspace(&selected.state))
        .ok_or_else(|| {
            LifecycleError::TeamSelect("active team spec workspace not found".to_string())
        })?;
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
