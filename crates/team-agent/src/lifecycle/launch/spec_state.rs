//! unit-8 (Stage 3) — `lifecycle::launch::spec_state` phase boundary.
//!
//! The dedicated home for spec/runtime-path resolution and state-tree
//! materialization phases of `quick_start`. Lives in the
//! `lifecycle/launch/` submodule so future commits can migrate the
//! existing inline phase fns (launch.rs:2781-2906 and 1680-1756 ranges)
//! here in small, reviewable pieces.
//!
//! Established phases (canonical names — keep stable for future
//! migration):
//!
//! * `resolve_spec_paths`   — `.team/runtime/<team>/team.spec.yaml`
//!                            resolution + workspace canonicalization
//! * `materialize_state`    — T1 layer state.json initialization including
//!                            agent capture fields and `spawn_cwd`
//!
//! This commit lands the boundary + a marker enum so unit-8's adoption
//! sites can reference the phases by name. The phase fns themselves
//! remain in launch.rs until the next batch of relocations.

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

use super::approval::detect_dangerous_approval;
use super::identity::spec_display_backend;
use super::leader_context::{
    attributed_provider_for_pane_across_tmux_sockets, caller_provider_for_seed_with_lookup,
    seed_unbound_launched_owner,
};
use super::worker_env::spawn_timestamp;

/// Named launch phases under spec_state. Used in phase labels for logs
/// and (future) for the orchestrator's step dispatcher.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpecStatePhase {
    ResolveSpecPaths,
    MaterializeState,
}

impl SpecStatePhase {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ResolveSpecPaths => "launch.spec_state.resolve_spec_paths",
            Self::MaterializeState => "launch.spec_state.materialize_state",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phase_labels_are_dotted_paths_under_launch_spec_state() {
        assert!(SpecStatePhase::ResolveSpecPaths
            .as_str()
            .starts_with("launch.spec_state."));
        assert!(SpecStatePhase::MaterializeState
            .as_str()
            .starts_with("launch.spec_state."));
    }
}

pub(super) fn initial_runtime_state(
    spec: &Value,
    spec_path: &Path,
    workspace: &Path,
    team_dir: &Path,
    team_key: &str,
) -> serde_json::Value {
    let mut agents = serde_json::Map::new();
    for agent in spec_agent_values(spec) {
        let Some(id) = agent.get("id").and_then(Value::as_str) else {
            continue;
        };
        let provider = agent
            .get("provider")
            .and_then(Value::as_str)
            .unwrap_or("codex");
        let role = agent.get("role").and_then(Value::as_str).unwrap_or(id);
        let model = agent.get("model").and_then(Value::as_str);
        let auth_mode = agent.get("auth_mode").and_then(Value::as_str);
        let mut value = serde_json::json!({
            "provider": provider,
            "role": role,
        });
        if let Some(obj) = value.as_object_mut() {
            if let Some(model) = model {
                obj.insert("model".to_string(), serde_json::json!(model));
            }
            if let Some(auth_mode) = auth_mode {
                obj.insert("auth_mode".to_string(), serde_json::json!(auth_mode));
            }
        }
        agents.insert(id.to_string(), value);
    }
    let display_backend = spec_display_backend(spec);
    let mut state = serde_json::Map::new();
    state.insert(
        "spec_path".to_string(),
        serde_json::json!(spec_path.to_string_lossy().to_string()),
    );
    state.insert(
        "workspace".to_string(),
        serde_json::json!(workspace.to_string_lossy().to_string()),
    );
    state.insert(
        "team_dir".to_string(),
        serde_json::json!(team_dir.to_string_lossy().to_string()),
    );
    state.insert("team_key".to_string(), serde_json::json!(team_key));
    state.insert(
        "session_name".to_string(),
        serde_json::json!(spec_session_name(spec).as_str()),
    );
    state.insert(
        "leader".to_string(),
        spec.get("leader")
            .map(yaml_value_to_json)
            .unwrap_or(serde_json::Value::Null),
    );
    state.insert("agents".to_string(), serde_json::Value::Object(agents));
    state.insert("tasks".to_string(), spec_tasks_json(spec));
    state.insert(
        "display_backend".to_string(),
        serde_json::json!(display_backend),
    );
    state.insert("is_external_leader".to_string(), serde_json::json!(false));
    let mut state = serde_json::Value::Object(state);
    if !seed_launched_owner_from_env(&mut state) {
        let team_id = crate::state::projection::team_state_key(&state);
        seed_unbound_launched_owner(&mut state, &team_id);
    }
    state
}

pub(super) fn seed_launched_owner_from_env(state: &mut serde_json::Value) -> bool {
    let team_id = crate::state::projection::team_state_key(state);
    let Ok(caller) = crate::state::identity::caller_identity_from_env(
        Some(state),
        &crate::state::identity::SystemEnv,
        Some(&team_id),
        None,
    ) else {
        return false;
    };
    seed_launched_owner_from_caller_with_provider_lookup(
        state,
        caller,
        attributed_provider_for_pane_across_tmux_sockets,
    )
}

pub(super) fn seed_launched_owner_from_caller_with_provider_lookup(
    state: &mut serde_json::Value,
    caller: crate::state::owner_gate::CallerIdentity,
    lookup_pane_provider: impl Fn(&PaneId) -> Option<Provider>,
) -> bool {
    if caller.pane_id.is_empty() {
        return false;
    }
    let provider = caller_provider_for_seed_with_lookup(&caller, lookup_pane_provider);
    let pane_id = caller.pane_id;
    let owner_epoch = 1u64;
    let mut owner = serde_json::json!({
        "pane_id": pane_id,
        "machine_fingerprint": caller.machine_fingerprint,
        "leader_session_uuid": caller.leader_session_uuid,
        "owner_epoch": owner_epoch,
        "claimed_at": spawn_timestamp(),
        "claimed_via": "quick-start",
        "os_user": std::env::var("USER")
            .or_else(|_| std::env::var("USERNAME"))
            .unwrap_or_default(),
    });
    let mut receiver = serde_json::json!({
        "mode": "direct_tmux",
        "status": "attached",
        "pane_id": owner.get("pane_id").cloned().unwrap_or(serde_json::Value::Null),
        "pane": owner.get("pane_id").cloned().unwrap_or(serde_json::Value::Null),
        "leader_session_uuid": owner.get("leader_session_uuid").cloned().unwrap_or(serde_json::Value::Null),
        "owner_epoch": owner_epoch,
        "discovery": "quick_start",
    });
    if let Some(provider) = provider.as_ref() {
        if let Some(owner) = owner.as_object_mut() {
            owner.insert("provider".to_string(), serde_json::json!(provider));
        }
        if let Some(receiver) = receiver.as_object_mut() {
            receiver.insert("provider".to_string(), serde_json::json!(provider));
        }
    }
    if let (Some(receiver), Some(socket)) = (
        receiver.as_object_mut(),
        crate::tmux_backend::socket_name_from_tmux_env(),
    ) {
        receiver.insert("tmux_socket".to_string(), serde_json::json!(socket));
    }
    // Stage 3a (identity-boundary unified plan, architect direction 2026-06-23):
    // route quick-start attached-from-env seed through ownership repository.
    let team_key = crate::state::projection::team_state_key(state);
    let record = crate::state::ownership::OwnershipWrite::new()
        .with_leader_receiver(receiver)
        .with_team_owner(owner)
        .with_owner_epoch(owner_epoch);
    crate::state::ownership::write_owner(state, &team_key, record);
    true
}

pub(super) fn has_positive_caller_leader_env() -> bool {
    env_nonempty("TEAM_AGENT_LEADER_PANE_ID")
        || env_nonempty("TEAM_AGENT_LEADER_SESSION_UUID")
        || env_nonempty("TEAM_AGENT_LEADER_SESSION_UUID_OVERRIDE")
        || env_nonempty("TEAM_AGENT_LEADER_PROVIDER")
}

pub(super) fn env_nonempty(key: &str) -> bool {
    std::env::var(key)
        .ok()
        .is_some_and(|value| !value.is_empty())
}

pub(super) fn spec_tasks_json(spec: &Value) -> serde_json::Value {
    spec.get("tasks")
        .and_then(Value::as_list)
        .map(|tasks| serde_json::Value::Array(tasks.iter().map(yaml_value_to_json).collect()))
        .unwrap_or_else(|| serde_json::json!([]))
}

pub(super) fn yaml_value_to_json(value: &Value) -> serde_json::Value {
    match value {
        Value::Null => serde_json::Value::Null,
        Value::Bool(v) => serde_json::json!(v),
        Value::Int(v) => serde_json::json!(v),
        Value::Float(v) => serde_json::json!(v),
        Value::Str(v) => serde_json::json!(v),
        Value::List(values) => {
            serde_json::Value::Array(values.iter().map(yaml_value_to_json).collect())
        }
        Value::Map(entries) => {
            let mut out = serde_json::Map::new();
            for (key, item) in entries {
                out.insert(key.clone(), yaml_value_to_json(item));
            }
            serde_json::Value::Object(out)
        }
    }
}

/// Set `runtime.session_name` on the compiled spec to `session_name`, creating the
/// `runtime` map and/or the `session_name` entry if absent. Used by quick-start to
/// derive the tmux session from the REQUESTED team identity (CR-040/042) rather
/// than the template's compiled-in name.
/// E5 Bug2(atomic 真修):原子写 runtime spec —— 写 `<spec>.tmp-<pid>` 再 rename 覆盖,
/// 避免崩溃/并发留下半截 spec(plain fs::write 会 in-place truncate 后逐字节写)。
/// rename 失败时清理 tmp,原 spec(若有)不动。
pub(crate) fn write_spec_atomic(spec_path: &Path, spec: &Value) -> Result<(), LifecycleError> {
    if let Some(parent) = spec_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| LifecycleError::StatePersist(format!("{}: {e}", parent.display())))?;
    }
    let tmp = spec_path.with_extension(format!("tmp-{}", std::process::id()));
    std::fs::write(&tmp, yaml::dumps(spec))
        .map_err(|e| LifecycleError::StatePersist(format!("{}: {e}", tmp.display())))?;
    if let Err(e) = std::fs::rename(&tmp, spec_path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(LifecycleError::StatePersist(format!(
            "{}: {e}",
            spec_path.display()
        )));
    }
    Ok(())
}

pub(crate) fn override_spec_session_name(spec: &mut Value, session_name: &str) {
    override_spec_runtime_str(spec, "session_name", session_name);
}

pub(crate) fn override_spec_workspace(spec: &mut Value, workspace: &Path) {
    let workspace_s = workspace.to_string_lossy().to_string();
    let Value::Map(root) = spec else { return };
    if let Some((_, Value::Map(team))) = root.iter_mut().find(|(k, _)| k == "team") {
        if let Some((_, value)) = team.iter_mut().find(|(k, _)| k == "workspace") {
            *value = Value::Str(workspace_s.clone());
        }
    }
    if let Some((_, Value::List(agents))) = root.iter_mut().find(|(k, _)| k == "agents") {
        for agent in agents {
            if let Value::Map(fields) = agent {
                if let Some((_, value)) = fields.iter_mut().find(|(k, _)| k == "working_directory")
                {
                    *value = Value::Str(workspace_s.clone());
                }
            }
        }
    }
}

pub(super) fn override_spec_display_backend(spec: &mut Value, display_backend: &str) {
    override_spec_runtime_str(spec, "display_backend", display_backend);
}

pub(super) fn override_spec_runtime_str(spec: &mut Value, key: &str, value: &str) {
    let Value::Map(root) = spec else { return };
    let runtime_slot = root
        .iter_mut()
        .find(|(k, _)| k == "runtime")
        .map(|(_, v)| v);
    match runtime_slot {
        Some(Value::Map(runtime)) => {
            if let Some((_, existing)) = runtime.iter_mut().find(|(k, _)| k == key) {
                *existing = Value::Str(value.to_string());
            } else {
                runtime.push((key.to_string(), Value::Str(value.to_string())));
            }
        }
        Some(other) => {
            *other = Value::Map(vec![(key.to_string(), Value::Str(value.to_string()))]);
        }
        None => {
            root.push((
                "runtime".to_string(),
                Value::Map(vec![(key.to_string(), Value::Str(value.to_string()))]),
            ));
        }
    }
}

pub(super) fn spec_session_name(spec: &Value) -> SessionName {
    if let Some(name) = spec
        .get("runtime")
        .and_then(|v| v.get("session_name"))
        .and_then(Value::as_str)
        .filter(|name| !name.is_empty())
    {
        return SessionName::new(name);
    }
    // Python launch/core.py:56 — fallback derives from the team name, not a constant.
    let team_name = spec
        .get("team")
        .and_then(|team| team.get("name"))
        .and_then(Value::as_str)
        .filter(|name| !name.is_empty())
        .unwrap_or("agent");
    SessionName::new(format!("team-{team_name}"))
}

/// 0.3.28 layout step 1: pub re-export of `spec_session_name` for the new
/// `layout::sessions::worker_session_name` to delegate to. Single underlying
/// impl; this just widens visibility without duplicating logic.
pub fn worker_session_name_pub(spec: &Value) -> SessionName {
    spec_session_name(spec)
}

pub(super) fn spec_agents(spec: &Value) -> Vec<AgentId> {
    spec_agent_values(spec)
        .into_iter()
        .filter_map(|agent| agent.get("id").and_then(Value::as_str).map(AgentId::new))
        .collect()
}

/// Bug 1 (0.4.2): expose spec agent id set so the restart path can filter
/// state.agents to only the agents currently defined in the rebuilt spec.
/// Returns a `BTreeSet<String>` for O(log n) membership checks.
pub(crate) fn spec_agent_id_set(spec: &Value) -> std::collections::BTreeSet<String> {
    spec_agent_values(spec)
        .into_iter()
        .filter_map(|agent| agent.get("id").and_then(Value::as_str).map(str::to_string))
        .collect()
}

pub(super) fn spec_agent_values(spec: &Value) -> Vec<&Value> {
    spec.get("agents")
        .and_then(Value::as_list)
        .map(|agents| agents.iter().collect())
        .unwrap_or_default()
}

pub(super) fn spec_routes(spec: &Value) -> Vec<RoutingDecision> {
    spec.get("tasks")
        .and_then(Value::as_list)
        .map(|tasks| {
            tasks
                .iter()
                .map(|task| {
                    let routed = crate::model::routing::route_task(spec, task);
                    RoutingDecision {
                        task_id: task.get("id").and_then(Value::as_str).map(str::to_string),
                        selected_agent: routed.agent_id,
                        reason: routed.reason,
                        manual_override: false,
                    }
                })
                .collect()
        })
        .unwrap_or_default()
}

pub(super) fn spec_default_assignee(spec: &Value) -> Option<AgentId> {
    spec.get("routing")
        .and_then(|v| v.get("default_assignee"))
        .and_then(Value::as_str)
        .map(AgentId::new)
        .or_else(|| spec_agents(spec).into_iter().next())
}

pub(crate) fn effective_runtime_config(spec: &Value) -> Result<DangerousApproval, LifecycleError> {
    let enabled = spec
        .get("runtime")
        .and_then(|v| v.get("dangerous_auto_approve"))
        .is_some_and(Value::is_truthy);
    if enabled {
        let leader = detect_dangerous_approval()?;
        Ok(DangerousApproval {
            enabled: true,
            source: DangerousApprovalSource::RuntimeConfig,
            inherited: false,
            provider: None,
            flag: None,
            worker_capability_above_leader: !leader.enabled,
            ancestry_binary_name: leader.ancestry_binary_name,
            unexpected_binary: false,
        })
    } else {
        Ok(detect_dangerous_approval()?)
    }
}

pub(crate) fn effective_runtime_config_for_worker_spawn(
) -> Result<DangerousApproval, LifecycleError> {
    detect_dangerous_approval()
}

pub(super) fn write_launch_permission_audit(
    workspace: &Path,
    safety: &DangerousApproval,
) -> Result<(), LifecycleError> {
    crate::event_log::EventLog::new(workspace)
        .write(
            "launch.permissions_resolved",
            serde_json::json!({
                "dangerous_auto_approve": safety.enabled,
                "dangerous_auto_approve_source": safety.source,
                "dangerous_auto_approve_inherited": safety.inherited,
                "dangerous_auto_approve_provider": safety.provider,
                "dangerous_auto_approve_flag": safety.flag,
                "worker_capability_above_leader": safety.worker_capability_above_leader,
                "ancestry_binary_name": safety.ancestry_binary_name,
            }),
        )
        .map_err(|e| LifecycleError::StatePersist(e.to_string()))?;
    if safety.unexpected_binary {
        crate::event_log::EventLog::new(workspace)
            .write(
                "dangerous_flag_in_unexpected_binary",
                serde_json::json!({
                    "provider": safety.provider,
                    "flag": safety.flag,
                    "ancestry_binary_name": safety.ancestry_binary_name,
                }),
            )
            .map_err(|e| LifecycleError::StatePersist(e.to_string()))?;
    }
    Ok(())
}

pub(super) fn team_workspace(team_dir: &Path) -> PathBuf {
    crate::model::paths::team_workspace(team_dir).unwrap_or_else(|_| {
        team_dir
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| team_dir.to_path_buf())
    })
}
