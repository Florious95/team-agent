//! Phase-DX E2: `agent_health` row capture/restore for the remove-agent flow.
//!
//! Extracted from `lifecycle/restart/remove.rs` so the SQL column reference to
//! `current_task_id` sits in the persistence layer (whitelisted by the E2 grep
//! guard) rather than in lifecycle policy code. Semantics are the golden Python
//! `_capture_agent_health` / `_restore_agent_health` — a plain
//! backup-across-delete that never treats the stored column as authoritative
//! task state.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::Path;

use crate::model::ids::AgentId;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CapturedHealth {
    pub owner_team_id: Option<String>,
    pub status: Option<String>,
    pub last_output_at: Option<String>,
    pub context_usage_pct: Option<i64>,
    pub current_task_id: Option<String>,
    pub updated_at: String,
}

/// golden agents.py:185 `copy.deepcopy(store.agent_health().get(agent_id))` — read the row BEFORE
/// delete so the rollback can re-upsert it. Returns the captured columns, or `None` if the row is
/// absent.
pub fn select_agent_health(
    workspace: &Path,
    owner_team_id: &str,
    agent_id: &AgentId,
) -> Result<Option<CapturedHealth>, crate::db::DbError> {
    let store = crate::message_store::MessageStore::open(workspace)
        .map_err(|e| crate::db::DbError::Schema(e.to_string()))?;
    let conn = crate::db::schema::open_db(store.db_path())?;
    let row = conn
        .query_row(
            "select owner_team_id, status, last_output_at, context_usage_pct, current_task_id, updated_at \
             from agent_health where owner_team_id = ?1 and agent_id = ?2",
            rusqlite::params![owner_team_id, agent_id.as_str()],
            |r| {
                Ok(CapturedHealth {
                    owner_team_id: r.get::<_, Option<String>>(0)?,
                    status: r.get::<_, Option<String>>(1)?,
                    last_output_at: r.get::<_, Option<String>>(2)?,
                    context_usage_pct: r.get::<_, Option<i64>>(3)?,
                    current_task_id: r.get::<_, Option<String>>(4)?,
                    updated_at: r.get::<_, String>(5)?,
                })
            },
        )
        .ok();
    Ok(row)
}

/// golden agents.py:268-278 `_restore_agent_health`: re-upsert the captured row (status||"IDLE"),
/// or delete the row when there was nothing to restore.
pub fn restore_agent_health(
    workspace: &Path,
    owner_team_id: &str,
    agent_id: &AgentId,
    row: &Option<CapturedHealth>,
) -> Result<(), crate::db::DbError> {
    let Some(row) = row else {
        return delete_agent_health(workspace, owner_team_id, agent_id);
    };
    let store = crate::message_store::MessageStore::open(workspace)
        .map_err(|e| crate::db::DbError::Schema(e.to_string()))?;
    let conn = crate::db::schema::open_db(store.db_path())?;
    let status = row.status.clone().unwrap_or_else(|| "IDLE".to_string());
    conn.execute(
        "insert into agent_health (owner_team_id, agent_id, status, last_output_at, context_usage_pct, current_task_id, updated_at) \
         values (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        rusqlite::params![
            owner_team_id,
            agent_id.as_str(),
            status,
            row.last_output_at,
            row.context_usage_pct,
            row.current_task_id,
            row.updated_at,
        ],
    )?;
    Ok(())
}

fn delete_agent_health(
    workspace: &Path,
    owner_team_id: &str,
    agent_id: &AgentId,
) -> Result<(), crate::db::DbError> {
    let store = crate::message_store::MessageStore::open(workspace)
        .map_err(|e| crate::db::DbError::Schema(e.to_string()))?;
    let conn = crate::db::schema::open_db(store.db_path())?;
    conn.execute(
        "delete from agent_health where owner_team_id = ?1 and agent_id = ?2",
        rusqlite::params![owner_team_id, agent_id.as_str()],
    )?;
    Ok(())
}

/// 0.5.32 (`.team/artifacts/restart-resumed-stale-activity-locate.md` §5):
/// clear the `(owner_team_id, agent_id)` health observation row on a new
/// worker process cohort boundary. Distinct from `delete_agent_health` (used
/// by remove-agent rollback): this narrow helper keyed on both owner_team_id
/// and agent_id so a sibling team with the same agent_id keeps its own row.
/// Silently no-ops when the row is absent.
pub fn clear_agent_health_observation(
    workspace: &Path,
    owner_team_id: &str,
    agent_id: &AgentId,
) -> Result<(), crate::db::DbError> {
    if owner_team_id.is_empty() {
        return Ok(());
    }
    let store = crate::message_store::MessageStore::open(workspace)
        .map_err(|e| crate::db::DbError::Schema(e.to_string()))?;
    let conn = crate::db::schema::open_db(store.db_path())?;
    conn.execute(
        "delete from agent_health where owner_team_id = ?1 and agent_id = ?2",
        rusqlite::params![owner_team_id, agent_id.as_str()],
    )?;
    Ok(())
}
