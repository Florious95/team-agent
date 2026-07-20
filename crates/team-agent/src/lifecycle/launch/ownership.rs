use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::lifecycle::*;
use crate::model::enums::{AuthMode, DisplayBackend, PaneLiveness, Provider, ProviderEffort};
use crate::model::ids::AgentId;
use crate::model::permissions::{self, AgentPermissionInput};
use crate::model::yaml::{self, Value};
use crate::state::persist::load_runtime_state;
use crate::transport::{PaneId, SessionName, Target, Transport, WindowName};

use crate::lifecycle::lock::{acquire_agent_lifecycle_lock, LifecycleLockRequest};

use super::*;

pub(crate) fn ensure_owner_allowed(workspace: &Path) -> Result<(), LifecycleError> {
    let state = crate::state::persist::load_runtime_state(workspace)
        .map_err(|e| LifecycleError::StatePersist(e.to_string()))?;
    ensure_owner_allowed_for_state(&state, None)
}

pub(crate) fn ensure_owner_allowed_for_state(
    state: &serde_json::Value,
    target_role: Option<&AgentId>,
) -> Result<(), LifecycleError> {
    struct NoopLiveness;
    impl crate::state::owner_gate::PaneLivenessProbe for NoopLiveness {
        fn liveness(&self, _pane_id: &str) -> crate::model::enums::PaneLiveness {
            crate::model::enums::PaneLiveness::Live
        }
    }

    let target_team = crate::state::projection::team_state_key(state);
    if caller_is_target_role_in_team(&target_team, target_role) {
        return Ok(());
    }
    let caller = crate::state::identity::caller_identity_from_env(
        Some(state),
        &crate::state::identity::SystemEnv,
        Some(&target_team),
        None,
    )
    .map_err(|e| LifecycleError::StatePersist(e.to_string()))?;
    if let Some(refusal) =
        crate::state::owner_gate::check_team_owner(state, &caller, false, &NoopLiveness)
    {
        return Err(LifecycleError::OwnerRefused(refusal.to_string()));
    }
    Ok(())
}

pub(super) fn caller_is_target_role_in_team(
    target_team: &str,
    target_role: Option<&AgentId>,
) -> bool {
    let Some(target_role) = target_role else {
        return false;
    };
    std::env::var("TEAM_AGENT_ID").ok().as_deref() == Some(target_role.as_str())
        && std::env::var("TEAM_AGENT_TEAM_ID").ok().as_deref() == Some(target_team)
}

pub(crate) fn state_path(workspace: &Path) -> std::path::PathBuf {
    crate::state::persist::runtime_state_path(workspace)
}
