//! unit-8 (Stage 3) — `lifecycle::launch::readiness` phase boundary.
//!
//! Dedicated home for coordinator-start + readiness-verdict computation.
//! Future commits migrate the inline phase fns at launch.rs:2928-2944
//! here.

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

pub(super) fn quick_start_worker_readiness(
    workspace: &Path,
    team_key: &str,
) -> QuickStartReadiness {
    let Ok(state) = load_runtime_state(workspace) else {
        return QuickStartReadiness::PendingToolLoad;
    };
    let team_state = state
        .get("teams")
        .and_then(serde_json::Value::as_object)
        .and_then(|teams| teams.get(team_key))
        .unwrap_or(&state);
    let Some(agents) = team_state
        .get("agents")
        .and_then(serde_json::Value::as_object)
    else {
        return QuickStartReadiness::PendingToolLoad;
    };
    let all_spawned = !agents.is_empty();
    let leader_receiver_attached = launched_team_receiver_is_attached(workspace, team_key);
    let all_attached_receiver = leader_receiver_attached;
    let mut unhealthy: Vec<String> = agents
        .iter()
        .filter_map(|(id, agent)| {
            let status = agent.get("status").and_then(serde_json::Value::as_str);
            match status {
                Some("running") => None,
                _ => Some(id.clone()),
            }
        })
        .collect();
    if !unhealthy.is_empty() {
        unhealthy.sort();
        unhealthy.dedup();
        QuickStartReadiness::Degraded {
            unhealthy_agents: unhealthy,
        }
    } else {
        let incomplete_agents =
            crate::session_capture::incomplete_interacted_resumable_agent_ids(team_state);
        let all_resumable_have_session = incomplete_agents.is_empty();
        let _readiness_ready = all_spawned && all_attached_receiver && all_resumable_have_session;
        QuickStartReadiness::PendingToolLoad
    }
}

pub(super) fn quick_start_session_capture_incomplete_agents(
    workspace: &Path,
    team_key: &str,
) -> Vec<String> {
    let Ok(state) = load_runtime_state(workspace) else {
        return Vec::new();
    };
    let team_state = state
        .get("teams")
        .and_then(serde_json::Value::as_object)
        .and_then(|teams| teams.get(team_key))
        .unwrap_or(&state);
    crate::session_capture::incomplete_interacted_resumable_agent_ids(team_state)
}

/// Host registry is the deliverability authority. Workspace `state.json`
/// is only a copy. Detection failure is unbound, never attached.
pub fn launched_team_receiver_is_attached(workspace: &Path, team_key: &str) -> bool {
    match registry_deliverability(workspace, team_key) {
        RegistryDeliverability::Attached => {}
        RegistryDeliverability::Unbound | RegistryDeliverability::Undecidable => return false,
    }
    load_runtime_state(workspace).is_ok()
}

enum RegistryDeliverability {
    Attached,
    Unbound,
    Undecidable,
}

fn registry_deliverability(workspace: &Path, team_key: &str) -> RegistryDeliverability {
    let Some(dir) = crate::leader::registry::registry_dir() else {
        return RegistryDeliverability::Undecidable;
    };
    if team_key.is_empty() {
        return RegistryDeliverability::Unbound;
    }
    let hash = crate::leader::registry::workspace_hash(workspace);
    let path = dir.join(format!("{hash}__{team_key}.json"));
    match std::fs::read_to_string(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            RegistryDeliverability::Unbound
        }
        Err(_) => RegistryDeliverability::Undecidable,
        Ok(text) => {
            match serde_json::from_str::<crate::leader::registry::LeaderRegistryEntry>(&text) {
                Err(_) => RegistryDeliverability::Undecidable,
                Ok(entry) => {
                    if entry.status != "attached" {
                        return RegistryDeliverability::Unbound;
                    }
                    if let Some(authorized) = entry
                        .channel
                        .get("authorized_team_workspace")
                        .and_then(serde_json::Value::as_str)
                        .filter(|value| !value.is_empty())
                    {
                        if !same_workspace_path(Path::new(authorized), workspace) {
                            return RegistryDeliverability::Unbound;
                        }
                    }
                    RegistryDeliverability::Attached
                }
            }
        }
    }
}

fn same_workspace_path(left: &Path, right: &Path) -> bool {
    let left = std::fs::canonicalize(left).unwrap_or_else(|_| left.to_path_buf());
    let right = std::fs::canonicalize(right).unwrap_or_else(|_| right.to_path_buf());
    left == right
}
