use super::super::*;

pub(super) struct CompleteForkInput<'a> {
    pub workspace: &'a Path,
    pub team_key: &'a str,
    pub agent_id: &'a AgentId,
    pub spawn: &'a crate::transport::SpawnResult,
    pub window: &'a WindowName,
    pub transport: &'a dyn Transport,
    pub session_name: &'a SessionName,
    pub mcp_config_path: &'a Path,
    pub profile_launch: &'a crate::provider::ProviderProfileLaunch,
    pub materialized_role: &'a mut MaterializedRole,
    pub claude_fork:
        &'a mut Option<crate::provider::adapters::claude_fork::ClaudeForkMaterialization>,
    pub copilot_fork:
        &'a mut Option<crate::provider::adapters::copilot_fork::CopilotForkMaterialization>,
}

pub(super) fn complete_fork(input: CompleteForkInput<'_>) -> Result<bool, LifecycleError> {
    if let Err(error) = verify_fork_registration(
        input.workspace,
        input.team_key,
        input.agent_id,
        input.spawn,
        input.window,
    ) {
        rollback_fork_after_spawn(
            input.workspace,
            input.transport,
            input.session_name,
            input.window,
            input.mcp_config_path,
            input.agent_id,
            input.profile_launch,
            input.team_key,
        );
        return Err(error);
    }
    let coordinator_started = start_fork_coordinator(ForkCoordinatorInput {
        workspace: input.workspace,
        team_key: input.team_key,
        agent_id: input.agent_id,
        transport: input.transport,
        session_name: input.session_name,
        window: input.window,
        mcp_config_path: input.mcp_config_path,
        profile_launch: input.profile_launch,
    })?;
    input.materialized_role.keep();
    if let Some(materialized) = input.claude_fork.as_mut() {
        materialized.keep();
    }
    if let Some(materialized) = input.copilot_fork.as_mut() {
        materialized.keep();
    }
    Ok(coordinator_started)
}
