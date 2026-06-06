//! lifecycle::launch —— 冷启 / quick-start / 危险审批探测 + add/fork / plan 起步与推进。

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::model::enums::{AuthMode, DisplayBackend, PaneLiveness, Provider};
use crate::model::ids::AgentId;
use crate::model::permissions::{self, AgentPermissionInput};
use crate::model::yaml::{self, Value};
use crate::state::persist::{load_runtime_state, save_runtime_state};
use crate::transport::{SessionName, Target, Transport, WindowName};

use super::*;

// ── lifecycle::launch —— 冷启 / quick-start / 危险审批探测 ──────────────────

/// `launch(spec_path, dry_run, auto_approve, skip_profile_smoke)`(`launch/core.py:29`)。
/// 冷启全队:路由 tasks、resolve 权限/危险审批门、session 冲突检查(冲突 →
/// `SessionConflict` 拒绝不 kill)、按 startup 顺序起每个 worker、捕获 session、开显示、
/// 写 state/team_state、attach leader receiver。
pub fn launch(
    spec_path: &Path,
    dry_run: bool,
    auto_approve: bool,
    skip_profile_smoke: bool,
) -> Result<LaunchReport, LifecycleError> {
    // CP-1: bind the spawn backend to the per-team socket (derived from the run workspace, the same
    // path the daemon/CLI derive from) so spawn + later has_session/inject/kill all hit one server.
    let team_dir = spec_path.parent().unwrap_or_else(|| Path::new("."));
    let transport = crate::tmux_backend::TmuxBackend::for_workspace(&team_workspace(team_dir));
    launch_with_transport(
        spec_path,
        dry_run,
        auto_approve,
        skip_profile_smoke,
        &transport,
    )
}

pub fn launch_with_transport(
    spec_path: &Path,
    dry_run: bool,
    auto_approve: bool,
    skip_profile_smoke: bool,
    transport: &dyn Transport,
) -> Result<LaunchReport, LifecycleError> {
    let _ = skip_profile_smoke;
    if !spec_path.exists() {
        return Err(LifecycleError::Compile(format!(
            "spec path not found: {}",
            spec_path.display()
        )));
    }
    let text = std::fs::read_to_string(spec_path).map_err(|e| {
        LifecycleError::Compile(format!("{}: {e}", spec_path.display()))
    })?;
    let spec = yaml::loads(&text).map_err(|e| LifecycleError::Compile(e.to_string()))?;
    let session_name = spec_session_name(&spec);
    let safety = effective_runtime_config(&spec)?;
    if safety.enabled && !safety.inherited && !auto_approve && !dry_run {
        return Err(LifecycleError::DangerousApprovalRequired(
            "runtime dangerous_auto_approve is enabled".to_string(),
        ));
    }
    if !dry_run && transport_has_session(transport, &session_name) {
        return Err(LifecycleError::SessionConflict(format!(
            "tmux session already exists: {}",
            session_name.as_str()
        )));
    }
    let permissions = spec_agents(&spec)
        .into_iter()
        .map(|agent| PermissionSummary {
            agent_id: agent,
            raw: serde_json::json!({"source": "compiled_spec"}),
        })
        .collect::<Vec<_>>();
    write_launch_permission_audit(&team_workspace(spec_path.parent().unwrap_or_else(|| Path::new("."))), &safety)?;
    let routes = spec_routes(&spec);
    let started = if dry_run {
        Vec::new()
    } else {
        let started = spawn_agents(spec_path, &spec, &session_name, &safety, transport)?;
        persist_spawn_agent_state(spec_path, &spec, &session_name, transport, &started)?;
        started
    };
    Ok(LaunchReport {
        session_name,
        started,
        dry_run,
        routes,
        permissions,
        safety,
        leader_receiver_attached: false,
    })
}

fn spawn_agents(
    spec_path: &Path,
    spec: &Value,
    session_name: &SessionName,
    safety: &DangerousApproval,
    transport: &dyn Transport,
) -> Result<Vec<StartedAgent>, LifecycleError> {
    let team_dir = spec_path.parent().unwrap_or_else(|| Path::new("."));
    let workspace = team_workspace(team_dir);
    let mut started = Vec::new();
    for agent in spec_agent_values(spec) {
        let Some(agent_id_raw) = agent.get("id").and_then(Value::as_str) else {
            continue;
        };
        if agent_is_paused(agent) {
            continue;
        }
        let agent_id = AgentId::new(agent_id_raw);
        let provider = agent
            .get("provider")
            .and_then(Value::as_str)
            .and_then(parse_provider)
            .unwrap_or(Provider::Codex);
        let auth_mode = agent
            .get("auth_mode")
            .and_then(Value::as_str)
            .and_then(parse_auth_mode)
            .unwrap_or(AuthMode::Subscription);
        let model = agent.get("model").and_then(Value::as_str);
        let adapter = crate::provider::get_adapter(provider);
        // Contract C / F6.4: pass the COMPILED agent context (resolved role/system prompt,
        // tools list, per-worker MCP config) into command construction so a real worker
        // has both the role instruction AND the callable Team Agent MCP capability.
        // probe5 RED proved that `build_command(.., None, None, ..)` left the worker
        // without `report_result`; placeholders are substituted at spawn time.
        let role = agent.get("role").and_then(Value::as_str);
        let tools = worker_tool_refs(agent_tool_strings(agent), safety);
        let tool_refs: Vec<&str> = tools.iter().map(String::as_str).collect();
        let mcp_team_id =
            runtime_active_team_key_for_spawn(&workspace, spec_path, spec, session_name);
        let process_team_id = process_team_id_for_spawn(&workspace, spec);
        let mcp_config = adapter
            .mcp_config(auth_mode)
            .map_err(|e| LifecycleError::Provider(e.to_string()))?;
        let mcp_config = resolve_mcp_config(mcp_config, &workspace, agent_id_raw, &mcp_team_id);
        let mcp_config_path = write_worker_mcp_config(&workspace, agent_id_raw, &mcp_config)?;
        let mut argv = adapter
            .build_command_with_tools(
                auth_mode,
                Some(&mcp_config),
                role,
                model,
                &tool_refs,
            )
            .map_err(|e| LifecycleError::Provider(e.to_string()))?;
        point_native_mcp_config_at_file(&mut argv, provider, &mcp_config_path);
        fill_spawn_placeholders_full(
            &mut argv,
            &workspace,
            agent_id_raw,
            process_team_id.as_deref(),
        );
        let window = WindowName::new(agent_id_raw);
        let env = inherited_env_with_team_overrides(
            &workspace,
            agent_id_raw,
            process_team_id.as_deref(),
        );
        let spawn = if started.is_empty() {
            transport.spawn_first(session_name, &window, &argv, team_dir, &env)
        } else {
            transport.spawn_into(session_name, &window, &argv, team_dir, &env)
        }
        .map_err(|e| LifecycleError::Transport(e.to_string()))?;
        let _ = adapter.handle_startup_prompts(
            transport,
            &Target::Pane(spawn.pane_id.clone()),
            30,
            0.5,
        );
        if matches!(transport.liveness(&spawn.pane_id), Ok(PaneLiveness::Dead)) {
            continue;
        }
        started.push(StartedAgent {
            agent_id,
            start_mode: StartMode::Fresh,
            target: spawn.pane_id.as_str().to_string(),
            session_id: None,
            rollout_path: None,
            display: WorkerDisplay::Blocked {
                reason: AdaptiveBlockReason::NotImplementedThisPlatform,
            },
        });
    }
    Ok(started)
}

fn persist_spawn_agent_state(
    spec_path: &Path,
    spec: &Value,
    session_name: &SessionName,
    transport: &dyn Transport,
    started: &[StartedAgent],
) -> Result<(), LifecycleError> {
    let team_dir = spec_path.parent().unwrap_or_else(|| Path::new("."));
    let workspace = team_workspace(team_dir);
    let state_path = crate::state::persist::runtime_state_path(&workspace);
    let mut state = if state_path.exists() {
        let text = std::fs::read_to_string(&state_path)
            .map_err(|e| LifecycleError::StatePersist(format!("{}: {e}", state_path.display())))?;
        serde_json::from_str::<serde_json::Value>(&text)
            .map_err(|e| LifecycleError::StatePersist(format!("{}: {e}", state_path.display())))?
    } else {
        serde_json::json!({"agents": {}})
    };
    let team_id = explicit_active_team_key(&state)
        .unwrap_or_else(|| runtime_team_key_for_spec(spec_path, spec, session_name));
    let worker_tmux_socket = launched_worker_tmux_socket(transport, &workspace);
    drop_worker_pane_seeded_owner(
        &mut state,
        &team_id,
        started,
        worker_tmux_socket.as_deref(),
    );
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
    let mut agents = serde_json::Map::new();
    let spawned_at = spawn_timestamp();
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
        let window = agent.get("window").and_then(Value::as_str).unwrap_or(id);
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
        agents.insert(
            id.to_string(),
            running_agent_state(agent, id, provider, &workspace, &spawned_at, &team_id)?,
        );
    }
    if let Some(obj) = state.as_object_mut() {
        obj.insert("agents".to_string(), serde_json::Value::Object(agents));
    } else {
        let mut obj = serde_json::Map::new();
        obj.insert("agents".to_string(), serde_json::Value::Object(agents));
        state = serde_json::Value::Object(obj);
    }
    save_launched_team_state(&workspace, &state)
}

fn save_launched_team_state(workspace: &Path, launched: &serde_json::Value) -> Result<(), LifecycleError> {
    let existing = load_runtime_state(workspace).unwrap_or_else(|_| serde_json::json!({}));
    let launched_key = crate::state::projection::team_state_key(launched);
    let mut launched = launched.clone();
    promote_launched_binding_from_team_entry(&mut launched, &launched_key);
    drop_foreign_seeded_owner(&existing, &launched_key, &mut launched);
    let merged = crate::state::projection::merge_workspace_team_state(&existing, &launched);
    let mut projected = crate::state::projection::project_top_level_view(&merged, &launched_key);
    drop_unbound_top_level_owner(&mut projected);
    save_runtime_state(workspace, &projected).map_err(|e| LifecycleError::StatePersist(e.to_string()))
}

fn promote_launched_binding_from_team_entry(launched: &mut serde_json::Value, launched_key: &str) {
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

fn drop_unbound_top_level_owner(state: &mut serde_json::Value) {
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

fn drop_foreign_seeded_owner(existing: &serde_json::Value, launched_key: &str, launched: &mut serde_json::Value) {
    let Some(pane) = launched
        .get("team_owner")
        .and_then(|owner| owner.get("pane_id"))
        .and_then(serde_json::Value::as_str)
        .filter(|pane| !pane.is_empty())
    else {
        return;
    };
    if owner_pane_belongs_to_other_team(existing, launched_key, pane) {
        seed_unbound_launched_owner(launched, launched_key);
    }
}

fn drop_worker_pane_seeded_owner(
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
    let has_leader_identity_env = leader_pane.is_some()
        || env_nonempty("TEAM_AGENT_LEADER_SESSION_UUID")
        || env_nonempty("TEAM_AGENT_LEADER_SESSION_UUID_OVERRIDE")
        || env_nonempty("TEAM_AGENT_LEADER_PROVIDER")
        || env_nonempty("TEAM_AGENT_ID")
        || env_nonempty("TEAM_AGENT_TEAM_ID");
    let seeded_from_bare_tmux =
        !has_leader_identity_env && tmux_pane.as_deref() == Some(pane);
    let caller_tmux_socket = crate::tmux_backend::socket_name_from_tmux_env();
    if seeded_from_bare_tmux
        && tmux_sockets_match_or_unknown(caller_tmux_socket.as_deref(), worker_tmux_socket)
        && started.iter().any(|agent| agent.target == pane)
    {
        seed_unbound_launched_owner(launched, launched_key);
    }
}

fn launched_worker_tmux_socket(
    transport: &dyn Transport,
    workspace: &Path,
) -> Option<String> {
    if matches!(transport.kind(), crate::transport::BackendKind::Tmux) {
        Some(crate::tmux_backend::socket_name_for_workspace(workspace))
    } else {
        None
    }
}

fn tmux_sockets_match_or_unknown(
    caller_socket: Option<&str>,
    worker_socket: Option<&str>,
) -> bool {
    match (caller_socket, worker_socket) {
        (Some(caller), Some(worker)) => caller == worker,
        (Some(_), None) => false,
        (None, _) => true,
    }
}

fn env_nonempty(key: &str) -> bool {
    std::env::var(key).ok().is_some_and(|value| !value.is_empty())
}

fn seed_unbound_launched_owner(launched: &mut serde_json::Value, launched_key: &str) {
    let provider = launched
        .get("team_owner")
        .and_then(|owner| owner.get("provider"))
        .and_then(serde_json::Value::as_str)
        .filter(|provider| !provider.is_empty())
        .unwrap_or("codex");
    let machine_fingerprint = launched
        .get("team_owner")
        .and_then(|owner| owner.get("machine_fingerprint"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    let workspace = launched
        .get("workspace")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    let os_user = std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_default();
    let Ok(uuid) = crate::model::ids::LeaderSessionUuid::derive(
        machine_fingerprint,
        workspace,
        &os_user,
        launched_key,
    ) else {
        return;
    };
    let owner_epoch = 1u64;
    let owner = serde_json::json!({
        "pane_id": "__team_agent_unbound__",
        "provider": provider,
        "machine_fingerprint": machine_fingerprint,
        "leader_session_uuid": uuid.as_str(),
        "owner_epoch": owner_epoch,
        "claimed_at": spawn_timestamp(),
        "claimed_via": "quick-start",
        "os_user": os_user,
    });
    let receiver = serde_json::json!({
        "mode": "direct_tmux",
        "status": "attached",
        "provider": provider,
        "pane_id": "__team_agent_unbound__",
        "leader_session_uuid": uuid.as_str(),
        "owner_epoch": owner_epoch,
        "discovery": "quick_start",
    });
    if let Some(obj) = launched.as_object_mut() {
        obj.insert("leader_receiver".to_string(), receiver);
        obj.insert("team_owner".to_string(), owner);
        obj.insert("owner_epoch".to_string(), serde_json::json!(owner_epoch));
    }
}

fn owner_pane_belongs_to_other_team(existing: &serde_json::Value, launched_key: &str, pane: &str) -> bool {
    existing
        .get("teams")
        .and_then(serde_json::Value::as_object)
        .is_some_and(|teams| {
            teams.iter().any(|(key, team)| {
                key != launched_key
                    && team
                        .get("team_owner")
                        .and_then(|owner| owner.get("pane_id"))
                        .and_then(serde_json::Value::as_str)
                        == Some(pane)
            })
        })
}

fn running_agent_state(
    agent: &Value,
    id: &str,
    provider: Provider,
    workspace: &Path,
    spawned_at: &str,
    team_id: &str,
) -> Result<serde_json::Value, LifecycleError> {
    let model = agent.get("model").and_then(Value::as_str);
    let auth_mode = agent
        .get("auth_mode")
        .and_then(Value::as_str)
        .and_then(parse_auth_mode)
        .unwrap_or(AuthMode::Subscription);
    let profile = agent.get("profile").map(yaml_value_to_json).unwrap_or(serde_json::Value::Null);
    let window = agent.get("window").and_then(Value::as_str).unwrap_or(id);
    let mcp_config = crate::provider::get_adapter(provider)
        .mcp_config(auth_mode)
        .map_err(|e| LifecycleError::Provider(e.to_string()))?;
    let mcp_config = resolve_mcp_config(mcp_config, workspace, id, team_id);
    let mcp_config_path = write_worker_mcp_config(workspace, id, &mcp_config)?;
    let mut state = serde_json::Map::new();
    state.insert("status".to_string(), serde_json::json!("running"));
    state.insert("provider".to_string(), serde_json::json!(provider));
    state.insert("agent_id".to_string(), serde_json::json!(id));
    state.insert("model".to_string(), model.map_or(serde_json::Value::Null, |m| serde_json::json!(m)));
    state.insert("auth_mode".to_string(), serde_json::json!(auth_mode));
    state.insert("profile".to_string(), profile);
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
    state.insert("session_id".to_string(), serde_json::Value::Null);
    state.insert("rollout_path".to_string(), serde_json::Value::Null);
    state.insert("captured_at".to_string(), serde_json::Value::Null);
    state.insert("captured_via".to_string(), serde_json::Value::Null);
    state.insert("attribution_confidence".to_string(), serde_json::Value::Null);
    state.insert(
        "spawn_cwd".to_string(),
        serde_json::json!(workspace.to_string_lossy().to_string()),
    );
    state.insert("spawned_at".to_string(), serde_json::json!(spawned_at));
    Ok(serde_json::Value::Object(state))
}

fn resolve_mcp_config(
    config: crate::provider::McpConfig,
    workspace: &Path,
    agent_id: &str,
    team_id: &str,
) -> crate::provider::McpConfig {
    crate::provider::McpConfig {
        raw: resolve_mcp_placeholders(config.raw, workspace, agent_id, team_id),
    }
}

fn resolve_mcp_placeholders(
    value: serde_json::Value,
    workspace: &Path,
    agent_id: &str,
    team_id: &str,
) -> serde_json::Value {
    match value {
        serde_json::Value::String(s) => serde_json::Value::String(
            s.replace("{workspace}", &workspace.to_string_lossy())
                .replace("{agent_id}", agent_id)
                .replace("{team_id}", team_id),
        ),
        serde_json::Value::Array(items) => serde_json::Value::Array(
            items
                .into_iter()
                .map(|item| resolve_mcp_placeholders(item, workspace, agent_id, team_id))
                .collect(),
        ),
        serde_json::Value::Object(map) => serde_json::Value::Object(
            map.into_iter()
                .map(|(key, value)| {
                    (
                        key,
                        resolve_mcp_placeholders(value, workspace, agent_id, team_id),
                    )
                })
                .collect(),
        ),
        other => other,
    }
}

fn write_worker_mcp_config(
    workspace: &Path,
    agent_id: &str,
    config: &crate::provider::McpConfig,
) -> Result<PathBuf, LifecycleError> {
    let path = workspace
        .join(".team/runtime/mcp")
        .join(format!("{agent_id}.json"));
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| LifecycleError::StatePersist(format!("{}: {e}", parent.display())))?;
    }
    let body = serde_json::to_string_pretty(&serde_json::json!({"mcpServers": config.raw}))
        .map_err(|e| LifecycleError::StatePersist(format!("serialize mcp config: {e}")))?;
    std::fs::write(&path, body)
        .map_err(|e| LifecycleError::StatePersist(format!("{}: {e}", path.display())))?;
    Ok(path)
}

fn point_native_mcp_config_at_file(argv: &mut [String], provider: Provider, path: &Path) {
    if !matches!(provider, Provider::Claude | Provider::ClaudeCode) {
        return;
    }
    let Some(index) = argv.iter().position(|arg| arg == "--mcp-config") else {
        return;
    };
    if let Some(value) = argv.get_mut(index.saturating_add(1)) {
        *value = path.to_string_lossy().to_string();
    }
}

fn permissions_json(
    agent: &Value,
    id: &str,
    provider: Provider,
) -> Result<serde_json::Value, crate::model::ModelError> {
    let tools = agent.get("tools").and_then(Value::as_list).map(|items| {
        items
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect::<Vec<_>>()
    });
    let resolved = permissions::resolve_permissions(&AgentPermissionInput {
        id: Some(AgentId::new(id)),
        provider,
        role: agent.get("role").and_then(Value::as_str).map(str::to_string),
        tools,
    })?;
    let mut out = serde_json::Map::new();
    out.insert("agent_id".to_string(), serde_json::json!(id));
    out.insert("provider".to_string(), serde_json::json!(provider));
    out.insert("tools".to_string(), serde_json::json!(resolved.sorted_tool_strings()));
    out.insert(
        "resolved_tools".to_string(),
        serde_json::Value::Array(
            resolved
                .resolved_tools
                .iter()
                .map(|tool| {
                    serde_json::json!({
                        "tool": tool.tool,
                        "enforcement": tool.enforcement,
                    })
                })
                .collect(),
        ),
    );
    out.insert("has_prompt_only".to_string(), serde_json::json!(resolved.has_prompt_only));
    Ok(serde_json::Value::Object(out))
}

fn agent_is_paused(agent: &Value) -> bool {
    matches!(agent.get("paused"), Some(Value::Bool(true)))
}

fn spawn_timestamp() -> String {
    match std::env::var("TEAM_AGENT_TEST_FIXED_SPAWNED_AT") {
        Ok(value) => value,
        Err(_) => chrono::Utc::now()
            .format("%Y-%m-%dT%H:%M:%S%.6f+00:00")
            .to_string(),
    }
}

pub(crate) fn fill_spawn_placeholders(argv: &mut [String], workspace: &Path, agent_id: &str) {
    fill_spawn_placeholders_full(argv, workspace, agent_id, None);
}

/// #229 B-layer worker env contract (`worker_spawn_inherits_parent_process_env_for_proxy_and_ca`):
/// every worker `transport.spawn_first/into` MUST receive an env map that is the **complete**
/// `team-agent` process environ (so the child sees the user's PATH ordering, HTTP_PROXY /
/// HTTPS_PROXY / ALL_PROXY / NO_PROXY, NODE_EXTRA_CA_CERTS / SSL_CERT_FILE / CURL_CA_BUNDLE /
/// REQUESTS_CA_BUNDLE / GIT_SSL_CAINFO, plus any wrapper-sourced vars), **then** overlay the
/// Team Agent identity three-tuple. This equals POSIX "child inherits parent environ" — the same
/// behavior the user gets when typing `codex` from their own shell. Zero hardcoded paths, zero
/// wrapper-name assumptions, generic across providers.
///
/// `TMUX` / `TMUX_PANE` are stripped because they bind the inherited shell to the **launching**
/// tmux pane; leaving them in would point worker-side tmux integrations at the wrong pane.
pub(crate) fn inherited_env_with_team_overrides(
    workspace: &Path,
    agent_id: &str,
    team_id: Option<&str>,
) -> BTreeMap<String, String> {
    // Only POSIX-valid shell identifier keys ([A-Za-z_][A-Za-z0-9_]*) — Bash/dash refuses
    // `KEY=val` assignment whose KEY has dashes/dots (e.g. `CARGO_BIN_EXE_team-agent=...`
    // shipped by cargo's integration-test runner) and would fail the entire `sh -lc`
    // line, leaving tmux's session dead-on-arrival. POSIX-invalid keys are runtime
    // metadata that workers never legitimately need; the user's PATH/proxy/CA always use
    // valid identifiers.
    let mut env: BTreeMap<String, String> = std::env::vars()
        .filter(|(k, _)| is_posix_shell_identifier(k))
        .collect();
    env.remove("TMUX");
    env.remove("TMUX_PANE");
    env.insert(
        "TEAM_AGENT_WORKSPACE".to_string(),
        workspace.to_string_lossy().to_string(),
    );
    env.insert("TEAM_AGENT_AGENT_ID".to_string(), agent_id.to_string());
    if let Some(tid) = team_id.filter(|s| !s.is_empty()) {
        env.insert("TEAM_AGENT_OWNER_TEAM_ID".to_string(), tid.to_string());
    }
    env
}

fn is_posix_shell_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Same as [`fill_spawn_placeholders`] plus `{team_id}` substitution everywhere it
/// appears as a SUBSTRING (the MCP config encodes it as `mcp_servers.team_orchestrator
/// .env.TEAM_AGENT_OWNER_TEAM_ID="{team_id}"`, embedded inside `-c key=value` strings,
/// so a token-equality replace would miss it).
pub(crate) fn fill_spawn_placeholders_full(
    argv: &mut [String],
    workspace: &Path,
    agent_id: &str,
    team_id: Option<&str>,
) {
    let workspace_text = workspace.to_string_lossy().to_string();
    let team_text = team_id.unwrap_or("").to_string();
    for arg in argv {
        if arg == "{workspace}" {
            *arg = workspace_text.clone();
        } else if arg == "{agent_id}" {
            *arg = agent_id.to_string();
        } else if arg.contains("{workspace}") || arg.contains("{agent_id}") || arg.contains("{team_id}") {
            *arg = arg
                .replace("{workspace}", &workspace_text)
                .replace("{agent_id}", agent_id)
                .replace("{team_id}", &team_text);
        }
    }
}

fn agent_tool_strings(agent: &Value) -> Vec<String> {
    agent
        .get("tools")
        .and_then(Value::as_list)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn spec_team_id(spec: &Value) -> Option<String> {
    spec.get("team")
        .and_then(|v| v.get("id").or_else(|| v.get("name")))
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| {
            spec.get("name")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
}

fn runtime_active_team_key_for_spawn(
    workspace: &Path,
    spec_path: &Path,
    spec: &Value,
    session_name: &SessionName,
) -> String {
    load_runtime_state(workspace)
        .ok()
        .and_then(|state| explicit_active_team_key(&state))
        .unwrap_or_else(|| runtime_team_key_for_spec(spec_path, spec, session_name))
}

fn process_team_id_for_spawn(workspace: &Path, spec: &Value) -> Option<String> {
    load_runtime_state(workspace)
        .ok()
        .and_then(|state| explicit_active_team_key(&state))
        .or_else(|| spec_team_id(spec))
}

fn explicit_active_team_key(state: &serde_json::Value) -> Option<String> {
    state
        .get("active_team_key")
        .and_then(serde_json::Value::as_str)
        .filter(|team| !team.is_empty() && *team != "current")
        .map(str::to_string)
}

fn runtime_team_key_for_spec(spec_path: &Path, spec: &Value, session_name: &SessionName) -> String {
    let team_dir = spec_path.parent().unwrap_or_else(|| Path::new("."));
    let state = serde_json::json!({
        "team_dir": team_dir.to_string_lossy(),
        "spec_path": spec_path.to_string_lossy(),
        "session_name": session_name.as_str(),
        "team": spec.get("team").map(yaml_value_to_json).unwrap_or(serde_json::Value::Null),
    });
    crate::state::projection::team_state_key(&state)
}

fn transport_has_session(transport: &dyn Transport, session_name: &SessionName) -> bool {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        transport.has_session(session_name)
    })) {
        Ok(Ok(live)) => live,
        Ok(Err(_)) | Err(_) => false,
    }
}

fn parse_provider(raw: &str) -> Option<Provider> {
    match raw {
        "claude" => Some(Provider::Claude),
        "claude_code" => Some(Provider::ClaudeCode),
        "codex" => Some(Provider::Codex),
        "gemini_cli" => Some(Provider::GeminiCli),
        "fake" => Some(Provider::Fake),
        _ => None,
    }
}

fn parse_auth_mode(raw: &str) -> Option<AuthMode> {
    match raw {
        "subscription" => Some(AuthMode::Subscription),
        "official_api" => Some(AuthMode::OfficialApi),
        "compatible_api" => Some(AuthMode::CompatibleApi),
        _ => None,
    }
}

/// `quick_start(agents_dir, name, yes, fresh, team_id)`(`diagnose/quick_start.py:18`)。
/// 面向用户的零配置入口:编译 team_dir → `launch` → autobind leader receiver → 起
/// coordinator → `wait_ready` 轮询就绪。归入 lifecycle module(不与 diagnose 混)。
pub fn quick_start(
    agents_dir: &Path,
    name: Option<&str>,
    yes: bool,
    fresh: bool,
    team_id: Option<&str>,
) -> Result<QuickStartReport, LifecycleError> {
    quick_start_with_transport(
        agents_dir,
        name,
        yes,
        fresh,
        team_id,
        // CP-1: per-team socket bound to the run workspace (team_workspace(agents_dir)).
        &crate::tmux_backend::TmuxBackend::for_workspace(&team_workspace(agents_dir)),
    )
}

/// `quick_start` with an injected transport — tests inject a recording mock so the REAL spawn path
/// (launch dry_run=false → spawn_agents) is asserted without a live tmux; prod uses the real TmuxBackend.
pub fn quick_start_with_transport(
    agents_dir: &Path,
    name: Option<&str>,
    yes: bool,
    fresh: bool,
    team_id: Option<&str>,
    transport: &dyn Transport,
) -> Result<QuickStartReport, LifecycleError> {
    if !agents_dir.exists() {
        return Err(LifecycleError::Compile(format!(
            "agents dir not found: {}",
            agents_dir.display()
        )));
    }
    let workspace = team_workspace(agents_dir);
    if !fresh {
        let state_path = crate::state::persist::runtime_state_path(&workspace);
        if state_path.exists() {
            let state = crate::state::persist::load_runtime_state(&workspace)
                .map_err(|e| LifecycleError::StatePersist(e.to_string()))?;
            return Ok(QuickStartReport::ExistingRuntime {
                team: team_id.map(str::to_string),
                session_name: state
                    .get("session_name")
                    .and_then(serde_json::Value::as_str)
                    .filter(|s| !s.is_empty())
                    .map(SessionName::new),
                state_path: Some(state_path),
                next_actions: vec![
                    "run restart to resume the existing team or pass --fresh to replace it".to_string(),
                ],
            });
        }
    }
    let mut spec = crate::compiler::compile_team(agents_dir)
        .map_err(|e| LifecycleError::Compile(e.to_string()))?;
    // CR-040/042: repeated quick-start from one template with distinct --team-id/--name
    // must NOT collide on the template-derived tmux session. Override the compiled
    // spec's runtime.session_name with one derived from the REQUESTED team identity
    // so launch_with_transport (which reads runtime.session_name) spawns into an
    // isolated session per requested team.
    if let Some(requested) = team_id.or(name).filter(|s| !s.is_empty()) {
        override_spec_session_name(&mut spec, &format!("team-{requested}"));
    }
    let spec_path = agents_dir.join("team.spec.yaml");
    std::fs::write(&spec_path, yaml::dumps(&spec)).map_err(|e| {
        LifecycleError::StatePersist(format!("{}: {e}", spec_path.display()))
    })?;
    let _store = crate::message_store::MessageStore::open(&workspace)
        .map_err(|e| LifecycleError::StatePersist(e.to_string()))?;
    let session_name = spec_session_name(&spec);
    let resolved_spec_path = std::fs::canonicalize(&spec_path).unwrap_or_else(|_| spec_path.clone());
    let state = initial_runtime_state(&spec, &resolved_spec_path, &workspace, agents_dir);
    save_launched_team_state(&workspace, &state)?;
    // FIX (rt-host-a real-machine finding): dry_run=false so launch_with_transport calls spawn_agents
    // and really creates the tmux session + worker windows (was hardcoded true → never spawned, which
    // also starved the coordinator: no session → first tick TmuxSessionMissing → run_daemon loop exits).
    let launch = launch_with_transport(&spec_path, false, yes, true, transport)?;
    let coordinator_workspace = crate::coordinator::WorkspacePath::new(workspace.clone());
    let coordinator_started = crate::coordinator::start_coordinator(&coordinator_workspace)
        .map(|report| report.ok)
        .map_err(|e| LifecycleError::StatePersist(e.to_string()))?;
    let coordinator_action = if coordinator_started {
        "coordinator started"
    } else {
        "coordinator not started"
    };
    // BUG-7: build an honest readiness verdict from the post-spawn runtime state.
    // - If persist_spawn_agent_state (BUG-2 fix) marked any agent non-running, the
    //   team is observably Degraded.
    // - Otherwise the framework cannot itself verify that the worker's MCP tool set
    //   loaded successfully (provider-side codex/claude schema rejections happen
    //   asynchronously after spawn), so the verdict is PendingToolLoad — never
    //   bare Ready.
    let worker_readiness = quick_start_worker_readiness(&workspace);
    Ok(QuickStartReport::Ready {
        session_name,
        launch: Box::new(launch),
        next_actions: vec![format!(
            "team compiled; real spawn is behind the transport/provider boundary; {coordinator_action}"
        )],
        worker_readiness,
    })
}

/// BUG-7 helper: derive a [`QuickStartReadiness`] verdict from the just-written
/// runtime state. Reads `agents[*].status`; any non-`running` agent flips the
/// verdict to `Degraded { unhealthy_agents }` (sorted, deduped); otherwise
/// `PendingToolLoad` — never bare Ready. State read failure is treated as
/// PendingToolLoad rather than fabricated success.
fn quick_start_worker_readiness(workspace: &Path) -> QuickStartReadiness {
    let Ok(state) = load_runtime_state(workspace) else {
        return QuickStartReadiness::PendingToolLoad;
    };
    let Some(agents) = state.get("agents").and_then(serde_json::Value::as_object) else {
        return QuickStartReadiness::PendingToolLoad;
    };
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
    if unhealthy.is_empty() {
        QuickStartReadiness::PendingToolLoad
    } else {
        unhealthy.sort();
        unhealthy.dedup();
        QuickStartReadiness::Degraded { unhealthy_agents: unhealthy }
    }
}

/// `detect_inherited_dangerous_permissions`(`launch/config.py`):扫进程祖先链找
/// `--dangerously-*` flag,产出危险审批继承态。launch 在 inherited=false 且无 --yes 时拒。
pub fn detect_dangerous_approval() -> Result<DangerousApproval, LifecycleError> {
    if let Ok(raw) = std::env::var("TEAM_AGENT_TEST_PROCESS_ANCESTRY_ARGV_JSON") {
        let argv_tokens = serde_json::from_str::<Vec<String>>(&raw)
            .map_err(|e| LifecycleError::StatePersist(format!("invalid test ancestry argv: {e}")))?;
        return Ok(detect_dangerous_approval_in_argv(&argv_tokens).unwrap_or_else(disabled_dangerous_approval));
    }
    for argv_tokens in process_ancestry_argv(std::process::id()) {
        if let Some(detected) = detect_dangerous_approval_in_argv(&argv_tokens) {
            return Ok(detected);
        }
    }
    Ok(disabled_dangerous_approval())
}

fn detect_dangerous_approval_in_argv(argv_tokens: &[String]) -> Option<DangerousApproval> {
    let argv0 = argv_tokens.first().map(String::as_str).unwrap_or("");
    let ancestry_binary_name = binary_name(argv0);
    for token in argv_tokens {
        for (provider, flag) in dangerous_leader_flags() {
            if token == flag {
                let unexpected_binary = !binary_matches_provider(provider, ancestry_binary_name.as_deref());
                return Some(DangerousApproval {
                    enabled: true,
                    source: DangerousApprovalSource::LeaderProcess,
                    inherited: true,
                    provider: Some((*provider).to_string()),
                    flag: Some((*flag).to_string()),
                    worker_capability_above_leader: false,
                    ancestry_binary_name,
                    unexpected_binary,
                });
            }
        }
    }
    None
}

fn dangerous_leader_flags() -> &'static [(&'static str, &'static str)] {
    &[
        ("claude", "--dangerously-skip-permissions"),
        ("claude", "--dangerously-skip-permission"),
        ("codex", "--dangerously-bypass-approvals-and-sandbox"),
    ]
}

fn binary_matches_provider(provider: &str, binary: Option<&str>) -> bool {
    match (provider, binary) {
        ("codex", Some("codex")) => true,
        ("claude", Some("claude" | "claude-code" | "claude_code")) => true,
        _ => false,
    }
}

fn binary_name(argv0: &str) -> Option<String> {
    Path::new(argv0)
        .file_name()
        .and_then(|v| v.to_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn process_ancestry_argv(pid: u32) -> Vec<Vec<String>> {
    let mut out = Vec::new();
    let mut current = pid;
    let mut seen = std::collections::BTreeSet::new();
    for _ in 0..12 {
        if current == 0 || !seen.insert(current) {
            break;
        }
        if let Some(argv_tokens) = process_argv_tokens(current) {
            out.push(argv_tokens);
        }
        let Some(parent) = process_parent_pid(current) else {
            break;
        };
        if parent <= 1 || parent == current {
            break;
        }
        current = parent;
    }
    out
}

#[cfg(target_os = "linux")]
fn process_argv_tokens(pid: u32) -> Option<Vec<String>> {
    let bytes = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
    let argv_tokens = String::from_utf8_lossy(&bytes)
        .split('\0')
        .filter(|token| !token.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();
    (!argv_tokens.is_empty()).then_some(argv_tokens)
}

#[cfg(target_os = "macos")]
fn process_argv_tokens(pid: u32) -> Option<Vec<String>> {
    use std::mem::size_of;

    let mut mib = [
        libc::CTL_KERN,
        libc::KERN_PROCARGS2,
        i32::try_from(pid).ok()?,
    ];
    let mut size = 0usize;
    let rc = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            mib.len() as u32,
            std::ptr::null_mut(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 || size <= size_of::<libc::c_int>() {
        return None;
    }
    let mut buf = vec![0u8; size];
    let rc = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            mib.len() as u32,
            buf.as_mut_ptr().cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 || size <= size_of::<libc::c_int>() {
        return None;
    }
    let argc = i32::from_ne_bytes(buf.get(..size_of::<libc::c_int>())?.try_into().ok()?) as usize;
    let mut offset = size_of::<libc::c_int>();
    while offset < size && buf[offset] != 0 {
        offset += 1;
    }
    while offset < size && buf[offset] == 0 {
        offset += 1;
    }
    let raw = String::from_utf8_lossy(&buf[offset..size]);
    let argv_tokens = raw
        .split('\0')
        .filter(|token| !token.is_empty())
        .take(argc)
        .map(str::to_string)
        .collect::<Vec<_>>();
    (!argv_tokens.is_empty()).then_some(argv_tokens)
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn process_argv_tokens(pid: u32) -> Option<Vec<String>> {
    let output = Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "command="])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let argv_tokens = text
        .split_whitespace()
        .filter(|token| !token.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();
    (!argv_tokens.is_empty()).then_some(argv_tokens)
}

fn process_parent_pid(pid: u32) -> Option<u32> {
    let output = Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "ppid="])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse::<u32>()
        .ok()
}

/// `add_agent(workspace, agent_id, role_file_path, open_display, team)`
/// (`lifecycle/operations.py:143`)。动态 role doc 编译进 spec + 起 worker;失败**字节级回滚**
/// spec_yaml / workspace_state / **team_state.md** / role_file(Gap 15.11),每步发
/// `lifecycle.add_step_*` 事件(顺序被测试锁死)。
pub fn add_agent(
    workspace: &Path,
    agent_id: &AgentId,
    role_file_path: &Path,
    open_display: bool,
    team: Option<&str>,
) -> Result<AddAgentReport, LifecycleError> {
    let selected = match crate::state::selector::resolve_active_team(
        workspace,
        team,
        crate::state::selector::SelectorMode::RequireSpec,
    ) {
        Ok(selected) => selected,
        Err(_) if workspace.join("TEAM.md").exists() => {
            return add_agent_with_transport(
                workspace,
                agent_id,
                role_file_path,
                open_display,
                team,
                &crate::tmux_backend::TmuxBackend::for_workspace(&team_workspace(workspace)),
            );
        }
        Err(error) => return Err(LifecycleError::TeamSelect(error.to_string())),
    };
    let team_dir = selected
        .spec_workspace
        .ok_or_else(|| LifecycleError::TeamSelect("active team spec workspace not found".to_string()))?;
    add_agent_with_transport(
        &team_dir,
        agent_id,
        role_file_path,
        open_display,
        team,
        &crate::tmux_backend::TmuxBackend::for_workspace(&selected.run_workspace),
    )
}

/// `add_agent` with an injected transport — after the recompile+write, wires the new worker spawn
/// (via start_agent_with_transport) + start_coordinator (rt-host-a sweep: recompiled but never spawned).
pub fn add_agent_with_transport(
    workspace: &Path,
    agent_id: &AgentId,
    role_file_path: &Path,
    open_display: bool,
    team: Option<&str>,
    transport: &dyn Transport,
) -> Result<AddAgentReport, LifecycleError> {
    let run_workspace = crate::model::paths::canonical_run_workspace(workspace)
        .map_err(|e| LifecycleError::StatePersist(e.to_string()))?;
    let owner_state = if team.is_some() {
        crate::state::projection::select_runtime_state(&run_workspace, team)
            .map_err(|e| LifecycleError::TeamSelect(e.to_string()))?
    } else {
        load_runtime_state(&run_workspace).map_err(|e| LifecycleError::StatePersist(e.to_string()))?
    };
    ensure_owner_allowed_for_state(&owner_state, Some(agent_id))?;
    if !role_file_path.exists() {
        return Err(LifecycleError::Compile(format!(
            "role file not found: {}",
            role_file_path.display()
        )));
    }
    let team_dir = workspace;
    if agent_id_exists_in_team_dir(team_dir, agent_id) {
        return Err(LifecycleError::RequirementUnmet(format!(
            "agent id already exists: {agent_id}"
        )));
    }
    let dynamic_role_file = materialize_added_role_file(team_dir, agent_id, role_file_path)?;
    let spec = crate::compiler::compile_team(team_dir)
        .map_err(|e| LifecycleError::Compile(e.to_string()))?;
    let spec_path = team_dir.join("team.spec.yaml");
    std::fs::write(&spec_path, yaml::dumps(&spec)).map_err(|e| {
        LifecycleError::StatePersist(format!("{}: {e}", spec_path.display()))
    })?;
    let (meta, _) = crate::compiler::read_front_matter(&dynamic_role_file)
        .map_err(|e| LifecycleError::Compile(e.to_string()))?;
    let run_ws = team_workspace(team_dir);
    upsert_agent_state_from_role(&run_ws, agent_id, &meta, &dynamic_role_file)?;
    let started = crate::lifecycle::restart::start_agent_at_paths(
        &run_ws,
        team_dir,
        agent_id,
        false,
        open_display,
        true,
        team,
        transport,
    )?;
    let (env, start_mode) = match started {
        StartAgentOutcome::Running {
            env, start_mode, ..
        } => (env, start_mode),
        StartAgentOutcome::Noop { env, .. } => (env, StartMode::Noop),
        StartAgentOutcome::Paused { .. } => {
            return Err(LifecycleError::RequirementUnmet(format!(
                "added agent {agent_id} is paused"
            )));
        }
    };
    Ok(AddAgentReport {
        env,
        start_mode,
        role_file: role_file_path.to_path_buf(),
    })
}

fn upsert_agent_state_from_role(
    workspace: &Path,
    agent_id: &AgentId,
    meta: &Value,
    dynamic_role_file: &Path,
) -> Result<(), LifecycleError> {
    let mut state = crate::state::persist::load_runtime_state(workspace)
        .map_err(|e| LifecycleError::StatePersist(e.to_string()))?;
    if !state.is_object() {
        state = serde_json::json!({});
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
    let provider = meta
        .get("provider")
        .and_then(Value::as_str)
        .unwrap_or("codex");
    let auth_mode = meta
        .get("auth_mode")
        .and_then(Value::as_str)
        .unwrap_or("subscription");
    let role = meta
        .get("role")
        .and_then(Value::as_str)
        .unwrap_or_else(|| agent_id.as_str());
    let mut entry = serde_json::json!({
        "provider": provider,
        "auth_mode": auth_mode,
        "role": role,
        "status": "running",
        "dynamic_role_file": dynamic_role_file.to_string_lossy().to_string(),
    });
    if let Some(model) = meta.get("model").and_then(Value::as_str) {
        if let Some(obj) = entry.as_object_mut() {
            obj.insert("model".to_string(), serde_json::json!(model));
        }
    }
    agent_map.insert(agent_id.as_str().to_string(), entry);
    save_runtime_state(workspace, &state).map_err(|e| LifecycleError::StatePersist(e.to_string()))
}

fn materialize_added_role_file(
    team_dir: &Path,
    agent_id: &AgentId,
    role_file_path: &Path,
) -> Result<PathBuf, LifecycleError> {
    let agents_dir = team_dir.join("agents");
    std::fs::create_dir_all(&agents_dir)
        .map_err(|e| LifecycleError::StatePersist(format!("create agents dir: {e}")))?;
    let target = agents_dir.join(format!("{}.md", agent_id.as_str()));
    if role_file_path == target {
        return Ok(target);
    }
    std::fs::copy(role_file_path, &target).map_err(|e| {
        LifecycleError::StatePersist(format!(
            "copy role file {} -> {}: {e}",
            role_file_path.display(),
            target.display()
        ))
    })?;
    Ok(target)
}

/// `fork_agent(workspace, source_agent_id, as_agent_id, ...)`(`lifecycle/operations.py:284`)。
/// native session fork(provider 须 supports_session_fork ∧ auth_mode!=compatible_api);
/// 失败回滚,每条失败臂 `adapter.cleanup_mcp`。
pub fn fork_agent(
    workspace: &Path,
    source_agent_id: &AgentId,
    as_agent_id: &AgentId,
    open_display: bool,
    team: Option<&str>,
) -> Result<ForkAgentReport, LifecycleError> {
    let selected = crate::state::selector::resolve_active_team(
        workspace,
        team,
        crate::state::selector::SelectorMode::RequireSpec,
    )
    .map_err(|e| LifecycleError::TeamSelect(e.to_string()))?;
    fork_agent_with_transport(
        workspace,
        source_agent_id,
        as_agent_id,
        open_display,
        team,
        &crate::tmux_backend::TmuxBackend::for_workspace(&selected.run_workspace),
    )
}

pub fn fork_agent_with_transport(
    workspace: &Path,
    source_agent_id: &AgentId,
    as_agent_id: &AgentId,
    open_display: bool,
    team: Option<&str>,
    transport: &dyn Transport,
) -> Result<ForkAgentReport, LifecycleError> {
    let _ = open_display;
    let selected = crate::state::selector::resolve_active_team(
        workspace,
        team,
        crate::state::selector::SelectorMode::RequireSpec,
    )
    .map_err(|e| LifecycleError::TeamSelect(e.to_string()))?;
    let spec_workspace = selected
        .spec_workspace
        .ok_or_else(|| LifecycleError::TeamSelect("active team spec workspace not found".to_string()))?;
    let workspace = selected.run_workspace;
    let state = selected.state;
    ensure_owner_allowed_for_state(&state, Some(source_agent_id))?;
    let spec_path = spec_workspace.join("team.spec.yaml");
    let text = std::fs::read_to_string(&spec_path)
        .map_err(|e| LifecycleError::Compile(format!("{}: {e}", spec_path.display())))?;
    let spec = yaml::loads(&text).map_err(|e| LifecycleError::Compile(e.to_string()))?;
    if find_spec_agent(&spec, as_agent_id).is_some() || leader_id_matches(&spec, as_agent_id) {
        return Err(LifecycleError::RequirementUnmet(format!(
            "agent id already exists: {as_agent_id}"
        )));
    }
    let source_agent = find_spec_agent(&spec, source_agent_id)
        .ok_or_else(|| LifecycleError::RequirementUnmet(format!("unknown worker agent id: {source_agent_id}")))?;
    let session_id = state
        .get("agents")
        .and_then(|v| v.get(source_agent_id.as_str()))
        .and_then(|v| v.get("session_id"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(crate::provider::SessionId::new)
        .ok_or_else(|| {
            LifecycleError::Provider(format!(
                "cannot fork {source_agent_id}: source session_id is missing"
            ))
        })?;
    let session_name = state
        .get("session_name")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(SessionName::new)
        .unwrap_or_else(|| spec_session_name(&spec));
    if transport
        .list_windows(&session_name)
        .map(|windows| windows.iter().any(|w| w.as_str() == as_agent_id.as_str()))
        .unwrap_or(false)
    {
        return Err(LifecycleError::Transport(format!(
            "tmux window already exists for fork target: {}:{}",
            session_name.as_str(),
            as_agent_id.as_str()
        )));
    }
    let new_spec = append_forked_agent(&spec, source_agent, source_agent_id, as_agent_id)?;
    crate::model::spec::validate_spec(&new_spec, &spec_workspace)
        .map_err(|e| LifecycleError::Compile(e.to_string()))?;
    std::fs::write(&spec_path, yaml::dumps(&new_spec))
        .map_err(|e| LifecycleError::StatePersist(format!("{}: {e}", spec_path.display())))?;
    let new_agent = find_spec_agent(&new_spec, as_agent_id)
        .ok_or_else(|| LifecycleError::RequirementUnmet(format!("unknown worker agent id: {as_agent_id}")))?;
    let provider = new_agent
        .get("provider")
        .and_then(Value::as_str)
        .and_then(parse_provider)
        .unwrap_or(Provider::Codex);
    let auth_mode = new_agent
        .get("auth_mode")
        .and_then(Value::as_str)
        .and_then(parse_auth_mode)
        .unwrap_or(AuthMode::Subscription);
    let adapter = crate::provider::get_adapter(provider);
    let provider_str = new_agent
        .get("provider")
        .and_then(Value::as_str)
        .unwrap_or("codex");
    if auth_mode == AuthMode::CompatibleApi || !adapter.caps().fork {
        let _ = std::fs::write(&spec_path, text.as_bytes());
        return Err(LifecycleError::Provider(format!(
            "{provider_str} does not support native session fork"
        )));
    }
    let role = new_agent.get("role").and_then(Value::as_str);
    let model = new_agent.get("model").and_then(Value::as_str);
    let safety = effective_runtime_config(&new_spec)?;
    let tools = worker_tool_refs(agent_tool_strings(new_agent), &safety);
    let tool_refs: Vec<&str> = tools.iter().map(String::as_str).collect();
    let mcp_config = adapter
        .mcp_config(auth_mode)
        .map_err(|e| {
            let _ = std::fs::write(&spec_path, text.as_bytes());
            LifecycleError::Provider(e.to_string())
        })?;
    let mut argv = adapter
        .fork_with_context(
            Some(&session_id),
            auth_mode,
            Some(&mcp_config),
            role,
            model,
            &tool_refs,
        )
        .map_err(|e| {
            let _ = std::fs::write(&spec_path, text.as_bytes());
            LifecycleError::Provider(e.to_string())
        })?;
    let fork_team = crate::messaging::leader_receiver::active_team_key(&workspace, &state);
    fill_spawn_placeholders_full(&mut argv, &workspace, as_agent_id.as_str(), Some(&fork_team));
    let window = WindowName::new(as_agent_id.as_str());
    // fork inherits the parent agent's owner team via runtime state (`active_team_key`).
    let env = inherited_env_with_team_overrides(
        &workspace,
        as_agent_id.as_str(),
        Some(&fork_team),
    );
    // golden operations.py:336 -> _tmux_start_command_for_agent_window (runtime.py:1017-1020): branch on
    // _tmux_session_exists — an ABSENT session => new-session (spawn_first), present => new-window
    // (spawn_into). The Rust restart seam (restart.rs spawn_agent_window) uses the same branch.
    let session_live = transport.has_session(&session_name).unwrap_or(false);
    let spawn_result = if session_live {
        transport.spawn_into(&session_name, &window, &argv, &workspace, &env)
    } else {
        transport.spawn_first(&session_name, &window, &argv, &workspace, &env)
    };
    let _spawn = spawn_result.map_err(|e| {
        let _ = std::fs::write(&spec_path, text.as_bytes());
        LifecycleError::Transport(e.to_string())
    })?;
    let old_state = state.clone();
    let mut next_state = state;
    upsert_forked_agent_state(&mut next_state, source_agent_id, as_agent_id, new_agent)?;
    if let Err(e) = save_runtime_state(&workspace, &next_state) {
        rollback_fork_after_spawn(&workspace, &spec_path, &text, &old_state, transport, &session_name, &window);
        return Err(LifecycleError::StatePersist(e.to_string()));
    }
    let coordinator_started =
        crate::coordinator::start_coordinator(&crate::coordinator::WorkspacePath::new(
            workspace.to_path_buf(),
        ))
        .map(|report| report.ok)
        .map_err(|e| {
            rollback_fork_after_spawn(&workspace, &spec_path, &text, &old_state, transport, &session_name, &window);
            LifecycleError::StatePersist(e.to_string())
        })?;
    Ok(ForkAgentReport {
        source_agent_id: source_agent_id.clone(),
        new_agent_id: as_agent_id.clone(),
        env: AgentActionEnvelope {
            agent_id: as_agent_id.clone(),
            state_file: crate::state::persist::runtime_state_path(&workspace),
            coordinator_started,
        },
        session_id: None,
    })
}

fn rollback_fork_after_spawn(
    workspace: &Path,
    spec_path: &Path,
    spec_text: &str,
    old_state: &serde_json::Value,
    transport: &dyn Transport,
    session_name: &SessionName,
    window: &WindowName,
) {
    let _ = transport.kill_window(&Target::SessionWindow {
        session: session_name.clone(),
        window: window.clone(),
    });
    let _ = std::fs::write(spec_path, spec_text.as_bytes());
    let _ = save_runtime_state(workspace, old_state);
}

fn leader_id_matches(spec: &Value, agent_id: &AgentId) -> bool {
    spec.get("leader")
        .and_then(|v| v.get("id"))
        .and_then(Value::as_str)
        .map(|id| id == agent_id.as_str())
        .unwrap_or(false)
}

fn find_spec_agent<'a>(spec: &'a Value, agent_id: &AgentId) -> Option<&'a Value> {
    let leader_is_agent = spec
        .get("leader")
        .and_then(|v| v.get("id"))
        .and_then(Value::as_str)
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
                .and_then(Value::as_str)
                .map(|id| id == agent_id.as_str())
                .unwrap_or(false)
        })
}

fn append_forked_agent(
    spec: &Value,
    source_agent: &Value,
    source_agent_id: &AgentId,
    as_agent_id: &AgentId,
) -> Result<Value, LifecycleError> {
    let mut new_agent = source_agent.clone();
    set_yaml_map_value(
        &mut new_agent,
        "id",
        Value::Str(as_agent_id.as_str().to_string()),
    )?;
    // golden operations.py:315 `str(label or new_agent.get("role") or as_agent_id)` — Python `or`
    // falsiness: an EMPTY-string role is falsy and falls through to as_agent_id.
    let role = new_agent
        .get("role")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
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
        return Err(LifecycleError::Compile("spec root is not a map".to_string()));
    };
    let mut out = Vec::new();
    for (key, value) in pairs {
        if key == "agents" {
            let mut agents = value.as_list().map(|items| items.to_vec()).unwrap_or_default();
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

fn set_yaml_map_value(value: &mut Value, key: &str, next: Value) -> Result<(), LifecycleError> {
    let Value::Map(pairs) = value else {
        return Err(LifecycleError::Compile("agent entry is not a map".to_string()));
    };
    if let Some((_, existing)) = pairs.iter_mut().find(|(k, _)| k == key) {
        *existing = next;
    } else {
        pairs.push((key.to_string(), next));
    }
    Ok(())
}

fn runtime_with_startup_agent(runtime: &Value, agent_id: &AgentId) -> Value {
    let Value::Map(pairs) = runtime else {
        return runtime.clone();
    };
    let mut out = Vec::new();
    let mut saw_startup = false;
    for (key, value) in pairs {
        if key == "startup_order" {
            saw_startup = true;
            let mut order = value.as_list().map(|items| items.to_vec()).unwrap_or_default();
            let already_present = order
                .iter()
                .any(|item| item.as_str().map(|id| id == agent_id.as_str()).unwrap_or(false));
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

fn upsert_forked_agent_state(
    state: &mut serde_json::Value,
    source_agent_id: &AgentId,
    as_agent_id: &AgentId,
    spec_agent: &Value,
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
    agent_map.insert(
        as_agent_id.as_str().to_string(),
        serde_json::json!({
            "status": "running",
            "provider": provider,
            "window": as_agent_id.as_str(),
            "forked_from": source_agent_id.as_str(),
        }),
    );
    Ok(())
}

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
    if let Some(refusal) = crate::state::owner_gate::check_team_owner(
        state,
        &caller,
        false,
        &NoopLiveness,
    ) {
        return Err(LifecycleError::OwnerRefused(refusal.to_string()));
    }
    Ok(())
}

fn caller_is_target_role_in_team(target_team: &str, target_role: Option<&AgentId>) -> bool {
    let Some(target_role) = target_role else {
        return false;
    };
    std::env::var("TEAM_AGENT_ID").ok().as_deref() == Some(target_role.as_str())
        && std::env::var("TEAM_AGENT_TEAM_ID").ok().as_deref() == Some(target_team)
}

pub(crate) fn state_path(workspace: &Path) -> std::path::PathBuf {
    crate::state::persist::runtime_state_path(workspace)
}

fn initial_runtime_state(
    spec: &Value,
    spec_path: &Path,
    workspace: &Path,
    team_dir: &Path,
) -> serde_json::Value {
    let mut agents = serde_json::Map::new();
    for agent in spec_agent_values(spec) {
        let Some(id) = agent.get("id").and_then(Value::as_str) else {
            continue;
        };
        let provider = agent.get("provider").and_then(Value::as_str).unwrap_or("codex");
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
    let requested_display = spec
        .get("runtime")
        .and_then(|runtime| runtime.get("display_backend"))
        .and_then(Value::as_str)
        .and_then(|backend| serde_json::from_value::<DisplayBackend>(serde_json::json!(backend)).ok());
    let display_backend =
        crate::lifecycle::display::resolve_display_backend(requested_display, None).backend;
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
    state.insert(
        "session_name".to_string(),
        serde_json::json!(spec_session_name(spec).as_str()),
    );
    state.insert(
        "leader".to_string(),
        spec.get("leader").map(yaml_value_to_json).unwrap_or(serde_json::Value::Null),
    );
    state.insert("agents".to_string(), serde_json::Value::Object(agents));
    state.insert("tasks".to_string(), spec_tasks_json(spec));
    state.insert("display_backend".to_string(), serde_json::json!(display_backend));
    let mut state = serde_json::Value::Object(state);
    if !seed_launched_owner_from_env(&mut state) {
        let team_id = crate::state::projection::team_state_key(&state);
        seed_unbound_launched_owner(&mut state, &team_id);
    }
    state
}

fn seed_launched_owner_from_env(state: &mut serde_json::Value) -> bool {
    let team_id = crate::state::projection::team_state_key(state);
    let Ok(caller) = crate::state::identity::caller_identity_from_env(
        Some(state),
        &crate::state::identity::SystemEnv,
        Some(&team_id),
        None,
    ) else {
        return false;
    };
    let provider = if caller.provider.is_empty() {
        "codex".to_string()
    } else {
        caller.provider
    };
    let pane_id = caller.pane_id;
    if pane_id.is_empty() {
        return false;
    }
    let owner_epoch = 1u64;
    let owner = serde_json::json!({
        "pane_id": pane_id,
        "provider": provider.clone(),
        "machine_fingerprint": caller.machine_fingerprint,
        "leader_session_uuid": caller.leader_session_uuid,
        "owner_epoch": owner_epoch,
        "claimed_at": spawn_timestamp(),
        "claimed_via": "quick-start",
        "os_user": std::env::var("USER")
            .or_else(|_| std::env::var("USERNAME"))
            .unwrap_or_default(),
    });
    let receiver = serde_json::json!({
        "mode": "direct_tmux",
        "status": "attached",
        "provider": provider,
        "pane_id": owner.get("pane_id").cloned().unwrap_or(serde_json::Value::Null),
        "leader_session_uuid": owner.get("leader_session_uuid").cloned().unwrap_or(serde_json::Value::Null),
        "owner_epoch": owner_epoch,
        "discovery": "quick_start",
    });
    let mut receiver = receiver;
    if let (Some(receiver), Some(socket)) = (
        receiver.as_object_mut(),
        crate::tmux_backend::socket_name_from_tmux_env(),
    ) {
        receiver.insert("tmux_socket".to_string(), serde_json::json!(socket));
    }
    if let Some(obj) = state.as_object_mut() {
        obj.insert("leader_receiver".to_string(), receiver);
        obj.insert("team_owner".to_string(), owner);
        obj.insert("owner_epoch".to_string(), serde_json::json!(owner_epoch));
    }
    true
}

fn spec_tasks_json(spec: &Value) -> serde_json::Value {
    spec.get("tasks")
        .and_then(Value::as_list)
        .map(|tasks| {
            serde_json::Value::Array(tasks.iter().map(yaml_value_to_json).collect())
        })
        .unwrap_or_else(|| serde_json::json!([]))
}

fn yaml_value_to_json(value: &Value) -> serde_json::Value {
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
fn override_spec_session_name(spec: &mut Value, session_name: &str) {
    let Value::Map(root) = spec else { return };
    let runtime_slot = root
        .iter_mut()
        .find(|(k, _)| k == "runtime")
        .map(|(_, v)| v);
    match runtime_slot {
        Some(Value::Map(runtime)) => {
            if let Some((_, existing)) = runtime.iter_mut().find(|(k, _)| k == "session_name") {
                *existing = Value::Str(session_name.to_string());
            } else {
                runtime.push(("session_name".to_string(), Value::Str(session_name.to_string())));
            }
        }
        Some(other) => {
            *other = Value::Map(vec![(
                "session_name".to_string(),
                Value::Str(session_name.to_string()),
            )]);
        }
        None => {
            root.push((
                "runtime".to_string(),
                Value::Map(vec![(
                    "session_name".to_string(),
                    Value::Str(session_name.to_string()),
                )]),
            ));
        }
    }
}

fn spec_session_name(spec: &Value) -> SessionName {
    let name = spec
        .get("runtime")
        .and_then(|v| v.get("session_name"))
        .and_then(Value::as_str)
        .unwrap_or("team-agent");
    SessionName::new(name)
}

fn spec_agents(spec: &Value) -> Vec<AgentId> {
    spec_agent_values(spec)
        .into_iter()
        .filter_map(|agent| agent.get("id").and_then(Value::as_str).map(AgentId::new))
        .collect()
}

fn spec_agent_values(spec: &Value) -> Vec<&Value> {
    spec.get("agents")
        .and_then(Value::as_list)
        .map(|agents| agents.iter().collect())
        .unwrap_or_default()
}

fn spec_routes(spec: &Value) -> Vec<RoutingDecision> {
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

fn spec_default_assignee(spec: &Value) -> Option<AgentId> {
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

fn disabled_dangerous_approval() -> DangerousApproval {
    DangerousApproval {
        enabled: false,
        source: DangerousApprovalSource::Disabled,
        inherited: false,
        provider: None,
        flag: None,
        worker_capability_above_leader: false,
        ancestry_binary_name: None,
        unexpected_binary: false,
    }
}

pub(crate) fn effective_runtime_config_for_worker_spawn() -> Result<DangerousApproval, LifecycleError> {
    detect_dangerous_approval()
}

pub(crate) fn worker_tool_refs(
    mut tools: Vec<String>,
    safety: &DangerousApproval,
) -> Vec<String> {
    if safety.enabled && !tools.iter().any(|tool| tool == "dangerous_auto_approve") {
        tools.push("dangerous_auto_approve".to_string());
    }
    tools
}

fn write_launch_permission_audit(
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

fn team_workspace(team_dir: &Path) -> PathBuf {
    crate::model::paths::team_workspace(team_dir).unwrap_or_else(|_| {
        team_dir
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| team_dir.to_path_buf())
    })
}

fn agent_id_exists_in_team_dir(team_dir: &Path, agent_id: &AgentId) -> bool {
    let spec_path = team_dir.join("team.spec.yaml");
    if let Ok(text) = std::fs::read_to_string(&spec_path) {
        if let Ok(spec) = yaml::loads(&text) {
            return spec_agents(&spec)
                .into_iter()
                .any(|existing| existing.as_str() == agent_id.as_str());
        }
    }
    team_dir
        .join("agents")
        .join(format!("{}.md", agent_id.as_str()))
        .exists()
}


mod plan;
pub use plan::{handle_report_result, start_plan};
