//!
//! Scheduled-event dispatch and coordinator stuck detection (card §18/§68/§73).

use std::path::Path;

use rusqlite::params;

use crate::event_log::EventLog;
use crate::message_store::MessageStore;
use crate::transport::{PaneId, Transport};

use super::delivery::{deliver_stored_message, handle_trust_retry_needed};
use super::helpers::{parse_scheduled_kind, status_wire};
use super::{MessagingError, ScheduledKind, TrustRetryPayload, TRUST_RETRY_MAX_ATTEMPTS};

/// `_fire_due_scheduled_events` (`scheduler.py:41`):coordinator tick 的调度器心脏 —— 分派到期
/// `send`/`health_ping`/`trust_retry` ([`ScheduledKind`] 穷尽 match),send 失败有界重试。
/// 返回 fired 的 scheduled_event id 列表。**daemon-path** (step 12 调) → Result。
pub fn fire_due_scheduled_events(
    workspace: &Path,
    store: &MessageStore,
    transport: &dyn Transport,
    event_log: &EventLog,
) -> Result<Vec<i64>, MessagingError> {
    let _ = transport;
    let conn = crate::db::schema::open_db(store.db_path())?;
    let due_events = {
        let mut stmt = conn.prepare(
            "select id, kind, target, payload_json from scheduled_events where status = 'pending' and due_at <= ?1 order by due_at, id",
        )?;
        let rows = stmt.query_map(params![chrono::Utc::now().to_rfc3339()], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?;
        rows.collect::<Result<Vec<_>, _>>()?
    };
    let mut fired = Vec::new();
    for (id, kind, target, payload_json) in due_events {
        // U1 #5: per-event isolation — one poison event (bad payload / fire error) must not
        // halt the whole pass. The fire body is confined to a closure; on Err we emit
        // `scheduler.event_failed`, mark the row terminal 'failed' (no re-fire), and continue.
        let fire_one = || -> Result<serde_json::Value, MessagingError> {
            let scheduled_kind = parse_scheduled_kind(&kind)?;
            let result = match scheduled_kind {
                ScheduledKind::Send => {
                    let payload: serde_json::Value = serde_json::from_str(&payload_json)?;
                    let outcome = deliver_stored_message(
                        workspace,
                        Some(&target),
                        payload
                            .get("content")
                            .and_then(|v| v.as_str())
                            .unwrap_or(""),
                        None,
                        payload
                            .get("sender")
                            .and_then(|v| v.as_str())
                            .unwrap_or("leader"),
                        payload
                            .get("requires_ack")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false),
                        payload
                            .get("wait_visible")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false),
                        payload
                            .get("timeout")
                            .and_then(|v| v.as_f64())
                            .unwrap_or(30.0),
                        None,
                    )?;
                    serde_json::json!({
                        "ok": outcome.ok,
                        "status": status_wire(outcome.status),
                        "message_id": outcome.message_id,
                    })
                }
                ScheduledKind::HealthPing => {
                    event_log.write(
                        "scheduled.health_ping",
                        serde_json::json!({"event_id": id, "target": target}),
                    )?;
                    serde_json::json!({"ok": true, "status": "logged"})
                }
                ScheduledKind::TrustRetry => {
                    let payload: serde_json::Value = serde_json::from_str(&payload_json)?;
                    let outcome = handle_trust_retry_needed(
                        store,
                        &TrustRetryPayload {
                            message_id: payload
                                .get("message_id")
                                .and_then(|v| v.as_str())
                                .unwrap_or_default()
                                .to_string(),
                            attempt: payload
                                .get("attempt")
                                .and_then(|v| v.as_u64())
                                .and_then(|v| u8::try_from(v).ok())
                                .unwrap_or(1),
                            max_attempts: payload
                                .get("max_attempts")
                                .and_then(|v| v.as_u64())
                                .and_then(|v| u8::try_from(v).ok())
                                .unwrap_or(TRUST_RETRY_MAX_ATTEMPTS),
                            first_target: PaneId::new(
                                payload
                                    .get("first_target")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or(""),
                            ),
                        },
                        event_log,
                    )?;
                    serde_json::json!({"ok": outcome.ok, "status": status_wire(outcome.status)})
                }
            };
            Ok(result)
        };

        let (status, result_json) = match fire_one() {
            Ok(result) => {
                let ok = result
                    .get("ok")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false);
                (if ok { "done" } else { "failed" }, result.to_string())
            }
            Err(error) => {
                // Poison event: loud + terminal, never halts the pass or re-fires.
                let _ = event_log.write(
                    "scheduler.event_failed",
                    serde_json::json!({
                        "event_id": id,
                        "kind": kind,
                        "error": error.to_string(),
                    }),
                );
                (
                    "failed",
                    serde_json::json!({"ok": false, "status": "failed", "error": error.to_string()})
                        .to_string(),
                )
            }
        };
        // The row-status write stays `?`: a DB write failure is a real store fault, not a
        // per-event poison, and should surface (consistent with the rest of the fn).
        conn.execute(
            "update scheduled_events set status = ?2, fired_at = ?3, result_json = ?4 where id = ?1",
            params![id, status, chrono::Utc::now().to_rfc3339(), result_json],
        )?;
        fired.push(id);
    }
    Ok(fired)
}

/// `_detect_stuck_agents` (`scheduler.py:146`):stuck 检测。**§84 守门**:`_agent_has_stuck_relevant_work`
/// 仅在有 active task / inbound message 时推送,无 pending obligation 时绝不注入探索性 prompt。
pub fn detect_stuck_agents(
    workspace: &Path,
    state: &serde_json::Value,
    store: &MessageStore,
    event_log: &EventLog,
) -> Result<Vec<String>, MessagingError> {
    let _ = (workspace, event_log);
    let team = crate::state::projection::team_state_key(state);
    let conn = crate::db::schema::open_db(store.db_path())?;
    let mut stmt = conn.prepare(
        "select agent_id, last_output_at from agent_health
         where upper(status) in ('RUNNING', 'WORKING')
           and owner_team_id = ?1
           and last_output_at is not null
           and exists (
             select 1 from messages
              where recipient = agent_health.agent_id
                and status in (
                  'pending', 'accepted', 'queued_until_idle', 'queued_until_start',
                  'queued_stopped', 'queued_pane_missing', 'target_resolved',
                  'injected', 'visible', 'submitted', 'submitted_unverified', 'delivered'
                )
           )
         order by agent_id",
    )?;
    let rows = stmt.query_map(params![team], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    let mut stuck = Vec::new();
    for row in rows {
        let (agent_id, last_output_at) = row?;
        if output_is_stale(&last_output_at, 300) {
            stuck.push(agent_id);
        }
    }
    Ok(stuck)
}

fn output_is_stale(last_output_at: &str, timeout_seconds: i64) -> bool {
    let Ok(ts) = chrono::DateTime::parse_from_rfc3339(last_output_at) else {
        return false;
    };
    chrono::Utc::now()
        .signed_duration_since(ts.with_timezone(&chrono::Utc))
        .num_seconds()
        >= timeout_seconds
}
