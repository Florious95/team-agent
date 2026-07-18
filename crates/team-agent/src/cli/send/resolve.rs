use super::*;

pub(super) fn warn_send_alias(flag: &str) {
    let spec = crate::cli::spec::command_spec("send");
    let sunset = spec
        .and_then(|spec| spec.sunset)
        .unwrap_or("next compatibility release");
    let action = spec
        .and_then(|spec| spec.action)
        .unwrap_or("use positional logical TARGET addressing");
    eprintln!("warning: {flag} is deprecated; sunset: {sunset}; action: {action}");
}

pub(super) fn logical_to_from_args(
    args: &SendArgs,
    host_leader_to: Option<&str>,
) -> Result<String, CliError> {
    if args.to_name.is_some()
        && (args.target.is_some() || args.targets.is_some() || args.to_leader.is_some())
    {
        return Err(CliError::Usage(
            "--to-name and --pane/TARGET/--to are mutually exclusive".to_string(),
        ));
    }
    let supplied = [
        args.target.is_some(),
        args.targets.is_some(),
        args.to_name.is_some(),
        args.to_leader.is_some(),
    ]
    .into_iter()
    .filter(|present| *present)
    .count();
    if supplied > 1 {
        return Err(CliError::Usage(
            "TARGET, --targets, --to-name, and --to-leader are mutually exclusive".to_string(),
        ));
    }
    let logical_to = if args.to_leader.is_some() {
        host_leader_to.unwrap_or_default().to_string()
    } else if let Some(name) = args.to_name.as_deref() {
        name.to_string()
    } else if let Some(targets) = args.targets.as_deref() {
        targets.to_string()
    } else {
        args.target.clone().unwrap_or_default()
    };
    if args.target.is_none() && supplied > 0 && args.message.is_empty() {
        if args.to_name.is_some() {
            return Err(CliError::Usage(
                "--to-name requires a non-empty message".to_string(),
            ));
        }
        return Err(CliError::Usage(
            "send requires a non-empty message after logical TO".to_string(),
        ));
    }
    Ok(logical_to)
}

pub(super) fn resolve_host_leader_alias(
    name: &str,
) -> Result<(String, crate::leader::registry::LeaderRegistryEntry), Value> {
    let classified = crate::leader::registry::list_validated_no_gc();
    let candidates = classified
        .iter()
        .filter(|(entry, _, _)| {
            entry.delivery_name == name
                || entry.qualified_name == name
                || entry.stable_qualified_name == name
                || entry.aliases.iter().any(|alias| alias == name)
        })
        .map(|(entry, _, _)| entry.clone())
        .collect::<Vec<_>>();
    if candidates.is_empty() {
        return Err(json!({
            "ok": false,
            "status": "refused",
            "reason": "leader_name_not_found",
            "requested_name": name,
            "resolved_via": "host_leader_registry",
            "candidates": Vec::<Value>::new(),
            "workspace_hash": null,
            "stable_qualified_name": null,
            "channel": "leader_mailbox",
            "delivered": false,
            "message_status": "queued_until_leader_attach",
            "action": "run `team-agent leaders` to see registered leaders; inspect queued leader messages with `team-agent inbox`; retry with a qualified name",
            "registry_stale": false,
        }));
    }
    if candidates.len() > 1 {
        let candidates = candidates
            .iter()
            .map(|entry| {
                json!({
                    "name": entry.qualified_name,
                    "workspace": entry.workspace.display().to_string(),
                    "team_key": entry.team_key,
                    "workspace_hash": entry.workspace_hash,
                    "stable_qualified_name": entry.stable_qualified_name,
                })
            })
            .collect::<Vec<_>>();
        return Err(json!({
            "ok": false,
            "status": "refused",
            "reason": "name_ambiguous",
            "requested_name": name,
            "resolved_via": "host_leader_registry",
            "candidates": candidates,
            "channel": "leader_mailbox",
            "delivered": false,
            "action": "run `team-agent leaders` and retry with the qualified name",
        }));
    }
    let entry = candidates[0].clone();
    let (status, reason) = crate::leader::registry::classify(&entry);
    if status == "STALE" {
        let team_alive = crate::state::persist::load_runtime_state(&entry.workspace)
            .ok()
            .and_then(|state| {
                state
                    .get("teams")
                    .and_then(Value::as_object)
                    .and_then(|teams| teams.get(&entry.team_key))
                    .and_then(|team| team.get("status"))
                    .and_then(Value::as_str)
                    .map(|status| status == "alive" || status.is_empty())
            })
            .unwrap_or(false);
        if !team_alive {
            return Err(json!({
                "ok": false,
                "status": "refused",
                "reason": "registry_stale",
                "requested_name": name,
                "resolved_via": "host_leader_registry",
                "stale_reason": reason,
                "workspace_hash": entry.workspace_hash,
                "stable_qualified_name": entry.stable_qualified_name,
                "channel": "leader_mailbox",
                "delivered": false,
                "action": "target team is not alive; run `team-agent leaders` for current state",
            }));
        }
    }
    let logical_to = format!("{}::{}/leader", entry.workspace.display(), entry.team_key);
    Ok((logical_to, entry))
}

pub(super) fn decorate_host_leader_alias(
    value: &mut Value,
    entry: &crate::leader::registry::LeaderRegistryEntry,
) {
    let Some(object) = value.as_object_mut() else {
        return;
    };
    object.insert("resolved_via".to_string(), json!("host_leader_registry"));
    object.insert("to_leader".to_string(), json!(entry.qualified_name));
    object.insert("requested_name".to_string(), json!(entry.delivery_name));
    object.insert("workspace_hash".to_string(), json!(entry.workspace_hash));
    object.insert(
        "stable_qualified_name".to_string(),
        json!(entry.stable_qualified_name),
    );
}

pub(super) fn send_to_logical_to(
    args: &SendArgs,
    logical_to: &str,
    content: &str,
) -> Result<Value, CliError> {
    let names = logical_to
        .split(',')
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .collect::<Vec<_>>();
    if names.is_empty() || names.len() != logical_to.split(',').count() {
        return Err(CliError::Usage(
            "logical TO comma-list contains an empty recipient".to_string(),
        ));
    }

    let mut resolved = Vec::with_capacity(names.len());
    for name in names {
        match crate::cli::named_address::resolve_name_for_cli(
            &args.workspace,
            name,
            args.team.as_deref(),
        ) {
            Ok((recipient, _transport)) => resolved.push(recipient),
            Err(mut error) => {
                adapt_positional_bare_error(args, name, &mut error);
                if matches!(
                    error.kind,
                    crate::cli::named_address::NamedAddressErrorKind::StateNotFound
                ) {
                    return Ok(resolution_refusal_json(&error, logical_to, content, args));
                }
                if resolved.is_empty() && !logical_to.contains(',') {
                    if let Some(value) = maybe_enqueue_offline_leader_mailbox(
                        &args.workspace,
                        name,
                        content,
                        args.sender.as_str(),
                        args.task.as_deref(),
                        &error,
                    )? {
                        return Ok(value);
                    }
                }
                if args.json {
                    return Ok(error.to_json());
                }
                return Err(CliError::Usage(error.n38_message()));
            }
        }
    }

    if resolved.len() == 1 {
        return send_to_resolved_name(args, &resolved[0], content);
    }

    let first = &resolved[0];
    let one_scope = resolved.iter().all(|recipient| {
        recipient.target_workspace == first.target_workspace && recipient.team_key == first.team_key
    });
    if one_scope {
        let recipients = resolved
            .iter()
            .map(logical_recipient_id)
            .collect::<Result<Vec<_>, _>>()?;
        let target = MessageTarget::Fanout(recipients);
        return persist_resolved_target(args, first, &target, content);
    }

    let mut results = Vec::with_capacity(resolved.len());
    for recipient in &resolved {
        results.push(send_to_resolved_name(args, recipient, content)?);
    }
    let ok = results
        .iter()
        .all(|value| value.get("ok").and_then(Value::as_bool) == Some(true));
    let message_id = results
        .iter()
        .rev()
        .find_map(|value| value.get("message_id").and_then(Value::as_str))
        .map(str::to_string);
    Ok(json!({
        "ok": ok,
        "status": if ok { "fanout_delivered" } else { "fanout_partial" },
        "delivery_status": if ok { "pending" } else { "fanout_partial" },
        "delivered": false,
        "target": logical_to.split(',').map(str::trim).collect::<Vec<_>>(),
        "content_length_bytes": content.len(),
        "sender": args.sender,
        "message_id": message_id,
        "results": results,
    }))
}

pub(super) fn resolution_refusal_json(
    error: &crate::cli::named_address::NamedAddressError,
    logical_to: &str,
    content: &str,
    args: &SendArgs,
) -> Value {
    let mut value = error.to_json();
    if let Some(object) = value.as_object_mut() {
        object.insert("delivery_status".to_string(), json!("refused"));
        object.insert("delivered".to_string(), json!(false));
        object.insert("target".to_string(), json!(logical_to));
        object.insert("agent_id".to_string(), json!(logical_to));
        object.insert("content_length_bytes".to_string(), json!(content.len()));
        object.insert("sender".to_string(), json!(args.sender));
        object.insert("message_id".to_string(), Value::Null);
        object.insert("message_status".to_string(), json!("refused"));
        object.insert("verification".to_string(), Value::Null);
        object.insert("stage".to_string(), Value::Null);
        object.insert("channel".to_string(), Value::Null);
    }
    value
}

pub(super) fn adapt_positional_bare_error(
    args: &SendArgs,
    name: &str,
    error: &mut crate::cli::named_address::NamedAddressError,
) {
    if args.target.as_deref() != Some(name)
        || args.team.is_none()
        || name.contains('/')
        || name.contains(':')
        || name.contains(',')
    {
        return;
    }
    error.requested_name = Some(name.to_string());
    for candidate in &mut error.candidates {
        let agent_id = candidate
            .get("agent_id")
            .and_then(Value::as_str)
            .map(str::to_string);
        if let (Some(object), Some(agent_id)) = (candidate.as_object_mut(), agent_id) {
            object.insert("name".to_string(), json!(agent_id));
        }
    }
    error.suggested_name = error
        .suggested_name
        .as_deref()
        .and_then(|suggested| suggested.rsplit('/').next())
        .map(str::to_string);
    if let Some(suggested) = error.suggested_name.as_deref() {
        error.action = format!("Did you mean `{suggested}`? Retry with `{suggested}` as TO.");
    }
}

pub(super) fn logical_recipient_id(
    resolved: &crate::cli::named_address::ResolvedNamedAddress,
) -> Result<String, CliError> {
    match resolved.target_kind {
        crate::cli::named_address::NamedTargetKind::Worker => resolved
            .agent_id
            .clone()
            .ok_or_else(|| CliError::Runtime("resolved worker is missing agent id".to_string())),
        crate::cli::named_address::NamedTargetKind::Leader => Ok("leader".to_string()),
        crate::cli::named_address::NamedTargetKind::SessionWindow => Err(CliError::Usage(
            "named session/window delivery is sunset; use a logical agent or leader name"
                .to_string(),
        )),
    }
}

pub(super) fn send_to_resolved_name(
    args: &SendArgs,
    resolved: &crate::cli::named_address::ResolvedNamedAddress,
    content: &str,
) -> Result<Value, CliError> {
    let recipient = logical_recipient_id(resolved)?;
    if let Some(warning) = &resolved.warning {
        eprintln!("warning: {warning}");
    }
    let target = MessageTarget::Single(recipient);
    let mut value = persist_resolved_target(args, resolved, &target, content)?;
    if args.to_name.is_some() || args.to_leader.is_some() {
        if let Some(obj) = value.as_object_mut() {
            obj.insert("to_name".to_string(), json!(resolved.raw_name));
            obj.insert(
                "target_workspace".to_string(),
                json!(resolved.target_workspace.display().to_string()),
            );
            obj.insert("team_key".to_string(), json!(resolved.team_key));
        }
    }
    Ok(value)
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
