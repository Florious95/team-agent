use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::lifecycle::lock::{acquire_agent_lifecycle_lock, LifecycleLockRequest};
use crate::lifecycle::profile_launch::parse_provider;
use crate::lifecycle::*;
use crate::model::enums::{AuthMode, DisplayBackend, PaneLiveness, Provider, ProviderEffort};
use crate::model::ids::AgentId;
use crate::model::permissions::{self, AgentPermissionInput};
use crate::model::yaml::{self, Value};
use crate::state::persist::load_runtime_state;
use crate::transport::{PaneId, SessionName, Target, Transport, WindowName};

use super::agent_state::running_agent_state;
use super::identity::{explicit_active_team_key, runtime_team_key_for_spec};
use super::leader_context::{
    owner_pane_belongs_to_other_team, seed_unbound_launched_owner, unbound_launched_owner,
};
use super::spec_state::{has_positive_caller_leader_env, spec_agent_values};
use super::worker_env::{agent_is_paused, spawn_timestamp_for_agent};

pub(super) fn persist_spawn_agent_state(
    workspace: &Path,
    spec_path: &Path,
    spec: &Value,
    session_name: &SessionName,
    transport: &dyn Transport,
    started: &[StartedAgent],
    safety: &DangerousApproval,
) -> Result<(), LifecycleError> {
    let state_path = crate::state::persist::runtime_state_path(workspace);
    let mut state = match crate::state::repository::StateRepository::new(workspace)
        .load_workspace_if_exists_without_migrations()
    {
        Ok(Some(state)) => state,
        Ok(None) => serde_json::json!({"agents": {}}),
        Err(crate::state::StateError::Io(error)) => {
            return Err(LifecycleError::StatePersist(format!(
                "{}: {error}",
                state_path.display()
            )))
        }
        Err(crate::state::StateError::Json(error)) => {
            return Err(LifecycleError::StatePersist(format!(
                "{}: {error}",
                state_path.display()
            )))
        }
        Err(error) => {
            return Err(LifecycleError::StatePersist(format!(
                "{}: {error}",
                state_path.display()
            )))
        }
    };
    let team_id = runtime_team_key_for_spec(spec_path, spec, session_name);
    let worker_tmux_socket = launched_worker_tmux_socket(transport, workspace);
    drop_worker_pane_seeded_owner(&mut state, &team_id, started, worker_tmux_socket.as_deref());
    // Only persist running state for agents whose spawn still has a live target.
    let live_windows: BTreeSet<String> = transport
        .list_windows(session_name)
        .unwrap_or_default()
        .into_iter()
        .map(|w| w.as_str().to_string())
        .collect();
    let live_started_agents: BTreeSet<String> = started
        .iter()
        .map(|agent| agent.agent_id.as_str().to_string())
        .collect();
    let pane_pids_by_agent = pane_pids_by_started_agent(transport, started);
    // E5 解耦:profiles 随**角色定义**(team_dir),不随 spec(已迁出到 .team/runtime)。
    // 优先 state.team_dir(角色目录),回落 spec_path.parent()(legacy 同目录布局)。
    let profile_dir = state
        .get("team_dir")
        .and_then(serde_json::Value::as_str)
        .filter(|s| !s.is_empty())
        .map(|dir| Path::new(dir).join("profiles"))
        .unwrap_or_else(|| spec_path.parent().unwrap_or(workspace).join("profiles"));
    let mut agents = serde_json::Map::new();
    let mut spawn_index = 0_u32;
    for agent in spec_agent_values(spec) {
        let Some(id) = agent.get("id").and_then(Value::as_str) else {
            continue;
        };
        let provider = agent
            .get("provider")
            .and_then(Value::as_str)
            .and_then(parse_provider)
            .unwrap_or(Provider::Codex);
        if agent_is_paused(agent) {
            let mut paused = serde_json::Map::new();
            paused.insert("status".to_string(), serde_json::json!("paused"));
            paused.insert("provider".to_string(), serde_json::json!(provider));
            agents.insert(id.to_string(), serde_json::Value::Object(paused));
            continue;
        }
        let started_agent = started.iter().find(|agent| agent.agent_id.as_str() == id);
        let window = started_agent
            .and_then(|started| started.layout_window.as_ref())
            .map(WindowName::as_str)
            .or_else(|| agent.get("window").and_then(Value::as_str))
            .unwrap_or(id);
        if !live_started_agents.contains(id)
            || (!live_windows.is_empty() && !live_windows.contains(window))
        {
            let mut failed = serde_json::Map::new();
            failed.insert("status".to_string(), serde_json::json!("spawn_failed"));
            failed.insert("provider".to_string(), serde_json::json!(provider));
            failed.insert("agent_id".to_string(), serde_json::json!(id));
            failed.insert("window".to_string(), serde_json::json!(window));
            failed.insert(
                "reason".to_string(),
                serde_json::json!("tmux window not present after spawn"),
            );
            agents.insert(id.to_string(), serde_json::Value::Object(failed));
            continue;
        }
        let pane_pid = pane_pids_by_agent.get(id).copied();
        let spawned_at = started_agent
            .map(|started| started.spawned_at.clone())
            .unwrap_or_else(|| spawn_timestamp_for_agent(spawn_index));
        spawn_index = spawn_index.saturating_add(1);
        agents.insert(
            id.to_string(),
            running_agent_state(
                agent,
                id,
                provider,
                workspace,
                workspace,
                &spawned_at,
                &team_id,
                Some(agent_id_to_pane_id(started, id)),
                pane_pid,
                safety,
                started_agent,
                Some(&profile_dir),
            )?,
        );
    }
    if let Some(obj) = state.as_object_mut() {
        obj.insert("agents".to_string(), serde_json::Value::Object(agents));
    } else {
        let mut obj = serde_json::Map::new();
        obj.insert("agents".to_string(), serde_json::Value::Object(agents));
        state = serde_json::Value::Object(obj);
    }
    save_launched_team_state_for_key(workspace, &state, Some(&team_id))
}

pub(super) fn pane_pids_by_started_agent(
    transport: &dyn Transport,
    started: &[StartedAgent],
) -> BTreeMap<String, u32> {
    let panes = transport.list_targets().unwrap_or_default();
    started
        .iter()
        .filter_map(|agent| {
            panes
                .iter()
                .find(|pane| pane.pane_id.as_str() == agent.target)
                .and_then(|pane| pane.pane_pid)
                .map(|pid| (agent.agent_id.as_str().to_string(), pid))
        })
        .collect()
}

pub(super) fn agent_id_to_pane_id<'a>(started: &'a [StartedAgent], agent_id: &str) -> &'a str {
    started
        .iter()
        .find(|agent| agent.agent_id.as_str() == agent_id)
        .map(|agent| agent.target.as_str())
        .unwrap_or("")
}

pub(super) fn save_launched_team_state(
    workspace: &Path,
    launched: &serde_json::Value,
) -> Result<(), LifecycleError> {
    save_launched_team_state_for_key(workspace, launched, None)
}

pub(super) fn save_launched_team_state_for_key(
    workspace: &Path,
    launched: &serde_json::Value,
    team_key: Option<&str>,
) -> Result<(), LifecycleError> {
    save_team_state_for_key(workspace, launched, team_key, None)
}

pub(super) fn save_added_agent_state_for_key(
    workspace: &Path,
    launched: &serde_json::Value,
    team_key: &str,
    agent_id: &str,
) -> Result<(), LifecycleError> {
    save_team_state_for_key(workspace, launched, Some(team_key), Some(agent_id))
}

fn save_team_state_for_key(
    workspace: &Path,
    launched: &serde_json::Value,
    team_key: Option<&str>,
    added_agent_id: Option<&str>,
) -> Result<(), LifecycleError> {
    let existing = load_runtime_state(workspace).unwrap_or_else(|_| serde_json::json!({}));
    let launched_key = team_key
        .filter(|key| !key.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| crate::state::projection::team_state_key(launched));
    let mut launched = launched.clone();
    if let Some(obj) = launched.as_object_mut() {
        // RM-039-STAT-001 second-round fix (architect verdict
        // 2026-06-22): the canonical runtime team key MUST be written
        // explicitly to `state.team_key`, not only inferred from
        // `team_dir` by `team_state_key`'s cascade. The historical
        // bug: when `team_dir = "./.team/current"` and the runtime team
        // is `rm039-status-working-891`, `team_state_key` cascades to
        // the team_dir basename (`current`), but `active_team_key` is
        // the real team key. Coordinator tick uses `team_state_key`
        // when saving the team-scoped state, so writes land on
        // `teams.current` instead of `teams.<active_team_key>`. Status
        // reads `teams[active_team_key]`, sees stale data.
        //
        // Writing `team_key=launched_key` here pins the first branch of
        // `team_state_key` so the cascade returns the canonical runtime
        // team key everywhere — coordinator tick, save_team_scoped_state,
        // and status selector all agree.
        obj.insert(
            "team_key".to_string(),
            serde_json::Value::String(launched_key.clone()),
        );
        obj.insert(
            "active_team_key".to_string(),
            serde_json::Value::String(launched_key.clone()),
        );
        obj.entry("is_external_leader".to_string())
            .or_insert(serde_json::Value::Bool(false));
    }
    promote_launched_binding_from_team_entry(&mut launched, &launched_key);
    preserve_existing_leader_topology(&existing, &launched_key, &mut launched);
    drop_foreign_seeded_owner(&existing, &launched_key, &mut launched);
    drop_bare_worker_seeded_owner(&mut launched, &launched_key);
    let merged = if team_key.is_some() {
        merge_workspace_team_state_with_key(&existing, &launched, &launched_key)
    } else {
        crate::state::projection::merge_workspace_team_state(&existing, &launched)
    };
    let mut projected = crate::state::projection::project_top_level_view(&merged, &launched_key);
    drop_unbound_top_level_owner(&mut projected);
    let intent = match added_agent_id {
        Some(agent_id) => crate::state::repository::StateWriteIntent::AddAgent {
            team_key: &launched_key,
            agent_id,
        },
        None => crate::state::repository::StateWriteIntent::LaunchTeam {
            team_key: &launched_key,
        },
    };
    crate::state::repository::StateRepository::new(workspace)
        .save(intent, &projected)
        .map_err(|e| LifecycleError::StatePersist(e.to_string()))
}

pub(super) fn preserve_existing_leader_topology(
    existing: &serde_json::Value,
    launched_key: &str,
    launched: &mut serde_json::Value,
) {
    let Some(obj) = launched.as_object_mut() else {
        return;
    };
    let existing_team = existing
        .get("teams")
        .and_then(serde_json::Value::as_object)
        .and_then(|teams| teams.get(launched_key))
        .unwrap_or(existing);
    for key in ["is_external_leader", "leader_client"] {
        if !obj.contains_key(key) {
            if let Some(value) = existing_team.get(key).or_else(|| existing.get(key)) {
                obj.insert(key.to_string(), value.clone());
            }
        }
    }
}

pub(super) fn drop_bare_worker_seeded_owner(launched: &mut serde_json::Value, launched_key: &str) {
    if has_positive_caller_leader_env() {
        return;
    }
    let pane = launched
        .get("team_owner")
        .and_then(|owner| owner.get("pane_id"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    if pane.ends_with("-first") {
        seed_unbound_launched_owner(launched, launched_key);
    }
}

pub(super) fn merge_workspace_team_state_with_key(
    existing: &serde_json::Value,
    launched: &serde_json::Value,
    launched_key: &str,
) -> serde_json::Value {
    let mut launched_obj = launched.as_object().cloned().unwrap_or_default();
    let mut teams = existing
        .get("teams")
        .and_then(serde_json::Value::as_object)
        .cloned()
        .unwrap_or_default();
    let launched_entry = crate::state::projection::compact_team_state(launched);
    if !existing
        .get("session_name")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|session| !session.is_empty())
    {
        teams.insert(launched_key.to_string(), launched_entry);
        launched_obj.insert("teams".to_string(), serde_json::Value::Object(teams));
        return serde_json::Value::Object(launched_obj);
    }

    let existing_key = explicit_active_team_key(existing)
        .unwrap_or_else(|| crate::state::projection::team_state_key(existing));
    if existing_key == launched_key {
        let mut teams = existing
            .get("teams")
            .and_then(serde_json::Value::as_object)
            .cloned()
            .unwrap_or_default();
        teams.insert(launched_key.to_string(), launched_entry);
        launched_obj.insert("teams".to_string(), serde_json::Value::Object(teams));
        return serde_json::Value::Object(launched_obj);
    }

    let mut merged = existing.as_object().cloned().unwrap_or_default();
    let mut teams = merged
        .get("teams")
        .and_then(serde_json::Value::as_object)
        .cloned()
        .unwrap_or_default();
    teams
        .entry(existing_key)
        .or_insert_with(|| crate::state::projection::compact_team_state(existing));
    teams.insert(launched_key.to_string(), launched_entry);
    merged.insert("teams".to_string(), serde_json::Value::Object(teams));
    serde_json::Value::Object(merged)
}

#[cfg(test)]
mod merge_workspace_team_state_with_key_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn empty_top_level_existing_session_preserves_existing_teams() {
        let existing = json!({
            "session_name": "",
            "active_team_key": "parent",
            "teams": {
                "parent": {
                    "session_name": "team-parent",
                    "agents": {"parent_worker": {"status": "running"}}
                }
            }
        });
        let launched = json!({
            "session_name": "team-child",
            "agents": {"child_worker": {"status": "running"}}
        });
        let merged = merge_workspace_team_state_with_key(&existing, &launched, "child");
        assert_eq!(
            merged.pointer("/teams/parent/session_name"),
            Some(&json!("team-parent")),
            "existing.teams must survive even when existing.session_name is empty: {merged}"
        );
        assert_eq!(
            merged.pointer("/teams/child/session_name"),
            Some(&json!("team-child")),
            "launched team must still be inserted under its runtime key: {merged}"
        );
    }
}

pub(super) fn promote_launched_binding_from_team_entry(
    launched: &mut serde_json::Value,
    launched_key: &str,
) {
    let entry = launched
        .get("teams")
        .and_then(|teams| teams.get(launched_key))
        .cloned();
    let Some(entry) = entry else {
        return;
    };
    let Some(obj) = launched.as_object_mut() else {
        return;
    };
    for key in ["leader_receiver", "team_owner", "owner_epoch"] {
        if !obj.contains_key(key) {
            if let Some(value) = entry.get(key) {
                obj.insert(key.to_string(), value.clone());
            }
        }
    }
}

pub(super) fn drop_unbound_top_level_owner(state: &mut serde_json::Value) {
    let pane = state
        .get("team_owner")
        .and_then(|owner| owner.get("pane_id"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    if pane.starts_with('%') || pane.chars().all(|ch| ch.is_ascii_digit()) && !pane.is_empty() {
        return;
    }
    if let Some(obj) = state.as_object_mut() {
        obj.remove("leader_receiver");
        obj.remove("team_owner");
        obj.remove("owner_epoch");
    }
}

pub(super) fn drop_foreign_seeded_owner(
    existing: &serde_json::Value,
    launched_key: &str,
    launched: &mut serde_json::Value,
) {
    let Some(pane) = launched
        .get("team_owner")
        .and_then(|owner| owner.get("pane_id"))
        .and_then(serde_json::Value::as_str)
        .filter(|pane| !pane.is_empty())
    else {
        return;
    };
    if owner_pane_belongs_to_other_team(existing, launched_key, pane) {
        let replacement = unbound_launched_owner(launched, launched_key);
        // Stage 3a (identity-boundary unified plan, architect direction
        // 2026-06-23): route owner replace through ownership repository.
        // Both branches (Some owner replacement; None = removal) preserve
        // pre-3a behaviour: write_owner overwrites the top-level fields
        // when given; the None branch falls back to direct mutation (the
        // repository has no remove API yet — removal is uncommon enough
        // to leave it inline).
        if let Some(owner) = replacement {
            let record = crate::state::ownership::OwnershipWrite::new().with_team_owner(owner);
            crate::state::ownership::write_owner(launched, launched_key, record);
            if let Some(obj) = launched.as_object_mut() {
                obj.remove("owner_epoch");
            }
        } else if let Some(obj) = launched.as_object_mut() {
            obj.remove("team_owner");
            obj.remove("owner_epoch");
        }
    }
}

pub(super) fn drop_worker_pane_seeded_owner(
    launched: &mut serde_json::Value,
    launched_key: &str,
    started: &[StartedAgent],
    worker_tmux_socket: Option<&str>,
) {
    let Some(pane) = launched
        .get("team_owner")
        .and_then(|owner| owner.get("pane_id"))
        .and_then(serde_json::Value::as_str)
        .filter(|pane| !pane.is_empty())
    else {
        return;
    };
    let leader_pane = std::env::var("TEAM_AGENT_LEADER_PANE_ID")
        .ok()
        .filter(|value| !value.is_empty());
    let tmux_pane = std::env::var("TMUX_PANE")
        .ok()
        .filter(|value| !value.is_empty());
    let has_leader_identity_env = has_positive_caller_leader_env();
    let seeded_from_bare_tmux = !has_leader_identity_env && tmux_pane.as_deref() == Some(pane);
    let caller_tmux_socket = crate::tmux_backend::socket_name_from_tmux_env();
    if seeded_from_bare_tmux
        && (tmux_sockets_match_or_unknown(caller_tmux_socket.as_deref(), worker_tmux_socket)
            || pane.ends_with("-first"))
        && seeded_pane_looks_like_worker(pane, started)
    {
        seed_unbound_launched_owner(launched, launched_key);
    }
}

pub(super) fn seeded_pane_looks_like_worker(pane: &str, started: &[StartedAgent]) -> bool {
    pane.ends_with("-first")
        || started.iter().any(|agent| {
            pane == agent.target
                || pane.starts_with(agent.target.as_str())
                || agent.target.starts_with(pane)
        })
}

pub(super) fn launched_worker_tmux_socket(
    transport: &dyn Transport,
    workspace: &Path,
) -> Option<String> {
    if matches!(transport.kind(), crate::transport::BackendKind::Tmux) {
        Some(crate::tmux_backend::socket_name_for_workspace(workspace))
    } else {
        None
    }
}

pub(super) fn tmux_sockets_match_or_unknown(
    caller_socket: Option<&str>,
    worker_socket: Option<&str>,
) -> bool {
    match (caller_socket, worker_socket) {
        (Some(caller), Some(worker)) => caller == worker,
        (Some(_), None) => false,
        (None, _) => true,
    }
}
