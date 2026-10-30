//! Retired SQL health authority. Live status is sampled from C4/native evidence.
//! Compatibility signatures refuse without opening or modifying a legacy store.
use std::path::Path;
use crate::model::ids::AgentId;
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CapturedHealth {
    pub owner_team_id: Option<String>, pub status: Option<String>,
    pub last_output_at: Option<String>, pub context_usage_pct: Option<i64>,
    pub current_task_id: Option<String>, pub updated_at: String,
}
fn retired() -> crate::db::DbError { crate::db::DbError::Schema("SQL health authority retired; use C4/native observations".into()) }
pub fn select_agent_health(_workspace: &Path, _owner_team_id: &str, _agent_id: &AgentId) -> Result<Option<CapturedHealth>, crate::db::DbError> { Err(retired()) }
pub fn restore_agent_health(_workspace: &Path, _owner_team_id: &str, _agent_id: &AgentId, _row: &Option<CapturedHealth>) -> Result<(), crate::db::DbError> { Err(retired()) }
pub fn clear_agent_health_observation(_workspace: &Path, _owner_team_id: &str, _agent_id: &AgentId) -> Result<(), crate::db::DbError> { Err(retired()) }
