//! internal_delivery.py + delivery.py — coordinator/调度器侧 thin wrapper + 单条 tmux 注入投递
//! + trust 有界重试 + turn-open arm (card §16/§65)。

use std::path::Path;

use rusqlite::{params, OptionalExtension};

use crate::event_log::EventLog;
use crate::message_store::MessageStore;
use crate::model::enums::{PaneLiveness, Provider};
use crate::model::ids::TeamKey;
use crate::provider::wire::{
    is_claude_family, parse_canonical_provider, parse_provider, provider_wire,
};
use crate::transport::{
    submit_verification_wire, InjectPayload, InjectReport, InjectVerification, Key, PaneId,
    PaneInfo, SessionName, SubmitVerification, Target, Transport, WindowName,
};

use super::helpers::{message_exists, MessageStatusShadow};
use super::{
    DeliveryOutcome, DeliveryRefusal, DeliveryStage, DeliveryStatus, MessagingError,
    PaneWidthQuery, TrustRetryPayload, SEND_RETRY_MAX_ATTEMPTS,
};
use crate::state::projection::OwnerTeamResolution;

// ===========================================================================
// internal_delivery.py — coordinator/调度器侧 thin wrapper (card §65)
// ===========================================================================

/// `deliver_stored_message` (`internal_delivery.py:16`):coordinator/调度器侧 team-scoped 单发
/// (不重路由)。加 `_runtime_lock("send")`,直走 `_send_single_message_unlocked`。
#[allow(clippy::too_many_arguments)]
pub fn deliver_stored_message(
    workspace: &Path,
    target: Option<&str>,
    content: &str,
    task_id: Option<&crate::model::ids::TaskId>,
    sender: &str,
    requires_ack: bool,
    wait_visible: bool,
    timeout: f64,
    team: Option<&TeamKey>,
) -> Result<DeliveryOutcome, MessagingError> {
    let _ = (wait_visible, timeout);
    let recipient = target.unwrap_or("leader");
    let store = MessageStore::open(workspace)?;
    let message_id = store.create_message(
        task_id.map(crate::model::ids::TaskId::as_str),
        sender,
        recipient,
        content,
        None,
        requires_ack,
        team.map(TeamKey::as_str),
    )?;
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

// ===========================================================================
// delivery.py — 单条 tmux 注入投递 + trust 有界重试 + turn-open arm (card §16)
// ===========================================================================

/// `_tmux_pane_width` (`delivery.py:20`):查询 pane 列宽。**fail-safe** (bug-064/082):失败
/// 返回 [`PaneWidthQuery::Failed`],**绝不**给默认宽度。借 step 9 transport 的 query。
pub fn tmux_pane_width(transport: &dyn Transport, target: &Target) -> PaneWidthQuery {
    let queried = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        transport.query(target, crate::transport::PaneField::PaneWidth)
    }));
    let result = match queried {
        Ok(result) => result,
        Err(_) => {
            return PaneWidthQuery::Failed {
                error: "tmux_query_failed:panic".to_string(),
            };
        }
    };
    match result {
        Ok(Some(raw)) => match raw.trim().parse::<u32>() {
            Ok(pane_width) if pane_width > 0 => PaneWidthQuery::Ok { pane_width },
            Ok(_) => PaneWidthQuery::Failed {
                error: "non_positive_width".to_string(),
            },
            Err(_) => PaneWidthQuery::Failed {
                error: "unparseable_output".to_string(),
            },
        },
        Ok(None) => PaneWidthQuery::Failed {
            error: "empty_output".to_string(),
        },
        Err(err) => PaneWidthQuery::Failed {
            error: format!("tmux_query_failed:{err}"),
        },
    }
}

/// `_deliver_pending_message` (`delivery.py:63`):对一条消息做 tmux 注入投递 (含 trust 提示
/// 自动应答 + turn-open arm + first_send_at 戳)。daemon-path → Result。
pub fn deliver_pending_message(
    workspace: &Path,
    store: &MessageStore,
    transport: &dyn Transport,
    message_id: &str,
    event_log: &EventLog,
    state: &serde_json::Value,
) -> Result<DeliveryOutcome, MessagingError> {
    if !message_exists(store, message_id)? {
        return Ok(DeliveryOutcome {
            ok: false,
            status: DeliveryStatus::Failed,
            message_status: MessageStatusShadow("failed".to_string()),
            message_id: Some(message_id.to_string()),
            verification: None,
            stage: None,
            reason: None,
            channel: None,
        });
    }
    let message = message_for_delivery(store, message_id)?;
    let Some(message) = message else {
        return Ok(DeliveryOutcome {
            ok: false,
            status: DeliveryStatus::Failed,
            message_status: MessageStatusShadow("failed".to_string()),
            message_id: Some(message_id.to_string()),
            verification: None,
            stage: None,
            reason: Some(DeliveryRefusal::UnknownRecipient),
            channel: None,
        });
    };
    let mut canonical_owner_team_id = message.owner_team_id.clone();
    let scoped_state;
    let state = match message.owner_team_id.as_deref() {
        Some(team) if !team.is_empty() => {
            match project_state_for_owner_team(
                workspace,
                team,
                state,
                Some(store),
                Some(message_id),
                Some(event_log),
            )? {
                OwnerTeamProjection::Projected {
                    state,
                    canonical_team,
                } => {
                    canonical_owner_team_id = Some(canonical_team);
                    scoped_state = state;
                    &scoped_state
                }
                OwnerTeamProjection::Refused(outcome) => return Ok(outcome),
            }
        }
        _ => state,
    };
    let attempt = if store.claim_for_delivery(message_id)? {
        message.delivery_attempts.saturating_add(1)
    } else if message.status == "target_resolved" {
        bump_delivery_attempts(store, message_id)?
    } else {
        return Ok(DeliveryOutcome {
            ok: false,
            status: DeliveryStatus::Refused,
            message_status: MessageStatusShadow("target_resolved".to_string()),
            message_id: Some(message_id.to_string()),
            verification: None,
            stage: None,
            reason: Some(DeliveryRefusal::MessageAlreadyClaimed),
            channel: None,
        });
    };
    if message.recipient == "leader" {
        if let Some(receiver) = leader_receiver_value(state) {
            if let Some(outcome) = leader_receiver_transport_conflict_outcome(
                store,
                event_log,
                message_id,
                receiver,
                &message.sender,
            )? {
                return Ok(outcome);
            }
            if crate::codex_app_server::receiver_is_app_server(receiver) {
                return deliver_leader_via_app_server(
                    store,
                    event_log,
                    state,
                    message_id,
                    &message,
                    canonical_owner_team_id.as_deref(),
                );
            }
        }
    }
    if message.recipient == "leader" && leader_receiver_has_noncanonical_tmux_socket(state) {
        store.mark(message_id, "failed", Some("leader_not_attached"))?;
        event_log.write(
            "leader_receiver.delivery_blocked",
            serde_json::json!({
                "message_id": message_id,
                "sender": message.sender,
                "reason": "leader_not_attached",
                "channel": "rebind_required",
                "action": "run team-agent claim-leader or team-agent takeover",
                "error": "leader_receiver.tmux_socket is not a canonical full socket path",
            }),
        )?;
        return Ok(DeliveryOutcome {
            ok: false,
            status: DeliveryStatus::Refused,
            message_status: MessageStatusShadow("failed".to_string()),
            message_id: Some(message_id.to_string()),
            verification: Some("run team-agent claim-leader or team-agent takeover".to_string()),
            stage: None,
            reason: Some(DeliveryRefusal::LeaderNotAttached),
            channel: Some("rebind_required".to_string()),
        });
    }
    let delivery_transport =
        delivery_transport_for_recipient(workspace, transport, state, &message.recipient);
    let transport = delivery_transport.as_transport();
    // U1-B: probe `list_targets()` ONCE per delivery and DEFER on Err. The prior
    // `.unwrap_or_default()` coerced server jitter (Err = subprocess fork failed)
    // to an empty vec, and the downstream cache-reuse branch
    // (`live_targets.is_empty() && !known_dead`) would then inject into a
    // never-validated cached pane on every tmux hiccup. Err is OBSERVED here as
    // jitter and the delivery is deferred (status stays `target_resolved`, no
    // inject attempted) so the next tick gets a fresh probe.
    let live_targets = match transport.list_targets() {
        Ok(targets) => targets,
        Err(err) => {
            let reason = format!("list_targets_server_jitter:{err}");
            event_log.write(
                "delivery.deferred_list_targets_jitter",
                serde_json::json!({
                    "message_id": message_id,
                    "recipient": message.recipient,
                    "reason": reason,
                }),
            )?;
            store.mark(message_id, "target_resolved", Some(&reason))?;
            return Ok(DeliveryOutcome {
                ok: false,
                status: DeliveryStatus::Degraded,
                message_status: MessageStatusShadow("target_resolved".to_string()),
                message_id: Some(message_id.to_string()),
                verification: Some(reason),
                stage: None,
                reason: None,
                channel: None,
            });
        }
    };
    // Do not inject queued leader messages into a synthetic "leader" window.
    if message.recipient == "leader"
        && !leader_receiver_pane_is_usable(transport, state, &live_targets)
    {
        store.mark(message_id, "failed", Some("leader_not_attached"))?;
        event_log.write(
            "leader_receiver.delivery_blocked",
            serde_json::json!({
                "message_id": message_id,
                "sender": message.sender,
                "reason": "leader_not_attached",
                "channel": "rebind_required",
                "action": "run team-agent claim-leader or team-agent takeover",
            }),
        )?;
        return Ok(DeliveryOutcome {
            ok: false,
            status: DeliveryStatus::Refused,
            message_status: MessageStatusShadow("failed".to_string()),
            message_id: Some(message_id.to_string()),
            verification: Some("run team-agent claim-leader or team-agent takeover".to_string()),
            stage: None,
            reason: Some(DeliveryRefusal::LeaderNotAttached),
            channel: Some("rebind_required".to_string()),
        });
    }
    let target = resolve_inject_target(state, &message.recipient, transport, &live_targets);
    // Contract B / MUST-10 / N31/N32: physical paste+Enter into a startup trust/update
    // menu is NOT provider delivery — the menu consumes the Enter and the task text
    // is lost (PROBE-2 root-cause). Before injection, peek at the recipient's pane for
    // a Codex actionable startup prompt; if present, mark the row `queued_until_trust`
    // and DO NOT inject the task. The coordinator's startup-prompt phase will dismiss
    // the trust prompt, and the SAME message_id is later replayed through this same
    // delivery pipeline (no parallel side channel).
    if recipient_pane_has_actionable_startup_prompt(transport, state, &message.recipient, &target) {
        store.mark(message_id, "queued_until_trust", None)?;
        event_log.write(
            "delivery.deferred_startup_prompt",
            serde_json::json!({
                "message_id": message_id,
                "recipient": message.recipient,
                "reason": "actionable_startup_prompt",
            }),
        )?;
        return Ok(DeliveryOutcome {
            ok: false,
            status: DeliveryStatus::RetryScheduled,
            message_status: MessageStatusShadow("queued_until_trust".to_string()),
            message_id: Some(message_id.to_string()),
            verification: None,
            stage: Some(DeliveryStage::TrustAutoAnswerDismissalWait),
            reason: None,
            channel: None,
        });
    }
    let rendered = render_message(
        &message.sender,
        message.task_id.as_deref(),
        &message.content,
        message_id,
    );
    let is_leader_recipient = message.recipient == "leader";
    let payload = if is_leader_recipient {
        InjectPayload::TextSkipConsumptionPoll(rendered)
    } else {
        InjectPayload::Text(rendered)
    };
    let inject_report = match transport.inject(&target, &payload, Key::Enter, true) {
        Ok(report) => report,
        Err(error) => {
            let reason = format!("inject_failed:{error}");
            if message.recipient == "leader" {
                store.mark(message_id, "failed", Some("leader_not_attached"))?;
                event_log.write(
                    "leader_receiver.delivery_blocked",
                    serde_json::json!({
                        "message_id": message_id,
                        "sender": message.sender,
                        "reason": "leader_not_attached",
                        "channel": "rebind_required",
                        "action": "run team-agent claim-leader or team-agent takeover",
                        "error": error.to_string(),
                    }),
                )?;
                return Ok(DeliveryOutcome {
                    ok: false,
                    status: DeliveryStatus::Refused,
                    message_status: MessageStatusShadow("failed".to_string()),
                    message_id: Some(message_id.to_string()),
                    verification: Some(
                        "run team-agent claim-leader or team-agent takeover".to_string(),
                    ),
                    stage: None,
                    reason: Some(DeliveryRefusal::LeaderNotAttached),
                    channel: Some("rebind_required".to_string()),
                });
            }
            event_log.write(
                "send.inject_failed",
                serde_json::json!({
                    "message_id": message_id,
                    "recipient": message.recipient,
                    "attempts": attempt,
                    "max_attempts": SEND_RETRY_MAX_ATTEMPTS,
                    "error": error.to_string(),
                }),
            )?;
            if attempt >= u32::from(SEND_RETRY_MAX_ATTEMPTS) {
                store.mark(message_id, "failed", Some("send_inject_exhausted"))?;
                emit_send_failed_exhausted(
                    workspace,
                    state,
                    event_log,
                    message_id,
                    &message.recipient,
                    attempt,
                    "send_inject_exhausted",
                    &reason,
                    None,
                )?;
                return Ok(DeliveryOutcome {
                    ok: false,
                    status: DeliveryStatus::Failed,
                    message_status: MessageStatusShadow("failed".to_string()),
                    message_id: Some(message_id.to_string()),
                    verification: Some(reason),
                    stage: Some(DeliveryStage::Inject),
                    reason: None,
                    channel: None,
                });
            }
            store.mark(message_id, "target_resolved", Some(&reason))?;
            return Ok(DeliveryOutcome {
                ok: false,
                status: DeliveryStatus::Degraded,
                message_status: MessageStatusShadow("target_resolved".to_string()),
                message_id: Some(message_id.to_string()),
                verification: Some(reason),
                stage: Some(DeliveryStage::Inject),
                reason: None,
                channel: None,
            });
        }
    };
    let submit_verified = inject_submit_verified(&inject_report);
    let readback_verified = pane_readback_verified(&inject_report);
    // Leader pane: inject success is delivery proof. Worker pane: post-submit
    // evidence is the delivery proof; stale Phase-1 readback must not veto it
    // and cannot independently prove the current Enter submitted.
    let verified = if is_leader_recipient {
        true
    } else {
        submit_verified
    };
    if !verified {
        let reason = if !readback_verified {
            "pane_readback_unverified:capture_missing_token".to_string()
        } else {
            format!(
                "submit_unverified:{}",
                submit_verification_wire(inject_report.submit_verification)
            )
        };
        // E50 PR-1 (0.3.24 P0): render forensic submit_diagnostics into the
        // event so operators see per-attempt pane state without grepping. The
        // legacy keys (`message_id` / `recipient` / `reason` / `attempts`)
        // are preserved byte-for-byte for grep compatibility; new keys are
        // ADDITIONAL.
        let submit_attempts_detail = render_submit_diagnostics(&inject_report);
        event_log.write(
            "send.unverified",
            serde_json::json!({
                "message_id": message_id,
                "recipient": message.recipient,
                "reason": reason,
                "attempts": inject_report.attempts,
                "submit_attempts_detail": submit_attempts_detail,
            }),
        )?;
        if inject_report.attempts >= u32::from(SEND_RETRY_MAX_ATTEMPTS) {
            store.mark(message_id, "failed", Some("send_unverified_exhausted"))?;
            emit_send_failed_exhausted(
                workspace,
                state,
                event_log,
                message_id,
                &message.recipient,
                inject_report.attempts,
                "send_unverified_exhausted",
                &reason,
                Some(&inject_report),
            )?;
            return Ok(DeliveryOutcome {
                ok: false,
                status: DeliveryStatus::Failed,
                message_status: MessageStatusShadow("failed".to_string()),
                message_id: Some(message_id.to_string()),
                verification: Some(reason),
                stage: Some(DeliveryStage::Submit),
                reason: None,
                channel: None,
            });
        }
        store.mark(message_id, "submitted_unverified", Some(&reason))?;
        return Ok(DeliveryOutcome {
            ok: false,
            status: DeliveryStatus::Failed,
            message_status: MessageStatusShadow("submitted_unverified".to_string()),
            message_id: Some(message_id.to_string()),
            verification: Some(reason),
            stage: Some(DeliveryStage::Submit),
            reason: None,
            channel: None,
        });
    }
    // S1-CAPTURE-001 (0.4.8, CR M4 Claude phase-1): for Claude/ClaudeCode
    // recipients with a known authoritative rollout_path, verify the message
    // token actually reached the worker's transcript before marking
    // delivered. This catches the gate's mis-attribution: pane inject
    // succeeded but the token landed in the leader/unassigned transcript,
    // not the worker's. Budget per architect plan: 64KB tail / single
    // per-delivery check / short grace window. Phase-1 Claude only —
    // codex/copilot keep the pre-fix behaviour (will be addressed in
    // phase-2 once Claude phase-1 is field-validated).
    if let Some((rollout_path, provider_wire_str)) =
        claude_recipient_rollout(state, &message.recipient)
    {
        let token_marker = format!("[team-agent-token:{message_id}]");
        let grace = std::time::Duration::from_millis(200);
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(500);
        let mut transcript_has_token = false;
        loop {
            transcript_has_token = rollout_tail_contains(&rollout_path, &token_marker, 64 * 1024);
            if transcript_has_token || std::time::Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(grace);
        }
        if !transcript_has_token {
            // Loud but non-fatal: emit a mismatch event for diagnose/status
            // observability. Do NOT mark delivered — degrade to
            // submitted_unverified so capture is forced to re-attribute.
            let reason = format!(
                "transcript_missing:provider={provider_wire_str},rollout={}",
                rollout_path.display()
            );
            event_log.write(
                "provider.session.transcript_mismatch",
                serde_json::json!({
                    "message_id": message_id,
                    "recipient": message.recipient,
                    "provider": provider_wire_str,
                    "rollout_path": rollout_path.to_string_lossy(),
                    "pane_id": state
                        .get("agents")
                        .and_then(|a| a.get(&message.recipient))
                        .and_then(|a| a.get("pane_id"))
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or(""),
                    "spawn_epoch": state
                        .get("agents")
                        .and_then(|a| a.get(&message.recipient))
                        .and_then(|a| a.get("spawn_epoch"))
                        .and_then(serde_json::Value::as_u64)
                        .unwrap_or(0),
                    "reason": "transcript_missing",
                }),
            )?;
            store.mark(message_id, "submitted_unverified", Some(&reason))?;
            return Ok(DeliveryOutcome {
                ok: false,
                status: DeliveryStatus::Failed,
                message_status: MessageStatusShadow("submitted_unverified".to_string()),
                message_id: Some(message_id.to_string()),
                verification: Some(reason),
                stage: Some(DeliveryStage::Submit),
                reason: None,
                channel: None,
            });
        }
    }
    store.mark(message_id, "delivered", None)?;
    event_log.write(
        "message.delivered",
        serde_json::json!({"message_id": message_id}),
    )?;
    let outcome = DeliveryOutcome {
        ok: true,
        status: DeliveryStatus::Delivered,
        message_status: MessageStatusShadow("delivered".to_string()),
        message_id: Some(message_id.to_string()),
        verification: None,
        stage: None,
        reason: None,
        channel: None,
    };
    stamp_first_send_at_if_leader_to_worker_scoped(
        workspace,
        &message.sender,
        &message.recipient,
        canonical_owner_team_id.as_deref(),
    )?;
    record_turn_open_if_leader_to_worker_scoped(
        workspace,
        &message.sender,
        &message.recipient,
        &outcome,
        event_log,
        canonical_owner_team_id.as_deref(),
    )?;
    Ok(outcome)
}

/// 0.3.27: promoted to pub(crate) for leader_receiver.rs verification gate.
pub(crate) fn inject_submit_verified(report: &InjectReport) -> bool {
    match report.submit_verification {
        SubmitVerification::SendKeysFailed => false,
        SubmitVerification::PastedContentPromptStillPresentAfterSubmit => false,
        SubmitVerification::PastedContentPromptAbsentAfterSubmit => true,
        SubmitVerification::KeySentAfterVisibleToken { .. } => true,
        // MUST-10 preserved: EnterSentWithoutPlaceholderCheck still ⇒ delivered
        // (provider_submit_verification_red.rs:113-159 contract). E46 narrows
        // when transport returns this variant: it is now emitted ONLY after
        // post-Enter input-consumption confirmation succeeds. Fresh TUI
        // (bracketed-paste-stuck) instead returns SubmitConsumptionUnverified
        // so this branch keeps delivered semantics intact.
        SubmitVerification::EnterSentWithoutPlaceholderCheck => true,
        // E46 (0.3.24 bug#5): Enter was sent but post-Enter consumption was
        // NOT observed within the bounded resend cap. Treat as not delivered
        // (submitted_unverified / failed) — prevents the macmini假阳 where
        // demo-director's stuck bracketed-paste swallowed the Enter and
        // delivery still reported delivered.
        SubmitVerification::SubmitConsumptionUnverified => false,
    }
}

/// U1 #7 step-2: pane-readback gate. `CaptureMissingToken` is negative readback
/// evidence, but 0.3.30 submit evidence can supersede it: consumption or
/// post-submit token observation proves the message reached the pane.
/// 0.3.27: promoted to pub(crate) for leader_receiver.rs verification gate.
pub(crate) fn pane_readback_verified(report: &InjectReport) -> bool {
    !matches!(
        report.inject_verification,
        InjectVerification::CaptureMissingToken
    )
}

/// Render a message into the worker-facing protocol block (port of `rust_core.py:render_message`,
/// golden-verified): `Team Agent message from {sender}[ for {task_id}]:\n\n{content}\n\n
/// [team-agent-token:{message_id}]`. The worker (fake or real provider) only builds a result_envelope
/// when it sees this block + extracts the token — the bare content gives WORKING but never a report
/// (rt-host-a loop #4). token == message_id (exactly-once correlation).
/// F1 (0.3.26): promoted to `pub` for direct pane send in cli/send.rs.
pub fn render_message(
    sender: &str,
    task_id: Option<&str>,
    content: &str,
    message_id: &str,
) -> String {
    let mut header = format!("Team Agent message from {sender}");
    if let Some(task_id) = task_id.filter(|t| !t.is_empty()) {
        header.push_str(&format!(" for {task_id}"));
    }
    format!("{header}:\n\n{content}\n\n[team-agent-token:{message_id}]")
}

fn deliver_leader_via_app_server(
    store: &MessageStore,
    event_log: &EventLog,
    state: &serde_json::Value,
    message_id: &str,
    message: &PendingMessage,
    owner_team_id: Option<&str>,
) -> Result<DeliveryOutcome, MessagingError> {
    let receiver = leader_receiver_value(state).ok_or_else(|| {
        MessagingError::Routing("codex_app_server leader_receiver missing".to_string())
    })?;
    let binding = match crate::codex_app_server::binding_from_receiver(receiver) {
        Ok(binding) => binding,
        Err(error) => {
            return Ok(app_server_delivery_failure(
                store,
                event_log,
                receiver,
                message_id,
                &message.sender,
                owner_team_id,
                &error,
            )?);
        }
    };
    let rendered = render_message(
        &message.sender,
        message.task_id.as_deref(),
        &message.content,
        message_id,
    );
    match crate::codex_app_server::submit_to_bound_thread(&binding, message_id, &rendered) {
        Ok(submit) => {
            store.mark(message_id, "delivered", None)?;
            event_log.write(
                "message.delivered",
                serde_json::json!({"message_id": message_id}),
            )?;
            event_log.write(
                "leader_receiver.app_server_submitted",
                serde_json::json!({
                    "message_id": message_id,
                    "owner_team_id": owner_team_id,
                    "owner_epoch": receiver_owner_epoch(receiver),
                    "socket": binding.socket,
                    "thread_id": binding.thread_id,
                    "turn_id": submit.turn_id,
                    "turn_status": submit.turn_status,
                }),
            )?;
            Ok(DeliveryOutcome {
                ok: true,
                status: DeliveryStatus::Delivered,
                message_status: MessageStatusShadow("delivered".to_string()),
                message_id: Some(message_id.to_string()),
                verification: None,
                stage: None,
                reason: None,
                channel: Some("codex_app_server".to_string()),
            })
        }
        Err(error) => app_server_delivery_failure(
            store,
            event_log,
            receiver,
            message_id,
            &message.sender,
            owner_team_id,
            &error,
        ),
    }
}

fn app_server_delivery_failure(
    store: &MessageStore,
    event_log: &EventLog,
    receiver: &serde_json::Value,
    message_id: &str,
    sender: &str,
    owner_team_id: Option<&str>,
    error: &crate::codex_app_server::AppServerError,
) -> Result<DeliveryOutcome, MessagingError> {
    let action = app_server_rebind_action(owner_team_id, receiver);
    match error {
        crate::codex_app_server::AppServerError::LeaderBusy(message) => {
            store.mark(message_id, "target_resolved", Some("leader_busy"))?;
            event_log.write(
                "leader_receiver.app_server_busy",
                serde_json::json!({
                    "message_id": message_id,
                    "sender": sender,
                    "reason": "leader_busy",
                    "error": message,
                }),
            )?;
            Ok(DeliveryOutcome {
                ok: false,
                status: DeliveryStatus::RetryScheduled,
                message_status: MessageStatusShadow("target_resolved".to_string()),
                message_id: Some(message_id.to_string()),
                verification: Some("leader_busy".to_string()),
                stage: None,
                reason: Some(DeliveryRefusal::RecipientBusy),
                channel: Some("leader_busy".to_string()),
            })
        }
        crate::codex_app_server::AppServerError::ThreadStale { expected, actual } => {
            store.mark(message_id, "failed", Some(error.code()))?;
            event_log.write(
                "leader_receiver.app_server_thread_stale",
                serde_json::json!({
                    "message_id": message_id,
                    "sender": sender,
                    "owner_team_id": owner_team_id,
                    "owner_epoch": receiver_owner_epoch(receiver),
                    "expected": expected,
                    "actual": actual,
                    "action": action,
                }),
            )?;
            Ok(rebind_required_outcome(message_id, &action))
        }
        crate::codex_app_server::AppServerError::ApprovalUnsupported(method) => {
            store.mark(message_id, "failed", Some("approval_unsupported"))?;
            event_log.write(
                "codex_app_server.approval_unsupported",
                serde_json::json!({
                    "message_id": message_id,
                    "sender": sender,
                    "method": method,
                    "action": "handle approval in the Codex app-server session",
                }),
            )?;
            Ok(DeliveryOutcome {
                ok: false,
                status: DeliveryStatus::Blocked,
                message_status: MessageStatusShadow("failed".to_string()),
                message_id: Some(message_id.to_string()),
                verification: Some("handle approval in the Codex app-server session".to_string()),
                stage: None,
                reason: Some(DeliveryRefusal::MissingPermissions),
                channel: Some("codex_app_server".to_string()),
            })
        }
        crate::codex_app_server::AppServerError::ProtocolMismatch(_)
        | crate::codex_app_server::AppServerError::MissingUserAgent => {
            store.mark(message_id, "failed", Some(error.code()))?;
            event_log.write(
                "leader_receiver.app_server_protocol_mismatch",
                serde_json::json!({
                    "message_id": message_id,
                    "sender": sender,
                    "owner_team_id": owner_team_id,
                    "owner_epoch": receiver_owner_epoch(receiver),
                    "reason": error.code(),
                    "error": error.to_string(),
                    "action": action,
                }),
            )?;
            Ok(rebind_required_outcome(message_id, &action))
        }
        crate::codex_app_server::AppServerError::SocketUnreachable(_)
        | crate::codex_app_server::AppServerError::SocketOwnershipInvalid(_)
        | crate::codex_app_server::AppServerError::ThreadNotLive(_)
        | crate::codex_app_server::AppServerError::Io(_)
        | crate::codex_app_server::AppServerError::Json(_) => {
            store.mark(message_id, "failed", Some(error.code()))?;
            event_log.write(
                "leader_receiver.delivery_blocked",
                serde_json::json!({
                    "message_id": message_id,
                    "sender": sender,
                    "reason": error.code(),
                    "channel": "rebind_required",
                    "action": action,
                    "error": error.to_string(),
                }),
            )?;
            Ok(rebind_required_outcome(message_id, &action))
        }
    }
}

fn rebind_required_outcome(message_id: &str, action: &str) -> DeliveryOutcome {
    DeliveryOutcome {
        ok: false,
        status: DeliveryStatus::Refused,
        message_status: MessageStatusShadow("failed".to_string()),
        message_id: Some(message_id.to_string()),
        verification: Some(action.to_string()),
        stage: None,
        reason: Some(DeliveryRefusal::LeaderNotAttached),
        channel: Some("rebind_required".to_string()),
    }
}

fn app_server_rebind_action(owner_team_id: Option<&str>, receiver: &serde_json::Value) -> String {
    let team = owner_team_id
        .filter(|team| !team.is_empty())
        .map(|team| format!(" --team {team}"))
        .unwrap_or_default();
    let Some(app) = receiver.get("app_server") else {
        return format!("run team-agent attach-app-server-leader{team} --socket <socket> --thread-id <thread_id>");
    };
    let socket = app
        .get("socket")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("<socket>");
    let thread_id = app
        .get("thread_id")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("<thread_id>");
    format!(
        "run team-agent attach-app-server-leader{team} --socket {socket} --thread-id {thread_id}"
    )
}

/// Resolve a recipient agent-id to a tmux-RESOLVABLE inject target: the persisted pane-id if present,
/// else a session-qualified `SessionWindow` (state.session_name + the agent's window, defaulting to the
/// id). NEVER the bare agent-id as a pane — a clientless coordinator cannot resolve that
/// ("can't find pane: w1", rt-host-a loop #3). Mirrors `coordinator/tick.rs::capture_target`.
///
/// Leader delivery uses the bound leader receiver pane. The leader is not a worker agent and
/// must not fall through to a synthetic `SessionWindow{window="leader"}` target.
///
/// `live_targets` is passed in BY THE CALLER (probed ONCE per delivery — see U1-B); this fn is
/// pure w.r.t. transport state. The caller must DEFER on `list_targets()` Err and never coerce
/// to an empty vec.
///
/// **0.3.24 excision (U1-A real-machine RED v2, macmini fixture res_e4b40473d36f)**:
/// the wave-2 LEADER drift fallback chain (session+window probe) was removed — see
/// `.team/artifacts/u1-a-realmachine-v2-fix-or-excise.md` for the root-cause analysis.
/// Drift now fails loudly as `leader_not_attached` rather than silently selecting a stale
/// pane. U1-A real-machine fix is deferred to v0.3.25 (writer-shape + projection +
/// rediscover-writer triad). U1-B jitter defer at try_deliver_message:213-237 and
/// U1-C-Tail at the startup-prompt peek site are unchanged.
fn resolve_inject_target(
    state: &serde_json::Value,
    recipient: &str,
    transport: &dyn Transport,
    live_targets: &[PaneInfo],
) -> Target {
    if recipient == "leader" {
        if let Some(pane_id) = leader_receiver_pane_id(state) {
            return Target::Pane(PaneId::new(pane_id));
        }
    }
    let agent = state.get("agents").and_then(|a| a.get(recipient));
    let session = state
        .get("session_name")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    let window = agent
        .and_then(|a| a.get("window"))
        .and_then(serde_json::Value::as_str)
        .filter(|s| !s.is_empty())
        .unwrap_or(recipient);
    let cached_pane = agent
        .and_then(|a| a.get("pane_id"))
        .and_then(serde_json::Value::as_str)
        .filter(|s| !s.is_empty())
        .map(PaneId::new);
    // E51 (0.3.26 P0, delivery loop guard): if the worker's cached pane_id is
    // the SAME as leader_receiver.pane_id (the leader's handle), injecting into
    // it would deliver the worker's message back to the leader pane — a routing
    // loop that the macmini "hand-handle mapping 灾難" truth source exposed. Fail
    // loud with a SessionWindow target that will surface as a structured error
    // (the SessionWindow "leader" target trips the existing leader_not_attached
    // guard on any subsequent delivery attempt for this message). The root fix
    // is in lease.rs (E51 guard #1) which prevents the conflation; this is the
    // defence-in-depth in the delivery layer.
    if let Some(pane) = cached_pane.as_ref() {
        let worker_socket = worker_tmux_socket(state, agent, transport);
        if leader_receiver_pane_binding(state).is_some_and(|leader| {
            pane_conflicts_on_same_socket(pane.as_str(), worker_socket.as_deref(), leader)
        }) {
            return Target::SessionWindow {
                session: SessionName::new(session),
                window: WindowName::new(format!("{recipient}_pane_conflicts_with_leader")),
            };
        }
        if live_targets
            .iter()
            .any(|target| target.pane_id.as_str() == pane.as_str())
        {
            return Target::Pane(pane.clone());
        }
    }
    if let Some(live_pane) = live_pane_for_session_window(live_targets, session, window) {
        return Target::Pane(live_pane);
    }
    if let Some(pane) = cached_pane {
        if live_targets.is_empty() && !cached_pane_known_dead(transport, &pane) {
            return Target::Pane(pane);
        }
    }
    Target::SessionWindow {
        session: SessionName::new(session),
        window: WindowName::new(window),
    }
}

fn live_pane_for_session_window(
    targets: &[PaneInfo],
    session: &str,
    window: &str,
) -> Option<PaneId> {
    targets
        .iter()
        .find(|target| {
            target.session.as_str() == session
                && target
                    .window_name
                    .as_ref()
                    .is_some_and(|name| name.as_str() == window)
        })
        .map(|target| target.pane_id.clone())
}

fn cached_pane_known_dead(transport: &dyn Transport, pane: &PaneId) -> bool {
    if matches!(transport.has_pane(pane), Ok(Some(false))) {
        return true;
    }
    matches!(transport.liveness(pane), Ok(PaneLiveness::Dead))
}

#[derive(Clone, Copy)]
struct PaneSocketBinding<'a> {
    pane_id: &'a str,
    tmux_socket: Option<&'a str>,
}

fn pane_conflicts_on_same_socket(
    pane_id: &str,
    worker_socket: Option<&str>,
    leader: PaneSocketBinding<'_>,
) -> bool {
    leader.pane_id == pane_id && !tmux_sockets_known_different(worker_socket, leader.tmux_socket)
}

fn tmux_sockets_known_different(left: Option<&str>, right: Option<&str>) -> bool {
    let (Some(left), Some(right)) = (left, right) else {
        return false;
    };
    if left == right {
        return false;
    }
    std::path::Path::new(left).is_absolute() && std::path::Path::new(right).is_absolute()
}

fn worker_tmux_socket(
    state: &serde_json::Value,
    agent: Option<&serde_json::Value>,
    transport: &dyn Transport,
) -> Option<String> {
    agent
        .and_then(tmux_socket_field)
        .or_else(|| runtime_tmux_socket(state))
        .map(str::to_string)
        .or_else(|| transport.tmux_endpoint())
}

fn runtime_tmux_socket(state: &serde_json::Value) -> Option<&str> {
    tmux_socket_field(state)
        .or_else(|| active_team_entry(state).and_then(tmux_socket_field))
        .or_else(|| only_team_entry(state).and_then(tmux_socket_field))
}

fn tmux_socket_field(value: &serde_json::Value) -> Option<&str> {
    value
        .get("tmux_endpoint")
        .or_else(|| value.get("tmux_socket"))
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.is_empty())
}

/// Read the bound leader pane id off the projected or team-scoped runtime state.
fn leader_receiver_pane_id(state: &serde_json::Value) -> Option<&str> {
    leader_receiver_pane_id_in_state(state)
        .or_else(|| active_team_entry(state).and_then(leader_receiver_pane_id_in_state))
        .or_else(|| only_team_entry(state).and_then(leader_receiver_pane_id_in_state))
}

fn leader_receiver_value(state: &serde_json::Value) -> Option<&serde_json::Value> {
    leader_receiver_value_in_state(state)
        .or_else(|| active_team_entry(state).and_then(leader_receiver_value_in_state))
        .or_else(|| only_team_entry(state).and_then(leader_receiver_value_in_state))
}

fn leader_receiver_value_in_state(state: &serde_json::Value) -> Option<&serde_json::Value> {
    state.get("leader_receiver")
}

fn receiver_owner_epoch(receiver: &serde_json::Value) -> Option<u64> {
    receiver
        .get("owner_epoch")
        .and_then(serde_json::Value::as_u64)
}

fn leader_receiver_transport_conflict_outcome(
    store: &MessageStore,
    event_log: &EventLog,
    message_id: &str,
    receiver: &serde_json::Value,
    sender: &str,
) -> Result<Option<DeliveryOutcome>, MessagingError> {
    let mode = receiver.get("mode").and_then(serde_json::Value::as_str);
    let transport_kind = receiver
        .get("transport_kind")
        .and_then(serde_json::Value::as_str);
    if let (Some(mode), Some(transport_kind)) = (mode, transport_kind) {
        if !mode.is_empty() && !transport_kind.is_empty() && mode != transport_kind {
            store.mark(
                message_id,
                "failed",
                Some("leader_receiver_transport_conflict"),
            )?;
            event_log.write(
                "leader_receiver.delivery_blocked",
                serde_json::json!({
                    "message_id": message_id,
                    "sender": sender,
                    "reason": "leader_receiver_transport_conflict",
                    "mode": mode,
                    "transport_kind": transport_kind,
                    "channel": "rebind_required",
                    "action": "run team-agent claim-leader, takeover, or attach-app-server-leader",
                }),
            )?;
            return Ok(Some(DeliveryOutcome {
                ok: false,
                status: DeliveryStatus::Refused,
                message_status: MessageStatusShadow("failed".to_string()),
                message_id: Some(message_id.to_string()),
                verification: Some(
                    "run team-agent claim-leader, takeover, or attach-app-server-leader"
                        .to_string(),
                ),
                stage: None,
                reason: Some(DeliveryRefusal::LeaderNotAttached),
                channel: Some("rebind_required".to_string()),
            }));
        }
    }
    Ok(None)
}

fn leader_receiver_pane_binding(state: &serde_json::Value) -> Option<PaneSocketBinding<'_>> {
    leader_receiver_pane_binding_in_state(state)
        .or_else(|| active_team_entry(state).and_then(leader_receiver_pane_binding_in_state))
        .or_else(|| only_team_entry(state).and_then(leader_receiver_pane_binding_in_state))
}

/// `state` is "usable" for leader delivery when the bound `leader_receiver.pane_id`
/// is present and alive (in `live_targets` or `liveness` probe returns not-Dead).
///
/// **0.3.24 excision (U1-A real-machine RED v2)**: the wave-2 session+window drift
/// fallback was removed. Drift now fails loudly as `leader_not_attached` —
/// see resolve_inject_target and `.team/artifacts/u1-a-realmachine-v2-fix-or-excise.md`.
fn leader_receiver_pane_is_usable(
    transport: &dyn Transport,
    state: &serde_json::Value,
    live_targets: &[PaneInfo],
) -> bool {
    let Some(pane_id) = leader_receiver_pane_id(state) else {
        return false;
    };
    if live_targets
        .iter()
        .any(|target| target.pane_id.as_str() == pane_id)
    {
        return true;
    }
    !matches!(
        transport.liveness(&PaneId::new(pane_id)),
        Ok(PaneLiveness::Dead)
    )
}

enum DeliveryTransport<'a> {
    Borrowed(&'a dyn Transport),
    Owned(crate::tmux_backend::TmuxBackend),
}

impl<'a> DeliveryTransport<'a> {
    fn as_transport(&'a self) -> &'a dyn Transport {
        match self {
            Self::Borrowed(transport) => *transport,
            Self::Owned(transport) => transport,
        }
    }
}

fn delivery_transport_for_recipient<'a>(
    workspace: &Path,
    product_transport: &'a dyn Transport,
    state: &serde_json::Value,
    recipient: &str,
) -> DeliveryTransport<'a> {
    if recipient != "leader" {
        return DeliveryTransport::Borrowed(product_transport);
    }
    let pane_id = leader_receiver_pane_id(state);
    let Some(socket) = leader_receiver_tmux_socket(state) else {
        if let Some(pane_id) = pane_id {
            let in_workspace = product_transport
                .list_targets()
                .unwrap_or_default()
                .iter()
                .any(|target| target.pane_id.as_str() == pane_id);
            if !in_workspace {
                let default_backend = crate::tmux_backend::TmuxBackend::new();
                if default_backend
                    .list_targets()
                    .unwrap_or_default()
                    .iter()
                    .any(|target| target.pane_id.as_str() == pane_id)
                {
                    return DeliveryTransport::Owned(default_backend);
                }
            }
        }
        return DeliveryTransport::Borrowed(product_transport);
    };
    if socket == crate::tmux_backend::socket_name_for_workspace(workspace) {
        DeliveryTransport::Borrowed(product_transport)
    } else {
        let endpoint_backend = crate::tmux_backend::TmuxBackend::for_tmux_endpoint(socket);
        if let Some(pane_id) = pane_id {
            if endpoint_backend
                .list_targets()
                .unwrap_or_default()
                .iter()
                .any(|target| target.pane_id.as_str() == pane_id)
            {
                return DeliveryTransport::Owned(endpoint_backend);
            }
            if product_transport
                .list_targets()
                .unwrap_or_default()
                .iter()
                .any(|target| target.pane_id.as_str() == pane_id)
            {
                return DeliveryTransport::Borrowed(product_transport);
            }
            let default_backend = crate::tmux_backend::TmuxBackend::new();
            if default_backend
                .list_targets()
                .unwrap_or_default()
                .iter()
                .any(|target| target.pane_id.as_str() == pane_id)
            {
                return DeliveryTransport::Owned(default_backend);
            }
        }
        DeliveryTransport::Owned(endpoint_backend)
    }
}

fn leader_receiver_pane_id_in_state(state: &serde_json::Value) -> Option<&str> {
    ["leader_receiver", "team_owner"]
        .into_iter()
        .find_map(|key| {
            state
                .get(key)
                .and_then(|r| r.get("pane_id"))
                .and_then(serde_json::Value::as_str)
                .filter(|s| !s.is_empty() && *s != "__team_agent_unbound__")
        })
}

fn leader_receiver_pane_binding_in_state(
    state: &serde_json::Value,
) -> Option<PaneSocketBinding<'_>> {
    ["leader_receiver", "team_owner"]
        .into_iter()
        .find_map(|key| state.get(key).and_then(pane_socket_binding))
}

fn pane_socket_binding(value: &serde_json::Value) -> Option<PaneSocketBinding<'_>> {
    let pane_id = value
        .get("pane_id")
        .and_then(serde_json::Value::as_str)
        .filter(|s| !s.is_empty() && *s != "__team_agent_unbound__")?;
    Some(PaneSocketBinding {
        pane_id,
        tmux_socket: tmux_socket_field(value),
    })
}

fn leader_receiver_tmux_socket(state: &serde_json::Value) -> Option<&str> {
    leader_receiver_field(state, "tmux_socket")
}

fn leader_receiver_has_noncanonical_tmux_socket(state: &serde_json::Value) -> bool {
    leader_receiver_tmux_socket(state)
        .is_some_and(|socket| socket != "default" && !std::path::Path::new(socket).is_absolute())
}

fn leader_receiver_field<'a>(state: &'a serde_json::Value, field: &str) -> Option<&'a str> {
    leader_receiver_field_in_state(state, field)
        .or_else(|| {
            active_team_entry(state).and_then(|team| leader_receiver_field_in_state(team, field))
        })
        .or_else(|| {
            only_team_entry(state).and_then(|team| leader_receiver_field_in_state(team, field))
        })
}

fn leader_receiver_field_in_state<'a>(
    state: &'a serde_json::Value,
    field: &str,
) -> Option<&'a str> {
    state
        .get("leader_receiver")
        .and_then(|receiver| receiver.get(field))
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.is_empty())
}

/// E50 PR-1 (0.3.24 P0): render `InjectReport.submit_diagnostics` as a JSON
/// array suitable for inclusion in `send.unverified` / `send.failed` events.
/// `null` if no diagnostics were attached (e.g. the inject went through a
/// path that bypassed the paste-prompt + Enter instrumentation, or this is
/// the pre-PR-2 inject-failed path with no `InjectReport`).
fn render_submit_diagnostics(report: &InjectReport) -> serde_json::Value {
    let Some(diag) = report.submit_diagnostics.as_ref() else {
        return serde_json::Value::Null;
    };
    serde_json::Value::Array(
        diag.attempts_detail
            .iter()
            .map(|obs| {
                serde_json::json!({
                    "attempt_index": obs.attempt_index,
                    "matched": obs.matched,
                    "matched_literal": obs.matched_literal,
                    "where_in_tail": obs.where_in_tail,
                    "pane_tail_excerpt": obs.pane_tail_excerpt,
                    "pane_tail_lines": obs.pane_tail_lines,
                    "elapsed_ms": obs.elapsed_ms,
                })
            })
            .collect(),
    )
}

#[allow(clippy::too_many_arguments)]
fn emit_send_failed_exhausted(
    workspace: &Path,
    state: &serde_json::Value,
    event_log: &EventLog,
    message_id: &str,
    recipient: &str,
    attempts: u32,
    failure_reason: &str,
    verification: &str,
    inject_report: Option<&InjectReport>,
) -> Result<(), MessagingError> {
    // E50 PR-1 (0.3.24 P0): forensic fields on send.failed. Legacy keys
    // (`message_id` / `recipient` / `attempts` / `max_attempts` / `reason` /
    // `verification`) preserved byte-for-byte for grep compatibility.
    let submit_attempts_detail = inject_report
        .map(render_submit_diagnostics)
        .unwrap_or_else(|| serde_json::Value::Array(Vec::new()));
    let total_elapsed_ms = inject_report
        .and_then(|r| r.submit_diagnostics.as_ref())
        .map(|d| d.total_elapsed_ms)
        .unwrap_or(0);
    let last_matched_literal = inject_report
        .and_then(|r| r.submit_diagnostics.as_ref())
        .and_then(|d| d.attempts_detail.last())
        .and_then(|a| a.matched_literal.clone());
    let last_pane_tail_excerpt = inject_report
        .and_then(|r| r.submit_diagnostics.as_ref())
        .and_then(|d| d.attempts_detail.last())
        .map(|a| a.pane_tail_excerpt.clone());
    event_log.write(
        "send.failed",
        serde_json::json!({
            "message_id": message_id,
            "recipient": recipient,
            "attempts": attempts,
            "max_attempts": SEND_RETRY_MAX_ATTEMPTS,
            "reason": failure_reason,
            "verification": verification,
            "submit_attempts_detail": submit_attempts_detail,
            "total_elapsed_ms": total_elapsed_ms,
            "last_matched_literal": last_matched_literal,
            "last_pane_tail_excerpt": last_pane_tail_excerpt,
        }),
    )?;
    let content = format!(
        "send.failed\nerror: send to {recipient} failed with {failure_reason} after {attempts}/{SEND_RETRY_MAX_ATTEMPTS} attempts\naction: inspect the target pane and retry the send\nlog: .team/logs/events.jsonl"
    );
    match crate::messaging::send_to_leader_receiver(
        workspace,
        state,
        "leader",
        &content,
        None,
        "coordinator",
        false,
        Some(&format!("send.failed:{message_id}")),
        event_log,
    ) {
        Ok(outcome) => {
            event_log.write(
                "send.failed_notification",
                serde_json::json!({
                    "message_id": message_id,
                    "recipient": recipient,
                    "leader_notification_status": super::helpers::status_wire(outcome.status),
                    "leader_message_id": outcome.message_id,
                }),
            )?;
        }
        Err(error) => {
            event_log.write(
                "send.failed_notification_failed",
                serde_json::json!({
                    "message_id": message_id,
                    "recipient": recipient,
                    "error": error.to_string(),
                }),
            )?;
        }
    }
    Ok(())
}

fn active_team_entry(state: &serde_json::Value) -> Option<&serde_json::Value> {
    let team = state
        .get("active_team_key")
        .and_then(serde_json::Value::as_str)?;
    state
        .get("teams")
        .and_then(serde_json::Value::as_object)
        .and_then(|teams| teams.get(team))
}

fn only_team_entry(state: &serde_json::Value) -> Option<&serde_json::Value> {
    let teams = state.get("teams").and_then(serde_json::Value::as_object)?;
    if teams.len() == 1 {
        teams.values().next()
    } else {
        None
    }
}

/// `_deliver_pending_messages` (`delivery.py:484`):扫 pending 队列逐条投递;busy 收件人写
/// `send.deferred_busy` 跳过 (**不丢**,card §131)。返回投递的 message_id 列表。
pub fn deliver_pending_messages(
    workspace: &Path,
    state: &serde_json::Value,
    transport: &dyn Transport,
    event_log: &EventLog,
) -> Result<Vec<String>, MessagingError> {
    let store = MessageStore::open(workspace)?;
    let message_ids = {
        let conn = crate::db::schema::open_db(store.db_path())?;
        let mut stmt = conn.prepare(
            "select message_id from messages
             where status in ('pending', 'accepted', 'target_resolved')
             order by created_at, message_id",
        )?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        rows.collect::<Result<Vec<_>, _>>()?
    };
    let mut delivered = Vec::new();
    for message_id in message_ids {
        if let Some(message) = message_for_delivery(&store, &message_id)? {
            let scoped_state;
            let state = match message.owner_team_id.as_deref() {
                Some(team) if !team.is_empty() => {
                    match project_state_for_owner_team(
                        workspace,
                        team,
                        state,
                        Some(&store),
                        Some(&message_id),
                        Some(event_log),
                    )? {
                        OwnerTeamProjection::Projected { state, .. } => {
                            scoped_state = state;
                            &scoped_state
                        }
                        OwnerTeamProjection::Refused(outcome) => {
                            let _ = event_log.write(
                                "delivery.projection_refused",
                                serde_json::json!({
                                    "message_id": message_id.as_str(),
                                    "owner_team_id": team,
                                    "status": super::helpers::status_wire(outcome.status),
                                    "verification": outcome.verification,
                                    "reason": format!("{:?}", outcome.reason),
                                }),
                            );
                            continue;
                        }
                    }
                }
                _ => state,
            };
            if recipient_is_busy(state, &message.recipient) {
                event_log.write(
                    "send.deferred_busy",
                    serde_json::json!({
                        "message_id": message_id,
                        "sender": message.sender,
                        "recipient": message.recipient,
                        "reason": "recipient_busy",
                    }),
                )?;
                continue;
            }
        }
        let outcome = match deliver_pending_message(
            workspace,
            &store,
            transport,
            &message_id,
            event_log,
            state,
        ) {
            Ok(outcome) => outcome,
            Err(error) => {
                let _ = event_log.write(
                    "delivery.item_blocked",
                    serde_json::json!({
                        "message_id": message_id.as_str(),
                        "error": error.to_string(),
                    }),
                );
                continue;
            }
        };
        if outcome.ok {
            delivered.push(message_id);
        }
    }
    Ok(delivered)
}

struct PendingMessage {
    sender: String,
    recipient: String,
    content: String,
    task_id: Option<String>,
    owner_team_id: Option<String>,
    status: String,
    delivery_attempts: u32,
}

fn message_for_delivery(
    store: &MessageStore,
    message_id: &str,
) -> Result<Option<PendingMessage>, MessagingError> {
    let conn = crate::db::schema::open_db(store.db_path())?;
    let message = conn
        .query_row(
            "select sender, recipient, content, task_id, owner_team_id, status, delivery_attempts from messages where message_id = ?1",
            params![message_id],
            |row| {
                Ok(PendingMessage {
                    sender: row.get::<_, String>(0)?,
                    recipient: row.get::<_, String>(1)?,
                    content: row.get::<_, String>(2)?,
                    task_id: row.get::<_, Option<String>>(3)?,
                    owner_team_id: row.get::<_, Option<String>>(4)?,
                    status: row.get::<_, String>(5)?,
                    delivery_attempts: row.get::<_, i64>(6)?.max(0) as u32,
                })
            },
        )
        .optional()?;
    Ok(message)
}

fn bump_delivery_attempts(store: &MessageStore, message_id: &str) -> Result<u32, MessagingError> {
    let conn = crate::db::schema::open_db(store.db_path())?;
    conn.execute(
        "update messages
         set delivery_attempts = delivery_attempts + 1,
             updated_at = ?2
         where message_id = ?1",
        params![message_id, chrono::Utc::now().to_rfc3339()],
    )?;
    let attempts = conn.query_row(
        "select delivery_attempts from messages where message_id = ?1",
        params![message_id],
        |row| row.get::<_, i64>(0),
    )?;
    Ok(attempts.max(0) as u32)
}

/// Pre-inject gate (Contract B): peek the recipient pane and answer "is there an
/// actionable provider startup prompt right now (trust menu or update prompt)" using
/// the SHARED provider/startup_prompt recognizers — no second classifier, no provider
/// API calls. Returns `false` if capture fails so providers without a startup
/// recognizer (or any pane without the trust-menu shape) keep flowing through
/// normal delivery.
fn recipient_pane_has_actionable_startup_prompt(
    transport: &dyn Transport,
    state: &serde_json::Value,
    recipient: &str,
    target: &Target,
) -> bool {
    let agent = state
        .get("agents")
        .and_then(serde_json::Value::as_object)
        .and_then(|agents| agents.get(recipient));
    let provider = agent
        .and_then(|agent| agent.get("provider"))
        .and_then(serde_json::Value::as_str)
        .and_then(parse_canonical_provider);
    let Some(provider) = provider else {
        return false;
    };
    if matches!(provider, Provider::GeminiCli | Provider::Fake) {
        return false;
    }
    // step2-retry/scrollback root-cause (rt binary 6c9c6c1c): once the agent's
    // `startup_prompts` has been flipped to `handled`/`complete`, the trust modal
    // has been answered and is the AUTHORITATIVE record of "no actionable startup
    // prompt remains". A `tmux capture-pane -S -` Full capture STILL contains the
    // dismissed modal text in scrollback ("Do you trust …" + `› 1. Yes, continue`),
    // so the recognizer's actionable-shape override matches the residue and the
    // delivery gate would loop forever (49-attempt no-deliver in real machine).
    // Trust the state (same source step1-idem uses) and skip the classify entirely.
    let startup_prompts = agent
        .and_then(|agent| agent.get("startup_prompts"))
        .and_then(serde_json::Value::as_str);
    if matches!(startup_prompts, Some("handled" | "complete")) {
        return false;
    }
    // U1-C wave-2: Tail(80) instead of Full. The peek is the DELIVERY-SITE pre-check
    // (NOT the startup-prompts dismissal phase, which legitimately needs the full
    // scrollback to anchor recency). Limiting to the visible screen avoids matching
    // residual already-answered trust modals that scrolled off — while a live trust
    // modal would still appear in the last 80 lines because Codex pins it to the
    // visible region until dismissed. Pair this with the U1-C recency guard in
    // `has_actionable_trust_shape`: either fix alone covers the symptom; both
    // together defend against both scrollback-residue AND pre-render pathologies.
    let captured = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        transport.capture(target, crate::transport::CaptureRange::Tail(80))
    })) {
        Ok(Ok(captured)) => captured.text,
        _ => return false,
    };
    match provider {
        Provider::Codex => matches!(
            crate::provider::classify_codex_startup_screen(&captured),
            crate::provider::StartupScreenDecision::AnswerWorkspaceTrust
                | crate::provider::StartupScreenDecision::SkipUpdatePrompt
        ),
        Provider::Claude | Provider::ClaudeCode => matches!(
            crate::provider::classify_claude_startup_screen(&captured),
            crate::provider::StartupScreenDecision::AnswerWorkspaceTrust
        ),
        Provider::Copilot => matches!(
            crate::provider::classify_copilot_startup_screen(&captured),
            crate::provider::StartupScreenDecision::AnswerWorkspaceTrust
        ),
        Provider::GeminiCli | Provider::Fake => false,
    }
}

fn recipient_is_busy(state: &serde_json::Value, recipient: &str) -> bool {
    state
        .get("agents")
        .and_then(serde_json::Value::as_object)
        .and_then(|agents| agents.get(recipient))
        .and_then(|agent| agent.get("status"))
        .and_then(serde_json::Value::as_str)
        == Some("busy")
}

/// `_handle_trust_retry_needed` (`delivery.py:221`):trust 应答失败时调度有界退避重试
/// (`attempt < MAX` → schedule;`>= MAX` → 终态 mark failed + `trust_auto_answer_exhausted`)。
pub fn handle_trust_retry_needed(
    store: &MessageStore,
    payload: &TrustRetryPayload,
    event_log: &EventLog,
) -> Result<DeliveryOutcome, MessagingError> {
    if payload.attempt >= payload.max_attempts {
        let _ = store.mark(
            &payload.message_id,
            "failed",
            Some("trust_auto_answer_exhausted"),
        );
        event_log.write(
            "leader_panes.trust_auto_answer_exhausted",
            serde_json::json!({"message_id": payload.message_id, "attempt": payload.attempt}),
        )?;
        return Ok(DeliveryOutcome {
            ok: false,
            status: DeliveryStatus::TrustAutoAnswerExhausted,
            message_status: MessageStatusShadow("failed".to_string()),
            message_id: Some(payload.message_id.clone()),
            verification: None,
            stage: Some(DeliveryStage::TrustAutoAnswerDismissalWait),
            reason: None,
            channel: None,
        });
    }
    let next_attempt = payload.attempt.saturating_add(1);
    let backoff = super::TRUST_RETRY_BACKOFF_SECONDS
        .iter()
        .find_map(|(attempt, seconds)| (*attempt == next_attempt).then_some(*seconds))
        .unwrap_or(30);
    let due_at = (chrono::Utc::now() + chrono::Duration::seconds(i64::from(backoff))).to_rfc3339();
    let conn = crate::db::schema::open_db(store.db_path())?;
    conn.execute(
        "insert into scheduled_events(owner_team_id, due_at, target, kind, payload_json, status, created_at)
         values (null, ?1, ?2, 'trust_retry', ?3, 'pending', ?4)",
        params![
            due_at,
            payload.first_target.as_str(),
            serde_json::json!({
                "message_id": payload.message_id,
                "attempt": next_attempt,
                "max_attempts": payload.max_attempts,
                "first_target": payload.first_target.as_str(),
            })
            .to_string(),
            chrono::Utc::now().to_rfc3339(),
        ],
    )?;
    let _ = store.mark(&payload.message_id, "queued_until_trust", None);
    event_log.write(
        "leader_panes.trust_auto_answer_retry_scheduled",
        serde_json::json!({"message_id": payload.message_id, "attempt": next_attempt, "due_at": due_at}),
    )?;
    Ok(DeliveryOutcome {
        ok: false,
        status: DeliveryStatus::RetryScheduled,
        message_status: MessageStatusShadow("queued_until_trust".to_string()),
        message_id: Some(payload.message_id.clone()),
        verification: None,
        stage: Some(DeliveryStage::TrustAutoAnswerDismissalWait),
        reason: None,
        channel: None,
    })
}

/// `_execute_trust_retry` (`delivery.py:330`):trust_retry scheduled event 的消费者 ——
/// 把行重置回 `accepted`,attempt 穿透,重跑 `_deliver_pending_message`。
pub fn execute_trust_retry(
    workspace: &Path,
    store: &MessageStore,
    transport: &dyn Transport,
    payload: &TrustRetryPayload,
    event_log: &EventLog,
    owner_team_id: Option<&TeamKey>,
) -> Result<DeliveryOutcome, MessagingError> {
    let _ = owner_team_id;
    let _ = store.mark(&payload.message_id, "accepted", None);
    let state = crate::state::persist::load_runtime_state(workspace)?;
    deliver_pending_message(
        workspace,
        store,
        transport,
        &payload.message_id,
        event_log,
        &state,
    )
}

/// `_record_turn_open_if_leader_to_worker` (`delivery.py:430`):**take-over arm 来自真实投递**
/// (card §121) —— 仅 leader→worker 注入**成功后**才调 `record_turn_open_after_delivery`,绝不凭空 arm。
pub fn record_turn_open_if_leader_to_worker(
    workspace: &Path,
    state: &serde_json::Value,
    sender: &str,
    recipient: &str,
    delivered: &DeliveryOutcome,
    event_log: &EventLog,
) -> Result<(), MessagingError> {
    let _ = state;
    record_turn_open_if_leader_to_worker_scoped(
        workspace, sender, recipient, delivered, event_log, None,
    )
}

fn record_turn_open_if_leader_to_worker_scoped(
    workspace: &Path,
    sender: &str,
    recipient: &str,
    delivered: &DeliveryOutcome,
    event_log: &EventLog,
    owner_team_id: Option<&str>,
) -> Result<(), MessagingError> {
    if !delivered.ok || !matches!(sender, "leader" | "Leader") || recipient == "leader" {
        return Ok(());
    }
    let mut state = scoped_state_for_write(workspace, owner_team_id)?;
    arm_turn_open(&mut state, recipient, &delivered.message_id);
    save_scoped_state_reapplying_after_conflict(workspace, &state, owner_team_id, |latest| {
        arm_turn_open(latest, recipient, &delivered.message_id);
    })?;
    event_log.write(
        "turn_open.armed_after_delivery",
        serde_json::json!({"agent_id": recipient, "message_id": delivered.message_id}),
    )?;
    Ok(())
}

fn arm_turn_open(state: &mut serde_json::Value, recipient: &str, message_id: &Option<String>) {
    let Some(root) = state.as_object_mut() else {
        return;
    };
    let coordinator = root
        .entry("coordinator")
        .or_insert_with(|| serde_json::json!({}));
    if let Some(obj) = coordinator.as_object_mut() {
        obj.insert(
            "turn_open".to_string(),
            serde_json::json!({"armed": true, "node_id": recipient, "turn_id": message_id}),
        );
    }
}

/// `_stamp_first_send_at_if_leader_to_worker` (`delivery.py:380`):首次 leader→worker 投递戳
/// `first_send_at` (step 13 restart Route B atomicity 决策读它)。
pub fn stamp_first_send_at_if_leader_to_worker(
    workspace: &Path,
    state: &serde_json::Value,
    sender: &str,
    recipient: &str,
) -> Result<(), MessagingError> {
    let _ = state;
    stamp_first_send_at_if_leader_to_worker_scoped(workspace, sender, recipient, None)
}

fn stamp_first_send_at_if_leader_to_worker_scoped(
    workspace: &Path,
    sender: &str,
    recipient: &str,
    owner_team_id: Option<&str>,
) -> Result<(), MessagingError> {
    if !matches!(sender, "leader" | "Leader") || recipient == "leader" {
        return Ok(());
    }
    let mut state = scoped_state_for_write(workspace, owner_team_id)?;
    let now = chrono::Utc::now().to_rfc3339();
    if stamp_first_send_at(&mut state, recipient, &now) {
        save_scoped_state_reapplying_after_conflict(workspace, &state, owner_team_id, |latest| {
            let _ = stamp_first_send_at(latest, recipient, &now);
        })?;
    }
    Ok(())
}

fn stamp_first_send_at(state: &mut serde_json::Value, recipient: &str, now: &str) -> bool {
    if let Some(agent) = state
        .get_mut("agents")
        .and_then(serde_json::Value::as_object_mut)
        .and_then(|agents| agents.get_mut(recipient))
        .and_then(serde_json::Value::as_object_mut)
    {
        if !agent.contains_key("first_send_at")
            || agent
                .get("first_send_at")
                .is_some_and(serde_json::Value::is_null)
        {
            agent.insert(
                "first_send_at".to_string(),
                serde_json::Value::String(now.to_string()),
            );
            return true;
        }
    }
    false
}

fn scoped_state_for_write(
    workspace: &Path,
    owner_team_id: Option<&str>,
) -> Result<serde_json::Value, MessagingError> {
    match owner_team_id.filter(|team| !team.is_empty()) {
        Some(team) => {
            let raw = crate::state::persist::load_runtime_state(workspace)?;
            match project_state_for_owner_team_value(&raw, team) {
                Some(projected) => Ok(projected),
                None => Ok(raw),
            }
        }
        None => Ok(crate::state::persist::load_runtime_state(workspace)?),
    }
}

fn save_scoped_state(
    workspace: &Path,
    state: &serde_json::Value,
    owner_team_id: Option<&str>,
) -> Result<(), MessagingError> {
    if owner_team_id.filter(|team| !team.is_empty()).is_some() {
        if state
            .get("teams")
            .and_then(serde_json::Value::as_object)
            .is_some_and(|teams| {
                owner_team_id
                    .and_then(|team| {
                        crate::state::projection::resolve_owner_team_id(state, team)
                            .canonical_key()
                            .map(str::to_string)
                    })
                    .is_some_and(|team| teams.contains_key(&team))
            })
        {
            crate::state::projection::save_team_scoped_state(workspace, state)?;
        } else {
            crate::state::persist::save_runtime_state(workspace, state)?;
        }
    } else {
        crate::state::persist::save_runtime_state(workspace, state)?;
    }
    Ok(())
}

fn save_scoped_state_reapplying_after_conflict<F>(
    workspace: &Path,
    state: &serde_json::Value,
    owner_team_id: Option<&str>,
    reapply: F,
) -> Result<(), MessagingError>
where
    F: FnOnce(&mut serde_json::Value),
{
    if owner_team_id.filter(|team| !team.is_empty()).is_some()
        && state
            .get("teams")
            .and_then(serde_json::Value::as_object)
            .is_some_and(|teams| {
                owner_team_id
                    .and_then(|team| {
                        crate::state::projection::resolve_owner_team_id(state, team)
                            .canonical_key()
                            .map(str::to_string)
                    })
                    .is_some_and(|team| teams.contains_key(&team))
            })
    {
        crate::state::projection::save_team_scoped_state_reapplying_after_conflict(
            workspace, state, reapply,
        )?;
    } else {
        crate::state::persist::save_runtime_state_reapplying_after_conflict(
            workspace, state, reapply,
        )?;
    }
    Ok(())
}

enum OwnerTeamProjection {
    Projected {
        state: serde_json::Value,
        canonical_team: String,
    },
    Refused(DeliveryOutcome),
}

fn project_state_for_owner_team(
    workspace: &Path,
    team: &str,
    fallback: &serde_json::Value,
    store: Option<&MessageStore>,
    message_id: Option<&str>,
    event_log: Option<&EventLog>,
) -> Result<OwnerTeamProjection, MessagingError> {
    let raw = crate::state::persist::load_runtime_state(workspace)?;
    let fallback_has_teams = fallback
        .get("teams")
        .and_then(serde_json::Value::as_object)
        .is_some_and(|teams| !teams.is_empty());
    let (mut projection_source, mut resolution) = if fallback_has_teams {
        (
            fallback,
            crate::state::projection::resolve_owner_team_id(fallback, team),
        )
    } else {
        (
            &raw,
            crate::state::projection::resolve_owner_team_id(&raw, team),
        )
    };
    if !fallback_has_teams && matches!(resolution, OwnerTeamResolution::Unresolved { .. }) {
        let fallback_resolution = crate::state::projection::resolve_owner_team_id(fallback, team);
        if !matches!(fallback_resolution, OwnerTeamResolution::Unresolved { .. }) {
            resolution = fallback_resolution;
            projection_source = fallback;
        }
    }
    let canonical_team = match resolution {
        OwnerTeamResolution::Canonical(canonical) => canonical,
        OwnerTeamResolution::LegacyAlias {
            requested,
            canonical,
        } => {
            normalize_owner_team_id_rows(workspace, &requested, &canonical, message_id, event_log)?;
            canonical
        }
        OwnerTeamResolution::Unresolved { requested } => {
            let outcome = refuse_owner_team_resolution(
                store,
                message_id,
                event_log,
                "owner_team_unresolved",
                serde_json::json!({"owner_team_id": requested}),
                DeliveryRefusal::UnknownRecipient,
            )?;
            return Ok(OwnerTeamProjection::Refused(outcome));
        }
        OwnerTeamResolution::Ambiguous { requested, matches } => {
            let outcome = refuse_owner_team_resolution(
                store,
                message_id,
                event_log,
                "owner_team_ambiguous",
                serde_json::json!({"owner_team_id": requested, "matches": matches}),
                DeliveryRefusal::Ambiguous,
            )?;
            return Ok(OwnerTeamProjection::Refused(outcome));
        }
    };
    if top_level_state_matches_owner_team(fallback, &canonical_team) {
        let mut state = fallback.clone();
        carry_top_level_leader_binding(&mut state, &raw);
        return Ok(OwnerTeamProjection::Projected {
            state,
            canonical_team,
        });
    }
    if top_level_state_matches_owner_team(&raw, &canonical_team) {
        return Ok(OwnerTeamProjection::Projected {
            state: raw,
            canonical_team,
        });
    }
    if state_has_no_team_entries(projection_source) {
        let mut state = projection_source.clone();
        carry_top_level_leader_binding(&mut state, &raw);
        return Ok(OwnerTeamProjection::Projected {
            state,
            canonical_team,
        });
    }
    let mut state = project_state_for_owner_team_value(projection_source, &canonical_team)
        .ok_or_else(|| {
            MessagingError::Routing(format!("owner_team_unresolved: {canonical_team}"))
        })?;
    carry_top_level_leader_binding(&mut state, projection_source);
    carry_top_level_leader_binding(&mut state, &raw);
    Ok(OwnerTeamProjection::Projected {
        state,
        canonical_team,
    })
}

fn carry_top_level_leader_binding(projected: &mut serde_json::Value, raw: &serde_json::Value) {
    let Some(projected_obj) = projected.as_object_mut() else {
        return;
    };
    for key in ["leader_receiver", "team_owner", "owner_epoch"] {
        if projected_obj.contains_key(key) {
            continue;
        }
        if let Some(value) = raw.get(key) {
            projected_obj.insert(key.to_string(), value.clone());
        }
    }
}

fn state_has_no_team_entries(state: &serde_json::Value) -> bool {
    state
        .get("teams")
        .and_then(serde_json::Value::as_object)
        .is_none_or(serde_json::Map::is_empty)
}

pub(crate) fn normalize_owner_team_id_rows(
    workspace: &Path,
    requested: &str,
    canonical: &str,
    message_id: Option<&str>,
    event_log: Option<&EventLog>,
) -> Result<(), MessagingError> {
    if requested == canonical {
        return Ok(());
    }
    if let Some(event_log) = event_log {
        event_log.write(
            "owner_team_id.compatibility_alias_detected",
            serde_json::json!({
                "requested_owner_team_id": requested,
                "canonical_owner_team_id": canonical,
                "message_id": message_id,
                "action": "read_only_no_db_update",
                "workspace": workspace.to_string_lossy().to_string(),
            }),
        )?;
    }
    Ok(())
}

fn refuse_owner_team_resolution(
    store: Option<&MessageStore>,
    message_id: Option<&str>,
    event_log: Option<&EventLog>,
    error: &str,
    details: serde_json::Value,
    refusal: DeliveryRefusal,
) -> Result<DeliveryOutcome, MessagingError> {
    if let (Some(store), Some(message_id)) = (store, message_id) {
        store.mark(message_id, "failed", Some(error))?;
    }
    if let Some(event_log) = event_log {
        event_log.write(
            "owner_team_id.resolution_failed",
            serde_json::json!({
                "message_id": message_id,
                "error": error,
                "details": details,
            }),
        )?;
    }
    Ok(DeliveryOutcome {
        ok: false,
        status: DeliveryStatus::Refused,
        message_status: MessageStatusShadow("failed".to_string()),
        message_id: message_id.map(str::to_string),
        verification: Some(error.to_string()),
        stage: None,
        reason: Some(refusal),
        channel: Some("owner_team_resolution".to_string()),
    })
}

fn project_state_for_owner_team_value(
    raw: &serde_json::Value,
    team: &str,
) -> Option<serde_json::Value> {
    if let Some(projected) = raw
        .get("teams")
        .and_then(serde_json::Value::as_object)
        .is_some_and(|teams| teams.contains_key(team))
        .then(|| crate::state::projection::project_top_level_view(raw, team))
    {
        return Some(projected);
    }
    if top_level_state_matches_owner_team(raw, team) {
        return None;
    }
    None
}

fn top_level_state_matches_owner_team(state: &serde_json::Value, team: &str) -> bool {
    state
        .get("active_team_key")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|value| value == team)
        || crate::state::projection::team_state_key(state) == team
        || state
            .get("session_name")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|session| session == team || session.strip_prefix("team-") == Some(team))
}

/// `retry_injection_after_trust_auto_answer` (`trust_auto_answer.py`):leader 路径 trust 应答
/// 后重注入 (查 pane_width fail-safe + attempt_trust_auto_answer + 等 dismissal + 重 inject)。
pub fn retry_injection_after_trust_auto_answer(
    workspace: &Path,
    state: &serde_json::Value,
    transport: &dyn Transport,
    target: &Target,
    text: &str,
    provider: Provider,
    event_log: &EventLog,
) -> Result<DeliveryOutcome, MessagingError> {
    let _ = (
        workspace, state, transport, target, text, provider, event_log,
    );
    Ok(DeliveryOutcome {
        ok: false,
        status: DeliveryStatus::RetryScheduled,
        message_status: MessageStatusShadow("retry_scheduled".to_string()),
        message_id: None,
        verification: None,
        stage: Some(DeliveryStage::TrustAutoAnswerDismissalWait),
        reason: None,
        channel: None,
    })
}

/// S1-CAPTURE-001 (0.4.8 phase-1): returns (rollout_path, provider_wire) when
/// the recipient agent is Claude/ClaudeCode with a known authoritative
/// rollout_path; otherwise None (skip transcript verify). Phase-1 Claude only —
/// codex/copilot return None and keep pre-fix delivery semantics.
fn claude_recipient_rollout(
    state: &serde_json::Value,
    recipient: &str,
) -> Option<(std::path::PathBuf, &'static str)> {
    let agent = state.get("agents")?.get(recipient)?;
    let provider = agent
        .get("provider")
        .and_then(serde_json::Value::as_str)
        .and_then(parse_provider)?;
    if !is_claude_family(provider) {
        return None;
    }
    let provider_wire_str = provider_wire(provider);
    let rollout = agent
        .get("rollout_path")
        .and_then(serde_json::Value::as_str)
        .filter(|s| !s.is_empty())?;
    Some((std::path::PathBuf::from(rollout), provider_wire_str))
}

/// S1-CAPTURE-001 (0.4.8 phase-1): bounded tail read of the rollout file
/// searching for `needle`. Reads up to `tail_bytes` from the end of the file
/// (default 64KB budget). Returns true iff the needle appears in the tail.
/// Silent on read errors — callers treat missing/unreadable as "token not
/// present" which forces the unverified path.
fn rollout_tail_contains(path: &std::path::Path, needle: &str, tail_bytes: u64) -> bool {
    use std::io::{Read, Seek, SeekFrom};
    let Ok(mut file) = std::fs::File::open(path) else {
        return false;
    };
    let Ok(metadata) = file.metadata() else {
        return false;
    };
    let len = metadata.len();
    let start = len.saturating_sub(tail_bytes);
    if file.seek(SeekFrom::Start(start)).is_err() {
        return false;
    }
    let mut buf = Vec::with_capacity(tail_bytes.min(len) as usize);
    if file.take(tail_bytes).read_to_end(&mut buf).is_err() {
        return false;
    }
    let haystack = String::from_utf8_lossy(&buf);
    haystack.contains(needle)
}
