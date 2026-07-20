use super::coordinator::{
    append_loud_ensure_fields, dirty_topology_refusal_value, loud_ensure_coordinator,
    target_has_known_worker,
};
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
        ..SendOptions::default()
    }
}

pub(super) fn persist_resolved_target(
    args: &SendArgs,
    resolved: &crate::cli::named_address::ResolvedNamedAddress,
    target: &MessageTarget,
    content: &str,
) -> Result<Value, CliError> {
    let selected = crate::state::selector::resolve_active_team(
        &resolved.target_workspace,
        resolved.team_key.as_deref(),
        crate::state::selector::SelectorMode::RuntimeOnly,
    )?;
    if let Some(value) = dirty_topology_refusal_value(&selected, resolved.team_key.as_deref()) {
        return Ok(value);
    }
    let mut opts = send_options_from_args(args);
    opts.route_task_id = args.to_name.is_none() && args.to_leader.is_none();
    opts.team = Some(TeamKey::new(selected.team_key.clone()));
    let coordinator_ensure =
        if target_has_known_worker(&selected.state, target, opts.sender.as_str()) {
            loud_ensure_coordinator(&selected)?
        } else {
            None
        };
    let outcome = messaging::send_message(&selected.run_workspace, target, content, &opts)?;
    let mut value = delivery_outcome_json(&outcome, target, content, &opts);
    append_loud_ensure_fields(&mut value, coordinator_ensure.as_ref());
    Ok(value)
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
