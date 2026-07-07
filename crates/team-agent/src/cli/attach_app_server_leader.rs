use super::*;

/// `team-agent attach-app-server-leader` writes a typed codex_app_server
/// leader_receiver through the leader lease primitive.
pub fn cmd_attach_app_server_leader(
    args: &AttachAppServerLeaderArgs,
) -> Result<CmdResult, CliError> {
    let mut value = crate::leader::attach_app_server_leader(
        &args.workspace,
        args.team.as_deref(),
        &args.socket,
        &args.thread_id,
    )
    .map_err(|error| CliError::Runtime(error.to_string()))?;
    // E7 register-after-success (host-leader-registry-design §8.4): mirror
    // the tmux binding flow so app-server bindings appear in the host
    // discovery index too. Registry write failure is discoverability
    // degradation, not binding failure.
    if value.get("ok").and_then(serde_json::Value::as_bool) == Some(true) {
        register_app_server_binding_in_registry(&args.workspace, args.team.as_deref(), &mut value);
    }
    Ok(CmdResult::from_json(value, args.json))
}

fn register_app_server_binding_in_registry(
    workspace: &std::path::Path,
    team: Option<&str>,
    response: &mut serde_json::Value,
) {
    let Ok(state) = crate::state::persist::load_runtime_state(workspace) else {
        return;
    };
    let team_key = match team.filter(|t| !t.is_empty()) {
        Some(t) => t.to_string(),
        None => crate::state::projection::team_state_key(&state),
    };
    let receiver = state
        .get("teams")
        .and_then(|v| v.as_object())
        .and_then(|teams| teams.get(&team_key))
        .and_then(|t| t.get("leader_receiver"))
        .cloned();
    let Some(receiver) = receiver else {
        return;
    };
    let transport_kind = receiver
        .get("transport_kind")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("codex_app_server")
        .to_string();
    let channel = receiver
        .get("app_server")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    let owner_epoch = receiver
        .get("owner_epoch")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    let entry = crate::leader::registry::build_entry(
        workspace,
        &team_key,
        &transport_kind,
        channel,
        owner_epoch,
        "attach-app-server-leader",
        chrono::Utc::now().to_rfc3339(),
    );
    let event_log = crate::event_log::EventLog::new(workspace);
    let write_result = crate::leader::registry::write_entry_best_effort(&entry);
    let registry_status = match &write_result {
        Some(path) => {
            let _ = event_log.write(
                crate::leader::registry::EVENT_REGISTERED,
                serde_json::json!({
                    "path": path.display().to_string(),
                    "team_key": team_key,
                    "workspace_hash": entry.workspace_hash,
                    "source": "attach-app-server-leader",
                }),
            );
            serde_json::json!({"status": "registered", "path": path.display().to_string()})
        }
        None => {
            let _ = event_log.write(
                crate::leader::registry::EVENT_WRITE_FAILED,
                serde_json::json!({
                    "team_key": team_key,
                    "source": "attach-app-server-leader",
                }),
            );
            serde_json::json!({"status": "write_failed"})
        }
    };
    if let Some(obj) = response.as_object_mut() {
        obj.insert("leader_registry".to_string(), registry_status);
    }
}
