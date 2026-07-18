//! cli · send — `cmd_send` + target 解析(`_send_target`)+ `SendArgs`→`SendOptions` 翻译
//! (`send_options_from_args`,旗标取反语义)。

use super::*;
use crate::messaging::{DeliveryOutcome, DeliveryRefusal, DeliveryStage, DeliveryStatus};

/// `cmd_send`(`commands.py:164`)。解析 target(`--to` fanout / 单 target / `*`)→ [`MessageTarget`],
/// 拼 [`SendOptions`](no_ack→requires_ack 取反、no_wait→wait_visible 取反等)→ `messaging::send_message`。
pub fn cmd_send(args: &SendArgs) -> Result<CmdResult, CliError> {
    // F1 (0.3.26, cross-team send): --pane <pane_id> direct targeting.
    // Mutually exclusive with target / --to (agent-name routing).
    if let Some(ref pane_id) = args.pane {
        warn_send_alias("--pane");
        if args.target.is_some()
            || args.targets.is_some()
            || args.to_name.is_some()
            || args.to_leader.is_some()
        {
            return Err(CliError::Usage(
                "--pane and logical TO aliases are mutually exclusive".to_string(),
            ));
        }
        let content = args.message.join(" ");
        if content.is_empty() {
            return Err(CliError::Usage(
                "--pane requires a non-empty message".to_string(),
            ));
        }
        return Err(CliError::Usage(format!(
            "--pane {pane_id} is deprecated and sunset; use a logical TARGET so the message is persisted before delivery"
        )));
    }
    if args.targets.is_some() {
        warn_send_alias("--targets");
    }
    if args.to_name.is_some() {
        warn_send_alias("--to-name");
    }
    if args.to_leader.is_some() {
        warn_send_alias("--to-leader");
    }
    let host_leader_alias = if let Some(name) = args.to_leader.as_deref() {
        match resolve_host_leader_alias(name) {
            Ok(resolved) => Some(resolved),
            Err(value) => return Ok(cmd_send_result(value, args.json)),
        }
    } else {
        None
    };
    let logical_to = logical_to_from_args(
        args,
        host_leader_alias
            .as_ref()
            .map(|(logical_to, _)| logical_to.as_str()),
    )?;
    let content = args.message.join(" ");
    if !logical_to.is_empty() && logical_to != "*" && !content.is_empty() {
        let mut value = send_to_logical_to(args, &logical_to, &content)?;
        if let Some((_, entry)) = host_leader_alias.as_ref() {
            decorate_host_leader_alias(&mut value, entry);
        }
        add_send_reminder_if_ok(&mut value);
        return Ok(cmd_send_result(value, args.json));
    }
    let selected = crate::state::selector::resolve_active_team(
        &args.workspace,
        args.team.as_deref(),
        crate::state::selector::SelectorMode::RuntimeOnly,
    )?;
    let target = send_target(None, Some(logical_to.as_str()));
    let mut opts = send_options_from_args(args);
    // `args.team` is a selector and may be a legacy session/team-dir alias.
    // All downstream membership and DB scope must use the canonical key that
    // resolve_active_team returned, never the original selector spelling.
    opts.team = Some(TeamKey::new(selected.team_key.clone()));
    // CR-061/N27 routing-ambiguous: a single positional with no `--to`/`--targets` and an
    // empty message body is a prompt-only invocation (`team-agent send "fix the build"`).
    // The lone positional is CONTENT, not a target — reject with `routing_ambiguous`
    // (NOT `target_not_in_team`, which would lie that the user did pick a target).
    if let Some(amb) =
        routing_ambiguous_value(&selected.run_workspace, args, &target, &content, &opts)
    {
        return Ok(cmd_send_result(amb, args.json));
    }
    if let Some(value) = dirty_topology_refusal_value(&selected, args.team.as_deref()) {
        return Ok(cmd_send_result(value, args.json));
    }
    let coordinator_ensure =
        if target_has_known_worker(&selected.state, &target, opts.sender.as_str()) {
            loud_ensure_coordinator(&selected)?
        } else {
            None
        };
    if let Some(value) =
        coordinator_ensure_unavailable_value(coordinator_ensure.as_ref(), &target, &content, &opts)
    {
        return Ok(cmd_send_result(value, args.json));
    }
    let mut outcome = messaging::send_message(&selected.run_workspace, &target, &content, &opts)?;
    if opts.watch_result {
        outcome = observe_initial_delivery_for_watch(&selected, &target, &outcome, &opts)?;
    }
    let mut value = delivery_outcome_json(&outcome, &target, &content, &opts);
    // 0.5.45 naming-addressing (design §3.5, RED-2/RED-3 positional):
    // when the refusal reason is `target_not_in_team` AND the target
    // was a Single non-special short id, attach scope-safe
    // suggestions ranked from `selected.state.agents` — never from
    // raw workspace `teams` (design §3.5 & risk table). Zero DB
    // write / zero inject: the request stays refused, only the JSON
    // envelope gains `requested_name`/`suggested_name`/`candidates`
    // and the human `Action` gains a "Did you mean" line.
    attach_positional_typo_suggestions(&mut value, &target, &selected.state);
    append_loud_ensure_fields(&mut value, coordinator_ensure.as_ref());
    if opts.watch_result && initial_delivery_allows_watch(outcome.status) {
        if let Some(obj) = value.as_object_mut() {
            obj.insert("watch".to_string(), watch_notice_json(&target, &opts));
        }
    }
    add_send_reminder_if_ok(&mut value);
    Ok(cmd_send_result(value, args.json))
}

fn dirty_topology_refusal_value(
    selected: &crate::state::selector::SelectedTeam,
    requested_team: Option<&str>,
) -> Option<Value> {
    let issue_ids = crate::topology::restart_dirty_topology_issue_ids(&selected.state);
    if issue_ids.is_empty() {
        return None;
    }
    let session_name = selected
        .state
        .get("session_name")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let reason = issue_ids
        .first()
        .cloned()
        .unwrap_or_else(|| "dirty_topology".to_string());
    let repair_team = requested_team
        .filter(|team| !team.is_empty())
        .unwrap_or(selected.team_key.as_str());
    Some(json!({
        "ok": false,
        "status": "refused_dirty_topology",
        "reason": reason,
        "session_name": session_name,
        "error": "send refused: tmux endpoint/socket topology is inconsistent; run diagnose from the intended leader socket before sending",
        "issues": issue_ids
            .iter()
            .map(|id| json!({"id": id}))
            .collect::<Vec<_>>(),
        "next_actions": [
            "team-agent diagnose --json",
            format!("team-agent claim-leader --team {repair_team} --confirm --json"),
            format!("team-agent takeover --team {repair_team} --confirm --json")
        ],
    }))
}

fn target_has_known_worker(state: &Value, target: &MessageTarget, sender: &str) -> bool {
    let Some(agents) = state.get("agents").and_then(Value::as_object) else {
        return false;
    };
    match target {
        MessageTarget::Single(target) => agents.contains_key(target),
        MessageTarget::Broadcast => agents.keys().any(|agent| agent != sender),
        MessageTarget::Fanout(recipients) => recipients
            .iter()
            .any(|recipient| agents.contains_key(recipient)),
    }
}

#[derive(Debug, Clone)]
struct LoudEnsureResult {
    previous_status: String,
    start: crate::coordinator::StartReport,
}

fn loud_ensure_coordinator(
    selected: &crate::state::selector::SelectedTeam,
) -> Result<Option<LoudEnsureResult>, CliError> {
    if in_process_unit_test() {
        return Ok(None);
    }
    let workspace = crate::coordinator::WorkspacePath::new(selected.run_workspace.clone());
    let previous = crate::coordinator::coordinator_health(&workspace);
    if previous.ok {
        return Ok(None);
    }
    if previous.service_available
        && matches!(
            previous.binary_identity_relation,
            crate::coordinator::CoordinatorBinaryIdentityRelation::DaemonNewerThanCaller
        )
    {
        return Ok(None);
    }
    let previous_status = coordinator_health_status_wire(previous.status).to_string();
    let start = crate::coordinator::start_coordinator_with_team(
        &workspace,
        Some(selected.team_key.as_str()),
    )
    .map_err(|error| CliError::Runtime(error.to_string()))?;
    if !start.ok {
        return Ok(Some(LoudEnsureResult {
            previous_status,
            start,
        }));
    }
    if matches!(
        start.status,
        crate::coordinator::StartOutcome::Started
            | crate::coordinator::StartOutcome::StartedAfterRotation
    ) {
        crate::event_log::EventLog::new(&selected.run_workspace)
            .write(
                "coordinator.ensure_restarted",
                json!({
                    "coordinator_previous_status": previous_status,
                    "status": start.status,
                    "pid": start.pid.map(|pid| pid.get()),
                    "previous_pid": start.previous_pid.map(|pid| pid.get()),
                    "binary_path": start.binary_path,
                    "binary_version": start.binary_version,
                    "rotation_reason": start.rotation_reason,
                }),
            )
            .map_err(|error| CliError::Runtime(error.to_string()))?;
        return Ok(Some(LoudEnsureResult {
            previous_status,
            start,
        }));
    }
    Ok(None)
}

#[cfg(test)]
fn in_process_unit_test() -> bool {
    true
}

#[cfg(not(test))]
fn in_process_unit_test() -> bool {
    false
}

fn coordinator_ensure_unavailable_value(
    ensure: Option<&LoudEnsureResult>,
    target: &MessageTarget,
    content: &str,
    opts: &SendOptions,
) -> Option<Value> {
    let ensure = ensure?;
    if ensure.start.ok {
        return None;
    }
    let warning = format!(
        "coordinator is not running; message was not queued for {}. Run `team-agent diagnose` or restart the team before sending again.",
        first_target(target)
    );
    let mut value = json!({
        "ok": false,
        "status": "degraded",
        "delivery_status": "degraded",
        "delivered": false,
        "target": target_json(target),
        "agent_id": first_target(target),
        "content_length_bytes": content.len(),
        "sender": opts.sender,
        "message_id": Value::Null,
        "message_status": "degraded",
        "verification": warning,
        "stage": Value::Null,
        "reason": "coordinator_unavailable",
        "channel": "coordinator_unavailable",
    });
    append_loud_ensure_fields(&mut value, Some(ensure));
    Some(value)
}

fn append_loud_ensure_fields(value: &mut Value, ensure: Option<&LoudEnsureResult>) {
    let Some(ensure) = ensure else {
        return;
    };
    if !ensure.start.ok {
        return;
    }
    if let Some(obj) = value.as_object_mut() {
        obj.insert("coordinator_auto_restarted".to_string(), json!(true));
        obj.insert(
            "coordinator_previous_status".to_string(),
            json!(ensure.previous_status),
        );
        obj.insert(
            "coordinator".to_string(),
            coordinator_start_json(&ensure.start),
        );
    }
}

fn coordinator_start_json(report: &crate::coordinator::StartReport) -> Value {
    let summary = crate::lifecycle::CoordinatorStartSummary::from_start_report(report);
    crate::lifecycle::coordinator_start_summary_value(&summary)
}

fn coordinator_health_status_wire(
    status: crate::coordinator::CoordinatorHealthStatus,
) -> &'static str {
    match status {
        crate::coordinator::CoordinatorHealthStatus::Missing => "missing",
        crate::coordinator::CoordinatorHealthStatus::InvalidPid => "invalid_pid",
        crate::coordinator::CoordinatorHealthStatus::Running => "running",
        crate::coordinator::CoordinatorHealthStatus::Stale => "stale",
    }
}

fn warn_send_alias(flag: &str) {
    let spec = crate::cli::spec::command_spec("send");
    let sunset = spec.and_then(|spec| spec.sunset).unwrap_or("next compatibility release");
    let action = spec
        .and_then(|spec| spec.action)
        .unwrap_or("use positional logical TARGET addressing");
    eprintln!("warning: {flag} is deprecated; sunset: {sunset}; action: {action}");
}

fn logical_to_from_args(args: &SendArgs, host_leader_to: Option<&str>) -> Result<String, CliError> {
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
        return Err(CliError::Usage(
            "send requires a non-empty message after logical TO".to_string(),
        ));
    }
    Ok(logical_to)
}

fn resolve_host_leader_alias(
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

fn decorate_host_leader_alias(
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

fn send_to_logical_to(args: &SendArgs, logical_to: &str, content: &str) -> Result<Value, CliError> {
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
        let mut opts = send_options_from_args(args);
        opts.route_task_id = false;
        opts.team = first.team_key.clone().map(TeamKey::new);
        let outcome = messaging::send_message(&first.target_workspace, &target, content, &opts)?;
        return Ok(delivery_outcome_json(&outcome, &target, content, &opts));
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

fn adapt_positional_bare_error(
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

fn logical_recipient_id(
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

fn send_to_resolved_name(
    args: &SendArgs,
    resolved: &crate::cli::named_address::ResolvedNamedAddress,
    content: &str,
) -> Result<Value, CliError> {
    let recipient = logical_recipient_id(resolved)?;
    if let Some(warning) = &resolved.warning {
        eprintln!("warning: {warning}");
    }
    let target = MessageTarget::Single(recipient);
    let mut opts = send_options_from_args(args);
    opts.route_task_id = false;
    opts.team = resolved.team_key.clone().map(TeamKey::new);
    let outcome = messaging::send_message(&resolved.target_workspace, &target, content, &opts)?;
    let mut value = delivery_outcome_json(&outcome, &target, content, &opts);
    if let Some(obj) = value.as_object_mut() {
        obj.insert("to_name".to_string(), json!(resolved.raw_name));
        obj.insert(
            "target_workspace".to_string(),
            json!(resolved.target_workspace.display().to_string()),
        );
        obj.insert("team_key".to_string(), json!(resolved.team_key));
    }
    Ok(value)
}

fn routing_ambiguous_value(
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

fn selected_state_with_active_key(selected: &crate::state::selector::SelectedTeam) -> Value {
    let mut state = selected.state.clone();
    if let Some(obj) = state.as_object_mut() {
        obj.insert(
            "active_team_key".to_string(),
            Value::String(selected.team_key.clone()),
        );
    }
    state
}

fn initial_delivery_allows_watch(status: DeliveryStatus) -> bool {
    matches!(
        status,
        DeliveryStatus::Delivered | DeliveryStatus::AlreadyDelivered
    )
}

fn observe_initial_delivery_for_watch(
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
    let store = crate::message_store::MessageStore::open(&selected.run_workspace)
        .map_err(|e| CliError::Runtime(e.to_string()))?;
    let transport = crate::lifecycle::restart::lifecycle_worker_tmux_backend_for_selected_state(
        &selected.run_workspace,
        opts.team.as_ref().map(TeamKey::as_str),
    )
    .map_err(|e| CliError::Runtime(e.to_string()))?;
    let event_log = crate::event_log::EventLog::new(&selected.run_workspace);
    let state = selected_state_with_active_key(selected);
    crate::messaging::delivery::deliver_pending_message(
        &selected.run_workspace,
        &store,
        &transport,
        message_id,
        &event_log,
        &state,
    )
    .map_err(CliError::from)
}

/// `_send_target`(`commands.py:181-184`):`--to` comma-split fanout / `target` 单值 / None。
pub fn send_target(targets: Option<&str>, target: Option<&str>) -> MessageTarget {
    if let Some(targets) = targets.filter(|s| !s.is_empty()) {
        let recipients: Vec<String> = targets
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(ToString::to_string)
            .collect();
        return MessageTarget::Fanout(recipients);
    }
    match target {
        Some("*") => MessageTarget::Broadcast,
        Some(target) => MessageTarget::Single(target.to_string()),
        None => MessageTarget::Single(String::new()),
    }
}

/// `cmd_send` 的 [`SendArgs`]→[`SendOptions`] 翻译(`commands.py:170-177`)。CLI **独占**的
/// 旗标取反语义(经典 off-by-inversion bug 面):`no_ack→!requires_ack`、`no_wait→!wait_visible`、
/// `watch_result` 直传、`task_id`/`sender`/`confirm_human`/`timeout`/`team` 透传。
/// (其余 `lock_timeout`/`block_until_delivered` 用 [`SendOptions::default`]。)
pub fn send_options_from_args(args: &SendArgs) -> SendOptions {
    SendOptions {
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

fn watch_notice_json(target: &MessageTarget, opts: &SendOptions) -> Value {
    let agent_id = match target {
        MessageTarget::Single(agent) => agent.clone(),
        MessageTarget::Broadcast => "*".to_string(),
        MessageTarget::Fanout(recipients) => recipients
            .first()
            .cloned()
            .unwrap_or_else(|| "-".to_string()),
    };
    json!({
        "status": "registered",
        "watcher_id": format!("watch-{agent_id}"),
        "task_id": opts.task_id.as_ref().map(|t| t.as_str().to_string()),
        "agent_id": agent_id,
        "notice": "Team Agent will collect the result and notify the leader when this task reports completion."
    })
}

/// 0.5.45 naming-addressing (design §3.5, RED-2/RED-3 positional):
/// after `messaging::send_message` refuses with `target_not_in_team`
/// for a Single non-special short id, attach scope-safe advisory
/// suggestions to the outbound JSON envelope. Candidate source =
/// selected team's projected `agents` map (never the raw workspace
/// `teams`) so sibling teams cannot leak. Zero DB write, zero inject
/// — the refusal exit code is unchanged.
fn attach_positional_typo_suggestions(
    value: &mut Value,
    target: &MessageTarget,
    selected_state: &Value,
) {
    use crate::model::name_similarity::{rank, Candidate};
    let requested = match target {
        MessageTarget::Single(id) if id != "*" && id != "leader" => id.clone(),
        _ => return,
    };
    let Some(obj) = value.as_object_mut() else {
        return;
    };
    if obj.get("reason").and_then(Value::as_str) != Some("target_not_in_team") {
        return;
    }
    let team_key = selected_state
        .get("active_team_key")
        .or_else(|| selected_state.get("team_key"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let candidates: Vec<Candidate<String>> = selected_state
        .get("agents")
        .and_then(Value::as_object)
        .map(|agents| {
            agents
                .keys()
                .map(|agent_id| Candidate {
                    match_key: agent_id.clone(),
                    stable_key: agent_id.clone(),
                    payload: agent_id.clone(),
                })
                .collect()
        })
        .unwrap_or_default();
    let ranked = rank(&requested, &candidates);
    let candidate_values: Vec<Value> = ranked
        .iter()
        .map(|agent_id| {
            json!({
                "name": agent_id,
                "team_key": team_key,
                "agent_id": agent_id,
                "advisory": true,
            })
        })
        .collect();
    obj.insert("requested_name".to_string(), json!(requested));
    if let Some(best) = ranked.first() {
        obj.insert("suggested_name".to_string(), json!(best));
    }
    obj.insert("candidates".to_string(), Value::Array(candidate_values));
}

fn delivery_outcome_json(
    outcome: &DeliveryOutcome,
    target: &MessageTarget,
    content: &str,
    opts: &SendOptions,
) -> Value {
    // Pre-release 0.4.0 user directive: send result MUST NOT carry the
    // message body — neither in human form (cli/emit.rs) NOR in --json.
    // External consumers who need the message content read it via `inbox`,
    // not from the send response. We surface `content_length_bytes` as a
    // size sanity field so callers can verify the body size they intended
    // to send arrived intact without exposing the body itself.
    let target_wire = target_json(target);
    json!({
        "ok": outcome.ok,
        "status": delivery_status_wire(outcome.status),
        "delivery_status": api_delivery_status(outcome),
        "delivered": delivery_proven(outcome.status),
        "target": target_wire,
        "agent_id": first_target(target),
        "content_length_bytes": content.len(),
        "sender": opts.sender,
        "message_id": outcome.message_id,
        "message_status": outcome.message_status.0,
        "verification": outcome.verification,
        "stage": outcome.stage.map(delivery_stage_wire),
        "reason": outcome.reason.map(delivery_refusal_wire),
        "channel": outcome.channel,
    })
}

fn api_delivery_status(outcome: &DeliveryOutcome) -> &'static str {
    if delivery_proven(outcome.status) {
        return "delivered";
    }
    if matches!(outcome.status, DeliveryStatus::Queued) && outcome.message_status.0 == "accepted" {
        return "pending";
    }
    delivery_status_wire(outcome.status)
}

fn delivery_proven(status: DeliveryStatus) -> bool {
    matches!(
        status,
        DeliveryStatus::Delivered
            | DeliveryStatus::AlreadyDelivered
            | DeliveryStatus::BroadcastDelivered
            | DeliveryStatus::FanoutDelivered
    )
}

fn add_send_reminder_if_ok(value: &mut Value) {
    if value.get("ok").and_then(Value::as_bool) != Some(true) {
        return;
    }
    let reminder = send_reminder_for_value(value);
    if let Some(obj) = value.as_object_mut() {
        obj.insert("reminder".to_string(), json!(reminder));
    }
}

/// E6 (0.5.9 offline-mailbox-toname-design §§3.1/6.2/8, real-machine
/// escape evidence
/// `.team/artifacts/0.5.9-subscription-gate.md` +
/// `.team/evidence/0.5.9-subscription-gate-20260707T143241Z-4645/`):
/// when the `--to-name <ws>::<team>/leader` resolver refused with
/// `leader_not_attached`, decide whether the target team is still alive
/// (worker + coordinator running without a bound leader) and, if so,
/// enqueue the mailbox row so `attach-leader` replays it exactly once.
///
/// Only queues for third-party senders (sender workspace ≠ target
/// workspace). Owner-scope refusals stay refused so status/diagnose can
/// keep pushing the operator toward `attach-leader`.
fn maybe_enqueue_offline_leader_mailbox(
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
fn target_team_is_alive_for_mailbox(state: &Value, team_key: &str) -> bool {
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

fn cmd_send_result(value: Value, as_json: bool) -> CmdResult {
    let exit = if value.get("ok").and_then(Value::as_bool) == Some(false) {
        ExitCode::Error
    } else {
        ExitCode::Ok
    };
    if as_json {
        CmdResult::from_json(value, true)
    } else {
        CmdResult {
            output: CmdOutput::Human(send_human_output(&value)),
            exit,
            as_json: false,
        }
    }
}

fn send_human_output(value: &Value) -> String {
    let mut parts = vec![
        send_human_field(value, "ok"),
        format!("status: {}", send_human_status(value)),
        send_human_field(value, "message_id"),
        format!("target: {}", send_human_target(value)),
    ];
    for key in ["verification", "stage", "reason", "channel"] {
        if !value.get(key).is_none_or(Value::is_null) {
            parts.push(send_human_field(value, key));
        }
    }
    // 0.5.45 naming-addressing (design §3.4/§3.5, RED-3 positional):
    // when the refusal envelope carries a scope-safe suggestion,
    // surface it verbatim in human output so users can copy the
    // right short id. `requested_name` echoes the typo, `suggested_
    // name` is the copyable canonical.
    if let Some(requested) = value
        .get("requested_name")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        parts.push(format!("requested_name: {requested}"));
    }
    if let Some(suggested) = value
        .get("suggested_name")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        parts.push(format!("Did you mean `{suggested}`? suggested_name: {suggested}"));
    }
    parts.join(" ")
}

fn send_human_field(value: &Value, key: &str) -> String {
    let rendered = value
        .get(key)
        .map(send_human_value)
        .unwrap_or_else(|| "None".to_string());
    format!("{key}: {rendered}")
}

fn send_human_target(value: &Value) -> String {
    ["target", "agent_id", "pane_id", "to_name"]
        .iter()
        .find_map(|key| value.get(*key).filter(|v| !v.is_null()))
        .map(send_human_value)
        .unwrap_or_else(|| "None".to_string())
}

fn send_human_status(value: &Value) -> String {
    value
        .get("status")
        .map(send_human_value)
        .unwrap_or_else(|| {
            if value.get("ok").and_then(Value::as_bool) == Some(true) {
                "delivered".to_string()
            } else {
                "failed".to_string()
            }
        })
}

fn send_human_value(value: &Value) -> String {
    let text = match value {
        Value::Null => "None".to_string(),
        Value::Bool(true) => "True".to_string(),
        Value::Bool(false) => "False".to_string(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => s.clone(),
        Value::Array(_) | Value::Object(_) => {
            serde_json::to_string(value).unwrap_or_else(|_| "None".to_string())
        }
    };
    text.replace(['\r', '\n'], " ")
}

fn send_reminder_for_value(value: &Value) -> &'static str {
    let delivered = value.get("delivered").and_then(Value::as_bool);
    let status = value.get("status").and_then(Value::as_str);
    let delivery_status = value.get("delivery_status").and_then(Value::as_str);
    if delivered == Some(false)
        || matches!(status, Some("queued"))
        || matches!(delivery_status, Some("pending"))
    {
        "Message queued; coordinator will notify when the worker receives it. Do not poll the worker terminal with capture-pane."
    } else {
        crate::cli::SEND_REMINDER
    }
}

fn target_json(target: &MessageTarget) -> Value {
    match target {
        MessageTarget::Single(agent) => json!(agent),
        MessageTarget::Broadcast => json!("*"),
        MessageTarget::Fanout(recipients) => json!(recipients),
    }
}

fn first_target(target: &MessageTarget) -> String {
    match target {
        MessageTarget::Single(agent) => agent.clone(),
        MessageTarget::Broadcast => "*".to_string(),
        MessageTarget::Fanout(recipients) => recipients.first().cloned().unwrap_or_default(),
    }
}

fn delivery_status_wire(status: DeliveryStatus) -> &'static str {
    match status {
        DeliveryStatus::Delivered => "delivered",
        DeliveryStatus::Failed => "failed",
        DeliveryStatus::Queued => "queued",
        DeliveryStatus::Blocked => "blocked",
        DeliveryStatus::Refused => "refused",
        DeliveryStatus::Degraded => "degraded",
        DeliveryStatus::RetryScheduled => "retry_scheduled",
        DeliveryStatus::TrustAutoAnswerExhausted => "trust_auto_answer_exhausted",
        DeliveryStatus::AlreadyDelivered => "already_delivered",
        DeliveryStatus::FallbackLog => "fallback_log",
        DeliveryStatus::BroadcastDelivered => "broadcast_delivered",
        DeliveryStatus::BroadcastPartial => "broadcast_partial",
        DeliveryStatus::FanoutDelivered => "fanout_delivered",
        DeliveryStatus::FanoutPartial => "fanout_partial",
    }
}

fn delivery_refusal_wire(reason: DeliveryRefusal) -> &'static str {
    match reason {
        DeliveryRefusal::TargetNotInTeam => "target_not_in_team",
        DeliveryRefusal::HumanConfirmationRequired => "human_confirmation_required",
        DeliveryRefusal::MissingPermissions => "missing_permissions",
        DeliveryRefusal::RecipientBusy => "recipient_busy",
        DeliveryRefusal::UnknownRecipient => "unknown_recipient",
        DeliveryRefusal::TmuxTargetMissing => "tmux_target_missing",
        DeliveryRefusal::MessageAlreadyClaimed => "message_already_claimed",
        DeliveryRefusal::LeaderNotAttached => "leader_not_attached",
        DeliveryRefusal::CoordinatorUnavailable => "coordinator_unavailable",
        DeliveryRefusal::NoCallerPane => "no_caller_pane",
        DeliveryRefusal::TeamOwnerMismatch => "team_owner_mismatch",
        DeliveryRefusal::Ambiguous => "ambiguous",
        DeliveryRefusal::RecipientPaneInNonInputMode => "recipient_pane_in_non_input_mode",
        DeliveryRefusal::SessionDrift => "session_drift",
        DeliveryRefusal::Duplicate => "duplicate",
        DeliveryRefusal::RoutingAmbiguous => "routing_ambiguous",
        DeliveryRefusal::EmptyTargetList => "empty_target_list",
    }
}

fn delivery_stage_wire(stage: DeliveryStage) -> &'static str {
    match stage {
        DeliveryStage::TrustAutoAnswerDismissalWait => "trust_auto_answer_dismissal_wait",
        DeliveryStage::Inject => "inject",
        DeliveryStage::Submit => "submit",
        DeliveryStage::VisibleCheck => "visible_check",
    }
}

/// E7 (0.5.9 host-leader-registry-design §8.3): resolve `NAME` through
/// `~/.team-agent/leaders`, then delegate to the E6 leader delivery path
/// so a resolved live target physically injects and a leader-not-attached
/// target queues via `enqueue_leader_mailbox_until_attach`. Ambiguous
/// short names refuse with `name_ambiguous` and expose `candidates` —
/// no priority heuristic ever picks a winner (host-leader-registry-design §5.2).
///
/// Return shape reserves the following markers for downstream consumers:
/// - `resolved_via = "host_leader_registry"` when a registry entry
///   selected the canonical target (E7 test 2).
/// - `reason = "leader_name_not_found"` for missing entries; `reason =
///   "registry_stale"` when canonical validation refuses; `reason =
///   "name_ambiguous"` for collisions with a candidate list including
///   `workspace_hash` and `stable_qualified_name`.
///
/// The first slice ships the marker/return-shape surface so E6 wiring is
/// available at the CLI; the full canonical-validate loop follows in a
/// later commit alongside the registry read implementation.
pub fn send_to_canonical_leader_target(
    sender_workspace: &std::path::Path,
    name: &str,
    content: &str,
    sender: &TrustedSender,
    task_id: Option<&str>,
) -> Result<serde_json::Value, CliError> {
    let (logical_to, entry) = match resolve_host_leader_alias(name) {
        Ok(resolved) => resolved,
        Err(value) => return Ok(value),
    };
    let args = SendArgs {
        target: Some(logical_to.clone()),
        message: vec![content.to_string()],
        targets: None,
        workspace: sender_workspace.to_path_buf(),
        team: None,
        task: task_id.map(str::to_string),
        sender: sender.clone(),
        no_ack: false,
        no_wait: true,
        watch_result: false,
        timeout: 0.0,
        confirm_human: false,
        json: true,
        message_id: None,
        pane: None,
        to_name: None,
        to_leader: None,
    };
    let mut value = send_to_logical_to(&args, &logical_to, content)?;
    decorate_host_leader_alias(&mut value, &entry);
    Ok(value)
}
