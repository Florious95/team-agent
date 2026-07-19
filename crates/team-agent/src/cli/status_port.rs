//! Status read-model facade. RuntimeSnapshot is the sole live + persisted assembler.
use super::*;
use crate::state::projection::OwnerTeamResolution;
use crate::transport::Transport;
use rusqlite::params;
use std::path::PathBuf;

mod snapshot;
pub(crate) use snapshot::RuntimeSnapshot;
use snapshot::*;

mod format;
pub(super) use format::*;
pub use format::{format_approvals, format_status, format_status_scoped};

mod approvals;
pub(super) use approvals::*;
pub use approvals::{approvals, approvals_scoped};

mod inbox;
pub use inbox::inbox;
pub(super) use inbox::*;

mod agents;
pub(super) use agents::*;

mod store;
pub(super) use store::*;

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
