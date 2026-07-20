use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::lifecycle::profile_launch::parse_provider;
use crate::lifecycle::*;
use crate::model::enums::{AuthMode, DisplayBackend, PaneLiveness, Provider, ProviderEffort};
use crate::model::ids::AgentId;
use crate::model::permissions::{self, AgentPermissionInput};
use crate::model::yaml::{self, Value};
use crate::state::persist::load_runtime_state;
use crate::transport::{PaneId, SessionName, Target, Transport, WindowName};

use crate::lifecycle::lock::{acquire_agent_lifecycle_lock, LifecycleLockRequest};

use super::*;

/// `fork_agent(workspace, source_agent_id, as_agent_id, ...)`(`lifecycle/operations.py:284`)。
/// native session fork(provider 须 supports_session_fork ∧ auth_mode!=compatible_api);
/// 失败回滚,每条失败臂 `adapter.cleanup_mcp`。
pub fn fork_agent(
    workspace: &Path,
    source_agent_id: &AgentId,
    as_agent_id: &AgentId,
    label: Option<&str>,
    open_display: bool,
    team: Option<&str>,
) -> Result<ForkAgentReport, LifecycleError> {
    let selected = crate::state::selector::resolve_active_team(
        workspace,
        team,
        crate::state::selector::SelectorMode::RequireSpec,
    )
    .map_err(|e| LifecycleError::TeamSelect(e.to_string()))?;
    // **0.3.24 add-agent socket drift fix** (same root cause): fork-agent must
    // also route to the live team's persisted tmux endpoint, not the workspace-
    // hash for_workspace socket. Same orphan-on-wrong-socket pathology.
    let transport = crate::lifecycle::restart::lifecycle_worker_tmux_backend_for_selected_state(
        &selected.run_workspace,
        Some(selected.team_key.as_str()),
    )
    .unwrap_or_else(|_| crate::tmux_backend::TmuxBackend::for_workspace(&selected.run_workspace));
    fork_agent_with_transport(
        workspace,
        source_agent_id,
        as_agent_id,
        label,
        open_display,
        team,
        &transport,
    )
}

pub fn fork_agent_with_transport(
    workspace: &Path,
    source_agent_id: &AgentId,
    as_agent_id: &AgentId,
    label: Option<&str>,
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
    let lock_workspace = selected.run_workspace.clone();
    let lock_team_key = selected.team_key.clone();
    let _lock = acquire_agent_lifecycle_lock(LifecycleLockRequest {
        workspace: &lock_workspace,
        operation: "fork-agent",
        team: Some(lock_team_key.as_str()),
        agent_id: Some(as_agent_id),
    })?;
    let selected = crate::state::selector::resolve_active_team(
        &lock_workspace,
        Some(lock_team_key.as_str()),
        crate::state::selector::SelectorMode::RequireSpec,
    )
    .map_err(|e| LifecycleError::TeamSelect(e.to_string()))?;
    // E5 §3:team_dir(角色定义+profiles)恒用户目录。spec 读用 selector 解析的 spec_path
    // (读序 B:runtime 优先、legacy 回落),写恒走 runtime_spec_path(canonical 落点)。
    let fork_team_dir = selected.team_dir.clone();
    let fork_team = selected.team_key.clone();
    let read_spec_path = selected
        .spec_path
        .clone()
        .ok_or_else(|| LifecycleError::TeamSelect("active team spec not found".to_string()))?;
    let workspace = selected.run_workspace;
    let state = selected.state;
    ensure_owner_allowed_for_state(&state, Some(source_agent_id))?;
    let spec_path = crate::model::paths::runtime_spec_path(&workspace, &fork_team);
    let text = std::fs::read_to_string(&read_spec_path)
        .map_err(|e| LifecycleError::Compile(format!("{}: {e}", read_spec_path.display())))?;
    let spec = yaml::loads(&text).map_err(|e| LifecycleError::Compile(e.to_string()))?;
    if find_spec_agent(&spec, as_agent_id).is_some() || leader_id_matches(&spec, as_agent_id) {
        return Err(LifecycleError::RequirementUnmet(format!(
            "agent id already exists: {as_agent_id}"
        )));
    }
    let source_agent = find_spec_agent(&spec, source_agent_id).ok_or_else(|| {
        LifecycleError::RequirementUnmet(format!("unknown worker agent id: {source_agent_id}"))
    })?;
    // 0.4.6 tuple-atomic contract (audit §Fork 修改清单, line 201): fork
    // must require the COMPLETE source tuple (session_id + rollout_path +
    // captured_at + captured_via) before treating the scalar session_id
    // as resumable truth. A row carrying only `session_id` is a partial
    // tuple (pre-0.4.6 bug source), and the native fork would attach to
    // a session that has no confirmed backing.
    let source_agent_state = state
        .get("agents")
        .and_then(|v| v.get(source_agent_id.as_str()))
        .ok_or_else(|| {
            LifecycleError::Provider(format!(
                "cannot fork {source_agent_id}: source agent row not in state"
            ))
        })?;
    let tuple_field_ok = |field: &str| -> bool {
        source_agent_state
            .get(field)
            .and_then(|v| v.as_str())
            .is_some_and(|s| !s.is_empty())
    };
    let session_id_str = source_agent_state
        .get("session_id")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty());
    let rollout_path_str = source_agent_state
        .get("rollout_path")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty());
    if session_id_str.is_none()
        || rollout_path_str.is_none()
        || !tuple_field_ok("captured_at")
        || !tuple_field_ok("captured_via")
    {
        return Err(LifecycleError::Provider(format!(
            "cannot fork {source_agent_id}: source session backing is missing or incomplete \
             (session_id+rollout_path+captured_at+captured_via required)"
        )));
    }
    let session_id = crate::provider::SessionId::new(session_id_str.unwrap().to_string());
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
    let new_spec = append_forked_agent(&spec, source_agent, source_agent_id, as_agent_id, label)?;
    // validate 用角色定义目录的 team_workspace(校验 working_directory),非 spec 落点。
    let validate_ws =
        crate::model::paths::team_workspace(&fork_team_dir).unwrap_or_else(|_| workspace.clone());
    crate::model::spec::validate_spec(&new_spec, &validate_ws)
        .map_err(|e| LifecycleError::Compile(e.to_string()))?;
    write_spec_atomic(&spec_path, &new_spec)?;
    let new_agent = find_spec_agent(&new_spec, as_agent_id).ok_or_else(|| {
        LifecycleError::RequirementUnmet(format!("unknown worker agent id: {as_agent_id}"))
    })?;
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
    let model = new_agent.get("model").and_then(Value::as_str);
    let safety = effective_runtime_config(&new_spec)?;
    let command_agent = crate::lifecycle::worker_command_context::WorkerCommandAgent::from_yaml(
        new_agent,
        Some(as_agent_id.as_str()),
        provider,
    );
    let system_prompt =
        crate::lifecycle::worker_command_context::compile_worker_system_prompt(&command_agent)?;
    let tools = crate::lifecycle::worker_command_context::resolved_tool_strings_for_command(
        &command_agent,
        provider,
        &safety,
    )?;
    let resolved_tool_refs: Vec<&str> = tools.iter().map(String::as_str).collect();
    let mcp_config = adapter.mcp_config(auth_mode).map_err(|e| {
        let _ = std::fs::write(&spec_path, text.as_bytes());
        LifecycleError::Provider(e.to_string())
    })?;
    let mcp_config = resolve_mcp_config(mcp_config, &workspace, as_agent_id.as_str(), &fork_team);
    let mcp_config_path = write_worker_mcp_config_for_provider(
        &workspace,
        as_agent_id.as_str(),
        &mcp_config,
        Some(provider),
    )
    .map_err(|e| {
        let _ = std::fs::write(&spec_path, text.as_bytes());
        e
    })?;
    // E5 §3:profiles 随角色定义目录(team_dir),不随已迁出的 spec。
    let profile_dir = fork_team_dir.join("profiles");
    let profile_launch =
        crate::lifecycle::profile_launch::prepare_provider_profile_launch_with_profile_dir(
            &workspace,
            as_agent_id.as_str(),
            new_agent,
            Some(&profile_dir),
            Some(&mcp_config),
        )?;
    let command_model = profile_launch.command_overrides.model.as_deref().or(model);
    // 0.4.x provider effort MVP: fork inherits effort from the new agent JSON
    // (compiler.rs propagated the role/team effort into the agent at fork-spawn).
    let fork_effort = provider_effort_for_spawn(new_agent, provider);
    if let Some(event_value) =
        provider_effort_event_if_dropped(new_agent, provider, as_agent_id.as_str())
    {
        let _ = crate::event_log::EventLog::new(&workspace)
            .write("provider.effort_unsupported", event_value);
    }
    let mut plan = adapter
        .fork_plan(
            Some(&session_id),
            crate::provider::ProviderCommandContext {
                auth_mode,
                mcp_config: Some(&mcp_config),
                system_prompt: Some(system_prompt.as_str()),
                model: command_model,
                tools: &resolved_tool_refs,
                profile_launch: Some(&profile_launch),
                agent_id_hint: Some(as_agent_id.as_str()),
                effort: fork_effort,
            },
        )
        .map_err(|e| {
            let _ = std::fs::write(&spec_path, text.as_bytes());
            LifecycleError::Provider(e.to_string())
        })?;
    if !plan.managed_mcp_config && !profile_launch.managed_mcp_config {
        point_native_mcp_config_at_file(&mut plan.argv, provider, &mcp_config_path);
    }
    fill_spawn_placeholders_full(
        &mut plan.argv,
        &workspace,
        as_agent_id.as_str(),
        Some(&fork_team),
    );
    let window = WindowName::new(as_agent_id.as_str());
    // fork inherits the parent agent's owner team via runtime state (`active_team_key`).
    let mut env =
        inherited_env_with_team_overrides(&workspace, as_agent_id.as_str(), Some(&fork_team));
    apply_profile_launch_env(&mut env, &profile_launch);
    apply_mcp_auto_approval_env(&mut env, &safety);
    // golden operations.py:336 -> _tmux_start_command_for_agent_window (runtime.py:1017-1020): branch on
    // _tmux_session_exists — an ABSENT session => new-session (spawn_first), present => new-window
    // (spawn_into). The Rust restart seam (restart.rs spawn_agent_window) uses the same branch.
    let session_live = transport.has_session(&session_name).unwrap_or(false);
    let env_unset = crate::layout::worker_env::isolate_worker_spawn_env(
        provider,
        &mut env,
        extend_worker_env_unset_for_effort(
            profile_launch.env_unset.iter().cloned().collect(),
            provider,
        ),
    );
    let spawn_result = if session_live {
        transport.spawn_into_with_env_unset(
            &session_name,
            &window,
            &plan.argv,
            &workspace,
            &env,
            &env_unset,
        )
    } else {
        transport.spawn_first_with_env_unset(
            &session_name,
            &window,
            &plan.argv,
            &workspace,
            &env,
            &env_unset,
        )
    };
    let spawn = spawn_result.map_err(|e| {
        let _ = std::fs::write(&spec_path, text.as_bytes());
        LifecycleError::Transport(e.to_string())
    })?;
    let old_state = state.clone();
    let mut next_state = state;
    upsert_forked_agent_state(
        &mut next_state,
        source_agent_id,
        as_agent_id,
        new_agent,
        &safety,
        &plan,
        &profile_launch,
        &spawn,
        &workspace,
        Some(&profile_dir),
    )?;
    if let Some(agent) = next_state
        .get_mut("agents")
        .and_then(serde_json::Value::as_object_mut)
        .and_then(|agents| agents.get_mut(as_agent_id.as_str()))
        .and_then(serde_json::Value::as_object_mut)
    {
        persist_effective_approval_policy(agent, &safety);
    }
    if let Err(e) = maybe_fail_fork_after_spawn("save_runtime_state") {
        rollback_fork_after_spawn(
            &workspace,
            &spec_path,
            &text,
            &old_state,
            transport,
            &session_name,
            &window,
            &mcp_config_path,
            as_agent_id,
            &profile_launch,
            &fork_team,
        );
        return Err(e);
    }
    if let Err(e) = crate::state::repository::StateRepository::new(&workspace).save(
        crate::state::repository::StateWriteIntent::ForkAgent {
            team_key: &fork_team,
            agent_id: as_agent_id.as_str(),
        },
        &next_state,
    ) {
        rollback_fork_after_spawn(
            &workspace,
            &spec_path,
            &text,
            &old_state,
            transport,
            &session_name,
            &window,
            &mcp_config_path,
            as_agent_id,
            &profile_launch,
            &fork_team,
        );
        return Err(LifecycleError::StatePersist(e.to_string()));
    }
    let registration =
        crate::state::projection::select_runtime_state(&workspace, Some(fork_team.as_str()))
            .map_err(|e| e.to_string())
            .and_then(|saved| {
                let agent = saved
                    .get("agents")
                    .and_then(|agents| agents.get(as_agent_id.as_str()))
                    .ok_or_else(|| "canonical team row is missing".to_string())?;
                if agent.get("pane_id").and_then(serde_json::Value::as_str)
                    != Some(spawn.pane_id.as_str())
                {
                    return Err("canonical team pane_id does not match spawned pane".to_string());
                }
                if agent.get("window").and_then(serde_json::Value::as_str) != Some(window.as_str())
                {
                    return Err("canonical team window does not match spawned window".to_string());
                }
                if let Some(pid) = spawn.child_pid {
                    if agent.get("pane_pid").and_then(serde_json::Value::as_u64)
                        != Some(u64::from(pid))
                    {
                        return Err(
                            "canonical team pane_pid does not match spawned process".to_string()
                        );
                    }
                }
                Ok(())
            });
    if let Err(reason) = registration {
        rollback_fork_after_spawn(
            &workspace,
            &spec_path,
            &text,
            &old_state,
            transport,
            &session_name,
            &window,
            &mcp_config_path,
            as_agent_id,
            &profile_launch,
            &fork_team,
        );
        return Err(LifecycleError::StatePersist(format!(
            "fork spawned but team registration readback failed: {reason}"
        )));
    }
    if let Err(e) = maybe_fail_fork_after_spawn("start_coordinator") {
        rollback_fork_after_spawn(
            &workspace,
            &spec_path,
            &text,
            &old_state,
            transport,
            &session_name,
            &window,
            &mcp_config_path,
            as_agent_id,
            &profile_launch,
            &fork_team,
        );
        return Err(e);
    }
    let coordinator_started = crate::coordinator::start_coordinator(
        &crate::coordinator::WorkspacePath::new(workspace.to_path_buf()),
    )
    .map(|report| report.ok)
    .map_err(|e| {
        rollback_fork_after_spawn(
            &workspace,
            &spec_path,
            &text,
            &old_state,
            transport,
            &session_name,
            &window,
            &mcp_config_path,
            as_agent_id,
            &profile_launch,
            &fork_team,
        );
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

pub(super) fn rollback_fork_after_spawn(
    workspace: &Path,
    spec_path: &Path,
    spec_text: &str,
    old_state: &serde_json::Value,
    transport: &dyn Transport,
    session_name: &SessionName,
    window: &WindowName,
    mcp_config_path: &Path,
    agent_id: &AgentId,
    profile_launch: &crate::provider::ProviderProfileLaunch,
    team_key: &str,
) {
    let _ = transport.kill_window(&Target::SessionWindow {
        session: session_name.clone(),
        window: window.clone(),
    });
    let _ = std::fs::write(spec_path, spec_text.as_bytes());
    let _ = crate::state::repository::StateRepository::new(workspace).save(
        crate::state::repository::StateWriteIntent::AgentRollback {
            team_key: Some(team_key),
            agent_id: agent_id.as_str(),
        },
        old_state,
    );
    cleanup_fork_mcp_artifacts(workspace, agent_id, mcp_config_path, profile_launch);
}
