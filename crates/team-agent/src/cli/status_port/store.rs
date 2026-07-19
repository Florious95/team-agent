use crate::cli::CliError;
use rusqlite::params;
use serde_json::{json, Map, Value};
use std::path::Path;

pub(super) fn recent_agent_messages(
    workspace: &Path,
    agent_id: &str,
) -> Result<Vec<Value>, CliError> {
    crate::message_store::MessageStore::open(workspace)
        .map_err(|e| CliError::Runtime(e.to_string()))?
        .inbox(agent_id, 3, None)
        .map_err(|e| CliError::Runtime(e.to_string()))
}

/// `latest_result_summaries`(`queries.py:83-89`)。
pub(super) fn latest_result_summaries(
    store: &crate::message_store::MessageStore,
    owner_team_id: Option<&str>,
) -> Result<Value, CliError> {
    let rows = store
        .latest_results(5, owner_team_id)
        .map_err(|e| CliError::Runtime(e.to_string()))?;
    Ok(Value::Array(
        rows.iter()
            .filter_map(crate::message_store::result_summary_from_row)
            .collect(),
    ))
}

pub(super) fn message_counts(
    conn: &rusqlite::Connection,
    owner_team_id: Option<&str>,
) -> Result<Value, CliError> {
    status_counts(conn, "messages", owner_team_id)
}

pub(super) fn result_counts(
    conn: &rusqlite::Connection,
    owner_team_id: Option<&str>,
) -> Result<Value, CliError> {
    let by_status = result_status_counts(conn, owner_team_id)?;
    let total = count_rows(conn, "results", owner_team_id)?;
    let invalid = count_where_status(conn, "results", owner_team_id, "invalid")?;
    let collected = count_where_status(conn, "results", owner_team_id, "collected")?;
    let uncollected = total.saturating_sub(collected).saturating_sub(invalid);
    Ok(json!({
        "total": total,
        "uncollected": uncollected,
        "collected": collected,
        "invalid": invalid,
        "by_status": by_status,
    }))
}

pub(super) fn status_counts(
    conn: &rusqlite::Connection,
    table: &str,
    owner_team_id: Option<&str>,
) -> Result<Value, CliError> {
    let sql = match owner_team_id {
        Some(_) => format!(
            "select status, count(*) from {table}
                 where owner_team_id = ?1
                 group by status order by status"
        ),
        None => format!("select status, count(*) from {table} group by status order by status"),
    };
    let mut stmt = conn
        .prepare(&sql)
        .map_err(|e| CliError::Runtime(e.to_string()))?;
    let mut rows = match owner_team_id {
        Some(team) => stmt
            .query(params![team])
            .map_err(|e| CliError::Runtime(e.to_string()))?,
        None => stmt
            .query([])
            .map_err(|e| CliError::Runtime(e.to_string()))?,
    };
    let mut out = Map::new();
    while let Some(row) = rows.next().map_err(|e| CliError::Runtime(e.to_string()))? {
        let status: String = row.get(0).map_err(|e| CliError::Runtime(e.to_string()))?;
        let count: i64 = row.get(1).map_err(|e| CliError::Runtime(e.to_string()))?;
        out.insert(status, json!(count));
    }
    Ok(Value::Object(out))
}

pub(super) fn result_status_counts(
    conn: &rusqlite::Connection,
    owner_team_id: Option<&str>,
) -> Result<Value, CliError> {
    let sql = match owner_team_id {
        Some(_) => {
            "select status, count(*) from results
                 where status not in ('collected', 'invalid') and owner_team_id = ?1
                 group by status
                 order by status"
        }
        None => {
            "select status, count(*) from results
                 where status not in ('collected', 'invalid')
                 group by status
                 order by status"
        }
    };
    let mut stmt = conn
        .prepare(sql)
        .map_err(|e| CliError::Runtime(e.to_string()))?;
    let mut rows = match owner_team_id {
        Some(team) => stmt
            .query(params![team])
            .map_err(|e| CliError::Runtime(e.to_string()))?,
        None => stmt
            .query([])
            .map_err(|e| CliError::Runtime(e.to_string()))?,
    };
    let mut out = Map::new();
    while let Some(row) = rows.next().map_err(|e| CliError::Runtime(e.to_string()))? {
        let status: String = row.get(0).map_err(|e| CliError::Runtime(e.to_string()))?;
        let count: i64 = row.get(1).map_err(|e| CliError::Runtime(e.to_string()))?;
        out.insert(status, json!(count));
    }
    Ok(Value::Object(out))
}

pub(super) fn queued_messages(
    conn: &rusqlite::Connection,
    owner_team_id: Option<&str>,
    limit: usize,
) -> Result<Value, CliError> {
    let limit = i64::try_from(limit).unwrap_or(i64::MAX);
    let sql = match owner_team_id {
        Some(_) => {
            "select message_id, recipient, status, created_at, delivery_attempts
                 from messages
                 where status like 'queued%' and owner_team_id = ?1
                 order by created_at desc
                 limit ?2"
        }
        None => {
            "select message_id, recipient, status, created_at, delivery_attempts
                 from messages
                 where status like 'queued%'
                 order by created_at desc
                 limit ?1"
        }
    };
    let mut stmt = conn
        .prepare(sql)
        .map_err(|e| CliError::Runtime(e.to_string()))?;
    let map_row = |row: &rusqlite::Row<'_>| {
        Ok(json!({
            "message_id": row.get::<_, String>(0)?,
            "recipient": row.get::<_, Option<String>>(1)?,
            "status": row.get::<_, String>(2)?,
            "created_at": row.get::<_, Option<String>>(3)?,
            "delivery_attempts": row.get::<_, i64>(4)?,
        }))
    };
    let rows = match owner_team_id {
        Some(team) => stmt.query_map(params![team, limit], map_row),
        None => stmt.query_map(params![limit], map_row),
    }
    .map_err(|e| CliError::Runtime(e.to_string()))?;
    let values = rows
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| CliError::Runtime(e.to_string()))?;
    Ok(Value::Array(values))
}

/// 0.5.5 gate054 round-2: leader notifications that were refused with
/// `rebind_required` (status=failed, error=leader_not_attached) sit as
/// failed rows in the store; without a dedicated status field the
/// operator sees only `messages.failed=N` and cannot tell that the
/// notifications are waiting for a rebind. This field surfaces them
/// alongside `queued_messages` so `attach-leader` / `takeover` is
/// visibly the fix. Once the pane is rebound the requeue path flips
/// each row back to `status=accepted` and it drops out of this list.
pub(super) fn pending_leader_notifications(
    conn: &rusqlite::Connection,
    owner_team_id: Option<&str>,
    limit: usize,
) -> Result<Value, CliError> {
    let limit = i64::try_from(limit).unwrap_or(i64::MAX);
    // E6 (0.5.9 offline-mailbox §6.6): also surface `queued_until_leader_attach`
    // rows (third-party sends into the leader mailbox) so the target owner can
    // see them alongside the rebind-required failures. Channel wire label
    // distinguishes the two so the operator can tell the two shapes apart.
    let sql = match owner_team_id {
        Some(_) => {
            "select message_id, sender, status, error, created_at, delivery_attempts
                 from messages
                 where recipient = 'leader'
                   and owner_team_id = ?1
                   and (
                     (status = 'failed' and error = 'leader_not_attached')
                     or status = 'queued_until_leader_attach'
                   )
                 order by created_at desc
                 limit ?2"
        }
        None => {
            "select message_id, sender, status, error, created_at, delivery_attempts
                 from messages
                 where recipient = 'leader'
                   and (
                     (status = 'failed' and error = 'leader_not_attached')
                     or status = 'queued_until_leader_attach'
                   )
                 order by created_at desc
                 limit ?1"
        }
    };
    let mut stmt = conn
        .prepare(sql)
        .map_err(|e| CliError::Runtime(e.to_string()))?;
    let map_row = |row: &rusqlite::Row<'_>| {
        let status: String = row.get(2)?;
        let channel = if status == "queued_until_leader_attach" {
            "leader_mailbox"
        } else {
            "rebind_required"
        };
        Ok(json!({
            "message_id": row.get::<_, String>(0)?,
            "sender": row.get::<_, Option<String>>(1)?,
            "status": status,
            "error": row.get::<_, Option<String>>(3)?,
            "created_at": row.get::<_, Option<String>>(4)?,
            "delivery_attempts": row.get::<_, i64>(5)?,
            "channel": channel,
            "action": "run team-agent attach-leader or team-agent takeover",
        }))
    };
    let rows = match owner_team_id {
        Some(team) => stmt.query_map(params![team, limit], map_row),
        None => stmt.query_map(params![limit], map_row),
    }
    .map_err(|e| CliError::Runtime(e.to_string()))?;
    let values = rows
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| CliError::Runtime(e.to_string()))?;
    Ok(Value::Array(values))
}

/// 0.4.x: slim default compact payload — exactly 7 top-level fields.
/// Diagnostic detail moves to `--detail`. Plan:
/// /Users/alauda/Documents/code/team-agent-public/.team/artifacts/status-compact-plan.md
pub(super) fn count_rows(
    conn: &rusqlite::Connection,
    table: &str,
    owner_team_id: Option<&str>,
) -> Result<i64, CliError> {
    match owner_team_id {
        Some(team) => {
            let sql = format!("select count(*) from {table} where owner_team_id = ?1");
            conn.query_row(&sql, [team], |row| row.get::<_, i64>(0))
                .map_err(|e| CliError::Runtime(e.to_string()))
        }
        None => {
            let sql = format!("select count(*) from {table}");
            conn.query_row(&sql, [], |row| row.get::<_, i64>(0))
                .map_err(|e| CliError::Runtime(e.to_string()))
        }
    }
}

pub(super) fn count_where_status(
    conn: &rusqlite::Connection,
    table: &str,
    owner_team_id: Option<&str>,
    status: &str,
) -> Result<i64, CliError> {
    match owner_team_id {
        Some(team) => {
            let sql =
                format!("select count(*) from {table} where status = ?1 and owner_team_id = ?2");
            conn.query_row(&sql, params![status, team], |row| row.get::<_, i64>(0))
                .map_err(|e| CliError::Runtime(e.to_string()))
        }
        None => {
            let sql = format!("select count(*) from {table} where status = ?1");
            conn.query_row(&sql, [status], |row| row.get::<_, i64>(0))
                .map_err(|e| CliError::Runtime(e.to_string()))
        }
    }
}

pub(super) fn agent_health(
    conn: &rusqlite::Connection,
    owner_team_id: Option<&str>,
) -> Result<Value, CliError> {
    let sql = match owner_team_id {
            Some(_) => {
                "select agent_id, status, last_output_at, context_usage_pct, current_task_id, updated_at, owner_team_id
                 from agent_health where owner_team_id = ?1 order by agent_id"
            }
            None => {
                "select agent_id, status, last_output_at, context_usage_pct, current_task_id, updated_at, owner_team_id
                 from agent_health order by agent_id"
            }
        };
    let mut stmt = conn
        .prepare(sql)
        .map_err(|e| CliError::Runtime(e.to_string()))?;
    let mut rows = match owner_team_id {
        Some(team) => stmt
            .query(params![team])
            .map_err(|e| CliError::Runtime(e.to_string()))?,
        None => stmt
            .query([])
            .map_err(|e| CliError::Runtime(e.to_string()))?,
    };
    let mut out = Map::new();
    while let Some(row) = rows.next().map_err(|e| CliError::Runtime(e.to_string()))? {
        let agent_id: String = row.get(0).map_err(|e| CliError::Runtime(e.to_string()))?;
        let status: String = row.get(1).map_err(|e| CliError::Runtime(e.to_string()))?;
        let mut item = Map::new();
        item.insert("status".to_string(), json!(status));
        item.insert(
            "health_status".to_string(),
            json!(crate::provider::agent_health_status(
                item.get("status").and_then(Value::as_str).unwrap_or("")
            )),
        );
        insert_optional_string(
            &mut item,
            "last_output_at",
            row.get(2).map_err(|e| CliError::Runtime(e.to_string()))?,
        );
        insert_optional_i64(
            &mut item,
            "context_usage_pct",
            row.get(3).map_err(|e| CliError::Runtime(e.to_string()))?,
        );
        let current_task_id: Option<String> =
            row.get(4).map_err(|e| CliError::Runtime(e.to_string()))?;
        let has_current_task = current_task_id.is_some();
        insert_optional_string(&mut item, "current_task_id", current_task_id);
        let updated_at: String = row.get(5).map_err(|e| CliError::Runtime(e.to_string()))?;
        // Phase-DX E2 (plan §4 / CR supplement A): expose the last agent_health
        // observation timestamp as `health_updated_at` alongside the legacy
        // `updated_at` alias. Two names for one column keep old scrapers working
        // while surfacing the semantic (heartbeat, not row bookkeeping).
        item.insert("updated_at".to_string(), json!(updated_at.clone()));
        item.insert("health_updated_at".to_string(), json!(updated_at));
        // Phase-DX E2 (CR P0 red line #6, supplements A/B): current_task is a
        // best-effort *display* field until A1 makes task FSM authoritative.
        // The structured source/confidence markers stop downstream code from
        // treating agent_health.current_task_id as authority. `current_task_source`
        // records where the display value came from (only "health" today —
        // Phase-DX never merges state tasks into this projection); the
        // `current_task_confidence` enum stays "best_effort" for the whole
        // Phase-DX slice (A1 will later flip it to "authoritative" when the
        // task FSM lands). Field is written unconditionally so consumers can
        // switch on it even when `current_task_id` is null.
        item.insert(
            "current_task_source".to_string(),
            json!(if has_current_task { "health" } else { "none" }),
        );
        item.insert("current_task_confidence".to_string(), json!("best_effort"));
        insert_optional_string(
            &mut item,
            "owner_team_id",
            row.get(6).map_err(|e| CliError::Runtime(e.to_string()))?,
        );
        out.insert(agent_id, Value::Object(item));
    }
    Ok(Value::Object(out))
}

pub(super) fn insert_optional_string(
    map: &mut Map<String, Value>,
    key: &str,
    value: Option<String>,
) {
    if let Some(value) = value {
        map.insert(key.to_string(), Value::String(value));
    }
}

pub(super) fn insert_optional_i64(map: &mut Map<String, Value>, key: &str, value: Option<i64>) {
    if let Some(value) = value {
        map.insert(key.to_string(), json!(value));
    }
}

/// B-5 / 036b N38 — status 出口的 runtime 块:把 coordinator_health 与
/// undelivered backlog 合体暴露。down-hint 只在【coordinator 不在跑 ∧ 有 backlog】
/// 两条件同时满足才挂(anti-nag);健康状态下不挂提示。auto-recovery 不做。
pub(super) fn count_undelivered_backlog(
    conn: &rusqlite::Connection,
    owner_team_id: Option<&str>,
) -> Result<i64, CliError> {
    // Backlog statuses chosen to mirror what `deliver_pending` would pick up.
    let sql = match owner_team_id {
            Some(_) => "select count(*) from messages
                       where owner_team_id = ?1 and status in ('accepted','pending','queued','queued_until_trust')",
            None => "select count(*) from messages
                     where status in ('accepted','pending','queued','queued_until_trust')",
        };
    let count: i64 = match owner_team_id {
        Some(team) => conn
            .query_row(sql, params![team], |row| row.get(0))
            .map_err(|e| CliError::Runtime(e.to_string()))?,
        None => conn
            .query_row(sql, [], |row| row.get(0))
            .map_err(|e| CliError::Runtime(e.to_string()))?,
    };
    Ok(count)
}
