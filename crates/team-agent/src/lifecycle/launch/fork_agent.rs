use super::*;
use crate::lifecycle::lock::{acquire_agent_lifecycle_lock, LifecycleLockRequest};
use crate::lifecycle::profile_launch::parse_provider;
use crate::lifecycle::*;
use crate::model::enums::{AuthMode, DisplayBackend, Provider, ProviderEffort};
use crate::model::ids::AgentId;
use crate::model::permissions::{self, AgentPermissionInput};
use crate::model::yaml::{self, Value};
use crate::state::persist::load_runtime_state;
use crate::transport::{PaneId, SessionName, Target, Transport, WindowName};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

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
    // Fork requires the complete source tuple before treating session_id as
    // resumable truth; a scalar-only row has no confirmed backing.
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
    let Some(source_backing_raw) = rollout_path_str else {
        return Err(LifecycleError::Provider(format!(
            "cannot fork {source_agent_id}: source session backing is missing"
        )));
    };
    let source_backing = Path::new(source_backing_raw);
    if !source_backing.is_file() {
        return Err(LifecycleError::Provider(format!(
            "cannot fork {source_agent_id}: source session backing is not readable: {}",
            source_backing.display()
        )));
    }
    let Some(source_session_id) = session_id_str else {
        return Err(LifecycleError::Provider(format!(
            "cannot fork {source_agent_id}: source session id is missing"
        )));
    };
    let session_id = crate::provider::SessionId::new(source_session_id.to_string());
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
    let mut materialized_role = materialize_latest_role(
        &workspace,
        &fork_team_dir,
        &state,
        source_agent_id,
        as_agent_id,
        label,
    )?;
    clamp_materialized_role_to_leader(materialized_role.path(), &spec)?;
    let team_meta = crate::compiler::read_front_matter(&fork_team_dir.join("TEAM.md"))
        .map(|(meta, _)| meta)
        .map_err(|error| LifecycleError::Compile(error.to_string()))?;
    let workspace_s = workspace.to_string_lossy().to_string();
    let compiled =
        crate::compiler::compile_role_agent(materialized_role.path(), &team_meta, &workspace_s)
            .map_err(|error| LifecycleError::Compile(error.to_string()))?;
    let new_spec =
        append_forked_agent(&spec, &compiled.agent, source_agent_id, as_agent_id, label)?;
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
    let mut profile_launch =
        match crate::lifecycle::profile_launch::prepare_provider_profile_launch_with_profile_dir(
            &workspace,
            as_agent_id.as_str(),
            new_agent,
            Some(&profile_dir),
            Some(&mcp_config),
        ) {
            Ok(profile_launch) => profile_launch,
            Err(error) => {
                let _ = std::fs::write(&spec_path, text.as_bytes());
                let _ = std::fs::remove_file(&mcp_config_path);
                return Err(error);
            }
        };
    let mut copilot_fork = None;
    if provider == Provider::Copilot {
        let materialized = crate::provider::adapters::copilot_fork::materialize_copilot_fork(
            &workspace,
            as_agent_id.as_str(),
            &session_id,
        )
        .map_err(|error| {
            let _ = std::fs::write(&spec_path, text.as_bytes());
            cleanup_fork_mcp_artifacts(&workspace, as_agent_id, &mcp_config_path, &profile_launch);
            LifecycleError::Provider(error.to_string())
        })?;
        profile_launch.env_overlay.insert(
            "COPILOT_HOME".to_string(),
            materialized.home().to_string_lossy().to_string(),
        );
        profile_launch.env_overlay.insert(
            "TEAM_AGENT_INTERNAL_COPILOT_FORK_SESSION_ID".to_string(),
            materialized.session_id().as_str().to_string(),
        );
        copilot_fork = Some(materialized);
    }
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
            cleanup_fork_mcp_artifacts(&workspace, as_agent_id, &mcp_config_path, &profile_launch);
            LifecycleError::Provider(e.to_string())
        })?;
    profile_launch
        .env_overlay
        .remove("TEAM_AGENT_INTERNAL_COPILOT_FORK_SESSION_ID");
    if !plan.managed_mcp_config && !profile_launch.managed_mcp_config {
        point_native_mcp_config_at_file(&mut plan.argv, provider, &mcp_config_path);
    }
    fill_spawn_placeholders_full(
        &mut plan.argv,
        &workspace,
        as_agent_id.as_str(),
        Some(&fork_team),
    );
    if matches!(provider, Provider::Claude | Provider::ClaudeCode) {
        plan.provider_projects_root = source_backing.parent().map(Path::to_path_buf);
    }
    let window = WindowName::new(as_agent_id.as_str());
    let backing_before = crate::provider::session::ContextBackingSnapshot::capture(provider, &plan);
    let mut claude_fork = prepare_claude_fork_backing(provider, &plan, source_backing, &session_id)
        .map_err(|error| {
            let _ = std::fs::write(&spec_path, text.as_bytes());
            cleanup_fork_mcp_artifacts(&workspace, as_agent_id, &mcp_config_path, &profile_launch);
            error
        })?;
    let mut env =
        inherited_env_with_team_overrides(&workspace, as_agent_id.as_str(), Some(&fork_team));
    apply_profile_launch_env(&mut env, &profile_launch);
    apply_mcp_auto_approval_env(&mut env, &safety);
    if provider == Provider::Copilot {
        apply_copilot_instructions_overlay(
            &workspace,
            as_agent_id.as_str(),
            &system_prompt,
            &mut env,
        )
        .map_err(|error| {
            let _ = std::fs::write(&spec_path, text.as_bytes());
            cleanup_fork_mcp_artifacts(&workspace, as_agent_id, &mcp_config_path, &profile_launch);
            error
        })?;
    }
    let mut reserved_state = state.clone();
    reserve_forked_agent_state(
        &mut reserved_state,
        source_agent_id,
        as_agent_id,
        new_agent,
        materialized_role.path(),
    )?;
    if let Err(error) = crate::state::repository::StateRepository::new(&workspace).save(
        crate::state::repository::StateWriteIntent::ForkAgent {
            team_key: &fork_team,
            agent_id: as_agent_id.as_str(),
        },
        &reserved_state,
    ) {
        let _ = std::fs::write(&spec_path, text.as_bytes());
        cleanup_fork_mcp_artifacts(&workspace, as_agent_id, &mcp_config_path, &profile_launch);
        return Err(LifecycleError::StatePersist(error.to_string()));
    }
    // Match launch/restart: absent session spawns first; otherwise add a window.
    let session_live = transport.has_session(&session_name).unwrap_or(false);
    let env_unset = crate::layout::worker_env::isolate_worker_spawn_env(
        provider,
        &mut env,
        extend_worker_env_unset_for_effort(
            profile_launch.env_unset.iter().cloned().collect(),
            provider,
        ),
    );
    // Release the metadata lock before per-seat provider convergence; finalize reacquires it.
    drop(_lock);
    let spawned_at = spawn_timestamp();
    let spawn_epoch = 1;
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
    let spawn = match spawn_result {
        Ok(spawn) => spawn,
        Err(error) => {
            rollback_fork_after_spawn(
                &workspace,
                transport,
                &session_name,
                &window,
                &mcp_config_path,
                as_agent_id,
                &profile_launch,
                &fork_team,
            );
            return Err(LifecycleError::Transport(error.to_string()));
        }
    };
    ensure_fork_spawn_live(ForkPostSpawnInput {
        workspace: &workspace,
        transport,
        session_name: &session_name,
        window: &window,
        mcp_config_path: &mcp_config_path,
        agent_id: as_agent_id,
        profile_launch: &profile_launch,
        team_key: &fork_team,
        spawn: &spawn,
    })?;
    let convergence_deadline =
        crate::provider::session::context_fork_convergence_deadline(provider);
    let context_proof = match crate::provider::session::verify_context_fork(
        provider,
        &session_id,
        &plan,
        &backing_before,
        claude_fork.as_ref().map(|materialized| materialized.path()),
        as_agent_id.as_str(),
        &workspace,
        &spawned_at,
        convergence_deadline,
    ) {
        Ok(proof) => proof,
        Err(error) => {
            rollback_fork_after_spawn(
                &workspace,
                transport,
                &session_name,
                &window,
                &mcp_config_path,
                as_agent_id,
                &profile_launch,
                &fork_team,
            );
            return Err(LifecycleError::Provider(error.to_string()));
        }
    };
    if let Err(error) = finalize_fork_state(ForkFinalizeInput {
        workspace: &workspace,
        team_key: &fork_team,
        source_agent_id,
        agent_id: as_agent_id,
        spec_agent: new_agent,
        safety: &safety,
        plan: &plan,
        profile_launch: &profile_launch,
        spawn: &spawn,
        profile_dir: &profile_dir,
        dynamic_role_file: materialized_role.path(),
        context_proof: &context_proof,
        spawned_at: &spawned_at,
        spawn_epoch,
    }) {
        rollback_fork_after_spawn(
            &workspace,
            transport,
            &session_name,
            &window,
            &mcp_config_path,
            as_agent_id,
            &profile_launch,
            &fork_team,
        );
        return Err(error);
    }
    if let Err(error) =
        verify_fork_registration(&workspace, &fork_team, as_agent_id, &spawn, &window)
    {
        rollback_fork_after_spawn(
            &workspace,
            transport,
            &session_name,
            &window,
            &mcp_config_path,
            as_agent_id,
            &profile_launch,
            &fork_team,
        );
        return Err(error);
    }
    let coordinator_started = start_fork_coordinator(ForkCoordinatorInput {
        workspace: &workspace,
        team_key: &fork_team,
        agent_id: as_agent_id,
        transport,
        session_name: &session_name,
        window: &window,
        mcp_config_path: &mcp_config_path,
        profile_launch: &profile_launch,
    })?;
    materialized_role.keep();
    if let Some(materialized) = claude_fork.as_mut() {
        materialized.keep();
    }
    if let Some(materialized) = copilot_fork.as_mut() {
        materialized.keep();
    }
    Ok(ForkAgentReport {
        source_agent_id: source_agent_id.clone(),
        new_agent_id: as_agent_id.clone(),
        env: AgentActionEnvelope {
            agent_id: as_agent_id.clone(),
            state_file: crate::state::persist::runtime_state_path(&workspace),
            coordinator_started,
        },
        session_id: Some(context_proof.new_session_id),
    })
}
