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
pub use format::format_approvals;

mod approvals;
pub(super) use approvals::*;
pub use approvals::{approvals, approvals_scoped};

mod agents;
pub(super) use agents::*;

mod compact;
pub(super) use compact::*;

mod brief;
pub(crate) use brief::{format_status_brief, registered_agent_exists, status_brief_scoped};
#[cfg(test)]
pub(crate) use brief::{
    with_test_nodeprobe, with_test_nodeprobe_candidates, with_test_nodeprobe_resolver_delay,
    with_test_nodeprobe_unbound, ResolverDelaySignal,
};

mod runtime;
pub(super) use runtime::*;

#[cfg(test)]
mod tests;

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
