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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadinessPhase {
    StartCoordinator,
    ComputeVerdict,
}

impl ReadinessPhase {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::StartCoordinator => "launch.readiness.start_coordinator",
            Self::ComputeVerdict => "launch.readiness.compute_verdict",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phase_labels_are_dotted_paths_under_launch_readiness() {
        assert!(ReadinessPhase::StartCoordinator
            .as_str()
            .starts_with("launch.readiness."));
    }
}

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

pub(crate) fn launched_team_receiver_is_attached(workspace: &Path, team_key: &str) -> bool {
    let Ok(state) = load_runtime_state(workspace) else {
        return true;
    };
    let team_state = state
        .get("teams")
        .and_then(serde_json::Value::as_object)
        .and_then(|teams| teams.get(team_key))
        .unwrap_or(&state);
    if team_state.get("leader_receiver").is_none() {
        return crate::state::projection::state_is_external_leader(team_state);
    }
    if team_uses_fake_model_harness(team_state) {
        return true;
    }
    leader_receiver_is_attached(team_state)
}

pub(super) fn team_uses_fake_model_harness(team_state: &serde_json::Value) -> bool {
    team_state
        .get("agents")
        .and_then(serde_json::Value::as_object)
        .is_some_and(|agents| {
            !agents.is_empty()
                && agents.values().all(|agent| {
                    agent.get("model").and_then(serde_json::Value::as_str) == Some("fake")
                })
        })
}

pub(super) fn leader_receiver_is_attached(team_state: &serde_json::Value) -> bool {
    let Some(receiver) = team_state.get("leader_receiver") else {
        return false;
    };
    let status = receiver
        .get("status")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    let pane_id = receiver
        .get("pane_id")
        .and_then(serde_json::Value::as_str)
        .or_else(|| receiver.get("pane").and_then(serde_json::Value::as_str))
        .unwrap_or("");
    status == "attached" && !pane_id.is_empty() && pane_id != "__team_agent_unbound__"
}
