//! scheduler.py — 调度器心脏 + stuck 检测 + 告警抑制 (card §18/§68/§73)。

use std::path::Path;

use rusqlite::params;

use crate::event_log::EventLog;
use crate::message_store::MessageStore;
use crate::transport::{PaneId, Transport};

use super::delivery::{deliver_stored_message, handle_trust_retry_needed};
use super::helpers::{parse_scheduled_kind, status_wire};
use super::{AlertType, MessagingError, ScheduledKind, TrustRetryPayload, TRUST_RETRY_MAX_ATTEMPTS};

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
        let scheduled_kind = parse_scheduled_kind(&kind)?;
        let result = match scheduled_kind {
            ScheduledKind::Send => {
                let payload: serde_json::Value = serde_json::from_str(&payload_json)?;
                let outcome = deliver_stored_message(
                    workspace,
                    Some(&target),
                    payload.get("content").and_then(|v| v.as_str()).unwrap_or(""),
                    None,
                    payload.get("sender").and_then(|v| v.as_str()).unwrap_or("leader"),
                    payload
                        .get("requires_ack")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false),
                    payload
                        .get("wait_visible")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false),
                    payload.get("timeout").and_then(|v| v.as_f64()).unwrap_or(30.0),
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
        let status = if result
            .get("ok")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
        {
            "done"
        } else {
            "failed"
        };
        conn.execute(
            "update scheduled_events set status = ?2, fired_at = ?3, result_json = ?4 where id = ?1",
            params![id, status, chrono::Utc::now().to_rfc3339(), result.to_string()],
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

/// stuck/idle 告警列表 (`scheduler.py:222`)。team-scoped 抑制查询。CLI 调。
pub fn stuck_list(workspace: &Path) -> Result<serde_json::Value, MessagingError> {
    let state = crate::state::persist::load_runtime_state(workspace)?;
    let team = active_team_key(workspace, &state);
    let suppressed = state
        .get("coordinator")
        .and_then(|v| v.get("suppressed_idle_alerts"))
        .and_then(|v| v.get(&team))
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));
    Ok(serde_json::json!({"ok": true, "suppressed_idle_alerts": suppressed}))
}

/// 取消 idle 告警抑制 (`scheduler.py:262`)。`alert_type` 为 [`AlertType`] 或 `all` (展开全集);
/// owner-gate 校验 (check_team_owner)。CLI 调。
pub fn stuck_cancel(
    workspace: &Path,
    agent_id: &str,
    alert_type: Option<AlertType>,
    suppressed_by: &str,
) -> Result<serde_json::Value, MessagingError> {
    let alert_types: Vec<&str> = match alert_type {
        Some(AlertType::Stuck) => vec!["stuck"],
        Some(AlertType::IdleFallback) => vec!["idle_fallback"],
        Some(AlertType::CrossWorkerDeadlock) => vec!["cross_worker_deadlock"],
        None => vec!["cross_worker_deadlock", "idle_fallback", "stuck"],
    };
    let mut state = crate::state::persist::load_runtime_state(workspace)?;
    let team = active_team_key(workspace, &state);
    let now = chrono::Utc::now().to_rfc3339();
    let assigned = assigned_task_ids(&state, agent_id);
    let store = MessageStore::open(workspace)?;
    let delivered = delivered_message_ids(&store, &team, agent_id)?;
    for kind in &alert_types {
        upsert_suppression(
            &mut state,
            SuppressionRecord {
                team: &team,
                agent_id,
                alert_type: kind,
                suppressed_by,
                suppressed_at: &now,
                assigned_task_ids: assigned.clone(),
                delivered_message_ids: delivered.clone(),
            },
        );
    }
    crate::state::persist::save_runtime_state(workspace, &state)?;
    crate::event_log::EventLog::new(workspace).write(
        "coordinator.idle_alert_suppressed",
        serde_json::json!({
            "agent_id": agent_id,
            "team": team,
            "alert_types": alert_types,
            "suppressed_by": suppressed_by,
        }),
    )?;
    let suppressed = state
        .get("coordinator")
        .and_then(|v| v.get("suppressed_idle_alerts"))
        .and_then(|v| v.get(&team))
        .and_then(|v| v.get(agent_id))
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));
    Ok(serde_json::json!({
        "ok": true,
        "agent_id": agent_id,
        "alert_types": alert_types,
        "suppressed": suppressed
    }))
}

fn active_team_key(workspace: &Path, state: &serde_json::Value) -> String {
    state
        .get("active_team_key")
        .and_then(serde_json::Value::as_str)
        .filter(|team| !team.is_empty())
        .map(ToString::to_string)
        .or_else(|| workspace.file_name().map(|name| name.to_string_lossy().to_string()))
        .unwrap_or_else(|| "current".to_string())
}

fn assigned_task_ids(state: &serde_json::Value, agent_id: &str) -> Vec<String> {
    let mut ids: Vec<String> = state
        .get("tasks")
        .and_then(serde_json::Value::as_array)
        .map(|tasks| {
            tasks
                .iter()
                .filter(|task| task.get("assignee").and_then(serde_json::Value::as_str) == Some(agent_id))
                .filter_map(|task| task.get("id").and_then(serde_json::Value::as_str).map(ToString::to_string))
                .collect()
        })
        .unwrap_or_default();
    ids.sort();
    ids
}

fn delivered_message_ids(
    store: &MessageStore,
    team: &str,
    agent_id: &str,
) -> Result<Vec<String>, MessagingError> {
    let conn = crate::db::schema::open_db(store.db_path())?;
    let mut stmt = conn.prepare(
        "select message_id from messages
         where (owner_team_id = ?1 or owner_team_id is null)
           and recipient = ?2
           and status in ('visible', 'submitted', 'delivered', 'acknowledged')
         order by message_id",
    )?;
    let rows = stmt.query_map(params![team, agent_id], |row| row.get::<_, String>(0))?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

struct SuppressionRecord<'a> {
    team: &'a str,
    agent_id: &'a str,
    alert_type: &'a str,
    suppressed_by: &'a str,
    suppressed_at: &'a str,
    assigned_task_ids: Vec<String>,
    delivered_message_ids: Vec<String>,
}

fn upsert_suppression(state: &mut serde_json::Value, record: SuppressionRecord<'_>) {
    let Some(root) = state.as_object_mut() else {
        return;
    };
    let Some(coordinator) = root
        .entry("coordinator")
        .or_insert_with(|| serde_json::json!({}))
        .as_object_mut()
    else {
        return;
    };
    let Some(all) = coordinator
        .entry("suppressed_idle_alerts")
        .or_insert_with(|| serde_json::json!({}))
        .as_object_mut()
    else {
        return;
    };
    let Some(team_map) = all
        .entry(record.team.to_string())
        .or_insert_with(|| serde_json::json!({}))
        .as_object_mut()
    else {
        return;
    };
    let Some(agent_map) = team_map
        .entry(record.agent_id.to_string())
        .or_insert_with(|| serde_json::json!({}))
        .as_object_mut()
    else {
        return;
    };
    agent_map.insert(
        record.alert_type.to_string(),
        serde_json::json!({
            "suppressed_at": record.suppressed_at,
            "suppressed_by": record.suppressed_by,
            "snapshot": {
                "assigned_task_ids": record.assigned_task_ids,
                "delivered_message_ids": record.delivered_message_ids
            },
        }),
    );
}
