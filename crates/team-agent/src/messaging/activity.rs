//! idle_alerts.py — idle/deadlock 检测 (team-scoped 抑制 + last-fire 节流;card §23) +
//! activity_detector.py — classifier 产物 (card §24)。
//! §11 IRON LAW (bug-071/077/085):无信号 → Uncertain,绝不 Idle。

use std::path::Path;

use rusqlite::params;
use serde_json::Value;

use crate::event_log::EventLog;
use crate::message_store::MessageStore;
use crate::model::ids::TeamKey;

use super::delivery::deliver_stored_message;
use super::helpers::{
    latest_prompt_signal, non_provider_command, recent_rfc3339, stale_rfc3339, working_seconds,
};
use super::{ActivityStatus, AgentActivity, MessagingError};

// ===========================================================================
// idle_alerts.py — idle/deadlock 检测 (team-scoped 抑制 + last-fire 节流;card §23)
// ===========================================================================

/// `detect_idle_fallbacks` (`idle_alerts.py:286`):team-scoped idle fallback 检测 (抑制 + 团队级
/// last-fire 节流)。coordinator tick (step 12) 调。daemon-path → Result。
pub fn detect_idle_fallbacks(
    workspace: &Path,
    state: &serde_json::Value,
    store: &MessageStore,
    event_log: &EventLog,
) -> Result<Vec<serde_json::Value>, MessagingError> {
    let team = active_team_key(workspace, state);
    if team_idle_acknowledged(state, &team) {
        return Ok(Vec::new());
    }
    if team_idle_fallback_debounced(state, &team) {
        return Ok(Vec::new());
    }
    let workers = state
        .get("agents")
        .and_then(Value::as_object)
        .map(|agents| agents.keys().cloned().collect::<Vec<_>>())
        .unwrap_or_default();
    if workers.is_empty() {
        return Ok(Vec::new());
    }
    let conn = crate::db::schema::open_db(store.db_path())?;
    let obligation_count = team_undelivered_obligations(&conn, &team, &workers)?;
    if obligation_count == 0 {
        return Ok(Vec::new());
    }
    let mut stmt = conn.prepare(
        "select agent_id
           from agent_health
          where (owner_team_id = ?1 or owner_team_id is null)
            and upper(status) = 'IDLE'
          order by agent_id",
    )?;
    let idle_workers = stmt
        .query_map(params![team.as_str()], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    let worker_set = workers
        .iter()
        .cloned()
        .collect::<std::collections::BTreeSet<_>>();
    let idle_set = idle_workers
        .iter()
        .cloned()
        .collect::<std::collections::BTreeSet<_>>();
    if worker_set != idle_set {
        return Ok(Vec::new());
    }
    for agent_id in &idle_workers {
        if idle_fallback_suppressed(state, store, &team, agent_id)? {
            return Ok(Vec::new());
        }
    }
    let now = chrono::Utc::now().to_rfc3339();
    let mut next_state = state.clone();
    let suppression_snapshots =
        idle_fallback_suppression_snapshots(&next_state, store, &team, &idle_workers)?;
    register_idle_fallback_suppression(&mut next_state, &team, &now, &suppression_snapshots);
    crate::state::repository::StateRepository::new(workspace).save_reapplying(
        crate::state::repository::StateWriteIntent::MessagingTurnArm {
            owner_team_id: Some(&team),
        },
        &next_state,
        |latest| {
            register_idle_fallback_suppression(latest, &team, &now, &suppression_snapshots);
        },
    )?;
    let alert_count = idle_workers.len();
    let content = format!(
        "Idle fallback: all workers idle while {obligation_count} undelivered obligation(s) remain."
    );
    let team_key = TeamKey::new(team.clone());
    let _ = deliver_stored_message(
        workspace,
        Some("leader"),
        &content,
        None,
        "coordinator",
        false,
        false,
        30.0,
        Some(&team_key),
    )?;
    let alert = serde_json::json!({
        "team": team,
        "idle_workers": idle_workers,
        "obligation_count": obligation_count,
        "alert_count": alert_count,
    });
    event_log.write("coordinator.idle_fallback", alert.clone())?;
    Ok(vec![alert])
}

/// `detect_cross_worker_deadlocks` (`idle_alerts.py:373`):跨 worker 死锁检测。coordinator tick 调。
pub fn detect_cross_worker_deadlocks(
    workspace: &Path,
    state: &serde_json::Value,
    store: &MessageStore,
    event_log: &EventLog,
) -> Result<Vec<serde_json::Value>, MessagingError> {
    let _ = (workspace, event_log);
    let team = crate::state::projection::team_state_key(state);
    let conn = crate::db::schema::open_db(store.db_path())?;
    let mut stmt = conn.prepare(
        "select distinct h.agent_id
           from agent_health h
          where h.owner_team_id = ?1
            and upper(h.status) = 'IDLE'
            and exists (
                select 1 from messages m
                 where m.recipient = h.agent_id
                   and m.status in (
                       'pending', 'accepted', 'queued_until_idle', 'queued_until_start',
                       'queued_stopped', 'queued_pane_missing', 'target_resolved',
                       'injected', 'visible', 'submitted', 'submitted_unverified', 'delivered'
                   )
            )
          order by h.agent_id",
    )?;
    let rows = stmt.query_map(params![team], |row| row.get::<_, String>(0))?;
    let mut alerts = Vec::new();
    for row in rows {
        let agent_id = row?;
        let alert = serde_json::json!({
            "alert_type": "cross_worker_deadlock",
            "agent_id": agent_id,
            "reason": "idle_recipient_with_undelivered_message",
        });
        event_log.write("cross_worker_deadlock", alert.clone())?;
        alerts.push(alert);
    }
    Ok(alerts)
}

// ===========================================================================
// activity_detector.py — classifier 产物 (step 8 recognizer 消费产物;card §24)
// ===========================================================================

/// `classify_agent_activity` (`activity_detector.py:90`):scrollback + pane 状态 → [`AgentActivity`]。
/// **铁律 (bug-071/077/085)**:无决定性信号 → [`ActivityStatus::Uncertain`] (confidence 0.5),
/// **绝不** fallthrough 成 idle。step 11/12 用它判 working/idle/stuck/uncertain。
pub fn classify_agent_activity(
    state: &serde_json::Value,
    scrollback: &str,
    pane_in_mode: bool,
    current_command: Option<&str>,
    last_output_at: Option<&str>,
) -> AgentActivity {
    let _ = state;
    if pane_in_mode {
        return AgentActivity {
            status: ActivityStatus::Uncertain,
            confidence: 0.9,
            rationale: "pane_in_mode".to_string(),
        };
    }
    if let Some(command) = current_command.and_then(non_provider_command) {
        return AgentActivity {
            status: ActivityStatus::Uncertain,
            confidence: 0.75,
            rationale: format!("current_command:{command}"),
        };
    }
    if let Some(seconds) = working_seconds(scrollback) {
        if seconds >= 300 {
            return AgentActivity {
                status: ActivityStatus::Stuck,
                confidence: 0.85,
                rationale: "stale_working_indicator".to_string(),
            };
        }
    }
    if let Some(signal) = latest_prompt_signal(scrollback) {
        return signal;
    }
    if let Some(last_output_at) = last_output_at {
        if stale_rfc3339(last_output_at, 300) {
            return AgentActivity {
                status: ActivityStatus::Stuck,
                confidence: 0.85,
                rationale: "stale_last_output".to_string(),
            };
        }
        if recent_rfc3339(last_output_at, 120) {
            return AgentActivity {
                status: ActivityStatus::Working,
                confidence: 0.7,
                rationale: "recent_provider_output".to_string(),
            };
        }
    }
    AgentActivity {
        status: ActivityStatus::Uncertain,
        confidence: 0.5,
        rationale: "no_decisive_signal".to_string(),
    }
}

fn active_team_key(workspace: &Path, state: &Value) -> String {
    state
        .get("active_team_key")
        .and_then(Value::as_str)
        .filter(|team| !team.is_empty())
        .map(ToString::to_string)
        .or_else(|| {
            workspace
                .file_name()
                .map(|name| name.to_string_lossy().to_string())
        })
        .unwrap_or_else(|| "current".to_string())
}

fn team_idle_acknowledged(state: &Value, team: &str) -> bool {
    state
        .get("coordinator")
        .and_then(|v| v.get("idle_acknowledged"))
        .and_then(|v| v.get(team))
        .and_then(|v| v.get("expires_at"))
        .and_then(Value::as_str)
        .is_some_and(timestamp_in_future)
}

fn team_idle_fallback_debounced(state: &Value, team: &str) -> bool {
    let Some(ts) = state
        .get("coordinator")
        .and_then(|v| v.get("idle_fallback_last_fired_at"))
        .and_then(|v| v.get(team))
        .and_then(Value::as_str)
    else {
        return false;
    };
    let Ok(parsed) = chrono::DateTime::parse_from_rfc3339(ts) else {
        return false;
    };
    chrono::Utc::now()
        .signed_duration_since(parsed.with_timezone(&chrono::Utc))
        .num_seconds()
        < 300
}

fn team_undelivered_obligations(
    conn: &rusqlite::Connection,
    team: &str,
    workers: &[String],
) -> Result<i64, MessagingError> {
    let mut count = 0;
    let mut stmt = conn.prepare(
        "select count(*) from messages
         where (owner_team_id = ?1 or owner_team_id is null)
           and recipient = ?2
           and status in (
               'pending', 'accepted', 'queued_until_idle', 'queued_until_start',
               'queued_stopped', 'queued_pane_missing', 'target_resolved',
               'injected', 'visible', 'submitted', 'submitted_unverified', 'delivered'
           )",
    )?;
    for worker in workers {
        count += stmt.query_row(params![team, worker], |row| row.get::<_, i64>(0))?;
    }
    Ok(count)
}

fn idle_fallback_suppressed(
    state: &Value,
    store: &MessageStore,
    team: &str,
    agent_id: &str,
) -> Result<bool, MessagingError> {
    let Some(entry) = state
        .get("coordinator")
        .and_then(|v| v.get("suppressed_idle_alerts"))
        .and_then(|v| v.get(team))
        .and_then(|v| v.get(agent_id))
        .and_then(|v| v.get("idle_fallback"))
    else {
        return Ok(false);
    };
    if entry
        .get("manual_acknowledge")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return Ok(entry
            .get("expires_at")
            .and_then(Value::as_str)
            .is_some_and(timestamp_in_future));
    }
    let assigned = string_array_at(entry, &["snapshot", "assigned_task_ids"]);
    let delivered = string_array_at(entry, &["snapshot", "delivered_message_ids"]);
    Ok(assigned == assigned_task_ids(state, agent_id)
        && delivered == delivered_message_ids(store, team, agent_id)?)
}

fn idle_fallback_suppression_snapshots(
    state: &Value,
    store: &MessageStore,
    team: &str,
    idle_workers: &[String],
) -> Result<Vec<(String, Vec<String>, Vec<String>)>, MessagingError> {
    idle_workers
        .iter()
        .map(|agent_id| {
            Ok((
                agent_id.clone(),
                assigned_task_ids(state, agent_id),
                delivered_message_ids(store, team, agent_id)?,
            ))
        })
        .collect::<Result<Vec<_>, MessagingError>>()
}

fn register_idle_fallback_suppression(
    state: &mut Value,
    team: &str,
    now: &str,
    snapshots: &[(String, Vec<String>, Vec<String>)],
) {
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
    let Some(last) = coordinator
        .entry("idle_fallback_last_fired_at")
        .or_insert_with(|| serde_json::json!({}))
        .as_object_mut()
    else {
        return;
    };
    last.insert(team.to_string(), serde_json::Value::String(now.to_string()));
    let Some(all) = coordinator
        .entry("suppressed_idle_alerts")
        .or_insert_with(|| serde_json::json!({}))
        .as_object_mut()
    else {
        return;
    };
    let Some(team_map) = all
        .entry(team.to_string())
        .or_insert_with(|| serde_json::json!({}))
        .as_object_mut()
    else {
        return;
    };
    for (agent_id, assigned, delivered) in snapshots {
        let Some(agent_map) = team_map
            .entry(agent_id.clone())
            .or_insert_with(|| serde_json::json!({}))
            .as_object_mut()
        else {
            continue;
        };
        agent_map.insert(
            "idle_fallback".to_string(),
            serde_json::json!({
                "suppressed_at": now,
                "suppressed_by": "idle_fallback",
                "snapshot": {
                    "assigned_task_ids": assigned,
                    "delivered_message_ids": delivered,
                },
            }),
        );
    }
}

fn timestamp_in_future(ts: &str) -> bool {
    let Ok(parsed) = chrono::DateTime::parse_from_rfc3339(ts) else {
        return false;
    };
    parsed.with_timezone(&chrono::Utc) > chrono::Utc::now()
}

fn string_array_at(value: &Value, path: &[&str]) -> Vec<String> {
    let mut current = value;
    for key in path {
        let Some(next) = current.get(*key) else {
            return Vec::new();
        };
        current = next;
    }
    let mut out = current
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(ToString::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    out.sort();
    out
}

fn assigned_task_ids(state: &Value, agent_id: &str) -> Vec<String> {
    let mut ids = state
        .get("tasks")
        .and_then(Value::as_array)
        .map(|tasks| {
            tasks
                .iter()
                .filter(|task| task.get("assignee").and_then(Value::as_str) == Some(agent_id))
                .filter_map(|task| {
                    task.get("id")
                        .and_then(Value::as_str)
                        .map(ToString::to_string)
                })
                .collect::<Vec<_>>()
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
