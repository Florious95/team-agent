//! Status read-model facade. RuntimeSnapshot is the sole live + persisted assembler.
use super::*;
use crate::state::projection::OwnerTeamResolution;
use crate::transport::Transport;
use rusqlite::params;
use std::path::PathBuf;

mod snapshot;
use snapshot::read_runtime_state;
pub(crate) use snapshot::RuntimeSnapshot;

mod format;
use format::format_agent_status;
pub use format::format_approvals;

mod approvals;
pub(super) use approvals::*;
pub use approvals::{approvals, approvals_scoped};

mod inbox;
pub use inbox::inbox;
pub(super) use inbox::*;

mod agents;
pub(super) use agents::*;

mod store;
use store::{
    agent_health, count_rows, count_undelivered_backlog, count_where_status, insert_optional_i64,
    insert_optional_string, latest_result_summaries, message_counts, pending_leader_notifications,
    queued_messages, recent_agent_messages, result_counts, result_status_counts, status_counts,
};

mod compact;
pub(super) use compact::*;

mod runtime;
pub(super) use runtime::*;

#[cfg(test)]
mod tests;

/// `status.status(workspace, as_json, compact)` (`queries.py:33`).
pub fn status(workspace: &Path, compact: bool, detail: bool) -> Result<Value, CliError> {
    let state = read_runtime_state(workspace);
    status_scoped(workspace, &state, None, compact, detail)
}

pub fn status_scoped(
    workspace: &Path,
    state: &Value,
    owner_team_id: Option<&str>,
    compact: bool,
    detail: bool,
) -> Result<Value, CliError> {
    let snapshot = RuntimeSnapshot::assemble(workspace, state, owner_team_id)?;
    if compact && !detail {
        Ok(snapshot.compact())
    } else {
        Ok(snapshot.into_full())
    }
}

pub fn format_status(workspace: &Path, agent: Option<&str>) -> Result<String, CliError> {
    let state = read_runtime_state(workspace);
    format_status_scoped(workspace, &state, None, agent)
}

pub fn format_status_scoped(
    workspace: &Path,
    state: &Value,
    owner_team_id: Option<&str>,
    agent: Option<&str>,
) -> Result<String, CliError> {
    let snapshot = RuntimeSnapshot::assemble(workspace, state, owner_team_id)?;
    match agent {
        Some(agent) => {
            let recent_messages = recent_agent_messages(workspace, agent)?;
            format_agent_status(snapshot.full(), agent, &recent_messages)
        }
        None => Ok(crate::cli::format_status_csv(snapshot.full())),
    }
}
