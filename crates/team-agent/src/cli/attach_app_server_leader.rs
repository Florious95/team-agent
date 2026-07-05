use super::*;

/// `team-agent attach-app-server-leader` writes a typed codex_app_server
/// leader_receiver through the leader lease primitive.
pub fn cmd_attach_app_server_leader(
    args: &AttachAppServerLeaderArgs,
) -> Result<CmdResult, CliError> {
    let value = crate::leader::attach_app_server_leader(
        &args.workspace,
        args.team.as_deref(),
        &args.socket,
        &args.thread_id,
    )
    .map_err(|error| CliError::Runtime(error.to_string()))?;
    Ok(CmdResult::from_json(value, args.json))
}
