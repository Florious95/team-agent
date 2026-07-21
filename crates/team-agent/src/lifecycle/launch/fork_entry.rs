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
    // Fork-agent routes to the selected live team's persisted endpoint, not
    // the workspace-hash fallback socket.
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
