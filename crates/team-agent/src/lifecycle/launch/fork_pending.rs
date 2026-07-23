use super::*;

#[allow(clippy::too_many_arguments)]
pub(crate) fn upsert_pending_forked_agent_state(
    state: &mut serde_json::Value,
    source_agent_id: &AgentId,
    as_agent_id: &AgentId,
    spec_agent: &Value,
    safety: &DangerousApproval,
    plan: &crate::provider::CommandPlan,
    profile_launch: &crate::provider::ProviderProfileLaunch,
    spawn: &crate::transport::SpawnResult,
    profile_dir: Option<&Path>,
    dynamic_role_file: &Path,
    pending: &crate::provider::session::PendingContextFork,
    spawn_epoch: u64,
) -> Result<(), LifecycleError> {
    let root = state.as_object_mut().ok_or_else(|| {
        LifecycleError::StatePersist("runtime state root is not an object".to_string())
    })?;
    let agents = root
        .entry("agents".to_string())
        .or_insert_with(|| serde_json::json!({}))
        .as_object_mut()
        .ok_or_else(|| {
            LifecycleError::StatePersist("runtime state agents is not an object".to_string())
        })?;
    let mut entry = serde_json::Map::new();
    entry.insert("status".to_string(), serde_json::json!("running"));
    entry.insert(
        "capture_state".to_string(),
        serde_json::json!("pending_context_fork"),
    );
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
        "fork_source_session_id".to_string(),
        serde_json::json!(pending.source_session_id.as_str()),
    );
    entry.insert(
        "pending_target_agent".to_string(),
        serde_json::json!(pending.target_agent),
    );
    entry.insert(
        "dynamic_role_file".to_string(),
        serde_json::json!(dynamic_role_file.to_string_lossy().to_string()),
    );
    entry.insert(
        "role_source_ownership".to_string(),
        serde_json::json!("managed"),
    );
    entry.insert(
        "spawn_cwd".to_string(),
        serde_json::json!(pending
            .scanner_context
            .spawn_cwd
            .to_string_lossy()
            .to_string()),
    );
    entry.insert(
        "pane_id".to_string(),
        serde_json::json!(spawn.pane_id.as_str()),
    );
    entry.insert(
        "spawned_at".to_string(),
        serde_json::json!(pending.spawned_at),
    );
    entry.insert("spawn_epoch".to_string(), serde_json::json!(spawn_epoch));
    entry.insert(
        "pending_grace_secs".to_string(),
        serde_json::json!(pending.grace_deadline.as_secs()),
    );
    if let Some(pid) = spawn.child_pid {
        entry.insert("pane_pid".to_string(), serde_json::json!(pid));
    }
    for key in [
        "provider",
        "auth_mode",
        "model",
        "profile",
        "role",
        "effort",
    ] {
        if let Some(value) = spec_agent.get(key) {
            entry.insert(key.to_string(), yaml_value_to_json(value));
        }
    }
    if spec_agent.get("profile").is_some() {
        if let Some(profile_dir) = profile_dir {
            entry.insert(
                "_profile_dir".to_string(),
                serde_json::json!(profile_dir.to_string_lossy().to_string()),
            );
        }
    }
    persist_command_plan_state(&mut entry, plan, profile_launch);
    persist_effective_approval_policy(&mut entry, safety);
    agents.insert(
        as_agent_id.as_str().to_string(),
        serde_json::Value::Object(entry),
    );
    Ok(())
}
