//! result_delivery.py — watcher 通知/去重/有界重试/claim-leader requeue (card §69-71)。

use std::path::Path;

use rusqlite::{params, OptionalExtension};

use crate::event_log::EventLog;
use crate::message_store::{MessageStore, NotificationClaimParams};
use crate::model::ids::{TaskId, TeamKey};
use crate::transport::PaneId;

use super::{MessagingError, WatcherNotice, RESULT_DELIVERY_MAX_ATTEMPTS};

/// `notify_result_watchers` (`result_delivery.py:38`):匹配 + 去重 + (有界) 投递 result 给 leader
/// watcher。**恰好一次** (Gap 32/38):同 result_id 多 watcher → 1 次注入,余 `superseded`。
/// 去重唯一原语 = [`MessageStore::claim_leader_notification_delivery`]。`collect`/coordinator tick 调。
pub fn notify_result_watchers(
    workspace: &Path,
    result: &serde_json::Value,
    event_log: &EventLog,
    watchers: Option<&[serde_json::Value]>,
    dedupe_reason: Option<&str>,
) -> Result<Vec<WatcherNotice>, MessagingError> {
    let _ = dedupe_reason;
    let Some(watchers) = watchers else {
        return Ok(Vec::new());
    };
    let store = MessageStore::open(workspace)?;
    let conn = crate::db::schema::open_db(store.db_path())?;
    let result_task = result.get("task_id").and_then(|v| v.as_str());
    let result_agent = result.get("agent_id").and_then(|v| v.as_str());
    let result_id = result
        .get("result_id")
        .and_then(|v| v.as_str())
        .map(ToString::to_string);
    let mut matched: Vec<&serde_json::Value> = watchers
        .iter()
        .filter(|w| watcher_matches(w, result_task, result_agent))
        .collect();
    matched.sort_by(|a, b| {
        let ac = a.get("created_at").and_then(|v| v.as_str()).unwrap_or("");
        let bc = b.get("created_at").and_then(|v| v.as_str()).unwrap_or("");
        ac.cmp(bc)
    });
    let mut notices = Vec::new();
    let mut primary_watcher_id = None;
    for (idx, watcher) in matched.iter().enumerate() {
        if let Some(watcher_id) = watcher.get("watcher_id").and_then(|v| v.as_str()) {
            if idx == 0 {
                primary_watcher_id = Some(watcher_id.to_string());
                notices.push(deliver_primary_watcher(
                    workspace,
                    &conn,
                    &store,
                    event_log,
                    watcher,
                    result,
                    result_id.as_deref(),
                    result_task,
                )?);
            } else {
                let primary = primary_watcher_id.clone().unwrap_or_default();
                let error = "superseded by earlier watcher for same (task_id, agent_id, result_id)";
                let now = chrono::Utc::now().to_rfc3339();
                conn.execute(
                    "update result_watchers
                     set status = 'superseded', completed_at = ?2, result_id = ?3, error = ?4
                     where watcher_id = ?1",
                    params![watcher_id, now, result_id, error],
                )?;
                event_log.write(
                    "result_watcher.superseded",
                    serde_json::json!({
                        "watcher_id": watcher_id,
                        "result_id": result_id,
                        "task_id": result_task,
                        "agent_id": result_agent,
                        "primary_watcher_id": primary,
                    }),
                )?;
                notices.push(WatcherNotice {
                    watcher_id: watcher_id.to_string(),
                    result_id: result_id.clone(),
                    ok: false,
                    status: Some("superseded".to_string()),
                    notified_message_id: None,
                    primary_watcher_id: Some(primary),
                    prior_state: None,
                    error: Some(error.to_string()),
                });
            }
        }
    }
    Ok(notices)
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
        "leader",
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
    let store = MessageStore::open(workspace)?;
    let conn = crate::db::schema::open_db(store.db_path())?;
    // result_delivery.py:19-35 — retries route through notify_result_watchers (the REAL
    // delivery path with dedupe/attempt bounds); a watcher is never flipped to
    // `notified` without a delivery. Missing result rows are skipped (still retryable).
    let mut stmt = conn.prepare(
        "select watcher_id, owner_team_id, task_id, agent_id, leader_id, status, created_at,
                result_id, notified_message_id
         from result_watchers
         where status in ('pending', 'notify_failed')
         order by created_at, watcher_id",
    )?;
    let watchers = stmt
        .query_map([], |row| {
            Ok(serde_json::json!({
                "watcher_id": row.get::<_, String>(0)?,
                "owner_team_id": row.get::<_, Option<String>>(1)?,
                "task_id": row.get::<_, Option<String>>(2)?,
                "agent_id": row.get::<_, Option<String>>(3)?,
                "leader_id": row.get::<_, Option<String>>(4)?,
                "status": row.get::<_, Option<String>>(5)?,
                "created_at": row.get::<_, Option<String>>(6)?,
                "result_id": row.get::<_, Option<String>>(7)?,
                "notified_message_id": row.get::<_, Option<String>>(8)?,
            }))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    drop(stmt);
    let mut notices = Vec::new();
    for watcher in watchers {
        if watcher.get("status").and_then(|v| v.as_str()) != Some("notify_failed") {
            continue;
        }
        let Some(result_id) = watcher
            .get("result_id")
            .and_then(|v| v.as_str())
            .filter(|id| !id.is_empty())
            .map(ToString::to_string)
        else {
            continue;
        };
        let row: Option<(String, Option<String>)> = conn
            .query_row(
                "select envelope, created_at from results where result_id = ?1",
                params![result_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let Some((envelope, created_at)) = row else {
            continue;
        };
        let result = result_entry_from_row(&result_id, &envelope, created_at.as_deref())?;
        notices.extend(notify_result_watchers(
            workspace,
            &result,
            event_log,
            Some(std::slice::from_ref(&watcher)),
            Some("rebind_retry"),
        )?);
    }
    Ok(notices)
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
    let conn = crate::db::schema::open_db(store.db_path())?;
    let mut stmt = conn.prepare(
        "select watcher_id, result_id, status, coalesce(completed_at, created_at) from result_watchers
         where owner_team_id = ?1 and result_id is not null and notified_message_id is null
         order by created_at, watcher_id",
    )?;
    let rows = stmt.query_map(params![owner_team_id.as_str()], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, Option<String>>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
        ))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (watcher_id, result_id, prior_state, latest_ts) = row?;
        if incident_ts.is_some_and(|incident| latest_ts.as_str() < incident) {
            continue;
        }
        let requeued_at = chrono::Utc::now().to_rfc3339();
        conn.execute(
            "update result_watchers set status = 'notify_failed', completed_at = ?2 where watcher_id = ?1",
            params![watcher_id.as_str(), requeued_at.as_str()],
        )?;
        out.push(WatcherNotice {
            watcher_id: watcher_id.clone(),
            result_id: result_id.clone(),
            ok: true,
            status: Some("notify_failed".to_string()),
            notified_message_id: None,
            primary_watcher_id: None,
            prior_state: Some(prior_state.clone()),
            error: None,
        });
        event_log.write(
            "leader_receiver.claim_requeue",
            serde_json::json!({
                "result_id": result_id,
                "watcher_id": watcher_id,
                "prior_state": prior_state,
                "requeued_at": requeued_at,
                "claimed_pane_id": claimed_pane_id.as_str(),
                "team_id": owner_team_id.as_str(),
            }),
        )?;
    }
    let requeued_blocked =
        requeue_blocked_leader_messages(&conn, event_log, owner_team_id, claimed_pane_id)?;
    if !out.is_empty() || requeued_blocked > 0 {
        let _ = retry_result_deliveries(workspace, event_log)?;
    }
    Ok(out)
}

/// 0.5.5 gate054 round-2: attach-leader (and claim-leader) requeue for leader messages
/// that were refused with `rebind_required` while no leader pane was attached.
///
/// #231 C-5 semantics: same row, same message_id — flip an eligible status back
/// to `accepted` so `deliver_pending_messages` replays it through the SAME
/// pipeline. The `leader_notification_log` PK prevents a second notification
/// row for watcher-backed messages. A `submitted_pending_acceptance` row has
/// already crossed the transport boundary, so its recovery is intentionally
/// at-least-once: the stable message id/receipt token is preserved, but a replay
/// can repeat the physical submit if a same-pane claim races the receipt window.
pub(crate) fn requeue_blocked_leader_messages(
    conn: &rusqlite::Connection,
    event_log: &EventLog,
    owner_team_id: &TeamKey,
    claimed_pane_id: &PaneId,
) -> Result<usize, MessagingError> {
    // E6 (0.5.9 offline-mailbox §6.5): also requeue rows that a third-party
    // sender left in `queued_until_leader_attach` via the leader mailbox.
    // Same idempotent requeue funnel (row/message_id/leader_notification_log
    // PK are all preserved) — after attach/claim the coordinator's normal
    // `deliver_pending_messages` picks them up as `accepted` and injects
    // exactly once. status `queued_until_leader_attach` is deliberately NOT
    // in the `claim_for_delivery` eligible set (see message_store.rs) so it
    // could not have churned while the leader was unattached. In contrast,
    // `submitted_pending_acceptance` is an explicit at-least-once recovery arm:
    // claim convergence favors an eventual receipt over preserving an
    // unobservable in-flight submit.
    let requeued = conn.execute(
        "update messages
         set status = 'accepted',
             error = null,
             updated_at = ?2
         where recipient = 'leader'
           and owner_team_id = ?1
           and (
             (status = 'failed' and error = 'leader_not_attached')
             or status = 'queued_until_leader_attach'
           )",
        params![owner_team_id.as_str(), chrono::Utc::now().to_rfc3339()],
    )?;
    if requeued > 0 {
        event_log.write(
            "leader_receiver.blocked_messages_requeued",
            serde_json::json!({
                "team_id": owner_team_id.as_str(),
                "claimed_pane_id": claimed_pane_id.as_str(),
                "count": requeued,
            }),
        )?;
    }
    Ok(requeued)
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
    _workspace: &Path,
    store: &MessageStore,
    event_log: &EventLog,
    owner_team_id: &TeamKey,
    claimed_pane_id: &PaneId,
) -> Result<Vec<WatcherNotice>, MessagingError> {
    let conn = crate::db::schema::open_db(store.db_path())?;
    let mut stmt = conn.prepare(
        "select watcher_id, result_id, status from result_watchers
         where owner_team_id = ?1 and status = 'delivery_exhausted' and notified_message_id is null
         order by created_at, watcher_id",
    )?;
    let rows = stmt.query_map(params![owner_team_id.as_str()], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, Option<String>>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (watcher_id, result_id, prior_state) = row?;
        conn.execute(
            "update result_watchers set status = 'notify_failed', completed_at = null, error = null where watcher_id = ?1",
            params![watcher_id.as_str()],
        )?;
        out.push(WatcherNotice {
            watcher_id: watcher_id.clone(),
            result_id: result_id.clone(),
            ok: true,
            status: Some("notify_failed".to_string()),
            notified_message_id: None,
            primary_watcher_id: None,
            prior_state: Some(prior_state.clone()),
            error: None,
        });
        event_log.write(
            "result_watcher.requeued",
            serde_json::json!({
                "watcher_id": watcher_id,
                "trigger": "attach_leader",
                "new_pane_id": claimed_pane_id.as_str(),
            }),
        )?;
    }
    drop(stmt);
    let _ = requeue_blocked_leader_messages(&conn, event_log, owner_team_id, claimed_pane_id)?;
    Ok(out)
}

/// `delivered_result_message` (`result_delivery.py:394`):内容级去重 —— 查某 result_id 是否已有
/// 投递的 leader 通知消息。
pub fn delivered_result_message(
    store: &MessageStore,
    result_id: &str,
    task_id: Option<&TaskId>,
    owner_team_id: Option<&TeamKey>,
) -> Result<Option<serde_json::Value>, MessagingError> {
    if result_id.trim().is_empty() {
        return Ok(None);
    }
    let conn = crate::db::schema::open_db(store.db_path())?;
    let mut sql = "select message_id, content from messages
         where recipient = 'leader'
           and status in ('visible', 'submitted', 'submitted_unverified', 'delivered', 'acknowledged')"
        .to_string();
    if task_id.is_some() {
        sql.push_str(" and task_id = ?1");
    }
    if owner_team_id.is_some() {
        sql.push_str(if task_id.is_some() {
            " and owner_team_id = ?2"
        } else {
            " and owner_team_id = ?1"
        });
    }
    let mut stmt = conn.prepare(&sql)?;
    let rows: Vec<(String, String)> = match (task_id, owner_team_id) {
        (Some(task), Some(team)) => stmt
            .query_map(params![task.as_str(), team.as_str()], message_content_row)?
            .collect::<Result<Vec<_>, _>>()?,
        (Some(task), None) => stmt
            .query_map(params![task.as_str()], message_content_row)?
            .collect::<Result<Vec<_>, _>>()?,
        (None, Some(team)) => stmt
            .query_map(params![team.as_str()], message_content_row)?
            .collect::<Result<Vec<_>, _>>()?,
        (None, None) => stmt
            .query_map([], message_content_row)?
            .collect::<Result<Vec<_>, _>>()?,
    };
    for row in rows.into_iter().rev() {
        let (message_id, content) = row;
        if result_id_from_text(&content).as_deref() == Some(result_id) {
            return Ok(Some(
                serde_json::json!({"message_id": message_id, "content": content}),
            ));
        }
    }
    Ok(None)
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
