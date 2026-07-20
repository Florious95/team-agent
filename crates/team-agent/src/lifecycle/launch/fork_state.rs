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

pub(super) fn maybe_fail_fork_after_spawn(step: &str) -> Result<(), LifecycleError> {
    let Ok(reason) = std::env::var("TEAM_AGENT_TEST_FAIL_FORK_AFTER_SPAWN") else {
        return Ok(());
    };
    if reason.is_empty() {
        return Ok(());
    }
    let should_fail = reason == step || (step == "start_coordinator" && reason == "coordinator");
    if !should_fail {
        return Ok(());
    }
    Err(LifecycleError::StatePersist(format!(
        "injected fork failure after spawn: {reason}"
    )))
}

pub(super) fn cleanup_fork_mcp_artifacts(
    workspace: &Path,
    agent_id: &AgentId,
    mcp_config_path: &Path,
    profile_launch: &crate::provider::ProviderProfileLaunch,
) {
    let _ = std::fs::remove_file(mcp_config_path);
    let _ = std::fs::remove_file(
        workspace
            .join(".team/runtime/provider-env")
            .join(format!("{}.env", agent_id.as_str())),
    );
    if let Some(config_dir) = profile_launch.claude_config_dir.as_ref() {
        let _ = std::fs::remove_dir_all(config_dir.parent().unwrap_or(config_dir));
    }
}

pub(super) fn leader_id_matches(spec: &Value, agent_id: &AgentId) -> bool {
    spec.get("leader")
        .and_then(|v| v.get("id"))
        .and_then(Value::as_str)
        .map(|id| id == agent_id.as_str())
        .unwrap_or(false)
}

pub(super) fn find_spec_agent<'a>(spec: &'a Value, agent_id: &AgentId) -> Option<&'a Value> {
    let leader_is_agent = spec
        .get("leader")
        .and_then(|v| v.get("id"))
        .and_then(Value::as_str)
        .map(|id| id == agent_id.as_str())
        .unwrap_or(false);
    if leader_is_agent {
        return None;
    }
    spec.get("agents")?.as_list()?.iter().find(|agent| {
        agent
            .get("id")
            .and_then(Value::as_str)
            .map(|id| id == agent_id.as_str())
            .unwrap_or(false)
    })
}

pub(super) fn append_forked_agent(
    spec: &Value,
    source_agent: &Value,
    source_agent_id: &AgentId,
    as_agent_id: &AgentId,
    label: Option<&str>,
) -> Result<Value, LifecycleError> {
    let mut new_agent = source_agent.clone();
    set_yaml_map_value(
        &mut new_agent,
        "id",
        Value::Str(as_agent_id.as_str().to_string()),
    )?;
    // golden operations.py:315 `str(label or new_agent.get("role") or as_agent_id)` — Python `or`
    // falsiness: an EMPTY-string label/role is falsy and falls through to the next tier.
    // The label IS the forked agent's new role (it feeds the identity prompt — B2 family).
    let role = label
        .filter(|s| !s.is_empty())
        .or_else(|| {
            new_agent
                .get("role")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
        })
        .unwrap_or_else(|| as_agent_id.as_str())
        .to_string();
    set_yaml_map_value(&mut new_agent, "role", Value::Str(role.clone()))?;
    set_yaml_map_value(
        &mut new_agent,
        "forked_from",
        Value::Str(source_agent_id.as_str().to_string()),
    )?;
    set_yaml_map_value(
        &mut new_agent,
        "preferred_for",
        Value::List(vec![
            Value::Str(as_agent_id.as_str().to_string()),
            Value::Str(role),
        ]),
    )?;

    let Value::Map(pairs) = spec else {
        return Err(LifecycleError::Compile(
            "spec root is not a map".to_string(),
        ));
    };
    let mut out = Vec::new();
    for (key, value) in pairs {
        if key == "agents" {
            let mut agents = value
                .as_list()
                .map(|items| items.to_vec())
                .unwrap_or_default();
            agents.push(new_agent.clone());
            out.push((key.clone(), Value::List(agents)));
        } else if key == "runtime" {
            out.push((key.clone(), runtime_with_startup_agent(value, as_agent_id)));
        } else {
            out.push((key.clone(), value.clone()));
        }
    }
    Ok(Value::Map(out))
}

pub(super) fn set_yaml_map_value(
    value: &mut Value,
    key: &str,
    next: Value,
) -> Result<(), LifecycleError> {
    let Value::Map(pairs) = value else {
        return Err(LifecycleError::Compile(
            "agent entry is not a map".to_string(),
        ));
    };
    if let Some((_, existing)) = pairs.iter_mut().find(|(k, _)| k == key) {
        *existing = next;
    } else {
        pairs.push((key.to_string(), next));
    }
    Ok(())
}

pub(super) fn runtime_with_startup_agent(runtime: &Value, agent_id: &AgentId) -> Value {
    let Value::Map(pairs) = runtime else {
        return runtime.clone();
    };
    let mut out = Vec::new();
    let mut saw_startup = false;
    for (key, value) in pairs {
        if key == "startup_order" {
            saw_startup = true;
            let mut order = value
                .as_list()
                .map(|items| items.to_vec())
                .unwrap_or_default();
            let already_present = order.iter().any(|item| {
                item.as_str()
                    .map(|id| id == agent_id.as_str())
                    .unwrap_or(false)
            });
            if !already_present {
                order.push(Value::Str(agent_id.as_str().to_string()));
            }
            out.push((key.clone(), Value::List(order)));
        } else {
            out.push((key.clone(), value.clone()));
        }
    }
    if !saw_startup {
        out.push((
            "startup_order".to_string(),
            Value::List(vec![Value::Str(agent_id.as_str().to_string())]),
        ));
    }
    Value::Map(out)
}

pub(super) fn upsert_forked_agent_state(
    state: &mut serde_json::Value,
    source_agent_id: &AgentId,
    as_agent_id: &AgentId,
    spec_agent: &Value,
    safety: &DangerousApproval,
    plan: &crate::provider::CommandPlan,
    profile_launch: &crate::provider::ProviderProfileLaunch,
    spawn: &crate::transport::SpawnResult,
    spawn_cwd: &Path,
    profile_dir: Option<&Path>,
) -> Result<(), LifecycleError> {
    if !state.is_object() {
        *state = serde_json::json!({});
    }
    let Some(root) = state.as_object_mut() else {
        return Err(LifecycleError::StatePersist(
            "runtime state root is not an object".to_string(),
        ));
    };
    let agents = root
        .entry("agents".to_string())
        .or_insert_with(|| serde_json::json!({}));
    if !agents.is_object() {
        *agents = serde_json::json!({});
    }
    let Some(agent_map) = agents.as_object_mut() else {
        return Err(LifecycleError::StatePersist(
            "runtime state agents is not an object".to_string(),
        ));
    };
    let provider = spec_agent
        .get("provider")
        .and_then(Value::as_str)
        .unwrap_or("codex");
    let mut entry = serde_json::Map::new();
    entry.insert("status".to_string(), serde_json::json!("running"));
    entry.insert("provider".to_string(), serde_json::json!(provider));
    entry.insert(
        "agent_id".to_string(),
        serde_json::json!(as_agent_id.as_str()),
    );
    entry.insert(
        "window".to_string(),
        serde_json::json!(as_agent_id.as_str()),
    );
    entry.insert(
        "forked_from".to_string(),
        serde_json::json!(source_agent_id.as_str()),
    );
    entry.insert(
        "spawn_cwd".to_string(),
        serde_json::json!(spawn_cwd.to_string_lossy().to_string()),
    );
    entry.insert(
        "pane_id".to_string(),
        serde_json::json!(spawn.pane_id.as_str()),
    );
    if let Some(pid) = spawn.child_pid {
        entry.insert("pane_pid".to_string(), serde_json::json!(pid));
    }
    for key in [
        "auth_mode",
        "model",
        "model_source",
        "profile",
        "_profile_dir",
        "role",
        // 0.4.x provider effort MVP step 8: fork inherits compiled effort.
        "effort",
    ] {
        if let Some(value) = spec_agent.get(key) {
            entry.insert(key.to_string(), yaml_value_to_json(value));
        }
    }
    if spec_agent.get("profile").is_some() && !entry.contains_key("_profile_dir") {
        if let Some(profile_dir) = profile_dir {
            entry.insert(
                "_profile_dir".to_string(),
                serde_json::json!(profile_dir.to_string_lossy().to_string()),
            );
        }
    }
    entry.insert("session_id".to_string(), serde_json::Value::Null);
    entry.insert("rollout_path".to_string(), serde_json::Value::Null);
    entry.insert("captured_at".to_string(), serde_json::Value::Null);
    entry.insert("captured_via".to_string(), serde_json::Value::Null);
    entry.insert(
        "attribution_confidence".to_string(),
        serde_json::Value::Null,
    );
    persist_command_plan_state(&mut entry, plan, profile_launch);
    agent_map.insert(
        as_agent_id.as_str().to_string(),
        serde_json::Value::Object(entry),
    );
    if let Some(entry) = agent_map
        .get_mut(as_agent_id.as_str())
        .and_then(serde_json::Value::as_object_mut)
    {
        persist_effective_approval_policy(entry, safety);
    }
    Ok(())
}
