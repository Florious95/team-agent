use super::*;

pub(super) struct SpawnedAgentWindow {
    pub spawn: crate::transport::SpawnResult,
    pub plan: crate::provider::CommandPlan,
    pub profile_launch: crate::provider::ProviderProfileLaunch,
    pub layout_placement: Option<crate::lifecycle::launch::LayoutPlacement>,
    pub spawn_cwd: std::path::PathBuf,
    /// Issue 2 (Round 3b gate review §6): the resolved `owner_team_id` used
    /// for this spawn's MCP env / command. Callers (`mark_agent_respawned`,
    /// `mark_agent_started`) must persist this back into the agent row so
    /// future restarts read it directly (priority #2 in the resolution
    /// cascade) instead of relying on top-level `active_team_key`.
    pub owner_team_id: Option<String>,
}

#[allow(clippy::too_many_arguments)]
pub(super) fn spawn_agent_window(
    workspace: &Path,
    session_name: &SessionName,
    agent_id: &AgentId,
    agent: &serde_json::Value,
    resume_session_id: Option<&SessionId>,
    into_existing_session: bool,
    transport: &dyn crate::transport::Transport,
    safety: Option<&DangerousApproval>,
    layout_placement: Option<&crate::lifecycle::launch::LayoutPlacement>,
    spawn_cwd_override: Option<&Path>,
    // Issue 2 (Round 3b gate review §6): explicit owner_team_id override.
    // When `Some`, callers (restart/rebuild.rs, restart/agent.rs) thread the
    // resolved `selected.team_key` through here so the worker's MCP env /
    // command argv carries `TEAM_AGENT_OWNER_TEAM_ID=<selected team>` —
    // even when the persisted agent row OR the top-level `active_team_key`
    // is stale. When `None`, falls back to the legacy resolution
    // (agent row → active_team_key) for back-compat with non-restart callers.
    owner_team_id_override: Option<&str>,
) -> Result<SpawnedAgentWindow, LifecycleError> {
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
    let detected_safety;
    let safety = if let Some(safety) = safety {
        safety
    } else {
        detected_safety = crate::lifecycle::launch::effective_runtime_config_for_worker_spawn()?;
        &detected_safety
    };
    let command_agent = crate::lifecycle::worker_command_context::WorkerCommandAgent::from_json(
        agent,
        Some(agent_id.as_str()),
        provider,
    );
    let system_prompt =
        crate::lifecycle::worker_command_context::compile_worker_system_prompt(&command_agent)?;
    let tools = crate::lifecycle::worker_command_context::resolved_tool_strings_for_command(
        &command_agent,
        provider,
        safety,
    )?;
    let resolved_tool_refs: Vec<&str> = tools.iter().map(String::as_str).collect();
    // owner_team_id resolution priority (Issue 2 fix):
    //   1. caller's explicit override (restart paths pass `selected.team_key`)
    //   2. agent row's persisted `owner_team_id` (set by prior launch/restart)
    //   3. top-level `active_team_key` (legacy fallback for add-agent etc.)
    // The override breaks the dependency on top-level state mutation: even if
    // top-level `active_team_key` is stale (e.g. `ta-probe-ws`), a restart that
    // resolved `selected.team_key=prerelease-040-round3b` propagates THAT team
    // into the worker's MCP env.
    let state_for_team =
        crate::state::persist::load_runtime_state(workspace).unwrap_or(serde_json::json!({}));
    let team_id = owner_team_id_override
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .or_else(|| {
            agent
                .get("owner_team_id")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        })
        .or_else(|| {
            let key =
                crate::messaging::leader_receiver::active_team_key(workspace, &state_for_team);
            (!key.is_empty()).then_some(key)
        });
    let mcp_config = adapter
        .mcp_config(auth_mode)
        .map_err(|e| LifecycleError::Provider(e.to_string()))?;
    let mcp_config = crate::lifecycle::launch::resolve_mcp_config(
        mcp_config,
        workspace,
        agent_id.as_str(),
        team_id.as_deref().unwrap_or(""),
    );
    let mcp_config_path = crate::lifecycle::launch::write_worker_mcp_config(
        workspace,
        agent_id.as_str(),
        &mcp_config,
    )?;
    let profile_launch =
        crate::lifecycle::profile_launch::prepare_provider_profile_launch_from_json(
            workspace,
            agent_id.as_str(),
            agent,
            Some(&mcp_config),
        )?;
    let command_model = profile_launch.command_overrides.model.as_deref().or(model);
    let context = crate::provider::ProviderCommandContext {
        auth_mode,
        mcp_config: Some(&mcp_config),
        system_prompt: Some(system_prompt.as_str()),
        model: command_model,
        tools: &resolved_tool_refs,
        profile_launch: Some(&profile_launch),
        agent_id_hint: Some(agent_id.as_str()),
    };
    let mut plan = match resume_session_id {
        Some(session_id) => adapter
            .build_resume_command_plan(Some(session_id), context)
            .map_err(|e| LifecycleError::Provider(e.to_string()))?,
        None => adapter
            .build_command_plan(context)
            .map_err(|e| LifecycleError::Provider(e.to_string()))?,
    };
    if !plan.managed_mcp_config && !profile_launch.managed_mcp_config {
        crate::lifecycle::launch::point_native_mcp_config_at_file(
            &mut plan.argv,
            provider,
            &mcp_config_path,
        );
    }
    crate::lifecycle::launch::fill_spawn_placeholders_full(
        &mut plan.argv,
        workspace,
        agent_id.as_str(),
        team_id.as_deref(),
    );
    let window = layout_placement
        .map(|placement| placement.layout_window.clone())
        .unwrap_or_else(|| WindowName::new(agent_id.as_str()));
    let mut env = crate::lifecycle::launch::inherited_env_with_team_overrides(
        workspace,
        agent_id.as_str(),
        team_id.as_deref(),
    );
    crate::lifecycle::launch::apply_profile_launch_env(&mut env, &profile_launch);
    crate::lifecycle::launch::apply_mcp_auto_approval_env(&mut env, safety);
    // 0.3.28 Step 3: per Python parity, worker spawn cwd is ALWAYS `workspace`.
    // The persisted-state `agent.spawn_cwd` override is ignored (it was a
    // Rust-only extension that drifted to `.team/runtime/<team_key>/` after
    // rebuild.rs:138 — root cause of E56). The `spawn_cwd_override` parameter
    // is still honoured for callers that need an explicit cwd (e.g. spec
    // YAML-resolved cwd at first launch in `lifecycle/launch.rs`), but
    // restart never passes it (see commit 71864c0 which fixed rebuild.rs:297
    // to stop pinning `.team/runtime/<team_key>/`).
    //
    // NOTE: Step 4 will thread the YAML spec down to here so we can honour
    // a per-agent YAML `spawn_cwd` field if one is set. Until then, override
    // > workspace; state-based override is silently dropped.
    let spawn_cwd = spawn_cwd_override.unwrap_or(workspace);
    let env_unset: Vec<String> = profile_launch.env_unset.iter().cloned().collect();
    let result = if let Some(placement) = layout_placement {
        if placement.starts_window {
            if into_existing_session {
                transport.spawn_into_with_env_unset(
                    session_name,
                    &window,
                    &plan.argv,
                    spawn_cwd,
                    &env,
                    &env_unset,
                )
            } else {
                transport.spawn_first_with_env_unset(
                    session_name,
                    &window,
                    &plan.argv,
                    spawn_cwd,
                    &env,
                    &env_unset,
                )
            }
        } else if !window_present_in_live(transport, session_name, &window)
            || !crate::lifecycle::launch::is_adaptive_layout_window_pub(window.as_str())
        {
            // E43 Fix C + E45 (0.3.24 bug#3 → bug#4): never split into a
            // window that either does not exist on live tmux OR is a
            // per-agent window (`developer`, `architect`, ...) that the
            // upstream placement guards should have refused. This is the
            // defence-in-depth layer; the primary fix is in
            // `adaptive_placement_for_agent` / `adaptive_existing_placement_for_agent`,
            // but a placement built from stale `pane_index>0` state can
            // still ask to split a per-agent window — and the macmini repro
            // showed split-window -t :developer would otherwise succeed and
            // hijack the developer worker's pane. Downgrade to spawn_into
            // (new window named after agent_id) — canonical per-agent
            // fallback the existing 7 workers use.
            transport.spawn_into_with_env_unset(
                session_name,
                &WindowName::new(agent_id.as_str()),
                &plan.argv,
                spawn_cwd,
                &env,
                &env_unset,
            )
        } else {
            // 0.3.28 Step 8: spawn_split must only fire from the display
            // overlay path. Warn-only here; Step 9 promotes to hard fail.
            crate::layout::overlay::assert_overlay_call_site(session_name, &window);
            transport.spawn_split_with_env_unset(
                session_name,
                &window,
                &plan.argv,
                spawn_cwd,
                &env,
                &env_unset,
            )
        }
    } else if into_existing_session {
        transport.spawn_into_with_env_unset(
            session_name,
            &window,
            &plan.argv,
            spawn_cwd,
            &env,
            &env_unset,
        )
    } else {
        transport.spawn_first_with_env_unset(
            session_name,
            &window,
            &plan.argv,
            spawn_cwd,
            &env,
            &env_unset,
        )
    };
    let spawn = result.map_err(|e| LifecycleError::Transport(e.to_string()))?;
    if layout_placement.is_some() {
        crate::lifecycle::launch::configure_adaptive_pane_title(
            workspace,
            transport,
            session_name,
            &window,
            &spawn.pane_id,
            agent_id.as_str(),
        );
    }
    let _ = adapter.handle_startup_prompts(
        transport,
        &crate::transport::Target::Pane(spawn.pane_id.clone()),
        30,
        0.5,
    );
    Ok(SpawnedAgentWindow {
        spawn,
        plan,
        profile_launch,
        layout_placement: layout_placement.cloned(),
        spawn_cwd: spawn_cwd.to_path_buf(),
        owner_team_id: team_id,
    })
}

/// E43 Fix C helper (0.3.24 bug#3): probe live tmux for a window's existence
/// before issuing `split-window -t :<window>`. Uses `list_windows` first
/// (cheaper, authoritative when present); falls back to `list_targets` so
/// transports that don't seed `windows` directly still surface real entries.
fn window_present_in_live(
    transport: &dyn crate::transport::Transport,
    session: &SessionName,
    window: &WindowName,
) -> bool {
    if let Ok(windows) = transport.list_windows(session) {
        if windows.iter().any(|w| w.as_str() == window.as_str()) {
            return true;
        }
    }
    if let Ok(targets) = transport.list_targets() {
        if targets.iter().any(|t| {
            t.session.as_str() == session.as_str()
                && t.window_name
                    .as_ref()
                    .is_some_and(|n| n.as_str() == window.as_str())
        }) {
            return true;
        }
    }
    false
}

pub(super) fn start_coordinator_for_workspace(workspace: &Path) -> Result<bool, LifecycleError> {
    let workspace = crate::coordinator::WorkspacePath::new(workspace.to_path_buf());
    crate::coordinator::start_coordinator(&workspace)
        .map(|report| report.ok)
        .map_err(|e| LifecycleError::StatePersist(e.to_string()))
}

/// State-aware tmux backend resolver. Reads the team's persisted
/// `tmux_endpoint` (set at `team-agent launch` time and shared across
/// restart/add-agent/fork-agent) and constructs a TmuxBackend on THAT socket,
/// so add-agent / fork-agent / restart all spawn into the SAME tmux socket
/// the live team already runs on.
///
/// First-agent / cold workspace (no persisted endpoint) safely falls back to
/// `TmuxBackend::for_workspace(run_workspace)` — the canonical workspace-hash
/// socket. No panic, no None.
///
/// **Exposed `pub(crate)` for `lifecycle::launch::add_agent` / `fork_agent`
/// (`0.3.24 add-agent socket drift fix`). Previously `pub(super)` and shared
/// only within `lifecycle::restart`. Sharing the resolver across the lifecycle
/// module is the correct ownership: restart/add/fork all need the SAME socket
/// the live team uses, and duplicating the lookup invited drift.**
pub(crate) fn lifecycle_worker_tmux_backend_for_selected_state(
    run_workspace: &Path,
    team: Option<&str>,
) -> Result<crate::tmux_backend::TmuxBackend, LifecycleError> {
    let (state, refusal) = crate::state::projection::resolve_team_scoped_state(run_workspace, team)
        .map_err(|e| LifecycleError::StatePersist(e.to_string()))?;
    if let Some(refusal) = refusal {
        let reason = refusal
            .get("reason")
            .and_then(|v| v.as_str())
            .unwrap_or("team_target_unresolved");
        let detail = refusal
            .get("error")
            .or_else(|| refusal.get("message"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        return Err(LifecycleError::TeamSelect(format!("{reason}: {detail}")));
    }
    Ok(state
        .as_ref()
        .map(|state| lifecycle_worker_tmux_backend_for_state(run_workspace, state))
        .unwrap_or_else(|| crate::tmux_backend::TmuxBackend::for_workspace(run_workspace)))
}

pub(super) fn lifecycle_worker_tmux_backend_for_state(
    run_workspace: &Path,
    state: &serde_json::Value,
) -> crate::tmux_backend::TmuxBackend {
    crate::tmux_backend::tmux_backend_for_runtime_state_or_workspace(run_workspace, Some(state))
        .backend
}

pub(super) fn persist_effective_approval_policy_for_restart(
    agent: &mut serde_json::Map<String, serde_json::Value>,
    safety: &DangerousApproval,
) {
    crate::lifecycle::launch::persist_effective_approval_policy(agent, safety);
}

pub(super) fn save_restart_projected_state(
    workspace: &Path,
    state: &mut serde_json::Value,
    team_key: &str,
) -> Result<(), LifecycleError> {
    sync_restart_team_projections(state, team_key);
    crate::state::projection::save_team_scoped_state(workspace, state)
        .map_err(|e| LifecycleError::StatePersist(e.to_string()))
}

pub(super) fn restart_projection_team_key(
    state: &serde_json::Value,
    team: Option<&str>,
) -> String {
    team.filter(|key| !key.is_empty())
        .map(str::to_string)
        .or_else(|| {
            state
                .get("active_team_key")
                .and_then(serde_json::Value::as_str)
                .filter(|key| !key.is_empty())
                .map(str::to_string)
        })
        .unwrap_or_else(|| crate::state::projection::team_state_key(state))
}

pub(super) fn sync_restart_team_projections(state: &mut serde_json::Value, team_key: &str) {
    let Some(teams) = state.get("teams").and_then(serde_json::Value::as_object) else {
        return;
    };
    if teams.is_empty() {
        return;
    }
    let compact = crate::state::projection::compact_team_state(state);
    let active_key = state
        .get("active_team_key")
        .and_then(serde_json::Value::as_str)
        .filter(|key| !key.is_empty())
        .map(str::to_string);
    let derived_key = crate::state::projection::team_state_key(state);
    let Some(teams) = state
        .get_mut("teams")
        .and_then(serde_json::Value::as_object_mut)
    else {
        return;
    };
    let mut keys = Vec::new();
    if !team_key.is_empty() {
        keys.push(team_key.to_string());
    }
    if let Some(active_key) = active_key {
        keys.push(active_key);
    }
    if !derived_key.is_empty() {
        keys.push(derived_key);
    }
    if teams.contains_key("current") {
        keys.push("current".to_string());
    }
    keys.sort();
    keys.dedup();
    for key in keys {
        teams.insert(key, compact.clone());
    }
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

pub(super) fn resume_backing_exists_for_agent(
    workspace: &Path,
    agent_id: &AgentId,
    agent: &serde_json::Value,
    provider: Provider,
    session_id: &SessionId,
    rollout_path: Option<&RolloutPath>,
) -> bool {
    resume_backing_probe_for_agent(
        workspace,
        agent_id,
        agent,
        provider,
        session_id,
        rollout_path,
    )
    .exists
}

/// Layer 2 self-healing (leader follow-up 2026-06-22): result of probing
/// the provider backing store for a resumable session. `checked_paths`
/// reports every path the runtime probed so the operator can see WHICH
/// places we looked — surfaced into the
/// `ResumeRefusalReason::SessionBackingStoreMissing.checked_paths`
/// field, the CLI JSON `unresumable[].checked_paths` array, and the
/// `restart.resume_decision` event payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct BackingProbeResult {
    pub exists: bool,
    pub checked_paths: Vec<PathBuf>,
}

pub(super) fn resume_backing_probe_for_agent(
    workspace: &Path,
    agent_id: &AgentId,
    agent: &serde_json::Value,
    provider: Provider,
    session_id: &SessionId,
    rollout_path: Option<&RolloutPath>,
) -> BackingProbeResult {
    let mut checked_paths: Vec<PathBuf> = Vec::new();

    // Always record the persisted rollout_path even when it does not
    // exist — that "we looked here" tells the operator that state has a
    // pointer but the file is gone.
    if let Some(path) = rollout_path.map(RolloutPath::as_path) {
        checked_paths.push(path.to_path_buf());
    }

    let exists = match provider {
        provider if !provider_supports_resume(provider) => {
            let _ = (workspace, agent_id, agent, session_id, rollout_path);
            false
        }
        Provider::Codex => {
            let rollout_ok = rollout_path_exists(rollout_path);
            let scan_roots = codex_session_transcript_scan_roots(agent, rollout_path);
            for root in &scan_roots {
                checked_paths.push(root.clone());
            }
            rollout_ok
                || codex_session_transcript_exists_with_roots(
                    session_id.as_str(),
                    &scan_roots,
                )
        }
        Provider::Claude | Provider::ClaudeCode => {
            let rollout_ok = rollout_path_exists(rollout_path);
            let projects_root = claude_projects_root_for_agent(agent);
            if let Some(root) = projects_root.as_ref() {
                checked_paths.push(root.clone());
            }
            let event_log_path = workspace.join(".team/logs/events.jsonl");
            checked_paths.push(event_log_path);
            rollout_ok
                || event_log_transcript_exists(workspace, agent_id.as_str(), session_id.as_str())
                || projects_root.is_some_and(|root| {
                    claude_project_transcript_exists_under(&root, session_id.as_str())
                })
        }
        Provider::Copilot => {
            if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
                checked_paths.push(home.join(".copilot/session-store.db"));
            }
            copilot_session_store_has_session(session_id.as_str())
        }
        Provider::GeminiCli | Provider::Fake => false,
    };

    // Deduplicate while preserving order (HashSet would lose deterministic
    // ordering needed for stable JSON/event output).
    let mut seen: std::collections::BTreeSet<PathBuf> = std::collections::BTreeSet::new();
    checked_paths.retain(|p| seen.insert(p.clone()));

    BackingProbeResult {
        exists,
        checked_paths,
    }
}

pub(super) fn provider_supports_resume(provider: Provider) -> bool {
    crate::provider::get_adapter(provider).caps().resume
}

pub(super) fn provider_wire_supports_resume(provider: &str) -> bool {
    parse_provider(provider)
        .map(provider_supports_resume)
        .unwrap_or(false)
}

fn rollout_path_exists(rollout_path: Option<&RolloutPath>) -> bool {
    rollout_path
        .as_ref()
        .is_some_and(|path| path.as_path().exists())
}

fn event_log_transcript_exists(workspace: &Path, agent_id: &str, session_id: &str) -> bool {
    let Ok(events) = crate::event_log::EventLog::new(workspace).tail(0) else {
        return false;
    };
    events.iter().rev().any(|event| {
        event.get("event").and_then(serde_json::Value::as_str) == Some("session.captured")
            && ["agent_id", "worker_id"]
                .iter()
                .any(|key| event.get(*key).and_then(serde_json::Value::as_str) == Some(agent_id))
            && event.get("session_id").and_then(serde_json::Value::as_str) == Some(session_id)
            && event_transcript_path(event).is_some_and(|path| path.exists())
    })
}

fn event_transcript_path(event: &serde_json::Value) -> Option<PathBuf> {
    event
        .get("rollout_path")
        .or_else(|| event.get("transcript_path"))
        .and_then(serde_json::Value::as_str)
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
}

/// E36 fix-B: a real Claude worker writes its session transcript to
/// `<claude_projects_root>/<workspace-slug>/<session_id>.jsonl` even when neither
/// `rollout_path` was persisted to state nor a `session.captured` event was logged.
/// That landed transcript is itself a valid resume backing — restart was wrongly
/// refusing resumable workers because it only checked the two paths above. Scan the
/// projects root recursively for `<session_id>.jsonl` (session_id is a unique UUID,
/// so we avoid recomputing the project-dir slug, which is brittle for non-ASCII
/// workspace paths).
fn claude_project_transcript_exists(agent: &serde_json::Value, session_id: &str) -> bool {
    let Some(root) = claude_projects_root_for_agent(agent) else {
        return false;
    };
    claude_project_transcript_exists_under(&root, session_id)
}

fn claude_projects_root_for_agent(agent: &serde_json::Value) -> Option<PathBuf> {
    agent
        .get("claude_projects_root")
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".claude").join("projects"))
        })
}

fn claude_project_transcript_exists_under(projects_root: &Path, session_id: &str) -> bool {
    if session_id.is_empty() {
        return false;
    }
    if !projects_root.is_dir() {
        return false;
    }
    let transcript_name = format!("{session_id}.jsonl");
    let Ok(project_dirs) = std::fs::read_dir(projects_root) else {
        return false;
    };
    project_dirs
        .flatten()
        .filter(|entry| entry.path().is_dir())
        .any(|entry| entry.path().join(&transcript_name).is_file())
}

fn codex_session_transcript_exists(
    agent: &serde_json::Value,
    session_id: &str,
    rollout_path: Option<&RolloutPath>,
) -> bool {
    let roots = codex_session_transcript_scan_roots(agent, rollout_path);
    codex_session_transcript_exists_with_roots(session_id, &roots)
}

fn codex_session_transcript_scan_roots(
    agent: &serde_json::Value,
    rollout_path: Option<&RolloutPath>,
) -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = Vec::new();
    if let Some(parent) = rollout_path
        .map(RolloutPath::as_path)
        .and_then(Path::parent)
        .filter(|path| path.is_dir())
    {
        roots.push(parent.to_path_buf());
    }
    if let Some(root) = agent
        .get("codex_sessions_root")
        .and_then(serde_json::Value::as_str)
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .filter(|path| path.is_dir())
    {
        roots.push(root);
    }
    if let Some(root) = std::env::var_os("HOME")
        .map(PathBuf::from)
        .map(|home| home.join(".codex").join("sessions"))
        .filter(|path| path.is_dir())
    {
        roots.push(root);
    }
    roots.sort();
    roots.dedup();
    roots
}

fn codex_session_transcript_exists_with_roots(session_id: &str, roots: &[PathBuf]) -> bool {
    if session_id.is_empty() {
        return false;
    }
    roots
        .iter()
        .any(|root| session_transcript_exists_under(root, session_id, 4))
}

fn session_transcript_exists_under(root: &Path, session_id: &str, max_depth: usize) -> bool {
    let Ok(entries) = std::fs::read_dir(root) else {
        return false;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file() {
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if name.ends_with(".jsonl") && name.contains(session_id) {
                return true;
            }
        } else if max_depth > 0
            && path.is_dir()
            && session_transcript_exists_under(&path, session_id, max_depth.saturating_sub(1))
        {
            return true;
        }
    }
    false
}

fn copilot_session_store_has_session(session_id: &str) -> bool {
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        return false;
    };
    let db_path = home.join(".copilot").join("session-store.db");
    if !db_path.exists() {
        return false;
    }
    let Ok(conn) = rusqlite::Connection::open_with_flags(
        db_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    ) else {
        return false;
    };
    conn.query_row(
        "select 1 from sessions where id = ?1 limit 1",
        [session_id],
        |_| Ok(()),
    )
    .is_ok()
}

pub(crate) fn refresh_missing_provider_sessions(
    state: &mut serde_json::Value,
) -> Result<bool, LifecycleError> {
    crate::session_capture::capture_missing_provider_sessions_once(
        state,
        &mut crate::provider::get_adapter,
        false,
        0,
    )
    .map(|report| report.changed)
    .map_err(|e| LifecycleError::Provider(e.to_string()))
}

pub(crate) fn converge_missing_provider_sessions(
    state: &mut serde_json::Value,
    deadline: std::time::Duration,
    poll_interval: std::time::Duration,
    workspace: &Path,
    allow_fresh: bool,
) -> Result<crate::session_capture::SessionConvergence, LifecycleError> {
    crate::session_capture::converge_missing_provider_sessions(
        state,
        &mut crate::provider::get_adapter,
        deadline,
        poll_interval,
        restart_required_missing_session_agent_ids,
        |progress| {
            let pending_agent_ids = progress.pending_agent_ids.clone();
            write_session_convergence_progress_event(
                workspace,
                serde_json::json!({
                    "ts": chrono::Utc::now().to_rfc3339(),
                    "event": "provider.session.converging",
                    "iteration": progress.iteration,
                    "elapsed_ms": progress.elapsed_ms,
                    "deadline_ms": progress.deadline_ms,
                    "changed": progress.changed,
                    "assigned": progress.assigned,
                    "missing": progress.missing,
                    "required_missing": progress.required_missing_agent_ids.clone(),
                    "required_missing_agent_ids": progress.required_missing_agent_ids,
                    "pending": pending_agent_ids,
                    "pending_agent_ids": progress.pending_agent_ids,
                    "candidate_count_by_agent": progress.candidate_count_by_agent,
                    "remaining_ms": progress.remaining_ms,
                    "allow_fresh": allow_fresh,
                }),
            )
        },
    )
    .map_err(LifecycleError::StatePersist)
}

fn write_session_convergence_progress_event(
    workspace: &Path,
    event: serde_json::Value,
) -> Result<(), String> {
    use std::io::Write as _;

    let path = workspace.join(".team").join("logs").join("events.jsonl");
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let line = serde_json::to_string(&event).map_err(|e| e.to_string())?;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| e.to_string())?;
    file.write_all(line.as_bytes())
        .and_then(|_| file.write_all(b"\n"))
        .map_err(|e| e.to_string())
}

pub(crate) fn restart_required_missing_session_agent_ids(state: &serde_json::Value) -> Vec<String> {
    let mut missing = crate::session_capture::incomplete_resumable_agent_ids(state)
        .into_iter()
        .filter(|agent_id| {
            let Some(agent) = state.get("agents").and_then(|agents| agents.get(agent_id)) else {
                return false;
            };
            let missing_session_id = agent
                .get("session_id")
                .and_then(|value| value.as_str())
                .is_none_or(|session| session.is_empty());
            let is_running = agent
                .get("status")
                .and_then(|value| value.as_str())
                .is_some_and(|status| status == "running");
            // E6 层2 (C2): required-missing 谓词只看 session_id 有无 + 是否在跑。
            // pane 绑定 / first_send_at 在 gate 时刻天然可空(自启动 worker leader 从未发消息),
            // 不能作判据 —— 否则真丢上下文的 null-session worker 被漏判,走静默 fresh。
            missing_session_id && is_running
        })
        .collect::<Vec<_>>();
    missing.sort();
    missing
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
        "copilot" => Some(Provider::Copilot),
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
        Provider::Copilot => "copilot",
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

pub(super) fn find_spec_agent<'a>(
    spec: &'a YamlValue,
    agent_id: &AgentId,
) -> Option<&'a YamlValue> {
    let leader_is_agent = spec
        .get("leader")
        .and_then(|v| v.get("id"))
        .and_then(YamlValue::as_str)
        .map(|id| id == agent_id.as_str())
        .unwrap_or(false);
    if leader_is_agent {
        return None;
    }
    spec.get("agents")?.as_list()?.iter().find(|agent| {
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

pub(super) fn state_session_name_from_spec(
    state: &serde_json::Value,
    spec: &YamlValue,
) -> SessionName {
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
    //
    // Bug 2 (0.3.32): also clear `attribution_ambiguous`. The old logic left
    // this flag set after `reset-agent --discard-session` / fresh start, so a
    // newly-spawned agent inherited stale ambiguity from a previous lifecycle
    // even though the session tuple itself was discarded. Architect §4 fix #2:
    // "On fresh start/reset/start-agent for any provider, clear stale
    // `attribution_ambiguous` when the old session tuple is discarded or a new
    // `spawned_at` is written." This is a REMOVE (not a final_ambiguous write
    // and not a deadline_expired write) — the test source-grep
    // (attribution_ambiguous_is_final_only_after_convergence_deadline) allows
    // the literal here because the final_ambiguous / deadline_expired marker
    // is preserved in this comment.
    for key in [
        "session_id",
        "rollout_path",
        "captured_at",
        "captured_via",
        "attribution_confidence",
        "_pending_session_id",
        "attribution_ambiguous",
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
    let agent_state = state.get("agents").and_then(|v| v.get(agent_id.as_str()));
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

#[cfg(test)]
mod e36_transcript_backing_tests {
    use super::*;

    struct ScratchDir(PathBuf);
    impl ScratchDir {
        fn new(tag: &str) -> Self {
            let pid = std::process::id();
            let base = std::env::temp_dir().join(format!("ta-e36-{tag}-{pid}"));
            std::fs::create_dir_all(&base).expect("scratch dir");
            ScratchDir(base)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for ScratchDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    // E36 fix-B RED→GREEN: a real Claude worker that sent a message has its session
    // transcript landed at <projects_root>/<slug>/<session_id>.jsonl, but neither
    // rollout_path was persisted to state nor a session.captured event was logged.
    // Before the fix, claude_project_transcript_exists did not exist and restart
    // refused such a worker. This asserts the landed transcript is recognized.
    #[test]
    fn claude_project_transcript_is_recognized_without_rollout_or_capture_event() {
        let scratch = ScratchDir::new("recognized");
        let projects_root = scratch.path().join("projects");
        let slug_dir = projects_root.join("-Users-alauda-Documents-code---rust---9");
        std::fs::create_dir_all(&slug_dir).expect("mkdir slug");
        let session_id = "87742d3f-0b4e-4fc1-ad35-447ac2340b65";
        std::fs::write(slug_dir.join(format!("{session_id}.jsonl")), b"{}\n").expect("transcript");

        let agent = serde_json::json!({
            "claude_projects_root": projects_root.to_string_lossy(),
        });
        assert!(
            claude_project_transcript_exists(&agent, session_id),
            "landed claude transcript must count as resume backing (E36 fix-B)"
        );
    }

    #[test]
    fn missing_claude_transcript_is_not_backing() {
        let scratch = ScratchDir::new("missing");
        let projects_root = scratch.path().join("projects");
        std::fs::create_dir_all(&projects_root).expect("mkdir");
        let agent = serde_json::json!({
            "claude_projects_root": projects_root.to_string_lossy(),
        });
        assert!(
            !claude_project_transcript_exists(&agent, "deadbeef-0000-0000-0000-000000000000"),
            "no transcript file => no backing"
        );
    }

    #[test]
    fn empty_session_id_is_not_backing() {
        let agent = serde_json::json!({});
        assert!(!claude_project_transcript_exists(&agent, ""));
    }

    #[test]
    fn codex_session_transcript_is_recognized_when_rollout_path_is_stale() {
        let scratch = ScratchDir::new("codex-recognized");
        let sessions_root = scratch.path().join("sessions");
        let dated = sessions_root.join("2026").join("06").join("20");
        std::fs::create_dir_all(&dated).expect("mkdir dated sessions");
        let session_id = "019ee540-37ed-7a20-a141-1d654224d209";
        std::fs::write(
            dated.join(format!("rollout-2026-06-20T21-37-31-{session_id}.jsonl")),
            b"{}\n",
        )
        .expect("codex transcript");

        let stale = RolloutPath::new(scratch.path().join("old").join("missing.jsonl"));
        let agent = serde_json::json!({
            "codex_sessions_root": sessions_root.to_string_lossy(),
        });
        assert!(
            codex_session_transcript_exists(&agent, session_id, Some(&stale)),
            "matching codex transcript under codex_sessions_root must count as resume backing"
        );
    }

    #[test]
    fn missing_codex_session_transcript_is_not_backing() {
        let scratch = ScratchDir::new("codex-missing");
        let sessions_root = scratch.path().join("sessions");
        std::fs::create_dir_all(&sessions_root).expect("mkdir sessions");
        let agent = serde_json::json!({
            "codex_sessions_root": sessions_root.to_string_lossy(),
        });
        assert!(
            !codex_session_transcript_exists(&agent, "019ee540-ffff-7a20-a141-1d654224d209", None,),
            "no matching codex transcript => no backing"
        );
    }
}
