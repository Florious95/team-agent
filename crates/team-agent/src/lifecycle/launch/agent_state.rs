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

pub(super) fn running_agent_state(
    agent: &Value,
    id: &str,
    provider: Provider,
    workspace: &Path,
    spawn_cwd: &Path,
    spawned_at: &str,
    team_id: &str,
    pane_id: Option<&str>,
    pane_pid: Option<u32>,
    safety: &DangerousApproval,
    started_agent: Option<&StartedAgent>,
    profile_dir: Option<&Path>,
) -> Result<serde_json::Value, LifecycleError> {
    let model = agent.get("model").and_then(Value::as_str);
    let auth_mode = agent
        .get("auth_mode")
        .and_then(Value::as_str)
        .and_then(parse_auth_mode)
        .unwrap_or(AuthMode::Subscription);
    let profile = agent
        .get("profile")
        .map(yaml_value_to_json)
        .unwrap_or(serde_json::Value::Null);
    let window = started_agent
        .and_then(|started| started.layout_window.as_ref())
        .map(WindowName::as_str)
        .or_else(|| agent.get("window").and_then(Value::as_str))
        .unwrap_or(id);
    let mcp_config = crate::provider::get_adapter(provider)
        .mcp_config(auth_mode)
        .map_err(|e| LifecycleError::Provider(e.to_string()))?;
    let mcp_config = resolve_mcp_config(mcp_config, workspace, id, team_id);
    let mcp_config_path =
        write_worker_mcp_config_for_provider(workspace, id, &mcp_config, Some(provider))?;
    let mut state = serde_json::Map::new();
    state.insert("status".to_string(), serde_json::json!("running"));
    state.insert("provider".to_string(), serde_json::json!(provider));
    state.insert("agent_id".to_string(), serde_json::json!(id));
    state.insert(
        "model".to_string(),
        model.map_or(serde_json::Value::Null, |m| serde_json::json!(m)),
    );
    state.insert("auth_mode".to_string(), serde_json::json!(auth_mode));
    // 0.4.x provider effort MVP step 8: persist resolved effort so restart
    // / resume reads the same value (no re-resolution from role/team).
    if let Some(effort_str) = agent.get("effort").and_then(Value::as_str) {
        if !effort_str.is_empty() {
            state.insert("effort".to_string(), serde_json::json!(effort_str));
        }
    }
    state.insert("profile".to_string(), profile);
    if agent.get("profile").is_some() {
        if let Some(profile_dir) = profile_dir {
            state.insert(
                "_profile_dir".to_string(),
                serde_json::json!(profile_dir.to_string_lossy().to_string()),
            );
        }
    }
    state.insert("window".to_string(), serde_json::json!(window));
    state.insert(
        "mcp_config".to_string(),
        serde_json::json!(mcp_config_path.to_string_lossy().to_string()),
    );
    state.insert(
        "permissions".to_string(),
        permissions_json(agent, id, provider)
            .map_err(|e| LifecycleError::Compile(e.to_string()))?,
    );
    persist_effective_approval_policy(&mut state, safety);
    state.insert("session_id".to_string(), serde_json::Value::Null);
    state.insert("rollout_path".to_string(), serde_json::Value::Null);
    state.insert("captured_at".to_string(), serde_json::Value::Null);
    state.insert("captured_via".to_string(), serde_json::Value::Null);
    state.insert(
        "attribution_confidence".to_string(),
        serde_json::Value::Null,
    );
    if let Some(started_agent) = started_agent {
        persist_started_agent_plan_state(&mut state, started_agent);
        if let Some(layout_window) = started_agent.layout_window.as_ref() {
            state.insert(
                "layout_window".to_string(),
                serde_json::json!(layout_window.as_str()),
            );
        }
        if let Some(layout_index) = started_agent.layout_index {
            state.insert("layout_index".to_string(), serde_json::json!(layout_index));
        }
        if let Some(pane_index) = started_agent.pane_index {
            state.insert("pane_index".to_string(), serde_json::json!(pane_index));
        }
        if !matches!(started_agent.display, WorkerDisplay::Blocked { .. }) {
            state.insert(
                "display".to_string(),
                serde_json::to_value(&started_agent.display)
                    .map_err(|e| LifecycleError::StatePersist(e.to_string()))?,
            );
        }
    }
    state.insert(
        "spawn_cwd".to_string(),
        serde_json::json!(spawn_cwd.to_string_lossy().to_string()),
    );
    state.insert("spawned_at".to_string(), serde_json::json!(spawned_at));
    if let Some(pane_id) = pane_id.filter(|pane| !pane.is_empty()) {
        state.insert("pane_id".to_string(), serde_json::json!(pane_id));
    }
    if let Some(pane_pid) = pane_pid {
        state.insert("pane_pid".to_string(), serde_json::json!(pane_pid));
    }
    Ok(serde_json::Value::Object(state))
}

pub(crate) fn effective_approval_policy(safety: &DangerousApproval) -> serde_json::Value {
    serde_json::json!({
        "enabled": safety.enabled,
        "source": dangerous_approval_source_str(safety.source),
        "inherited": safety.inherited,
        "explicit_yes_confirmed": safety.enabled && matches!(safety.source, DangerousApprovalSource::RuntimeConfig),
        "provider": safety.provider,
        "flag": safety.flag,
        "worker_capability_above_leader": safety.worker_capability_above_leader,
    })
}

pub(crate) fn persist_effective_approval_policy(
    agent_state: &mut serde_json::Map<String, serde_json::Value>,
    safety: &DangerousApproval,
) {
    agent_state.insert(
        "effective_approval_policy".to_string(),
        effective_approval_policy(safety),
    );
}

pub(super) fn dangerous_approval_source_str(source: DangerousApprovalSource) -> &'static str {
    match source {
        DangerousApprovalSource::RuntimeConfig => "runtime_config",
        DangerousApprovalSource::LeaderProcess => "leader_process",
        DangerousApprovalSource::Disabled => "disabled",
    }
}
