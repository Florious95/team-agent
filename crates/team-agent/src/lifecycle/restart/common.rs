use super::*;

pub(super) fn spawn_agent_window(
    workspace: &Path,
    session_name: &SessionName,
    agent_id: &AgentId,
    agent: &serde_json::Value,
    resume_session_id: Option<&SessionId>,
    into_existing_session: bool,
    transport: &dyn crate::transport::Transport,
) -> Result<crate::transport::SpawnResult, LifecycleError> {
    let provider = agent_provider(agent);
    let auth_mode = agent_auth_mode(agent);
    let model = agent.get("model").and_then(|v| v.as_str());
    let adapter = crate::provider::get_adapter(provider);
    let resume_session_id = if adapter.caps().resume {
        resume_session_id
    } else {
        None
    };
    // Contract C / F6.4: thread compiled role/tools/MCP context through restart as well —
    // a restarted worker must come back up with the SAME callable MCP capability + role
    // prompt as a fresh launch, else `report_result` becomes unreachable after every restart.
    let role = agent.get("role").and_then(|v| v.as_str());
    let safety = crate::lifecycle::launch::effective_runtime_config_for_worker_spawn()?;
    let tools = crate::lifecycle::launch::worker_tool_refs(agent_tool_strings(agent), &safety);
    let tool_refs: Vec<&str> = tools.iter().map(String::as_str).collect();
    let mcp_config = adapter
        .mcp_config(auth_mode)
        .map_err(|e| LifecycleError::Provider(e.to_string()))?;
    let mut argv = match resume_session_id {
        Some(session_id) => adapter
            .build_resume_command_with_context(
                Some(session_id),
                auth_mode,
                Some(&mcp_config),
                role,
                model,
                &tool_refs,
            )
            .map_err(|e| LifecycleError::Provider(e.to_string()))?,
        None => adapter
            .build_command_with_tools(auth_mode, Some(&mcp_config), role, model, &tool_refs)
            .map_err(|e| LifecycleError::Provider(e.to_string()))?,
    };
    // owner_team_id resolution: prefer the runtime-state row's `owner_team_id` (set by
    // launch/restart); fall back to the active team key for paths that don't write the
    // row first (e.g. add-agent calls spawn before upserting team metadata).
    let state_for_team =
        crate::state::persist::load_runtime_state(workspace).unwrap_or(serde_json::json!({}));
    let team_id = agent
        .get("owner_team_id")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .or_else(|| {
            let key = crate::messaging::leader_receiver::active_team_key(workspace, &state_for_team);
            (!key.is_empty()).then_some(key)
        });
    crate::lifecycle::launch::fill_spawn_placeholders_full(
        &mut argv,
        workspace,
        agent_id.as_str(),
        team_id.as_deref(),
    );
    let window = WindowName::new(agent_id.as_str());
    let env = crate::lifecycle::launch::inherited_env_with_team_overrides(
        workspace,
        agent_id.as_str(),
        team_id.as_deref(),
    );
    let result = if into_existing_session {
        transport.spawn_into(session_name, &window, &argv, workspace, &env)
    } else {
        transport.spawn_first(session_name, &window, &argv, workspace, &env)
    };
    let spawn = result.map_err(|e| LifecycleError::Transport(e.to_string()))?;
    let _ = adapter.handle_startup_prompts(
        transport,
        &crate::transport::Target::Pane(spawn.pane_id.clone()),
        30,
        0.5,
    );
    Ok(spawn)
}

pub(super) fn start_coordinator_for_workspace(workspace: &Path) -> Result<bool, LifecycleError> {
    let workspace = crate::coordinator::WorkspacePath::new(workspace.to_path_buf());
    crate::coordinator::start_coordinator(&workspace)
        .map(|report| report.ok)
        .map_err(|e| LifecycleError::StatePersist(e.to_string()))
}

pub(super) fn state_session_name(state: &serde_json::Value) -> SessionName {
    state
        .get("session_name")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(SessionName::new)
        .unwrap_or_else(|| SessionName::new("team-agent"))
}

pub(super) fn session_name_present(state: &serde_json::Value) -> bool {
    state
        .get("session_name")
        .and_then(|v| v.as_str())
        .map(|s| !s.is_empty())
        .unwrap_or(false)
}

pub(super) fn session_live_or_default(
    transport: &dyn crate::transport::Transport,
    session_name: &SessionName,
    default: bool,
) -> bool {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        transport.has_session(session_name)
    })) {
        Ok(Ok(live)) => live,
        Ok(Err(_)) => false,
        Err(_) => default,
    }
}

pub(super) fn agent_provider(agent: &serde_json::Value) -> Provider {
    agent
        .get("provider")
        .and_then(|v| v.as_str())
        .and_then(parse_provider)
        .unwrap_or(Provider::Codex)
}

pub(super) fn agent_auth_mode(agent: &serde_json::Value) -> AuthMode {
    agent
        .get("auth_mode")
        .and_then(|v| v.as_str())
        .and_then(parse_auth_mode)
        .unwrap_or(AuthMode::Subscription)
}

pub(super) fn agent_session_id(agent: &serde_json::Value) -> Option<SessionId> {
    agent
        .get("session_id")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(SessionId::new)
}

pub(super) fn agent_rollout_path(agent: &serde_json::Value) -> Option<RolloutPath> {
    agent
        .get("rollout_path")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(RolloutPath::new)
}

/// Tools list off an agent's runtime state entry (`tools: [...]`). Restart paths
/// don't have the full spec object, only the runtime state — so they read tools from
/// the state row, falling back to an empty list. Contract C requires the worker
/// command be built with the tool list, even on restart.
pub(super) fn agent_tool_strings(agent: &serde_json::Value) -> Vec<String> {
    agent
        .get("tools")
        .and_then(|v| v.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|v| v.as_str())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

pub(super) fn agent_window(agent: &serde_json::Value, agent_id: &AgentId) -> String {
    agent
        .get("window")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| agent_id.as_str())
        .to_string()
}

pub(super) fn parse_provider(raw: &str) -> Option<Provider> {
    match raw {
        "claude" => Some(Provider::Claude),
        "claude_code" => Some(Provider::ClaudeCode),
        "codex" => Some(Provider::Codex),
        "gemini_cli" => Some(Provider::GeminiCli),
        "fake" => Some(Provider::Fake),
        _ => None,
    }
}

pub(super) fn provider_wire(provider: Provider) -> &'static str {
    match provider {
        Provider::Claude => "claude",
        Provider::ClaudeCode => "claude_code",
        Provider::Codex => "codex",
        Provider::GeminiCli => "gemini_cli",
        Provider::Fake => "fake",
    }
}

pub(super) fn parse_auth_mode(raw: &str) -> Option<AuthMode> {
    match raw {
        "subscription" => Some(AuthMode::Subscription),
        "official_api" => Some(AuthMode::OfficialApi),
        "compatible_api" => Some(AuthMode::CompatibleApi),
        _ => None,
    }
}

pub(super) fn load_team_spec(workspace: &Path) -> Result<YamlValue, LifecycleError> {
    let spec_path = workspace.join("team.spec.yaml");
    if !spec_path.exists() {
        return Err(LifecycleError::TeamSelect(format!(
            "missing spec: {}",
            spec_path.display()
        )));
    }
    let text = std::fs::read_to_string(&spec_path)
        .map_err(|e| LifecycleError::Compile(format!("{}: {e}", spec_path.display())))?;
    yaml::loads(&text).map_err(|e| LifecycleError::Compile(e.to_string()))
}

pub(super) fn find_spec_agent<'a>(spec: &'a YamlValue, agent_id: &AgentId) -> Option<&'a YamlValue> {
    let leader_is_agent = spec
        .get("leader")
        .and_then(|v| v.get("id"))
        .and_then(YamlValue::as_str)
        .map(|id| id == agent_id.as_str())
        .unwrap_or(false);
    if leader_is_agent {
        return None;
    }
    spec.get("agents")?
        .as_list()?
        .iter()
        .find(|agent| {
            agent
                .get("id")
                .and_then(YamlValue::as_str)
                .map(|id| id == agent_id.as_str())
                .unwrap_or(false)
        })
}

pub(super) fn unknown_worker(agent_id: &AgentId) -> LifecycleError {
    LifecycleError::RequirementUnmet(format!("unknown worker agent id: {agent_id}"))
}

pub(super) fn state_session_name_from_spec(state: &serde_json::Value, spec: &YamlValue) -> SessionName {
    state
        .get("session_name")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(SessionName::new)
        .or_else(|| {
            spec.get("runtime")
                .and_then(|v| v.get("session_name"))
                .and_then(YamlValue::as_str)
                .map(SessionName::new)
        })
        .or_else(|| {
            spec.get("team")
                .and_then(|v| v.get("name"))
                .and_then(YamlValue::as_str)
                .map(|name| SessionName::new(format!("team-{name}")))
        })
        .unwrap_or_else(|| SessionName::new("team-agent"))
}

pub(super) fn mark_agent_stopped(
    state: &mut serde_json::Value,
    agent_id: &AgentId,
    spec_agent: &YamlValue,
    window: &str,
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
        .and_then(YamlValue::as_str)
        .unwrap_or("codex");
    let entry = agent_map
        .entry(agent_id.as_str().to_string())
        .or_insert_with(|| serde_json::json!({}));
    if !entry.is_object() {
        *entry = serde_json::json!({});
    }
    let Some(obj) = entry.as_object_mut() else {
        return Err(LifecycleError::StatePersist(
            "agent state is not an object".to_string(),
        ));
    };
    obj.insert("status".to_string(), serde_json::json!("stopped"));
    obj.insert("provider".to_string(), serde_json::json!(provider));
    obj.insert("agent_id".to_string(), serde_json::json!(agent_id.as_str()));
    obj.insert("last_window".to_string(), serde_json::json!(window));
    obj.remove("window");
    obj.remove("pane_id");
    Ok(())
}

pub(super) fn mark_agent_running_noop(
    state: &mut serde_json::Value,
    agent_id: &AgentId,
    session_name: &SessionName,
    window: &str,
) -> Result<(), LifecycleError> {
    if !state.is_object() {
        *state = serde_json::json!({});
    }
    let Some(root) = state.as_object_mut() else {
        return Err(LifecycleError::StatePersist(
            "runtime state root is not an object".to_string(),
        ));
    };
    root.insert(
        "session_name".to_string(),
        serde_json::json!(session_name.as_str()),
    );
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
    let entry = agent_map
        .entry(agent_id.as_str().to_string())
        .or_insert_with(|| serde_json::json!({}));
    if !entry.is_object() {
        *entry = serde_json::json!({});
    }
    let Some(obj) = entry.as_object_mut() else {
        return Err(LifecycleError::StatePersist(
            "agent state is not an object".to_string(),
        ));
    };
    obj.insert("status".to_string(), serde_json::json!("running"));
    obj.insert("agent_id".to_string(), serde_json::json!(agent_id.as_str()));
    obj.insert("window".to_string(), serde_json::json!(window));
    Ok(())
}

pub(super) fn write_start_agent_noop_event(
    workspace: &Path,
    agent_id: &AgentId,
    target: &str,
    coordinator_started: bool,
) -> Result<(), LifecycleError> {
    crate::event_log::EventLog::new(workspace)
        .write(
            "start_agent.noop",
            serde_json::json!({
                "agent_id": agent_id.as_str(),
                "target": target,
                "coordinator": coordinator_started,
            }),
        )
        .map_err(|e| LifecycleError::StatePersist(e.to_string()))?;
    Ok(())
}

pub(super) fn window_exists(
    transport: &dyn crate::transport::Transport,
    session_name: &SessionName,
    window: &str,
) -> bool {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        transport.list_windows(session_name)
    })) {
        Ok(Ok(windows)) => windows.iter().any(|w| w.as_str() == window),
        Ok(Err(_)) | Err(_) => false,
    }
}

pub(super) fn close_agent_display(state: &mut serde_json::Value, agent_id: &AgentId) {
    let Some(display) = state
        .get_mut("agents")
        .and_then(|v| v.as_object_mut())
        .and_then(|agents| agents.get_mut(agent_id.as_str()))
        .and_then(|agent| agent.get_mut("display"))
        .and_then(|display| display.as_object_mut())
    else {
        return;
    };
    let backend = display
        .get("backend")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    // golden operations.py:88-92: close_ghostty_display (display/close.py:17-48) mutates NOTHING in the
    // persisted state for a ghostty_window; only the ghostty_workspace slot is relabeled
    // (close.py:84-85: status="stopped", pane_title=f"stopped: {agent_id}") and re-assigned back.
    if backend == "ghostty_workspace" {
        display.insert("status".to_string(), serde_json::json!("stopped"));
        display.insert(
            "pane_title".to_string(),
            serde_json::json!(format!("stopped: {}", agent_id.as_str())),
        );
    }
}

pub(super) fn discard_agent_session_fields(
    state: &mut serde_json::Value,
    agent_id: &AgentId,
) -> Result<(), LifecycleError> {
    let Some(agent) = state
        .get_mut("agents")
        .and_then(|v| v.as_object_mut())
        .and_then(|agents| agents.get_mut(agent_id.as_str()))
    else {
        return Err(unknown_worker(agent_id));
    };
    let Some(obj) = agent.as_object_mut() else {
        return Err(LifecycleError::StatePersist(
            "agent state is not an object".to_string(),
        ));
    };
    // golden operations.py:119 pops EXACTLY `[*SESSION_CAPTURE_FIELDS, "_pending_session_id"]`.
    // spawn_cwd lives in SESSION_STATE_FIELDS (state.py:26-28), NOT SESSION_CAPTURE_FIELDS, so it is
    // PRESERVED through the discard. (Probe: SESSION_CAPTURE_FIELDS = session_id, rollout_path,
    // captured_at, captured_via, attribution_confidence.)
    for key in [
        "session_id",
        "rollout_path",
        "captured_at",
        "captured_via",
        "attribution_confidence",
        "_pending_session_id",
    ] {
        obj.remove(key);
    }
    obj.insert("status".to_string(), serde_json::json!("stopped"));
    Ok(())
}

pub(super) fn agent_is_running(
    state: &serde_json::Value,
    agent_id: &AgentId,
    transport: &dyn crate::transport::Transport,
) -> bool {
    let agent_state = state
        .get("agents")
        .and_then(|v| v.get(agent_id.as_str()));
    let status = agent_state
        .and_then(|v| v.get("status"))
        .and_then(|v| v.as_str())
        .map(str::to_ascii_lowercase);
    // golden agents.py:247-252 (_is_running): True ONLY for {running,busy}; EVERY other status (including
    // stopped/paused/failed/removed) falls through to the session_name + tmux-window-exists check.
    if matches!(status.as_deref(), Some("running" | "busy")) {
        return true;
    }
    let Some(session_name) = state
        .get("session_name")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(SessionName::new)
    else {
        return false;
    };
    let window = agent_state
        .and_then(|v| v.get("window"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| agent_id.as_str());
    window_exists(transport, &session_name, window)
}

pub(super) fn is_dynamic_agent(
    state: &serde_json::Value,
    spec_agent: &YamlValue,
    agent_id: &AgentId,
) -> bool {
    let dynamic_role = state
        .get("agents")
        .and_then(|v| v.get(agent_id.as_str()))
        .and_then(|v| v.get("dynamic_role_file"))
        .and_then(|v| v.as_str())
        .is_some_and(|s| !s.is_empty());
    dynamic_role
        || spec_agent
            .get("forked_from")
            .and_then(YamlValue::as_str)
            .is_some_and(|s| !s.is_empty())
}
