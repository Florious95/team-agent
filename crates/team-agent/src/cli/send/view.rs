use super::*;

pub(super) fn watch_notice_json(target: &MessageTarget, opts: &SendOptions) -> Value {
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
pub(super) fn attach_positional_typo_suggestions(
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

pub(super) fn delivery_outcome_json(
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

pub(super) fn api_delivery_status(outcome: &DeliveryOutcome) -> &'static str {
    if delivery_proven(outcome.status) {
        return "delivered";
    }
    if matches!(outcome.status, DeliveryStatus::Queued) && outcome.message_status.0 == "accepted" {
        return "pending";
    }
    delivery_status_wire(outcome.status)
}

pub(super) fn delivery_proven(status: DeliveryStatus) -> bool {
    matches!(
        status,
        DeliveryStatus::Delivered
            | DeliveryStatus::AlreadyDelivered
            | DeliveryStatus::BroadcastDelivered
            | DeliveryStatus::FanoutDelivered
    )
}

pub(super) fn add_send_reminder_if_ok(value: &mut Value) {
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

pub(super) fn cmd_send_result(value: Value, as_json: bool) -> CmdResult {
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

pub(super) fn send_human_output(value: &Value) -> String {
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
        parts.push(format!(
            "Did you mean `{suggested}`? suggested_name: {suggested}"
        ));
    }
    parts.join(" ")
}

pub(super) fn send_human_field(value: &Value, key: &str) -> String {
    let rendered = value
        .get(key)
        .map(send_human_value)
        .unwrap_or_else(|| "None".to_string());
    format!("{key}: {rendered}")
}

pub(super) fn send_human_target(value: &Value) -> String {
    ["target", "agent_id", "pane_id", "to_name"]
        .iter()
        .find_map(|key| value.get(*key).filter(|v| !v.is_null()))
        .map(send_human_value)
        .unwrap_or_else(|| "None".to_string())
}

pub(super) fn send_human_status(value: &Value) -> String {
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

pub(super) fn send_human_value(value: &Value) -> String {
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

pub(super) fn send_reminder_for_value(value: &Value) -> &'static str {
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

pub(super) fn target_json(target: &MessageTarget) -> Value {
    match target {
        MessageTarget::Single(agent) => json!(agent),
        MessageTarget::Broadcast => json!("*"),
        MessageTarget::Fanout(recipients) => json!(recipients),
    }
}

pub(super) fn first_target(target: &MessageTarget) -> String {
    match target {
        MessageTarget::Single(agent) => agent.clone(),
        MessageTarget::Broadcast => "*".to_string(),
        MessageTarget::Fanout(recipients) => recipients.first().cloned().unwrap_or_default(),
    }
}

pub(super) fn delivery_status_wire(status: DeliveryStatus) -> &'static str {
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

pub(super) fn delivery_refusal_wire(reason: DeliveryRefusal) -> &'static str {
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

pub(super) fn delivery_stage_wire(stage: DeliveryStage) -> &'static str {
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
