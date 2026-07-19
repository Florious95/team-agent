use crate::cli::{CliError, CmdOutput, CmdResult, ExitCode};
use crate::messaging::{
    DeliveryOutcome, DeliveryRefusal, DeliveryStage, DeliveryStatus, MessageTarget, SendOptions,
};
use serde_json::{json, Value};

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
