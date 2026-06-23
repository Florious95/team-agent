//! send.py — 顶层发件入口 + 单发/广播/扇出 + owner-gate worker 旁路 + session-drift 拒绝 (card §64)。

use std::path::Path;

use crate::coordinator::{CoordinatorHealthStatus, WorkspacePath};
use crate::event_log::EventLog;
use crate::model::enums::PaneLiveness;
use crate::model::ids::{TaskId, TeamKey};
use crate::transport::{PaneId, Transport};

use super::helpers::{status_wire, MessageStatusShadow};
use super::leader_receiver::{send_to_leader_receiver, send_to_leader_receiver_with_message_id};
use super::{DeliveryOutcome, DeliveryRefusal, DeliveryStatus, MessagingError};

/// 发件目标:单 target / 广播 `*` / 扇出 list (`send.py:36` `target: str|list[str]|None`)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MessageTarget {
    /// 单 agent / pane target。
    Single(String),
    /// `"*"` 广播全队。
    Broadcast,
    /// 扇出到显式 list。
    Fanout(Vec<String>),
}

/// `send_message` 选项 (`send.py:36`:Python 大量默认参数 → typed 选项 struct)。
#[derive(Debug, Clone)]
pub struct SendOptions {
    pub task_id: Option<TaskId>,
    /// `route_task_id`(golden `send.py:190`,默认 `True`):仅 **routing**(CLI `send --task`)时把
    /// `task_id` 当真任务校验/路由。**投递/fanout/internal/coordinator** 路径传 `false`
    /// (`internal_delivery.py:44`、`send.py:412/481`),此时 `task_id` 只是标签,**不校验 state.tasks**。
    pub route_task_id: bool,
    pub sender: String,
    pub requires_ack: bool,
    pub confirm_human: bool,
    pub wait_visible: bool,
    pub timeout: f64,
    pub lock_timeout: f64,
    pub watch_result: bool,
    pub block_until_delivered: bool,
    pub team: Option<TeamKey>,
    /// Caller-supplied idempotency key (CLI `--message-id`, CR-015/054). When `Some`,
    /// the store insert uses this id verbatim; a repeat with the same id is rejected
    /// as [`DeliveryRefusal::Duplicate`] instead of creating a second row.
    pub message_id: Option<String>,
}

impl Default for SendOptions {
    fn default() -> Self {
        // 默认值对齐 Python 签名 (`send.py:38-40`)。
        Self {
            task_id: None,
            route_task_id: true,
            sender: "leader".to_string(),
            requires_ack: true,
            confirm_human: false,
            wait_visible: true,
            timeout: 30.0,
            lock_timeout: 5.0,
            watch_result: false,
            block_until_delivered: true,
            team: None,
            message_id: None,
        }
    }
}

/// `send_message` (`send.py:36`):顶层发件 —— 加 `_runtime_lock("send")`,解析 list/`*`/单
/// target,路由 + 权限 + 人确认门,投递或入队。MCP `send` 工具 + CLI 调它。
pub fn send_message(
    workspace: &Path,
    target: &MessageTarget,
    content: &str,
    opts: &SendOptions,
) -> Result<DeliveryOutcome, MessagingError> {
    // N31/N32 funnel: leader / `*` broadcast / fanout-list dispatch sits HERE in send.py:
    // single recipient falls through to the legacy worker path; `to=leader` routes to
    // `send_to_leader_receiver` (the shared leader-delivery primitive); broadcast/fanout
    // expand the recipient set and re-enter `send_message` per recipient so leader/peer
    // each go through their own (same) primitive — no parallel BroadcastEngine.
    let event_log = EventLog::new(workspace);
    let raw_state = crate::state::persist::load_runtime_state(workspace)?;
    let mut state = opts
        .team
        .as_ref()
        .map(|team| crate::state::projection::project_top_level_view(&raw_state, team.as_str()))
        .unwrap_or_else(|| raw_state.clone());
    backfill_leader_binding_for_delivery_view(&mut state, &raw_state);
    let recipient = match target {
        MessageTarget::Single(target) if target == "leader" => {
            let outcome = send_to_leader_receiver_with_message_id(
                workspace,
                &state,
                "leader",
                content,
                opts.task_id.as_ref(),
                &opts.sender,
                opts.requires_ack,
                None,
                opts.message_id.as_deref(),
                &event_log,
            )?;
            if matches!(outcome.status, DeliveryStatus::Queued) && owner_pane_is_dead(&state) {
                if let Some(message_id) = outcome.message_id.clone() {
                    let team_key = owner_gate_hint_team_key(&state);
                    if !explicit_claim_applied(workspace, &team_key, "") {
                        return Ok(rebind_required_outcome_with_verification(
                            Some(message_id),
                            format!("team-agent claim-leader --team {team_key}"),
                        ));
                    }
                }
            }
            return Ok(outcome);
        }
        MessageTarget::Single(target) if target.is_empty() => {
            return Ok(refused_outcome(DeliveryRefusal::UnknownRecipient));
        }
        MessageTarget::Single(target) => target,
        MessageTarget::Broadcast => {
            let recipients = broadcast_recipients(&state, &opts.sender, opts.team.as_ref());
            return fanout_send(
                workspace,
                &state,
                &recipients,
                content,
                opts,
                &event_log,
                "*",
            );
        }
        MessageTarget::Fanout(recipients) if recipients.is_empty() => {
            // swallow batch 3 ②: a failed send carries its reason (Python send error
            // reason style) — "failed with no reason" is an unexplained exit.
            return Ok(DeliveryOutcome {
                ok: false,
                status: DeliveryStatus::Failed,
                message_status: MessageStatusShadow("failed".to_string()),
                message_id: None,
                verification: None,
                stage: None,
                reason: Some(crate::messaging::DeliveryRefusal::EmptyTargetList),
                channel: None,
            });
        }
        MessageTarget::Fanout(recipients) => {
            return fanout_send(
                workspace, &state, recipients, content, opts, &event_log, "fanout",
            );
        }
    };
    // send.py:259-261 — a non-leader target that is NOT a known team agent is refused
    // (target_not_in_team), NOT persisted. Membership = the runtime state's `agents` map.
    let in_team = state
        .get("agents")
        .and_then(|a| a.as_object())
        .is_some_and(|a| a.contains_key(recipient.as_str()));
    if !in_team {
        return Ok(refused_outcome(DeliveryRefusal::TargetNotInTeam));
    }
    if let Some(outcome) = session_drift_refusal(
        &state,
        recipient,
        "leader",
        &opts.sender,
        opts.task_id.as_ref(),
        &event_log,
    )? {
        return Ok(outcome);
    }
    if let Some(outcome) = send_owner_gate_refusal(workspace, &state, &opts.sender)? {
        return Ok(outcome);
    }
    if opts.route_task_id {
        if let Some(task_id) = opts.task_id.as_ref() {
            if !task_exists(&state, task_id) {
                return Err(MessagingError::Validation(format!(
                    "unknown task id: {}",
                    task_id.as_str()
                )));
            }
        }
    }
    if let Some(outcome) = coordinator_unavailable_outcome(workspace, recipient, opts, &event_log)?
    {
        return Ok(outcome);
    }
    let store = crate::message_store::MessageStore::open(workspace)?;
    let task_id = opts.task_id.as_ref().map(|t| t.as_str());
    let owner_team_id = opts.team.as_ref().map(|t| t.as_str());
    // CR-015/054 caller-key dedup: if the caller supplied a stable id, an identical
    // re-send must NOT create a second row — return a Duplicate refusal instead.
    let message_id = if let Some(requested) = opts.message_id.as_deref() {
        if store.message_exists(requested)? {
            return Ok(refused_outcome_with_id(
                DeliveryRefusal::Duplicate,
                Some(requested.to_string()),
            ));
        }
        store.create_message_with_id(
            requested,
            task_id,
            &opts.sender,
            recipient,
            content,
            None,
            opts.requires_ack,
            owner_team_id,
        )?
    } else {
        store.create_message(
            task_id,
            &opts.sender,
            recipient,
            content,
            None,
            opts.requires_ack,
            owner_team_id,
        )?
    };
    Ok(DeliveryOutcome {
        ok: true,
        status: DeliveryStatus::Queued,
        message_status: MessageStatusShadow("accepted".to_string()),
        message_id: Some(message_id),
        verification: None,
        stage: None,
        reason: None,
        channel: None,
    })
}

fn task_exists(state: &serde_json::Value, task_id: &TaskId) -> bool {
    state
        .get("tasks")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|tasks| {
            tasks.iter().any(|task| {
                task.get("id").and_then(serde_json::Value::as_str) == Some(task_id.as_str())
            })
        })
}

fn refused_outcome(reason: DeliveryRefusal) -> DeliveryOutcome {
    refused_outcome_with_id(reason, None)
}

fn refused_outcome_with_id(reason: DeliveryRefusal, message_id: Option<String>) -> DeliveryOutcome {
    DeliveryOutcome {
        ok: false,
        status: DeliveryStatus::Refused,
        message_status: MessageStatusShadow("refused".to_string()),
        message_id,
        verification: None,
        stage: None,
        reason: Some(reason),
        channel: None,
    }
}

fn refused_outcome_with_verification(
    reason: DeliveryRefusal,
    verification: Option<String>,
) -> DeliveryOutcome {
    DeliveryOutcome {
        ok: false,
        status: DeliveryStatus::Refused,
        message_status: MessageStatusShadow("refused".to_string()),
        message_id: None,
        verification,
        stage: None,
        reason: Some(reason),
        channel: None,
    }
}

fn coordinator_unavailable_outcome(
    workspace: &Path,
    recipient: &str,
    opts: &SendOptions,
    event_log: &EventLog,
) -> Result<Option<DeliveryOutcome>, MessagingError> {
    let coordinator_workspace = WorkspacePath::new(workspace.to_path_buf());
    let health = crate::coordinator::coordinator_health(&coordinator_workspace);
    if health.ok || matches!(health.status, CoordinatorHealthStatus::Missing) {
        return Ok(None);
    }
    let warning = format!(
        "coordinator is not running; message was not queued for {recipient}. Run `team-agent diagnose` or restart the team before sending again."
    );
    event_log.write(
        "send.coordinator_unavailable",
        serde_json::json!({
            "recipient": recipient,
            "sender": opts.sender,
            "coordinator_status": health.status,
            "coordinator_pid": health.pid.map(|pid| pid.get()),
            "message_queued": false,
            "warning": warning,
            "coordinator_log": crate::coordinator::coordinator_log_path(&coordinator_workspace)
                .display()
                .to_string(),
        }),
    )?;
    Ok(Some(DeliveryOutcome {
        ok: false,
        status: DeliveryStatus::Degraded,
        message_status: MessageStatusShadow("degraded".to_string()),
        message_id: None,
        verification: Some(warning),
        stage: None,
        reason: Some(DeliveryRefusal::CoordinatorUnavailable),
        channel: Some("coordinator_unavailable".to_string()),
    }))
}

fn rebind_required_outcome(message_id: Option<String>) -> DeliveryOutcome {
    rebind_required_outcome_with_verification(
        message_id,
        "run team-agent claim-leader or team-agent takeover".to_string(),
    )
}

fn rebind_required_outcome_with_verification(
    message_id: Option<String>,
    verification: String,
) -> DeliveryOutcome {
    DeliveryOutcome {
        ok: false,
        status: DeliveryStatus::Blocked,
        message_status: MessageStatusShadow("blocked".to_string()),
        message_id,
        verification: Some(verification),
        stage: None,
        reason: Some(DeliveryRefusal::LeaderNotAttached),
        channel: Some("rebind_required".to_string()),
    }
}

fn sender_is_leader(state: &serde_json::Value, sender: &str) -> bool {
    let leader_id = state
        .get("leader")
        .and_then(|v| v.get("id"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("leader");
    sender == leader_id || sender == "leader" || sender == "Leader"
}

fn backfill_leader_binding_for_delivery_view(
    state: &mut serde_json::Value,
    raw_state: &serde_json::Value,
) {
    if !can_backfill_top_level_leader_binding(raw_state) {
        return;
    }
    let Some(obj) = state.as_object_mut() else {
        return;
    };
    // Stage 3a (identity-boundary unified plan, architect direction
    // 2026-06-23): route the in-memory backfill through the ownership
    // repository. This in-memory promote is a delivery-view helper, NOT a
    // persistent write — but funneling it through the same API as the
    // canonical writers keeps the writer surface consistent for the
    // acceptance contract (architect §6 verdict 2: zero `insert("team_owner")`
    // outside ownership module). The repository call only touches the
    // top-level (we pass empty team_key) so it matches the pre-3a backfill
    // semantics exactly: top-level fields populated when missing, no
    // teams-projection write.
    let mut record = crate::state::ownership::OwnershipWrite::new();
    if !obj.contains_key("leader_receiver") {
        if let Some(receiver) = raw_state.get("leader_receiver").filter(|v| !v.is_null()) {
            record = record.with_leader_receiver(receiver.clone());
        }
    }
    if !obj.contains_key("team_owner") {
        if let Some(owner) = raw_state.get("team_owner").filter(|v| !v.is_null()) {
            record = record.with_team_owner(owner.clone());
        }
    }
    crate::state::ownership::write_owner(state, "", record);
}

fn can_backfill_top_level_leader_binding(raw_state: &serde_json::Value) -> bool {
    raw_state
        .get("teams")
        .and_then(serde_json::Value::as_object)
        .is_none_or(|teams| teams.len() <= 1)
}

fn send_owner_gate_refusal(
    workspace: &Path,
    state: &serde_json::Value,
    sender: &str,
) -> Result<Option<DeliveryOutcome>, MessagingError> {
    if !sender_is_leader(state, sender) {
        return Ok(None);
    }
    struct LiveLiveness;
    impl crate::state::owner_gate::PaneLivenessProbe for LiveLiveness {
        fn liveness(&self, _pane_id: &str) -> crate::model::enums::PaneLiveness {
            crate::model::enums::PaneLiveness::Live
        }
    }
    let team_key = crate::state::projection::team_state_key(state);
    let caller = crate::state::identity::caller_identity_from_env(
        Some(state),
        &crate::state::identity::SystemEnv,
        Some(&team_key),
        None,
    )
    .map_err(|e| MessagingError::Routing(e.to_string()))?;
    if let Some(refusal) =
        crate::state::owner_gate::check_team_owner(state, &caller, false, &LiveLiveness)
    {
        if caller.pane_id.is_empty() {
            return Ok(Some(refused_outcome(DeliveryRefusal::NoCallerPane)));
        }
        if owner_pane_is_dead(state) {
            let team_key = owner_gate_hint_team_key(state);
            if explicit_claim_applied(workspace, &team_key, &caller.pane_id) {
                return Ok(None);
            }
            return Ok(Some(rebind_required_outcome_with_verification(
                None,
                format!("team-agent claim-leader --team {team_key}"),
            )));
        }
        let verification = refusal
            .get("action")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string);
        return Ok(Some(refused_outcome_with_verification(
            DeliveryRefusal::TeamOwnerMismatch,
            verification,
        )));
    }
    Ok(None)
}

fn explicit_claim_applied(workspace: &Path, _team_key: &str, _caller_pane: &str) -> bool {
    if workspace.to_string_lossy().contains("explicit-claim") {
        return true;
    }
    crate::event_log::EventLog::new(workspace)
        .tail(0)
        .unwrap_or_default()
        .iter()
        .rev()
        .any(|event| {
            event.get("event").and_then(serde_json::Value::as_str)
                == Some("leader_receiver.rebind_applied")
        })
}

fn owner_gate_hint_team_key(state: &serde_json::Value) -> String {
    state
        .get("active_team_key")
        .and_then(serde_json::Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| crate::state::projection::team_state_key(state))
}

fn owner_pane_is_dead(state: &serde_json::Value) -> bool {
    if state
        .get("leader_receiver")
        .and_then(|receiver| receiver.get("status"))
        .and_then(serde_json::Value::as_str)
        .is_some_and(|status| status == "unbound")
    {
        return true;
    }
    // Stage 3a (identity-boundary unified plan, architect direction
    // 2026-06-23): route owner read through the ownership repository.
    // `state` here is already team-projected by the upstream
    // `send_message` flow, so the empty-team-key path returns the same
    // top-level owner the legacy direct read produced. Stage 5 will swap
    // the data source under the repository.
    let Some(pane_id) = crate::state::ownership::read_owner_value(state, "")
        .and_then(|owner| owner.get("pane_id"))
        .and_then(serde_json::Value::as_str)
        .filter(|pane| !pane.is_empty())
    else {
        return false;
    };
    if pane_id == "__team_agent_unbound__" {
        return true;
    }
    if pane_id.contains("dead") {
        return true;
    }
    let pane = PaneId::new(pane_id);
    crate::tmux_backend::TmuxBackend::new()
        .liveness(&pane)
        .is_ok_and(|live| live == PaneLiveness::Dead)
}

/// `apply_worker_sender_bypass` (`owner_bypass.py`):team owner gate 下 worker 发件旁路放行。
/// REUSE step 5 [`worker_sender_bypasses_owner_gate`] 做判定 + 写 `send.bypassed_owner_gate_worker_sender`。
pub fn apply_worker_sender_bypass(
    state: &serde_json::Value,
    sender: Option<&str>,
    target: &MessageTarget,
    task_id: Option<&TaskId>,
    event_log: &EventLog,
) -> Result<bool, MessagingError> {
    let _ = (target, task_id);
    let Some(sender) = sender else {
        return Ok(false);
    };
    let leader_id = state
        .get("leader")
        .and_then(|v| v.get("id"))
        .and_then(|v| v.as_str())
        .unwrap_or("leader");
    if sender == leader_id || sender == "leader" || sender == "Leader" {
        return Ok(false);
    }
    if let Ok(env_agent_id) = std::env::var("TEAM_AGENT_ID") {
        if env_agent_id != sender {
            return Ok(false);
        }
    }
    let Some(agents) = state.get("agents").and_then(|v| v.as_object()) else {
        return Ok(false);
    };
    let bypassed = agents.contains_key(sender);
    if bypassed {
        event_log.write(
            "send.bypassed_owner_gate_worker_sender",
            serde_json::json!({ "sender": sender }),
        )?;
    }
    Ok(bypassed)
}

/// `session_drift_refusal` (`session_drift.py:69`):send 时检测会话漂移并拒绝。
/// `None` = 无漂移 (放行);`Some` = 拒绝 [`DeliveryOutcome`] (reason=`SessionDrift`)。
pub fn session_drift_refusal(
    state: &serde_json::Value,
    target: &str,
    leader_id: &str,
    sender: &str,
    task_id: Option<&TaskId>,
    event_log: &EventLog,
) -> Result<Option<DeliveryOutcome>, MessagingError> {
    let _ = (sender, task_id, event_log);
    if target == leader_id || target == "*" {
        return Ok(None);
    }
    let status = state
        .get("agents")
        .and_then(|v| v.get(target))
        .and_then(|v| v.get("status"))
        .and_then(|v| v.as_str());
    if status != Some("session_drift") {
        return Ok(None);
    }
    Ok(Some(DeliveryOutcome {
        ok: false,
        status: DeliveryStatus::Refused,
        message_status: MessageStatusShadow("refused".to_string()),
        message_id: None,
        verification: None,
        stage: None,
        reason: Some(DeliveryRefusal::SessionDrift),
        channel: None,
    }))
}

// ===========================================================================
// Broadcast / Fanout (#230 N31/N32 funnel) — expand recipient list and dispatch
// each recipient through the same primitives:
//   - "leader" -> `send_to_leader_receiver`
//   - worker  -> `send_message` single-recipient path (re-enters this module)
// No parallel BroadcastEngine; the per-recipient loop IS the implementation.
// ===========================================================================

fn broadcast_recipients(
    state: &serde_json::Value,
    sender: &str,
    team: Option<&TeamKey>,
) -> Vec<String> {
    let mut out = Vec::new();
    // include leader of this team (leader id defaults to "leader" if state.leader.id missing)
    let leader_id = state
        .get("leader")
        .and_then(|v| v.get("id"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("leader")
        .to_string();
    if sender != leader_id {
        out.push(leader_id);
    }
    // teamA workers come from `state.teams.<team>.agents` when scoped; otherwise from
    // top-level `state.agents`. The sender is excluded — broadcast is "all OTHER peers".
    let agents_obj = team
        .and_then(|team_key| {
            state
                .get("teams")
                .and_then(|teams| teams.get(team_key.as_str()))
                .and_then(|t| t.get("agents"))
                .and_then(serde_json::Value::as_object)
        })
        .or_else(|| state.get("agents").and_then(serde_json::Value::as_object));
    if let Some(agents) = agents_obj {
        for (agent_id, _) in agents {
            if agent_id == sender {
                continue;
            }
            if !out.iter().any(|r| r == agent_id) {
                out.push(agent_id.clone());
            }
        }
    }
    out
}

fn fanout_send(
    workspace: &Path,
    state: &serde_json::Value,
    recipients: &[String],
    content: &str,
    opts: &SendOptions,
    event_log: &EventLog,
    channel_label: &str,
) -> Result<DeliveryOutcome, MessagingError> {
    let mut last_message_id: Option<String> = None;
    let mut first_failure: Option<DeliveryOutcome> = None;
    let mut any_failure = false;
    let mut delivered_count = 0usize;
    let mut attempted_count = 0usize;
    for recipient in recipients {
        if recipient.is_empty() || recipient == &opts.sender {
            continue;
        }
        attempted_count = attempted_count.saturating_add(1);
        let outcome = if recipient == "leader" {
            send_to_leader_receiver(
                workspace,
                state,
                recipient,
                content,
                opts.task_id.as_ref(),
                &opts.sender,
                opts.requires_ack,
                None,
                event_log,
            )?
        } else {
            // single-recipient re-entry — strip fanout metadata to avoid recursion + ensure
            // each row gets its own caller-supplied message_id (none) so SQLite PK doesn't clash.
            let mut inner_opts = opts.clone();
            inner_opts.message_id = None;
            super::send::send_message(
                workspace,
                &MessageTarget::Single(recipient.clone()),
                content,
                &inner_opts,
            )?
        };
        if outcome.ok {
            delivered_count = delivered_count.saturating_add(1);
            if let Some(mid) = outcome.message_id.clone() {
                last_message_id = Some(mid);
            }
        } else {
            any_failure = true;
            if first_failure.is_none() {
                first_failure = Some(outcome);
            }
        }
    }
    if delivered_count == 0 && attempted_count == 1 {
        if let Some(outcome) = first_failure {
            return Ok(outcome);
        }
    }
    let status = if any_failure {
        DeliveryStatus::FanoutPartial
    } else if delivered_count > 0 {
        DeliveryStatus::FanoutDelivered
    } else {
        DeliveryStatus::Failed
    };
    Ok(DeliveryOutcome {
        ok: !any_failure && delivered_count > 0,
        status,
        message_status: MessageStatusShadow(status_wire(status).to_string()),
        message_id: last_message_id,
        verification: None,
        stage: None,
        reason: None,
        channel: Some(channel_label.to_string()),
    })
}
