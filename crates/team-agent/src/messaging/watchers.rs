//! result_delivery.py — watcher 通知/去重/有界重试/claim-leader requeue (card §69-71)。

use std::path::Path;

#[cfg(unix)]
use std::fs::OpenOptions;
#[cfg(unix)]
use std::io::Write as _;
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;

use rusqlite::{params, OptionalExtension};

use crate::event_log::EventLog;
use crate::leader::LeaderIncident;
use crate::message_store::{MessageStore, NotificationClaimParams};
use crate::model::ids::{TaskId, TeamKey};
use crate::transport::PaneId;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RecoveryIncident {
    pub incident_id: String,
    pub occurred_at: String,
}

use super::{MessagingError, WatcherNotice, RESULT_DELIVERY_MAX_ATTEMPTS};

/// `notify_result_watchers` (`result_delivery.py:38`):匹配 + 去重 + (有界) 投递 result 给 leader
/// watcher。**恰好一次** (Gap 32/38):同 result_id 多 watcher → 1 次注入,余 `superseded`。
/// 去重唯一原语 = [`MessageStore::claim_leader_notification_delivery`]。由 watcher/rebind recovery 调用。
pub(crate) fn notify_result_watchers(
    workspace: &Path,
    result: &serde_json::Value,
    event_log: &EventLog,
    watchers: Option<&[serde_json::Value]>,
    dedupe_reason: Option<&str>,
) -> Result<Vec<WatcherNotice>, MessagingError> {
    Err(MessagingError::Validation("legacy SQL messaging retired; use station send and C3 inbox".into()))
}

fn watcher_matches(
    watcher: &serde_json::Value,
    result_task: Option<&str>,
    result_agent: Option<&str>,
) -> bool {
    let task_matches = watcher
        .get("task_id")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .is_none_or(|task| Some(task) == result_task);
    let agent_matches = watcher
        .get("agent_id")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .is_none_or(|agent| Some(agent) == result_agent);
    task_matches && agent_matches
}

pub(crate) fn notify_fifo_result_watchers(
    conn: &rusqlite::Connection,
    event_log: &EventLog,
    task_id: &str,
    result_id: &str,
) -> Result<(), MessagingError> {
    let mut stmt = conn.prepare(
        "select watcher_id, recipient from result_watchers
         where task_id = ?1 and status = 'pending' and recipient is not null
         order by created_at, watcher_id",
    )?;
    let rows = stmt
        .query_map(params![task_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    drop(stmt);
    for (watcher_id, recipient) in rows {
        let _ = notify_fifo_waiter(conn, event_log, &watcher_id, task_id, result_id, &recipient)?;
    }
    Ok(())
}

#[cfg(unix)]
fn notify_fifo_waiter(
    conn: &rusqlite::Connection,
    event_log: &EventLog,
    watcher_id: &str,
    task_id: &str,
    result_id: &str,
    recipient: &str,
) -> Result<WatcherNotice, MessagingError> {
    let write = (|| -> std::io::Result<()> {
        let mut fifo = OpenOptions::new()
            .write(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(recipient)?;
        fifo.write_all(format!("{result_id}\n").as_bytes())
    })();
    match write {
        Ok(()) => {
            event_log.write(
                "result_wake.notified",
                serde_json::json!({
                    "task_id": task_id,
                    "result_id": result_id,
                    "watcher_id": watcher_id,
                    "recipient": recipient,
                }),
            )?;
            Ok(WatcherNotice {
                watcher_id: watcher_id.to_string(),
                result_id: Some(result_id.to_string()),
                ok: true,
                status: Some("notified".to_string()),
                notified_message_id: None,
                primary_watcher_id: None,
                prior_state: None,
                error: None,
            })
        }
        Err(error)
            if matches!(
                error.raw_os_error(),
                Some(code) if code == libc::ENXIO || code == libc::ENOENT
            ) =>
        {
            let reason = if error.raw_os_error() == Some(libc::ENXIO) {
                "ENXIO"
            } else {
                "ENOENT"
            };
            conn.execute(
                "delete from result_watchers where watcher_id = ?1",
                params![watcher_id],
            )?;
            event_log.write(
                "result_wake.notify_failed",
                serde_json::json!({
                    "task_id": task_id,
                    "result_id": result_id,
                    "watcher_id": watcher_id,
                    "recipient": recipient,
                    "reason": reason,
                }),
            )?;
            Ok(WatcherNotice {
                watcher_id: watcher_id.to_string(),
                result_id: Some(result_id.to_string()),
                ok: false,
                status: Some("removed".to_string()),
                notified_message_id: None,
                primary_watcher_id: None,
                prior_state: None,
                error: Some(reason.to_string()),
            })
        }
        Err(error) => Err(error.into()),
    }
}

#[cfg(not(unix))]
fn notify_fifo_waiter(
    _conn: &rusqlite::Connection,
    _event_log: &EventLog,
    watcher_id: &str,
    _task_id: &str,
    result_id: &str,
    _recipient: &str,
) -> Result<WatcherNotice, MessagingError> {
    Ok(WatcherNotice {
        watcher_id: watcher_id.to_string(),
        result_id: Some(result_id.to_string()),
        ok: false,
        status: Some("unsupported".to_string()),
        notified_message_id: None,
        primary_watcher_id: None,
        prior_state: None,
        error: Some("POSIX FIFO support unavailable".to_string()),
    })
}

fn deliver_primary_watcher(
    workspace: &Path,
    conn: &rusqlite::Connection,
    store: &MessageStore,
    event_log: &EventLog,
    watcher: &serde_json::Value,
    result: &serde_json::Value,
    result_id: Option<&str>,
    result_task: Option<&str>,
) -> Result<WatcherNotice, MessagingError> {
    let watcher_id = watcher
        .get("watcher_id")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    let Some(result_id) = result_id else {
        return update_watcher_failure(
            conn,
            event_log,
            watcher_id,
            None,
            "notify_failed",
            "missing_result_id",
        );
    };
    if let Some(recipient) = watcher
        .get("recipient")
        .and_then(|value| value.as_str())
        .filter(|recipient| Path::new(recipient).is_absolute())
    {
        return notify_fifo_waiter(
            conn,
            event_log,
            watcher_id,
            result_task.unwrap_or_default(),
            result_id,
            recipient,
        );
    }
    if let Some(existing) = delivered_result_message(
        store,
        result_id,
        result_task.map(TaskId::new).as_ref(),
        watcher
            .get("owner_team_id")
            .and_then(|v| v.as_str())
            .map(TeamKey::new)
            .as_ref(),
    )? {
        let message_id = existing
            .get("message_id")
            .and_then(|v| v.as_str())
            .map(ToString::to_string);
        mark_watcher_notified(
            conn,
            event_log,
            watcher_id,
            result_id,
            message_id.as_deref(),
        )?;
        return Ok(WatcherNotice {
            watcher_id: watcher_id.to_string(),
            result_id: Some(result_id.to_string()),
            ok: true,
            status: Some("notified".to_string()),
            notified_message_id: message_id,
            primary_watcher_id: None,
            prior_state: None,
            error: None,
        });
    }
    let attempts = result_delivery_attempts(event_log, watcher_id, result_id)?;
    if attempts >= u64::from(RESULT_DELIVERY_MAX_ATTEMPTS) {
        return update_watcher_failure(
            conn,
            event_log,
            watcher_id,
            Some(result_id),
            "delivery_exhausted",
            "delivery_exhausted",
        );
    }
    let content = format_result_watcher_notification(result);
    let super::PersistResolution::Persisted(persisted) = super::persist::persist_internal_send(
        workspace,
        super::InternalSendKind::Watcher,
        watcher.get("owner_team_id").and_then(|v| v.as_str()),
        result_task,
        watcher
            .get("leader_id")
            .and_then(|v| v.as_str())
            .unwrap_or("team-agent"),
        watcher
            .get("recipient")
            .and_then(|value| value.as_str())
            .unwrap_or("leader"),
        &content,
        None,
        false,
        None,
        super::InitialDisposition::Accepted,
        None,
    )?
    else {
        unreachable!("watcher notifications do not accept caller-supplied ids")
    };
    let message_id = persisted.message_id;
    let claim = store.claim_leader_notification_delivery(NotificationClaimParams {
        result_id,
        owner_team_id: watcher.get("owner_team_id").and_then(|v| v.as_str()),
        owner_epoch: None,
        leader_session_uuid: None,
        proposed_message_id: &message_id,
        envelope_hash: "",
        pane_id: None,
    })?;
    if claim.status == "claimed_by_you" {
        mark_watcher_notified(
            conn,
            event_log,
            watcher_id,
            result_id,
            Some(&claim.notified_message_id),
        )?;
        Ok(WatcherNotice {
            watcher_id: watcher_id.to_string(),
            result_id: Some(result_id.to_string()),
            ok: true,
            status: Some("notified".to_string()),
            notified_message_id: Some(claim.notified_message_id),
            primary_watcher_id: None,
            prior_state: None,
            error: None,
        })
    } else {
        update_watcher_failure(
            conn,
            event_log,
            watcher_id,
            Some(result_id),
            "notify_failed",
            "already_notified_by",
        )
    }
}

fn mark_watcher_notified(
    conn: &rusqlite::Connection,
    event_log: &EventLog,
    watcher_id: &str,
    result_id: &str,
    message_id: Option<&str>,
) -> Result<(), MessagingError> {
    let now = chrono::Utc::now().to_rfc3339();
    conn.execute(
        "update result_watchers
         set status = 'notified', notified_message_id = ?3, completed_at = ?4, result_id = ?2, error = null
         where watcher_id = ?1",
        params![watcher_id, result_id, message_id, now],
    )?;
    event_log.write(
        "result_watcher.notified",
        serde_json::json!({"watcher_id": watcher_id, "result_id": result_id, "message_id": message_id}),
    )?;
    Ok(())
}

fn update_watcher_failure(
    conn: &rusqlite::Connection,
    event_log: &EventLog,
    watcher_id: &str,
    result_id: Option<&str>,
    status: &str,
    error: &str,
) -> Result<WatcherNotice, MessagingError> {
    let now = chrono::Utc::now().to_rfc3339();
    conn.execute(
        "update result_watchers
         set status = ?2, completed_at = ?3, result_id = ?4, error = ?5
         where watcher_id = ?1",
        params![watcher_id, status, now, result_id, error],
    )?;
    event_log.write(
        "result_watcher.notify_failed",
        serde_json::json!({"watcher_id": watcher_id, "result_id": result_id, "status": status, "error": error}),
    )?;
    Ok(WatcherNotice {
        watcher_id: watcher_id.to_string(),
        result_id: result_id.map(ToString::to_string),
        ok: false,
        status: Some(status.to_string()),
        notified_message_id: None,
        primary_watcher_id: None,
        prior_state: None,
        error: Some(error.to_string()),
    })
}

fn result_delivery_attempts(
    event_log: &EventLog,
    watcher_id: &str,
    result_id: &str,
) -> Result<u64, MessagingError> {
    let events = event_log.tail(0)?;
    Ok(events
        .iter()
        .filter(|event| {
            event.get("watcher_id").and_then(|v| v.as_str()) == Some(watcher_id)
                && event.get("result_id").and_then(|v| v.as_str()) == Some(result_id)
                && matches!(
                    event.get("event").and_then(|v| v.as_str()),
                    Some("result_watcher.notify_failed" | "result_watcher.retry_notified")
                )
        })
        .count()
        .try_into()
        .unwrap_or(u64::MAX))
}

/// `retry_result_deliveries` (`result_delivery.py:19`):重投 `notify_failed` watcher。
/// coordinator tick + claim-leader 调。daemon-path → Result。
pub fn retry_result_deliveries(
    workspace: &Path,
    event_log: &EventLog,
) -> Result<Vec<WatcherNotice>, MessagingError> {
    Err(MessagingError::Validation("legacy SQL messaging retired; use station send and C3 inbox".into()))
}

/// `_result_entry_from_row`(`result_delivery.py:365-377`)。
fn result_entry_from_row(
    result_id: &str,
    envelope: &str,
    created_at: Option<&str>,
) -> Result<serde_json::Value, MessagingError> {
    let envelope: serde_json::Value = serde_json::from_str(envelope)?;
    Ok(serde_json::json!({
        "result_id": result_id,
        "task_id": envelope.get("task_id").cloned().unwrap_or(serde_json::Value::Null),
        "agent_id": envelope.get("agent_id").cloned().unwrap_or(serde_json::Value::Null),
        "status": envelope.get("status").cloned().unwrap_or(serde_json::Value::Null),
        "summary": envelope.get("summary").cloned().unwrap_or(serde_json::Value::Null),
        "tests": envelope.get("tests").cloned().unwrap_or_else(|| serde_json::json!([])),
        "created_at": created_at,
        "scope": "task",
    }))
}

/// `requeue_after_claim_leader` (`result_delivery.py:428`):Gap 26 —— 认领新 leader pane 后把
/// 未投递 watcher 重路由到新 pane。**`notified_message_id` 必须存活** (Gap 32,清空会二次注入)。
/// step 10 claim-leader 调。
pub fn requeue_after_claim_leader(
    workspace: &Path,
    store: &MessageStore,
    event_log: &EventLog,
    owner_team_id: &TeamKey,
    claimed_pane_id: &PaneId,
    incident_ts: Option<&str>,
) -> Result<Vec<WatcherNotice>, MessagingError> {
    Err(MessagingError::Validation("legacy SQL messaging retired; use station send and C3 inbox".into()))
}

pub fn recover_watchers_for_incident(
    workspace: &Path,
    event_log: &EventLog,
    owner_team_id: &TeamKey,
    claimed_pane_id: &PaneId,
    incident: LeaderIncident,
) -> Result<Vec<WatcherNotice>, MessagingError> {
    Err(MessagingError::Validation("legacy SQL messaging retired; use station send and C3 inbox".into()))
}

/// 0.5.5 gate054 round-2: attach-leader (and claim-leader) requeue for leader messages
/// that were refused with `rebind_required` while no leader pane was attached.
///
/// #231 C-5 semantics: same row, same message_id — flip an eligible status back
/// to `accepted` so `deliver_pending_messages` replays it through the SAME
/// pipeline. The `leader_notification_log` PK prevents a second notification
/// row for watcher-backed messages. A `submitted_pending_acceptance` row has
/// already crossed the transport boundary and is claim-immutable. Any future
/// retry belongs to a separate typed recovery arm, not attach/claim convergence.
/// Compatibility helper retained for callers that only need the total.
/// The typed recovery path below is the single writer; this wrapper performs
/// no second requeue and only maps the first result to `total()`.
pub(crate) fn requeue_blocked_leader_messages(
    store: &MessageStore,
    event_log: &EventLog,
    owner_team_id: &TeamKey,
    claimed_pane_id: &PaneId,
) -> Result<usize, MessagingError> {
    requeue_blocked_leader_messages_with_counts(
        store,
        event_log,
        owner_team_id,
        claimed_pane_id,
    )
    .map(crate::message_store::BlockedLeaderRequeueCounts::total)
}

pub(crate) fn requeue_blocked_leader_messages_with_counts(
    store: &MessageStore,
    event_log: &EventLog,
    owner_team_id: &TeamKey,
    claimed_pane_id: &PaneId,
) -> Result<crate::message_store::BlockedLeaderRequeueCounts, MessagingError> {
    // E6 (0.5.9 offline-mailbox §6.5): also requeue rows that a third-party
    // sender left in `queued_until_leader_attach` via the leader mailbox.
    // Same idempotent requeue funnel (row/message_id/leader_notification_log
    // PK are all preserved) — after attach/claim the coordinator's normal
    // `deliver_pending_messages` picks them up as `accepted` and injects
    // exactly once. status `queued_until_leader_attach` is deliberately NOT
    // in the `claim_for_delivery` eligible set (see message_store.rs) so it
    // could not have churned while the leader was unattached.
    // `submitted_pending_acceptance` remains parked and claim-immutable; a
    // future typed recovery arm must own any deliberate retry.
    let counts = store.requeue_blocked_leader_messages(owner_team_id.as_str())?;
    let requeued = counts.total();
    if requeued > 0 {
        event_log.write(
            "leader_receiver.blocked_messages_requeued",
            serde_json::json!({
                "team_id": owner_team_id.as_str(),
                "claimed_pane_id": claimed_pane_id.as_str(),
                "count": requeued,
                "by_prior_state": {
                    "blocked_leader_unbound": counts.blocked_leader_unbound,
                    "queued_until_leader_attach": counts.queued_until_leader_attach,
                },
                "attach_window_failures": {
                    "reason": "leader_not_attached",
                    "count": counts.blocked_leader_unbound,
                    "status": "requeued_pending_physical_retry",
                },
            }),
        )?;
    }
    Ok(counts)
}

/// `requeue_delivery_exhausted_watchers`: attach-leader 成功后把已经耗尽投递
/// 重试的 watcher 放回 notify_failed, 留给 coordinator tick 重试投递。
///
/// 0.5.5 gate054 round-2: also requeue direct leader messages that were refused
/// with `rebind_required` (status=failed / error=leader_not_attached). Without
/// this the direct `report_result` path leaves a failed row that
/// `deliver_pending_messages` never re-picks — the new leader would never see the
/// pending notification.
pub fn requeue_delivery_exhausted_watchers(
    workspace: &Path,
    store: &MessageStore,
    event_log: &EventLog,
    owner_team_id: &TeamKey,
    claimed_pane_id: &PaneId,
) -> Result<Vec<WatcherNotice>, MessagingError> {
    requeue_delivery_exhausted_watchers_with_counts(
        workspace,
        store,
        event_log,
        owner_team_id,
        claimed_pane_id,
    )
    .map(|(notices, _)| notices)
}

/// Attach-leader's typed recovery result. The counts come from the same
/// owner-team scoped attach-recovery call (the watcher and message updates
/// remain their existing transactions), so an attach caller can expose only
/// this operation's debt rather than scanning arbitrary historical events.
pub(crate) fn requeue_delivery_exhausted_watchers_with_counts(
    _workspace: &Path,
    store: &MessageStore,
    event_log: &EventLog,
    owner_team_id: &TeamKey,
    claimed_pane_id: &PaneId,
) -> Result<(Vec<WatcherNotice>, crate::message_store::BlockedLeaderRequeueCounts), MessagingError> {
    Err(MessagingError::Validation("legacy SQL messaging retired; use station send and C3 inbox".into()))
}

/// `delivered_result_message` (`result_delivery.py:394`):内容级去重 —— 查某 result_id 是否已有
/// 投递的 leader 通知消息。
pub(crate) fn delivered_result_message(
    store: &MessageStore,
    result_id: &str,
    task_id: Option<&TaskId>,
    owner_team_id: Option<&TeamKey>,
) -> Result<Option<serde_json::Value>, MessagingError> {
    Err(MessagingError::Validation("legacy SQL messaging retired; use station send and C3 inbox".into()))
}

fn message_content_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<(String, String)> {
    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
}

/// `result_id_from_text` (`result_delivery.py:415`):解析通知文案的 `Result id: <id>` 行做内容级
/// 去重。**格式字节级稳定** (golden fixture,card §53)。
pub fn result_id_from_text(content: &str) -> Option<String> {
    for line in content.lines() {
        if let Some(rest) = line.strip_prefix("Result id: ") {
            let id = rest.trim();
            if id.is_empty() {
                return None;
            }
            return Some(id.to_string());
        }
    }
    None
}

/// `format_result_watcher_notification` (`result_delivery.py:521`):拼 watcher 通知文案 +
/// `Result id: <id>` 行。**格式必须字节级稳定** (golden fixture,与 [`result_id_from_text`] 对偶)。
pub fn format_result_watcher_notification(result: &serde_json::Value) -> String {
    let task_id = result
        .get("task_id")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown task");
    let agent_id = result
        .get("agent_id")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown agent");
    let status = result
        .get("status")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    let summary = result
        .get("summary")
        .and_then(|v| v.as_str())
        .unwrap_or("completed");
    let mut lines = vec![format!(
        "Task {task_id} reported {status} from {agent_id}: {summary}"
    )];
    if let Some(tests) = result.get("tests").and_then(|v| v.as_array()) {
        let rendered: Vec<String> = tests
            .iter()
            .take(3)
            .filter_map(|test| {
                let command = test.get("command").and_then(|v| v.as_str())?;
                let status = test.get("status").and_then(|v| v.as_str())?;
                Some(format!("{command}={status}"))
            })
            .collect();
        if !rendered.is_empty() {
            lines.push(format!("Tests: {}", rendered.join("; ")));
        }
    }
    if let Some(result_id) = result.get("result_id").and_then(|v| v.as_str()) {
        if !result_id.is_empty() {
            lines.push(format!("Result id: {result_id}"));
        }
    }
    lines.push(
        "Team Agent has collected this result and updated team_state.md. No manual polling is needed."
            .to_string(),
    );
    lines.join("\n")
}
