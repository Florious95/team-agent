//! cli · send — `cmd_send` + target 解析(`_send_target`)+ `SendArgs`→`SendOptions` 翻译
//! (`send_options_from_args`,旗标取反语义)。

use super::*;
use crate::messaging::{DeliveryOutcome, DeliveryRefusal, DeliveryStage, DeliveryStatus};

/// `cmd_send`(`commands.py:164`)。解析 target(`--to` fanout / 单 target / `*`)→ [`MessageTarget`],
/// 拼 [`SendOptions`](no_ack→requires_ack 取反、no_wait→wait_visible 取反等)→ `messaging::send_message`。
pub fn cmd_send(args: &SendArgs) -> Result<CmdResult, CliError> {
    if let Some(ref to_name) = args.to_name {
        if args.pane.is_some() || args.target.is_some() || args.targets.is_some() {
            return Err(CliError::Usage(
                "--to-name and --pane/TARGET/--to are mutually exclusive: \
                 --to-name resolves a stable workspace/team/name to a live pane"
                    .to_string(),
            ));
        }
        let content = args.message.join(" ");
        if content.is_empty() {
            return Err(CliError::Usage(
                "--to-name requires a non-empty message".to_string(),
            ));
        }
        let (resolved, transport) =
            match crate::cli::named_address::resolve_name_for_cli(&args.workspace, to_name) {
                Ok(resolved) => resolved,
                Err(error) if args.json => {
                    return Ok(CmdResult::from_json(error.to_json(), args.json));
                }
                Err(error) => return Err(CliError::Usage(error.n38_message())),
            };
        let mut value = send_to_named_pane_direct(
            &args.workspace,
            transport.as_ref(),
            &resolved,
            &content,
            &args.sender,
            args.task.as_deref(),
            args.json,
        )?;
        add_send_reminder_if_ok(&mut value);
        return Ok(CmdResult::from_json(value, args.json));
    }
    // F1 (0.3.26, cross-team send): --pane <pane_id> direct targeting.
    // Mutually exclusive with target / --to (agent-name routing).
    if let Some(ref pane_id) = args.pane {
        if args.target.is_some() || args.targets.is_some() {
            return Err(CliError::Usage(
                "--pane and TARGET/--to are mutually exclusive: \
                 --pane bypasses agent-name routing and injects directly into the \
                 specified tmux pane (cross-team capable)"
                    .to_string(),
            ));
        }
        let content = args.message.join(" ");
        if content.is_empty() {
            return Err(CliError::Usage(
                "--pane requires a non-empty message".to_string(),
            ));
        }
        let mut value = send_to_pane_direct(
            &args.workspace,
            pane_id,
            &content,
            &args.sender,
            args.task.as_deref(),
            args.team.as_deref(),
            args.json,
        )?;
        add_send_reminder_if_ok(&mut value);
        return Ok(CmdResult::from_json(value, args.json));
    }
    let selected = crate::state::selector::resolve_active_team(
        &args.workspace,
        args.team.as_deref(),
        crate::state::selector::SelectorMode::RuntimeOnly,
    )?;
    let target = send_target(args.targets.as_deref(), args.target.as_deref());
    let mut opts = send_options_from_args(args);
    if opts.team.is_none() {
        opts.team = Some(TeamKey::new(selected.team_key.clone()));
    }
    let content = args.message.join(" ");
    // CR-061/N27 routing-ambiguous: a single positional with no `--to`/`--targets` and an
    // empty message body is a prompt-only invocation (`team-agent send "fix the build"`).
    // The lone positional is CONTENT, not a target — reject with `routing_ambiguous`
    // (NOT `target_not_in_team`, which would lie that the user did pick a target).
    if let Some(amb) =
        routing_ambiguous_value(&selected.run_workspace, args, &target, &content, &opts)
    {
        return Ok(CmdResult::from_json(amb, args.json));
    }
    let outcome = messaging::send_message(&selected.run_workspace, &target, &content, &opts)?;
    let mut value = delivery_outcome_json(&outcome, &target, &content, &opts);
    if opts.watch_result {
        if let Some(obj) = value.as_object_mut() {
            obj.insert("watch".to_string(), watch_notice_json(&target, &opts));
        }
    }
    add_send_reminder_if_ok(&mut value);
    Ok(CmdResult::from_json(value, args.json))
}

/// F1 (0.3.26): direct pane-id send — bypasses agent-name routing + team
/// membership check. Constructs `Target::Pane`, renders the message with
/// the standard protocol block (Team Agent message from sender + token),
/// injects via the default tmux backend, and surfaces the inject report as
/// a JSON result. Cross-team capable: no restriction on which tmux session
/// or socket the pane belongs to.
fn send_to_pane_direct(
    workspace: &Path,
    pane_id: &str,
    content: &str,
    sender: &str,
    task_id: Option<&str>,
    team: Option<&str>,
    json: bool,
) -> Result<serde_json::Value, CliError> {
    use crate::messaging::delivery::render_message;
    use crate::transport::{InjectPayload, Key, PaneId, Target};

    let message_id = format!("pane_send_{}", chrono::Utc::now().timestamp_millis());
    let rendered = render_message(sender, task_id, content, &message_id);
    let target = Target::Pane(PaneId::new(pane_id));
    let run_workspace = crate::model::paths::canonical_run_workspace(workspace)
        .unwrap_or_else(|_| workspace.to_path_buf());
    let transport = crate::lifecycle::restart::lifecycle_worker_tmux_backend_for_selected_state(
        &run_workspace,
        team,
    )
    .unwrap_or_else(|_| crate::tmux_backend::TmuxBackend::for_workspace(&run_workspace));
    let event_log = crate::event_log::EventLog::new(&run_workspace);
    // Warn if the pane is not in the team's known agents (cross-team usage).
    let state = crate::state::persist::load_runtime_state(&run_workspace).ok();
    let in_team = state
        .as_ref()
        .and_then(|s| s.get("agents"))
        .and_then(serde_json::Value::as_object)
        .is_some_and(|agents| {
            agents.values().any(|agent| {
                agent
                    .get("pane_id")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|p| p == pane_id)
            })
        });
    if !in_team {
        eprintln!(
            "warning: pane {pane_id} is not in the team's known agents — \
             cross-team delivery (F1)"
        );
    }
    let transport: &dyn crate::transport::Transport = &transport;
    let report = transport
        .inject(&target, &InjectPayload::Text(rendered), Key::Enter, true)
        .map_err(|e| CliError::Runtime(format!("inject to pane {pane_id} failed: {e}")))?;
    let _ = event_log.write(
        "send.pane_direct",
        serde_json::json!({
            "pane_id": pane_id,
            "sender": sender,
            "message_id": message_id,
            "submit_verification": crate::transport::submit_verification_wire(report.submit_verification),
            "inject_verification": format!("{:?}", report.inject_verification),
            "in_team": in_team,
        }),
    );
    let ok = matches!(
        report.submit_verification,
        crate::transport::SubmitVerification::EnterSentWithoutPlaceholderCheck
            | crate::transport::SubmitVerification::PastedContentPromptAbsentAfterSubmit
            | crate::transport::SubmitVerification::KeySentAfterVisibleToken { .. }
    );
    Ok(serde_json::json!({
        "ok": ok,
        "pane_id": pane_id,
        "message_id": message_id,
        "submit_verification": crate::transport::submit_verification_wire(report.submit_verification),
        "inject_verification": format!("{:?}", report.inject_verification),
        "in_team": in_team,
    }))
}

fn send_to_named_pane_direct(
    sender_workspace: &Path,
    transport: &dyn crate::transport::Transport,
    resolved: &crate::cli::named_address::ResolvedNamedAddress,
    content: &str,
    sender: &str,
    task_id: Option<&str>,
    _json: bool,
) -> Result<serde_json::Value, CliError> {
    use crate::messaging::delivery::render_message;
    use crate::transport::{InjectPayload, Key, PaneId, Target};

    let message_id = format!("named_send_{}", chrono::Utc::now().timestamp_millis());
    let rendered = render_message(sender, task_id, content, &message_id);
    let sender_run_workspace = crate::model::paths::canonical_run_workspace(sender_workspace)
        .unwrap_or_else(|_| sender_workspace.to_path_buf());
    let event_log = crate::event_log::EventLog::new(&sender_run_workspace);
    if let Some(warning) = &resolved.warning {
        eprintln!("warning: {warning}");
    }
    if resolved.transport_kind.as_deref() == Some("codex_app_server") {
        return send_to_named_app_server_leader(
            &event_log,
            resolved,
            &message_id,
            &rendered,
            sender,
        );
    }
    let target = Target::Pane(PaneId::new(&resolved.pane_id));
    let report = transport
        .inject(&target, &InjectPayload::Text(rendered), Key::Enter, true)
        .map_err(|e| {
            CliError::Runtime(format!(
                "inject to named target {} pane {} failed: {e}",
                resolved.raw_name, resolved.pane_id
            ))
        })?;
    let target_kind = named_target_kind_wire(resolved.target_kind);
    let event = serde_json::json!({
        "to_name": resolved.raw_name,
        "target_kind": target_kind,
        "sender": sender,
        "sender_workspace": sender_run_workspace.display().to_string(),
        "target_workspace": resolved.target_workspace.display().to_string(),
        "team_key": resolved.team_key,
        "agent_id": resolved.agent_id,
        "pane_id": resolved.pane_id,
        "session_name": resolved.session_name,
        "window_name": resolved.window_name,
        "tmux_endpoint": resolved.tmux_endpoint,
        "state_pane_id": resolved.state_pane_id,
        "state_pane_stale": resolved.state_pane_stale,
        "agent_status": resolved.agent_status,
        "warning": resolved.warning,
        "message_id": message_id,
        "submit_verification": crate::transport::submit_verification_wire(report.submit_verification),
        "inject_verification": format!("{:?}", report.inject_verification),
    });
    let _ = event_log.write("send.name_direct", event.clone());
    let ok = matches!(
        report.submit_verification,
        crate::transport::SubmitVerification::EnterSentWithoutPlaceholderCheck
            | crate::transport::SubmitVerification::PastedContentPromptAbsentAfterSubmit
            | crate::transport::SubmitVerification::KeySentAfterVisibleToken { .. }
    );
    let mut value = event;
    if let Some(obj) = value.as_object_mut() {
        obj.insert("ok".to_string(), serde_json::json!(ok));
    }
    Ok(value)
}

fn send_to_named_app_server_leader(
    event_log: &crate::event_log::EventLog,
    resolved: &crate::cli::named_address::ResolvedNamedAddress,
    message_id: &str,
    rendered: &str,
    sender: &str,
) -> Result<serde_json::Value, CliError> {
    let receiver = serde_json::json!({
        "mode": "codex_app_server",
        "transport_kind": "codex_app_server",
        "app_server": resolved.app_server.clone().unwrap_or(serde_json::Value::Null),
    });
    let binding = crate::codex_app_server::binding_from_receiver(&receiver)
        .map_err(|error| CliError::Runtime(format!("invalid app-server named leader: {error}")))?;
    let target_kind = named_target_kind_wire(resolved.target_kind);
    let base_event = serde_json::json!({
        "to_name": resolved.raw_name,
        "target_kind": target_kind,
        "sender": sender,
        "sender_workspace": resolved.sender_workspace.display().to_string(),
        "target_workspace": resolved.target_workspace.display().to_string(),
        "team_key": resolved.team_key,
        "agent_id": resolved.agent_id,
        "transport_kind": "codex_app_server",
        "socket": binding.socket,
        "thread_id": binding.thread_id,
        "message_id": message_id,
    });
    match crate::codex_app_server::submit_to_bound_thread(&binding, message_id, rendered) {
        Ok(submit) => {
            let event = merge_json(
                base_event.clone(),
                serde_json::json!({
                    "ok": true,
                    "turn_id": submit.turn_id,
                    "turn_status": submit.turn_status,
                }),
            );
            let _ = event_log.write("send.name_app_server", event.clone());
            Ok(event)
        }
        Err(crate::codex_app_server::AppServerError::LeaderBusy(message)) => {
            let event = merge_json(
                base_event.clone(),
                serde_json::json!({
                    "ok": false,
                    "status": "retry_scheduled",
                    "reason": "leader_busy",
                    "channel": "leader_busy",
                    "error": message,
                }),
            );
            let _ = event_log.write("send.name_app_server", event.clone());
            Ok(event)
        }
        Err(error) => {
            let event = merge_json(
                base_event.clone(),
                serde_json::json!({
                    "ok": false,
                    "status": "refused",
                    "reason": error.code(),
                    "channel": "rebind_required",
                    "error": error.to_string(),
                    "action": "run team-agent attach-app-server-leader for the target team",
                }),
            );
            let _ = event_log.write("send.name_app_server", event.clone());
            Ok(event)
        }
    }
}

fn merge_json(mut left: serde_json::Value, right: serde_json::Value) -> serde_json::Value {
    if let (Some(left), Some(right)) = (left.as_object_mut(), right.as_object()) {
        for (key, value) in right {
            left.insert(key.clone(), value.clone());
        }
    }
    left
}

fn named_target_kind_wire(kind: crate::cli::named_address::NamedTargetKind) -> &'static str {
    match kind {
        crate::cli::named_address::NamedTargetKind::Worker => "worker",
        crate::cli::named_address::NamedTargetKind::Leader => "leader",
        crate::cli::named_address::NamedTargetKind::SessionWindow => "session_window",
    }
}

pub fn cmd_fallback_send_leader(args: &FallbackSendLeaderArgs) -> Result<CmdResult, CliError> {
    if let Some(value) = fallback_business_refusal(&args.primary_error, args.json) {
        return Ok(value);
    }
    let selected = crate::state::selector::resolve_active_team(
        &args.workspace,
        args.team.as_deref(),
        crate::state::selector::SelectorMode::RuntimeOnly,
    )?;
    let target = MessageTarget::Single("leader".to_string());
    let opts = SendOptions {
        task_id: args.task.as_ref().map(|task| TaskId::new(task.clone())),
        route_task_id: false,
        sender: args.sender.clone(),
        requires_ack: false,
        wait_visible: false,
        block_until_delivered: false,
        team: Some(TeamKey::new(selected.team_key.clone())),
        message_id: Some(args.message_id.clone()),
        ..SendOptions::default()
    };
    let primary = messaging::send_message(&selected.run_workspace, &target, &args.content, &opts);
    let message_id = match &primary {
        Ok(outcome) => outcome
            .message_id
            .clone()
            .unwrap_or_else(|| args.message_id.clone()),
        Err(_) => args.message_id.clone(),
    };
    if let Ok(outcome) = &primary {
        if is_business_refusal_outcome(outcome) {
            let value = json!({
                "ok": false,
                "status": "refused",
                "reason": "business_reject",
                "primary_error": args.primary_error,
                "message_id": outcome.message_id,
                "action": "N38 fallback refused: business rule refusals must not use fallback pane delivery",
            });
            return Ok(CmdResult::from_json(value, args.json));
        }
    }

    let state = selected_state_with_active_key(&selected);
    let event_log = crate::event_log::EventLog::new(&selected.run_workspace);
    let primary_error = match primary {
        Ok(outcome) if primary_delivery_succeeded(outcome.status) => {
            let mut value = delivery_outcome_json(&outcome, &target, &args.content, &opts);
            if let Some(obj) = value.as_object_mut() {
                obj.insert("fallback_used".to_string(), json!(false));
                obj.insert("primary_error".to_string(), json!(args.primary_error));
            }
            return Ok(CmdResult::from_json(value, args.json));
        }
        Ok(outcome) => format!(
            "{}; fallback_cli_primary_status={}",
            args.primary_error,
            delivery_status_wire(outcome.status)
        ),
        Err(error) => format!("{}; fallback_cli_primary_error={error}", args.primary_error),
    };
    let outcome = messaging::deliver_to_leader_fallback_pane(
        &selected.run_workspace,
        &state,
        &message_id,
        None,
        &args.content,
        false,
        Some(&primary_error),
        &event_log,
    )?;
    let mut value = delivery_outcome_json(&outcome, &target, &args.content, &opts);
    if let Some(obj) = value.as_object_mut() {
        obj.insert("primary_error".to_string(), json!(args.primary_error));
        obj.insert("delivered_via".to_string(), json!("fallback_pane"));
        obj.insert(
            "next_action".to_string(),
            json!("run team-agent restart-agent to refresh the worker MCP transport"),
        );
    }
    Ok(CmdResult::from_json(value, args.json))
}

pub fn cmd_fallback_report_result(args: &FallbackReportResultArgs) -> Result<CmdResult, CliError> {
    if let Some(value) = fallback_business_refusal(&args.primary_error, args.json) {
        return Ok(value);
    }
    let selected = crate::state::selector::resolve_active_team(
        &args.workspace,
        args.team.as_deref(),
        crate::state::selector::SelectorMode::RuntimeOnly,
    )?;
    let envelope = fallback_result_envelope(args)?;
    let value = messaging::report_result_for_owner_team_with_primary_error(
        &selected.run_workspace,
        &envelope,
        Some(&selected.team_key),
        Some(&args.primary_error),
    )?;
    let mut value = value;
    if let Some(obj) = value.as_object_mut() {
        obj.insert("primary_error".to_string(), json!(args.primary_error));
        obj.insert(
            "fallback_protocol".to_string(),
            json!("fallback-report-result"),
        );
        obj.insert(
            "next_action".to_string(),
            json!("run team-agent restart-agent to refresh the worker MCP transport"),
        );
    }
    Ok(CmdResult::from_json(value, args.json))
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

fn fallback_result_envelope(args: &FallbackReportResultArgs) -> Result<Value, CliError> {
    let mut envelope: Value = serde_json::from_str(&args.result_json)?;
    let Some(obj) = envelope.as_object_mut() else {
        return Err(CliError::Usage(
            "--result-json must be a JSON object".to_string(),
        ));
    };
    obj.entry("schema_version".to_string())
        .or_insert_with(|| json!("result_envelope_v1"));
    obj.entry("task_id".to_string())
        .or_insert_with(|| json!(args.task_id));
    obj.entry("agent_id".to_string())
        .or_insert_with(|| json!(args.agent_id));
    obj.entry("status".to_string())
        .or_insert_with(|| json!("success"));
    obj.entry("summary".to_string())
        .or_insert_with(|| json!("completed"));
    for key in ["changes", "tests", "risks", "artifacts", "next_actions"] {
        obj.entry(key.to_string()).or_insert_with(|| json!([]));
    }
    Ok(envelope)
}

fn fallback_business_refusal(primary_error: &str, as_json: bool) -> Option<CmdResult> {
    is_business_reject_text(primary_error).then(|| {
        CmdResult::from_json(
            json!({
                "ok": false,
                "status": "refused",
                "reason": "business_reject",
                "primary_error": primary_error,
                "action": "N38 fallback refused: business rule refusals must not use fallback pane delivery",
            }),
            as_json,
        )
    })
}

fn is_business_refusal_outcome(outcome: &DeliveryOutcome) -> bool {
    matches!(
        outcome.reason,
        Some(
            DeliveryRefusal::TargetNotInTeam
                | DeliveryRefusal::HumanConfirmationRequired
                | DeliveryRefusal::MissingPermissions
                | DeliveryRefusal::UnknownRecipient
                | DeliveryRefusal::TeamOwnerMismatch
                | DeliveryRefusal::Ambiguous
                | DeliveryRefusal::RecipientPaneInNonInputMode
                | DeliveryRefusal::SessionDrift
                | DeliveryRefusal::RoutingAmbiguous
                | DeliveryRefusal::EmptyTargetList
        )
    )
}

fn primary_delivery_succeeded(status: DeliveryStatus) -> bool {
    matches!(
        status,
        DeliveryStatus::Delivered | DeliveryStatus::AlreadyDelivered
    )
}

fn is_business_reject_text(error: &str) -> bool {
    let lower = error.to_ascii_lowercase();
    [
        "peer_not_in_scope",
        "target_not_in_team",
        "permission denied",
        "missing_permissions",
        "human_confirmation_required",
        "unknown_recipient",
        "routing_ambiguous",
        "quota",
        "rate limit",
        "rate_limit",
        "blacklist",
        "blacklisted",
        "forbidden",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
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

fn add_send_reminder_if_ok(value: &mut Value) {
    if value.get("ok").and_then(Value::as_bool) != Some(true) {
        return;
    }
    if let Some(obj) = value.as_object_mut() {
        obj.insert("reminder".to_string(), json!(crate::cli::SEND_REMINDER));
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

#[cfg(test)]
mod e23_tests {
    use super::*;

    #[test]
    fn fallback_error_classifier_allows_transport_and_primary_bugs() {
        for error in [
            "Transport closed",
            "Connection refused",
            "Broken pipe",
            "EOF on transport",
            "MCP timeout after 5s",
            "internal assertion failed: unwrap on Err",
            "primary_delivery_error: serialize failed",
        ] {
            assert!(
                !is_business_reject_text(error),
                "failure should be fallback-eligible, not classified as a business refusal: {error}"
            );
        }
    }

    #[test]
    fn fallback_error_classifier_blocks_business_refusals() {
        for error in [
            "peer_not_in_scope",
            "target_not_in_team",
            "permission denied",
            "missing_permissions",
            "human_confirmation_required",
            "unknown_recipient",
            "quota exceeded",
            "rate_limit",
            "blacklisted target",
        ] {
            assert!(
                is_business_reject_text(error),
                "business refusal must not use fallback pane delivery: {error}"
            );
        }
    }
}
