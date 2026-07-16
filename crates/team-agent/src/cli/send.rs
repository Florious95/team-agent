//! cli · send — `cmd_send` + target 解析(`_send_target`)+ `SendArgs`→`SendOptions` 翻译
//! (`send_options_from_args`,旗标取反语义)。

use super::*;
use crate::messaging::{DeliveryOutcome, DeliveryRefusal, DeliveryStage, DeliveryStatus};

/// `cmd_send`(`commands.py:164`)。解析 target(`--to` fanout / 单 target / `*`)→ [`MessageTarget`],
/// 拼 [`SendOptions`](no_ack→requires_ack 取反、no_wait→wait_visible 取反等)→ `messaging::send_message`。
pub fn cmd_send(args: &SendArgs) -> Result<CmdResult, CliError> {
    if let Some(ref to_leader) = args.to_leader {
        // E7 (0.5.9 host-leader-registry-design §4.2): `--to-leader NAME`
        // resolves NAME through `~/.team-agent/leaders`, canonical-validates
        // the entry, and delegates to the same E6 leader delivery path
        // (`send_to_canonical_leader_target`) — so live inject and offline
        // mailbox (`queued_until_leader_attach` / `leader_mailbox`) both
        // funnel through one code path. Mutually exclusive with
        // `--to-name`, TARGET/--to, `--pane`.
        if args.to_name.is_some()
            || args.pane.is_some()
            || args.target.is_some()
            || args.targets.is_some()
        {
            return Err(CliError::Usage(
                "--to-leader and --to-name/--pane/TARGET/--to are mutually exclusive: \
                 --to-leader resolves a host leader delivery name via the leader registry"
                    .to_string(),
            ));
        }
        let content = args.message.join(" ");
        if content.is_empty() {
            return Err(CliError::Usage(
                "--to-leader requires a non-empty message".to_string(),
            ));
        }
        let value = send_to_canonical_leader_target(
            &args.workspace,
            to_leader,
            &content,
            &args.sender,
            args.task.as_deref(),
        )?;
        return Ok(cmd_send_result(value, args.json));
    }
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
        // 0.5.45 naming-addressing (design §3.1, RED-1): thread
        // `--team` down to resolver so bare `--to-name agent --team T`
        // scopes to `T` BEFORE workspace scanning. Qualified addresses
        // (`team/agent`, `workspace::team/agent`) ignore the scope
        // per §1 priority ladder.
        let (resolved, transport) =
            match crate::cli::named_address::resolve_name_for_cli(
                &args.workspace,
                to_name,
                args.team.as_deref(),
            ) {
                Ok(resolved) => resolved,
                Err(error) => {
                    // E6 (0.5.9 offline-mailbox-toname-design §3.1/§6.2): when
                    // the resolver refuses with `leader_not_attached`, the team
                    // itself may still be alive (worker + coordinator running
                    // without a bound leader). Third-party senders in that
                    // shape must land in the offline mailbox — same
                    // canonical team.db row + queued_until_leader_attach
                    // status the coordinator/attach hook replay through the
                    // existing pipeline exactly once. Owner-scope refusals
                    // (same workspace as target) keep the actionable attach
                    // hint — E6 owner copy is documented as
                    // `run team-agent attach-leader`.
                    if let Some(mut value) = maybe_enqueue_offline_leader_mailbox(
                        &args.workspace,
                        to_name,
                        &content,
                        &args.sender,
                        args.task.as_deref(),
                        &error,
                    )? {
                        add_send_reminder_if_ok(&mut value);
                        return Ok(cmd_send_result(value, args.json));
                    }
                    if args.json {
                        return Ok(CmdResult::from_json(error.to_json(), args.json));
                    }
                    return Err(CliError::Usage(error.n38_message()));
                }
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
        return Ok(cmd_send_result(value, args.json));
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
        return Ok(cmd_send_result(value, args.json));
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
        return Ok(cmd_send_result(amb, args.json));
    }
    if let Some(value) = dirty_topology_refusal_value(&selected, args.team.as_deref()) {
        return Ok(cmd_send_result(value, args.json));
    }
    let coordinator_ensure = if target_has_known_worker(&selected.state, &target, &opts.sender) {
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

/// F1 (0.3.26): direct pane-id send — bypasses agent-name routing + team
/// membership check. Constructs `Target::Pane`, renders the message with
/// the standard protocol block (Team Agent message from sender + token),
/// injects via the selected team's endpoint-local tmux transport, and
/// surfaces the inject report as a JSON result.
///
/// 0.5.43 debt-sweep (§6.2): the pre-0.5.43 comment overstated the
/// scope. pane_id is inherently endpoint-local —
/// `lifecycle_worker_tmux_backend_for_selected_state` below builds the
/// transport from the SELECTED team's persisted endpoint. For cross-
/// workspace delivery, use `--to-name` / `--to-leader` instead; there
/// is intentionally no `--socket` flag.
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
    sender: &str,
    task_id: Option<&str>,
) -> Result<serde_json::Value, CliError> {
    // Resolve NAME through the registry. Ambiguity is decided *before*
    // canonical validation so an ambiguous short name never picks a
    // winner — even when only one candidate happens to be live. Send
    // uses the no-GC listing so stale entries can still refuse with
    // `registry_stale` — the leaders CLI is the one that prunes.
    let classified = crate::leader::registry::list_validated_no_gc();
    let mut candidates_all: Vec<crate::leader::registry::LeaderRegistryEntry> = Vec::new();
    for (entry, _status, _reason) in &classified {
        let matches = entry.delivery_name == name
            || entry.qualified_name == name
            || entry.stable_qualified_name == name
            || entry.aliases.iter().any(|a| a == name);
        if matches {
            candidates_all.push(entry.clone());
        }
    }
    if candidates_all.is_empty() {
        return Ok(serde_json::json!({
            "ok": false,
            "status": "refused",
            "reason": "leader_name_not_found",
            "requested_name": name,
            "resolved_via": "host_leader_registry",
            "candidates": Vec::<serde_json::Value>::new(),
            "workspace_hash": null,
            "stable_qualified_name": null,
            "channel": "leader_mailbox",
            "delivered": false,
            "message_status": "queued_until_leader_attach",
            "action": "run `team-agent leaders` to see registered leaders; inspect queued leader messages with `team-agent inbox`; retry with a qualified name",
            "registry_stale": false,
        }));
    }
    if candidates_all.len() > 1 {
        let cand_json: Vec<serde_json::Value> = candidates_all
            .iter()
            .map(|e| {
                serde_json::json!({
                    "name": e.qualified_name,
                    "workspace": e.workspace.display().to_string(),
                    "team_key": e.team_key,
                    "workspace_hash": e.workspace_hash,
                    "stable_qualified_name": e.stable_qualified_name,
                })
            })
            .collect();
        return Ok(serde_json::json!({
            "ok": false,
            "status": "refused",
            "reason": "name_ambiguous",
            "requested_name": name,
            "resolved_via": "host_leader_registry",
            "candidates": cand_json,
            "channel": "leader_mailbox",
            "delivered": false,
            "action": "run `team-agent leaders` and retry with the qualified name",
        }));
    }
    let entry = candidates_all.into_iter().next().ok_or_else(|| {
        CliError::Runtime("internal: candidate list must have at least one entry".to_string())
    })?;
    // Canonical-validate the entry against target workspace state. Send
    // through the same E6 --to-name path so live inject and mailbox both
    // funnel through one code path.
    let (status, reason) = crate::leader::registry::classify(&entry);
    if status == "STALE" {
        // Check whether the underlying team is still alive — if so we
        // may still queue via the E6 mailbox path (leader-not-attached
        // shape). If the workspace/team is gone we refuse `registry_stale`.
        let state = crate::state::persist::load_runtime_state(&entry.workspace).ok();
        let team_alive = state
            .as_ref()
            .and_then(|s| s.get("teams"))
            .and_then(|v| v.as_object())
            .and_then(|teams| teams.get(&entry.team_key))
            .and_then(|t| t.get("status"))
            .and_then(serde_json::Value::as_str)
            .map(|s| s == "alive" || s.is_empty())
            .unwrap_or(false);
        if !team_alive {
            return Ok(serde_json::json!({
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
        // Team alive but leader unattached → E6 mailbox.
        let event_log = crate::event_log::EventLog::new(&entry.workspace);
        let task = task_id.map(|s| crate::model::ids::TaskId::new(s.to_string()));
        let outcome = crate::messaging::enqueue_leader_mailbox_until_attach(
            &entry.workspace,
            &entry.team_key,
            content,
            task.as_ref(),
            sender,
            &event_log,
        )
        .map_err(|e| CliError::Runtime(e.to_string()))?;
        return Ok(serde_json::json!({
            "ok": true,
            "status": "queued_until_leader_attach",
            "message_status": "queued_until_leader_attach",
            "channel": "leader_mailbox",
            "delivered": false,
            "resolved_via": "host_leader_registry",
            "requested_name": name,
            "to_leader": entry.qualified_name,
            "target_workspace": entry.workspace.display().to_string(),
            "workspace_hash": entry.workspace_hash,
            "stable_qualified_name": entry.stable_qualified_name,
            "team_key": entry.team_key,
            "message_id": outcome.message_id,
        }));
    }
    // LIVE: canonical-validated. Delegate to the E6 --to-name path via a
    // synthesized `<workspace>::<team_key>/leader` name so live inject +
    // mailbox both go through one code path.
    let to_name = format!("{}::{}/leader", entry.workspace.display(), entry.team_key);
    // 0.5.45 naming-addressing (design §3.1 / §4.1): internal
    // registry-to-E6 delegation MUST pass None for bare_team_scope.
    // The synthesized name above is a full `workspace::team/leader`
    // form, and the caller's `--team` flag (if any) must not
    // override the registry's authoritative team_key.
    let (resolved, transport) =
        match crate::cli::named_address::resolve_name_for_cli(sender_workspace, &to_name, None) {
            Ok(r) => r,
            Err(err) => {
                // Named-address refusal — surface it verbatim but tag as
                // registry-resolved so callers can trace the origin.
                let mut body = err.to_json();
                if let Some(obj) = body.as_object_mut() {
                    obj.insert(
                        "resolved_via".to_string(),
                        serde_json::Value::String("host_leader_registry".to_string()),
                    );
                    obj.insert("delivered".to_string(), serde_json::Value::Bool(false));
                }
                return Ok(body);
            }
        };
    let mut value = send_to_named_pane_direct(
        sender_workspace,
        transport.as_ref(),
        &resolved,
        content,
        sender,
        task_id,
        true,
    )?;
    if let Some(obj) = value.as_object_mut() {
        obj.insert(
            "resolved_via".to_string(),
            serde_json::Value::String("host_leader_registry".to_string()),
        );
        obj.insert(
            "to_leader".to_string(),
            serde_json::Value::String(entry.qualified_name.clone()),
        );
        // Honest delivered marker. `send_to_named_pane_direct` sets `ok`
        // to whether physical inject verified — mirror that as
        // `delivered`.
        let ok = obj
            .get("ok")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        obj.insert("delivered".to_string(), serde_json::Value::Bool(ok));
    }
    Ok(value)
}
