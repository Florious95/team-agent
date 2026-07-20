use super::*;

pub(super) fn maybe_enqueue_offline_leader_mailbox(
    sender_workspace: &Path,
    to_name: &str,
    content: &str,
    sender: &str,
    task_id: Option<&str>,
    error: &crate::cli::named_address::NamedAddressError,
) -> Result<Option<Value>, CliError> {
    if error.kind != crate::cli::named_address::NamedAddressErrorKind::LeaderNotAttached {
        return Ok(None);
    }
    let parsed = match crate::cli::named_address::parse_leader_target_workspace_and_team(
        sender_workspace,
        to_name,
    ) {
        Ok(Some(v)) => v,
        Ok(None) => return Ok(None),
        Err(_) => return Ok(None),
    };
    let (target_workspace, team_key) = parsed;
    // Owner-scope refusal: sender workspace == target workspace. Keep
    // the actionable attach hint (owner sees status/diagnose copy that
    // points at `attach-leader`).
    let sender_canonical =
        std::fs::canonicalize(sender_workspace).unwrap_or_else(|_| sender_workspace.to_path_buf());
    let target_canonical =
        std::fs::canonicalize(&target_workspace).unwrap_or_else(|_| target_workspace.clone());
    if sender_canonical == target_canonical {
        return Ok(None);
    }
    // Verify the target team is actually alive on this host — mailbox
    // is only for `team live + leader unattached`. Fail-closed otherwise
    // so we never leave a message in a permanently-dead workspace's DB.
    let state = match crate::state::persist::load_runtime_state(&target_workspace) {
        Ok(s) => s,
        Err(_) => return Ok(None),
    };
    let team_alive = target_team_is_alive_for_mailbox(&state, &team_key);
    if !team_alive {
        return Ok(None);
    }
    let event_log = crate::event_log::EventLog::new(&target_workspace);
    let task = task_id.map(|s| crate::model::ids::TaskId::new(s.to_string()));
    let outcome = messaging::enqueue_leader_mailbox_until_attach(
        &target_workspace,
        &team_key,
        content,
        task.as_ref(),
        sender,
        &event_log,
    )
    .map_err(|e| CliError::Runtime(e.to_string()))?;
    let message_id = outcome.message_id.clone().unwrap_or_else(|| "".to_string());
    Ok(Some(json!({
        "ok": true,
        "status": "queued_until_leader_attach",
        "message_status": "queued_until_leader_attach",
        "channel": "leader_mailbox",
        "delivered": false,
        "to_name": to_name,
        "target_workspace": target_workspace.display().to_string(),
        "team_key": team_key,
        "recipient": "leader",
        "leader_attached": false,
        "message_id": message_id,
    })))
}

/// Positive-source liveness heuristic per offline-mailbox-toname-design.md §4:
/// - target workspace has state and the team key is present + not archived/down;
/// - AND at least one live tmux fact — a persisted `session_name` OR any
///   agent with a recorded pane on the recorded socket.
///
/// We deliberately do NOT poll coordinator health here — enqueuing is
/// safe even when the coordinator is transiently down; attach-leader
/// itself replays via `requeue_blocked_leader_messages` regardless.
pub(super) fn target_team_is_alive_for_mailbox(state: &Value, team_key: &str) -> bool {
    let team = state
        .get("teams")
        .and_then(|v| v.as_object())
        .and_then(|teams| teams.get(team_key));
    let Some(team) = team else {
        return false;
    };
    let status = team
        .get("status")
        .and_then(|v| v.as_str())
        .unwrap_or("alive");
    if matches!(status, "archived" | "down" | "stopped") {
        return false;
    }
    // A recorded session_name is enough — target's coordinator/attach
    // path will re-verify tmux presence when the replay fires.
    team.get("session_name")
        .and_then(|v| v.as_str())
        .is_some_and(|s| !s.is_empty())
}
