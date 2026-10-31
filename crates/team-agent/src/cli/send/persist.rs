//!
use super::presentation::delivery_outcome_json;
use crate::cli::{CliError, SendArgs};
use crate::messaging::{
    self, DeliveryOutcome, DeliveryStatus, MessageTarget, SendOptions, SendOrigin,
};
use crate::model::ids::{TaskId, TeamKey};
use serde_json::{json, Value};
use std::path::Path;

/// Translate SendArgs into the persisted-send options.
pub fn send_options_from_args(args: &SendArgs) -> SendOptions {
    SendOptions {
        origin: SendOrigin::Cli,
        task_id: args.task.as_ref().map(|s| TaskId::new(s.clone())),
        route_task_id: true,
        sender: args.sender.clone(),
        requires_ack: !args.no_ack,
        confirm_human: args.confirm_human,
        wait_visible: !args.no_wait,
        timeout: args.timeout,
        watch_result: args.watch_result,
        team: args.team.as_ref().map(|s| TeamKey::new(s.clone())),
        message_id: args.message_id.clone(),
        presentation: args.presentation.clone(),
        ..SendOptions::default()
    }
}

pub(super) fn persist_resolved_target(
    args: &SendArgs,
    resolved: &crate::cli::named_address::ResolvedNamedAddress,
    target: &MessageTarget,
    content: &str,
) -> Result<Value, CliError> {
    let to = match target {
        MessageTarget::Single(target) => target.clone(),
        MessageTarget::Fanout(targets) => targets.join(","),
        MessageTarget::Broadcast => "*".into(),
    };
    crate::station::send::execute(crate::station::send::Request {
        workspace: &resolved.target_workspace, team: resolved.team_key.as_deref(),
        to: &to, content, task_id: args.task.as_deref(), message_id: args.message_id.as_deref(),
        presentation: &args.presentation,
        mailbox: args.presentation.sink != crate::messaging::presentation::PresentationSink::Leader,
        envelope: None,
    }, crate::station::send::Authority::Operator { sender: args.sender.display_name() })
        .map_err(CliError::Usage)
}

pub(super) fn routing_ambiguous_value(
    workspace: &Path,
    args: &SendArgs,
    target: &MessageTarget,
    content: &str,
    opts: &SendOptions,
) -> Option<Value> {
    if args.targets.is_some() || !content.is_empty() {
        return None;
    }
    let MessageTarget::Single(name) = target else {
        return None;
    };
    if name.is_empty() {
        return None;
    }
    let state = crate::state::persist::load_runtime_state(workspace).ok()?;
    let in_team = state
        .get("agents")
        .and_then(|v| v.as_object())
        .is_some_and(|a| a.contains_key(name));
    if in_team {
        return None;
    }
    // aeab1c7 follow-up: `content` is no longer emitted anywhere from `send`
    // responses (including this refusal). Replace with `content_length_bytes`
    // to keep the size-sanity field consistent with the normal-send shape.
    Some(json!({
        "ok": false,
        "status": "refused",
        "target": null,
        "agent_id": null,
        "content_length_bytes": name.len(),
        "sender": opts.sender,
        "message_id": null,
        "message_status": "refused",
        "verification": null,
        "stage": null,
        "reason": "routing_ambiguous",
        "channel": null,
    }))
}

pub(super) fn selected_state_with_active_key(
    selected: &crate::state::selector::SelectedTeam,
) -> Value {
    let mut state = selected.state.clone();
    if let Some(obj) = state.as_object_mut() {
        obj.insert(
            "active_team_key".to_string(),
            Value::String(selected.team_key.clone()),
        );
    }
    state
}

pub(super) fn initial_delivery_allows_watch(status: DeliveryStatus) -> bool {
    matches!(
        status,
        DeliveryStatus::Delivered | DeliveryStatus::AlreadyDelivered
    )
}

pub(super) fn observe_initial_delivery_for_watch(
    selected: &crate::state::selector::SelectedTeam,
    target: &MessageTarget,
    outcome: &DeliveryOutcome,
    opts: &SendOptions,
) -> Result<DeliveryOutcome, CliError> {
    if !matches!(target, MessageTarget::Single(agent) if !agent.is_empty()) {
        return Ok(outcome.clone());
    }
    if !matches!(outcome.status, DeliveryStatus::Queued) {
        return Ok(outcome.clone());
    }
    let Some(message_id) = outcome.message_id.as_deref() else {
        return Ok(outcome.clone());
    };
    let transport = crate::lifecycle::restart::lifecycle_worker_tmux_backend_for_selected_state(
        &selected.run_workspace,
        opts.team.as_ref().map(TeamKey::as_str),
    )
    .map_err(|e| CliError::Runtime(e.to_string()))?;
    let event_log = crate::event_log::EventLog::new(&selected.run_workspace);
    let state = selected_state_with_active_key(selected);
    crate::messaging::deliver_persisted_message(
        &selected.run_workspace,
        &transport,
        message_id,
        &event_log,
        &state,
    )
    .map_err(CliError::from)
}
