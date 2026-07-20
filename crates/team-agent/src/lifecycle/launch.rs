//! lifecycle::launch —— 冷启 / quick-start / 危险审批探测 + add/fork / plan 起步与推进。

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::model::enums::{AuthMode, DisplayBackend, PaneLiveness, Provider, ProviderEffort};
use crate::model::ids::AgentId;
use crate::model::permissions::{self, AgentPermissionInput};
use crate::model::yaml::{self, Value};
use crate::state::persist::load_runtime_state;
use crate::transport::{PaneId, SessionName, Target, Transport, WindowName};

use crate::lifecycle::lock::{acquire_agent_lifecycle_lock, LifecycleLockRequest};

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
    let team_dir = spec_path.parent().unwrap_or_else(|| Path::new("."));
    let workspace = team_workspace(team_dir);
    launch_with_transport_in_workspace(
        &workspace,
        spec_path,
        dry_run,
        auto_approve,
        skip_profile_smoke,
        transport,
    )
}

pub fn launch_with_transport_in_workspace(
    workspace: &Path,
    spec_path: &Path,
    dry_run: bool,
    auto_approve: bool,
    skip_profile_smoke: bool,
    transport: &dyn Transport,
) -> Result<LaunchReport, LifecycleError> {
    let _ = skip_profile_smoke;
    // 0.5.38 (`.team/artifacts/startup-latency-locate.md` §5): launch.phase
    // timer with monotonic `elapsed_ms` for latency triage.
    let phase_timer = crate::lifecycle::restart::RestartPhaseTimer::start();
    if !spec_path.exists() {
        return Err(LifecycleError::Compile(format!(
            "spec path not found: {}",
            spec_path.display()
        )));
    }
    let text = std::fs::read_to_string(spec_path)
        .map_err(|e| LifecycleError::Compile(format!("{}: {e}", spec_path.display())))?;
    let spec = yaml::loads(&text).map_err(|e| LifecycleError::Compile(e.to_string()))?;
    phase_timer.emit(workspace, "launch.phase", "compile_spec");
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
    write_launch_permission_audit(workspace, &safety)?;
    let routes = spec_routes(&spec);
    let started = if dry_run {
        Vec::new()
    } else {
        phase_timer.emit(workspace, "launch.phase", "spawn_all");
        let started = spawn_agents(
            workspace,
            spec_path,
            &spec,
            &session_name,
            &safety,
            transport,
        )?;
        persist_spawn_agent_state(
            workspace,
            spec_path,
            &spec,
            &session_name,
            transport,
            &started,
            &safety,
        )?;
        // 0.5.38: per-worker timing tags (source="launch") so operators can
        // trace which worker's spawn dominates wall time. Zeros for now on
        // the sub-timings — Step 1 first enables shape assertion; a later
        // slice may thread real command_plan / transport_spawn / handler
        // timings from spawn_agents.
        for agent in &started {
            let provider = spec_agent_values(&spec)
                .into_iter()
                .find(|entry| {
                    entry
                        .get("id")
                        .and_then(Value::as_str)
                        .map(|id| id == agent.agent_id.as_str())
                        .unwrap_or(false)
                })
                .and_then(|entry| {
                    entry
                        .get("provider")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                })
                .unwrap_or_else(|| "fake".to_string());
            crate::lifecycle::restart::write_worker_spawn_timing_event(
                workspace,
                phase_timer.elapsed_ms(),
                agent.agent_id.as_str(),
                &provider,
                agent.start_mode,
                "new-window",
                0,
                0,
                0,
                0,
                "launch",
            );
        }
        started
    };
    // 0.3.28 Step 1: topology invariant guard (warn-only during migration).
    // Logs each violation to stderr; never panics. Promoted to hard error at
    // Step 10 once Steps 2–9 have eliminated structural co-location.
    if !dry_run {
        if let Ok(state_for_check) = crate::state::persist::load_runtime_state(workspace) {
            let violations =
                crate::layout::sessions::assert_topology_invariants(&state_for_check, &spec);
            crate::layout::sessions::log_topology_violations(&violations);
        }
    }
    Ok(LaunchReport {
        session_name,
        started,
        dry_run,
        tmux_endpoint: transport.tmux_endpoint(),
        routes,
        permissions,
        safety,
        leader_receiver_attached: false,
        session_capture_incomplete_agents: Vec::new(),
    })
}

mod plan;
pub use plan::{handle_report_result, start_plan};

pub mod spawn;
pub use spawn::SpawnPhase;
pub(super) use spawn::*;

mod layout;
pub(super) use layout::*;
pub(crate) use layout::{
    adaptive_existing_placement_for_agent, adaptive_layout_plan, adaptive_placement_for_agent,
    is_adaptive_layout_window_pub, state_uses_adaptive_layout, LayoutPlacement,
    ADAPTIVE_LAYOUT_MAX_PER_WINDOW,
};

mod state_projection;
use state_projection::{
    agent_id_to_pane_id, drop_bare_worker_seeded_owner, drop_foreign_seeded_owner,
    drop_unbound_top_level_owner, drop_worker_pane_seeded_owner, launched_worker_tmux_socket,
    merge_workspace_team_state_with_key, pane_pids_by_started_agent, persist_spawn_agent_state,
    preserve_existing_leader_topology, promote_launched_binding_from_team_entry,
    save_added_agent_state_for_key, save_launched_team_state, save_launched_team_state_for_key,
    seeded_pane_looks_like_worker, tmux_sockets_match_or_unknown,
};

mod leader_context;
pub(super) use leader_context::*;
pub(crate) use leader_context::{
    active_leader_pane_state_across_transports, validate_active_leader_pane_env,
    validate_active_leader_pane_env_with_workspace,
    validate_active_leader_pane_env_with_workspaces, LeaderPaneEnvState,
};

mod agent_state;
pub(super) use agent_state::*;
pub(crate) use agent_state::{effective_approval_policy, persist_effective_approval_policy};

mod mcp_config;
pub(super) use mcp_config::*;
pub(crate) use mcp_config::{
    point_native_mcp_config_at_file, resolve_mcp_config, write_worker_mcp_config,
    write_worker_mcp_config_for_provider,
};

mod worker_env;
pub(super) use worker_env::*;
pub(crate) use worker_env::{
    apply_copilot_instructions_overlay, apply_mcp_auto_approval_env, apply_profile_launch_env,
    fill_spawn_placeholders, fill_spawn_placeholders_full, inherited_env_with_team_overrides,
    persist_command_plan_state,
};

mod identity;
pub(super) use identity::*;
pub(crate) use identity::{
    extend_worker_env_unset_for_effort, provider_effort_event_if_dropped,
    provider_effort_event_if_dropped_json, provider_effort_event_payload,
    provider_effort_for_spawn, provider_effort_for_spawn_json, provider_effort_from_raw,
};

mod quick_start;
pub(super) use quick_start::*;
pub use quick_start::{
    quick_start, quick_start_in_workspace, quick_start_in_workspace_with_display,
    quick_start_in_workspace_with_display_and_backend, quick_start_with_transport,
    quick_start_with_transport_in_workspace, quick_start_with_transport_in_workspace_with_display,
};

mod quick_start_transport;
pub use quick_start_transport::annotate_runtime_transport;
pub(super) use quick_start_transport::*;
pub(crate) use quick_start_transport::{
    annotate_runtime_tmux_endpoint, attach_window_names_for_state_agents,
    configure_adaptive_pane_title, quick_start_tmux_backend, selected_tmux_socket_source,
};

pub mod readiness;
pub(crate) use readiness::launched_team_receiver_is_attached;
pub use readiness::ReadinessPhase;
pub(super) use readiness::*;

mod approval;
pub use approval::detect_dangerous_approval;
use approval::{
    binary_matches_provider, binary_name, dangerous_leader_flags,
    detect_dangerous_approval_in_argv, disabled_dangerous_approval, process_ancestry_argv,
    process_argv_tokens, process_parent_pid,
};

mod add_agent;
pub(super) use add_agent::*;
pub use add_agent::{
    add_agent, add_agent_force, add_agent_with_transport, add_agent_with_transport_force,
};

mod add_agent_state;
pub(crate) use add_agent_state::inject_agent_into_spec;
pub(super) use add_agent_state::*;

mod fork_agent;
pub(super) use fork_agent::*;
pub use fork_agent::{fork_agent, fork_agent_with_transport};

mod fork_state;
pub(super) use fork_state::*;

mod ownership;
pub(super) use ownership::*;
pub(crate) use ownership::{ensure_owner_allowed, ensure_owner_allowed_for_state, state_path};

pub mod spec_state;
pub(crate) use spec_state::{
    effective_runtime_config, effective_runtime_config_for_worker_spawn,
    override_spec_session_name, override_spec_workspace, spec_agent_id_set, write_spec_atomic,
};
use spec_state::{
    env_nonempty, has_positive_caller_leader_env, initial_runtime_state,
    override_spec_display_backend, override_spec_runtime_str,
    seed_launched_owner_from_caller_with_provider_lookup, seed_launched_owner_from_env,
    spec_agent_values, spec_agents, spec_default_assignee, spec_routes, spec_session_name,
    spec_tasks_json, team_workspace, write_launch_permission_audit, yaml_value_to_json,
};
pub use spec_state::{worker_session_name_pub, SpecStatePhase};
