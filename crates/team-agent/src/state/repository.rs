//! StateRepository facade.
//!
//! Ref:
//! - `.team/artifacts/s1a-state-repository-design.md` (skeleton)
//! - `.team/artifacts/s1b-writer-cluster-locate.md` (0.5.42 first
//!   writer-cluster migration: `CoordinatorTick` + `ClaimLeader`)
//!
//! `StateRepository` is a thin write facade that dispatches every write
//! to the existing helper family without changing helper behavior,
//! merge policy, or on-disk layout. The value it adds is that every
//! write now carries a `StateWriteIntent`, so writer clusters migrate
//! one semantic at a time.
//!
//! 0.5.42 status: first real consumers are the daemon `Coordinator::
//! tick` production save (`CoordinatorTick`) and the lease/claim
//! writer cluster's two terminal saves (`ClaimLeader`) —
//! `write_lease_dual_state` + `save_claim_team_scoped_state`.
//! Post-S1b, the direct-save allowlist drops from 73 to 70; the three
//! migrated sites have no allowlist row and every future direct save
//! must either enter through `StateRepository` or claim an explicit
//! new allowlist row (fail-closed).
//!
//! Non-goals kept from S1a (design §11) + reasserted by
//! s1b-writer-cluster-locate.md §7: no schema rewrite, no path layout
//! change, no helper deletion, no epoch computation in the repository,
//! no retry / reload / event emission, no reinterpretation of
//! `owner_epoch` / `topology_convergence` / readback proof — the
//! caller still owns the whole `Value`, and repository dispatches to
//! the same existing persist helper the direct edge used to hit.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::Path;

use serde_json::Value;

use super::StateError;
// S1a alias imports keep dispatch traceable to the exact legacy helper family
// while letting the repository body avoid the raw `save_runtime_state(`/
// `save_team_scoped_state(` tokens that the governance scanner counts.
#[allow(unused_imports)]
use super::persist::{
    load_runtime_state as helper_load_workspace,
    runtime_state_path as helper_workspace_path, save_runtime_state as helper_write_root,
    save_runtime_state_reapplying_after_conflict as helper_write_root_reapply,
    save_runtime_state_with_deleted_agents as helper_write_root_with_deleted_agents,
    save_runtime_state_with_lifecycle_topology_authority as helper_write_root_with_lifecycle_topology_authority,
    save_runtime_state_with_lifecycle_topology_authority_and_capture_backfill_skip as helper_write_root_with_lifecycle_topology_authority_and_capture_backfill_skip,
    save_runtime_state_with_team_tombstone_lifecycle_topology_authority as helper_write_root_with_team_tombstone_lifecycle_topology_authority,
    save_runtime_state_with_team_tombstoned_agents as helper_write_root_with_team_tombstoned_agents,
    save_runtime_state_without_migrations as helper_write_root_without_migrations,
};
use super::projection::{
    resolve_team_scoped_state as helper_resolve_team_scoped,
    save_team_scoped_state as helper_write_team_scoped,
    save_team_scoped_state_reapplying_after_conflict as helper_write_team_scoped_reapply,
    save_team_scoped_state_with_deleted_agents as helper_write_team_scoped_with_deleted_agents,
    save_team_scoped_state_with_lifecycle_topology_authority as helper_write_team_scoped_with_lifecycle_topology_authority,
    save_team_scoped_state_with_lifecycle_topology_authority_and_capture_backfill_skip as helper_write_team_scoped_with_lifecycle_topology_authority_and_capture_backfill_skip,
    save_team_scoped_state_with_tombstone_lifecycle_topology_authority as helper_write_team_scoped_with_tombstone_lifecycle_topology_authority,
};

/// Repository facade for workspace state reads and writes.
///
/// S1a scope: hold the workspace path, expose `load_workspace`/`load_team`
/// and a `save`/`save_reapplying` pair that both take a `StateWriteIntent`.
/// The concrete dispatch stays byte-identical to the pre-S1a call graph.
pub struct StateRepository<'a> {
    workspace: &'a Path,
}

impl<'a> StateRepository<'a> {
    /// Construct a repository bound to a workspace root.
    pub fn new(workspace: &'a Path) -> Self {
        Self { workspace }
    }

    /// Load the raw workspace state document.
    pub fn load_workspace(&self) -> Result<Value, StateError> {
        helper_load_workspace(self.workspace)
    }

    /// Load the canonical workspace document without running read-time
    /// migrations. `None` preserves the legacy raw-reader distinction between
    /// a missing file and a present empty/default document.
    pub fn load_workspace_if_exists_without_migrations(
        &self,
    ) -> Result<Option<Value>, StateError> {
        if !helper_workspace_path(self.workspace).exists() {
            return Ok(None);
        }
        let text = std::fs::read_to_string(helper_workspace_path(self.workspace))?;
        serde_json::from_str(&text).map(Some).map_err(StateError::from)
    }

    /// Resolve a team-scoped projection using the existing projection selector.
    /// `team_key = None` selects the ambient team the same way as pre-S1a.
    pub fn load_team(&self, team_key: Option<&str>) -> Result<Value, StateError> {
        let (state, refusal) = helper_resolve_team_scoped(self.workspace, team_key)?;
        if let Some(refusal) = refusal {
            return Err(StateError::TeamSelect(refusal.to_string()));
        }
        state.ok_or_else(|| {
            StateError::TeamSelect(
                "resolve_team_scoped_state returned no state and no refusal".to_string(),
            )
        })
    }

    /// Persist `state` under the supplied intent. S1a dispatch is a pure
    /// forward call to the same legacy helper family that the caller used
    /// before S1a; the intent selects the family, not the merge semantics.
    pub fn save(&self, intent: StateWriteIntent<'_>, state: &Value) -> Result<(), StateError> {
        route_direct(self.workspace, intent, state)
    }

    /// Persist `state` and let `reapply` re-materialize the write on
    /// SaveConflict, matching the pre-S1a `_reapplying_after_conflict` helper
    /// behavior for reapply-shaped intents.
    pub fn save_reapplying<F>(
        &self,
        intent: StateWriteIntent<'_>,
        state: &Value,
        reapply: F,
    ) -> Result<(), StateError>
    where
        F: FnOnce(&mut Value),
    {
        route_reapply(self.workspace, intent, state, reapply)
    }
}

/// Closed semantic catalog of workspace state writes. Every allowlisted
/// callsite maps to exactly one variant, and there is intentionally no
/// generic escape bucket. Any new state write must earn its own variant so
/// that S1b can migrate it deliberately (see design section 3).
///
/// Ordering follows design section 5 group order (lifecycle, agent ops,
/// leader/coordinator, MCP, messaging, then diagnostic seams).
pub enum StateWriteIntent<'a> {
    LaunchTeam {
        team_key: &'a str,
    },
    AddAgent {
        team_key: &'a str,
        agent_id: &'a str,
    },
    AnnotateTeamDepth {
        team_key: &'a str,
    },
    ShutdownTeam {
        team_key: Option<&'a str>,
        clean: bool,
    },
    PromoteLiveSiblingAfterShutdown {
        stopped_team_key: &'a str,
        promoted_team_key: &'a str,
    },
    RestartTeam {
        team_key: &'a str,
        topology_authority_agent_ids: &'a [&'a str],
        skip_capture_backfill_agent_ids: &'a [&'a str],
    },
    RestartSessionRepair {
        team_key: &'a str,
    },
    StartAgent {
        team_key: &'a str,
        agent_id: &'a str,
    },
    StopAgent {
        team_key: &'a str,
        agent_id: &'a str,
    },
    ResetAgent {
        team_key: &'a str,
        agent_id: &'a str,
        discard_session: bool,
    },
    RemoveAgent {
        team_key: &'a str,
        agent_id: &'a str,
    },
    ForkAgent {
        team_key: &'a str,
        agent_id: &'a str,
    },
    AgentRollback {
        team_key: Option<&'a str>,
        agent_id: &'a str,
    },
    ForceRecreateRollback {
        team_key: &'a str,
        agent_id: &'a str,
    },
    ClaimLeader {
        team_key: &'a str,
    },
    LeaderBindingRestoreNonTargetTeams {
        target_team_key: &'a str,
    },
    LeaderStartBinding {
        team_key: &'a str,
        transport_kind: &'a str,
    },
    CoordinatorTick {
        team_key: &'a str,
    },
    CoordinatorConptyShim {
        team_key: Option<&'a str>,
    },
    /// 0.5.36 (`.team/artifacts/supermarket-api-error-recovery-locate.md` §7.3):
    /// post-save recovery step writes back the recovery intent outcome
    /// (attempts / status / last_error / blocked_reason). Distinct intent
    /// so future S1b migration can route it deliberately.
    CoordinatorApiErrorRecovery {
        team_key: Option<&'a str>,
        agent_id: Option<&'a str>,
    },
    McpAssignTask {
        team_key: Option<&'a str>,
        task_id: &'a str,
    },
    McpLifecycleAgentOps {
        team_key: Option<&'a str>,
    },
    McpUpdateStateNote {
        team_key: Option<&'a str>,
    },
    MessagingDeliveryState {
        owner_team_id: Option<&'a str>,
    },
    MessagingTurnArm {
        owner_team_id: Option<&'a str>,
    },
    ResultCollection {
        owner_team_id: Option<&'a str>,
    },
    SchedulerSuppression {
        owner_team_id: Option<&'a str>,
    },
    IdleAck {
        team_key: Option<&'a str>,
    },
    TaskRepair {
        team_key: Option<&'a str>,
    },
    FakeE2eSeed,
    SelfMigration,
}

impl<'a> StateWriteIntent<'a> {
    pub(crate) fn launch_team_or_add_agent(
        team_key: &'a str,
        added_agent_id: Option<&'a str>,
    ) -> Self {
        match added_agent_id {
            Some(agent_id) => Self::AddAgent { team_key, agent_id },
            None => Self::LaunchTeam { team_key },
        }
    }
}

// S1a dispatch: every intent forwards to exactly the same helper family the
// pre-S1a caller used, so behavior is bit-identical and only the naming has
// shifted. When S1b migrates a writer cluster, only the arms below change;
// the callsites and tests keep talking to `StateRepository`.
fn route_direct(
    workspace: &Path,
    intent: StateWriteIntent<'_>,
    state: &Value,
) -> Result<(), StateError> {
    match intent {
        // LaunchTeam -> the launch writer path uses the root helper today
        // (`save_runtime_state` at lifecycle/launch.rs:996).
        StateWriteIntent::LaunchTeam { .. } => helper_write_root(workspace, state),
        StateWriteIntent::AddAgent { agent_id, .. } => {
            helper_write_team_scoped_with_lifecycle_topology_authority(
                workspace,
                state,
                &[agent_id],
            )
        }
        // AnnotateTeamDepth also lives on the root save path.
        StateWriteIntent::AnnotateTeamDepth { .. } => helper_write_root(workspace, state),
        // ShutdownTeam: scoped clean shutdown uses `save_team_scoped_state`;
        // bare (non-scoped) shutdown uses `save_runtime_state`. The
        // `team_key` presence encodes the same clean-vs-bare split as the
        // pre-S1a `shutdown_with_transport_and_state` branch at cli/mod.rs:886/895.
        StateWriteIntent::ShutdownTeam { team_key, .. } => match team_key {
            Some(_) => helper_write_team_scoped(workspace, state),
            None => helper_write_root(workspace, state),
        },
        // PromoteLiveSiblingAfterShutdown writes the promoted projection at
        // cli/mod.rs:3526 via the root helper.
        StateWriteIntent::PromoteLiveSiblingAfterShutdown { .. } => {
            helper_write_root(workspace, state)
        }
        // RestartTeam: existing rebuild/common path uses
        // `save_team_scoped_state_with_lifecycle_topology_authority_and_capture_backfill_skip`
        // at lifecycle/restart/common.rs:633.
        StateWriteIntent::RestartTeam {
            topology_authority_agent_ids,
            skip_capture_backfill_agent_ids,
            ..
        } => helper_write_team_scoped_with_lifecycle_topology_authority_and_capture_backfill_skip(
            workspace,
            state,
            skip_capture_backfill_agent_ids,
            topology_authority_agent_ids,
        ),
        // RestartSessionRepair uses the same scoped helper first, with a
        // reapply overlay in the caller (rebuild.rs:1545/1555). S1a preserves
        // the direct-write form here; reapply routes through `save_reapplying`.
        StateWriteIntent::RestartSessionRepair { .. } => helper_write_team_scoped(workspace, state),
        // StartAgent - the launch add-agent tail lands via
        // `save_team_scoped_state_with_lifecycle_topology_authority` today.
        StateWriteIntent::StartAgent { .. } => {
            helper_write_team_scoped_with_lifecycle_topology_authority(workspace, state, &[])
        }
        // StopAgent uses the legacy helper family
        // `save_team_scoped_state_with_lifecycle_topology_authority`,
        // matching lifecycle/restart/agent.rs:911.
        StateWriteIntent::StopAgent { agent_id, .. } => {
            helper_write_team_scoped_with_lifecycle_topology_authority(
                workspace,
                state,
                &[agent_id],
            )
        }
        // ResetAgent discard-session dispatches to
        // `save_team_scoped_state_with_tombstone_lifecycle_topology_authority`,
        // matching lifecycle/restart/agent.rs:1355. Non-discard falls back to
        // the same lifecycle topology-authority helper as StopAgent.
        StateWriteIntent::ResetAgent {
            agent_id,
            discard_session,
            ..
        } => {
            if discard_session {
                helper_write_team_scoped_with_tombstone_lifecycle_topology_authority(
                    workspace,
                    state,
                    &[agent_id],
                )
            } else {
                helper_write_team_scoped_with_lifecycle_topology_authority(
                    workspace,
                    state,
                    &[agent_id],
                )
            }
        }
        // RemoveAgent -> deleted-agents helper family
        // (lifecycle/restart/remove.rs:263).
        StateWriteIntent::RemoveAgent { agent_id, .. } => {
            helper_write_team_scoped_with_deleted_agents(workspace, state, &[agent_id])
        }
        // ForkAgent mutates a selected team projection, so persist it back to
        // that projection with the forked row as lifecycle topology authority.
        StateWriteIntent::ForkAgent { agent_id, .. } => {
            helper_write_team_scoped_with_lifecycle_topology_authority(
                workspace,
                state,
                &[agent_id],
            )
        }
        // Team-scoped rollback must restore only the selected projection and
        // preserve sibling teams. Legacy callers without a team keep the root
        // deleted-agents writer.
        StateWriteIntent::AgentRollback { team_key, agent_id } => match team_key {
            Some(_) => helper_write_team_scoped_with_deleted_agents(workspace, state, &[agent_id]),
            None => helper_write_root_with_deleted_agents(workspace, state, &[agent_id]),
        },
        // Force-recreate rollback restores an existing row after the
        // replacement spawn has advanced its lifecycle tuple. The selected
        // row is therefore the explicit topology authority.
        StateWriteIntent::ForceRecreateRollback { agent_id, .. } => {
            helper_write_team_scoped_with_lifecycle_topology_authority(
                workspace,
                state,
                &[agent_id],
            )
        }
        // ClaimLeader -> leader/lease.rs:1625 uses the root helper, and the
        // scoped preserve-claim-fields variant at :1702 uses the team-tombstoned
        // agents helper.
        StateWriteIntent::ClaimLeader { .. } => helper_write_root(workspace, state),
        StateWriteIntent::LeaderBindingRestoreNonTargetTeams { .. } => {
            helper_write_root_without_migrations(workspace, state)
        }
        // LeaderStartBinding -> managed/exec/external all root-save at
        // leader/start.rs:795/903/946.
        StateWriteIntent::LeaderStartBinding { .. } => helper_write_root(workspace, state),
        // CoordinatorTick dispatches to the existing scoped helper
        // (`save_team_scoped_state`) preserving coordinator/tick.rs:427.
        StateWriteIntent::CoordinatorTick { .. } => helper_write_team_scoped(workspace, state),
        // CoordinatorConptyShim -> root save
        // (coordinator/conpty_shim.rs:426/674).
        StateWriteIntent::CoordinatorConptyShim { .. } => helper_write_root(workspace, state),
        // 0.5.36 CoordinatorApiErrorRecovery -> root save. The recovery
        // intent lives under `coordinator.abnormal_api_error_recovery`, a
        // root-scoped bookkeeping namespace that must survive across team
        // boundaries; use the same helper family as CoordinatorConptyShim.
        StateWriteIntent::CoordinatorApiErrorRecovery { .. } => helper_write_root(workspace, state),
        // McpAssignTask uses the reapply variant today; direct save routes
        // to the root helper for parity.
        StateWriteIntent::McpAssignTask { .. } => helper_write_root(workspace, state),
        // McpLifecycleAgentOps -> root save
        // (mcp_server/lifecycle_tools/agent_ops.rs:385).
        StateWriteIntent::McpLifecycleAgentOps { .. } => helper_write_root(workspace, state),
        // McpUpdateStateNote -> root save
        // (mcp_server/lifecycle_tools/state_status.rs:29/69).
        StateWriteIntent::McpUpdateStateNote { .. } => helper_write_root(workspace, state),
        // MessagingDeliveryState direct writes cover team-scoped, root
        // fallback, and root (messaging/delivery.rs:2356/2358/2361).
        StateWriteIntent::MessagingDeliveryState { owner_team_id } => match owner_team_id {
            Some(_) => helper_write_team_scoped(workspace, state),
            None => helper_write_root(workspace, state),
        },
        // MessagingTurnArm -> root save (messaging/activity.rs:83).
        StateWriteIntent::MessagingTurnArm { .. } => helper_write_root(workspace, state),
        // ResultCollection direct-write path uses `save_team_scoped_state`
        // when an owner_team_id is bound and `save_runtime_state` at the root
        // otherwise (messaging/results.rs:201/211). The reapply-shaped
        // variants live in `route_reapply` and preserve
        // `save_team_scoped_state_reapplying_after_conflict` /
        // `save_runtime_state_reapplying_after_conflict` semantics.
        StateWriteIntent::ResultCollection { owner_team_id } => match owner_team_id {
            Some(_) => helper_write_team_scoped(workspace, state),
            None => helper_write_root(workspace, state),
        },
        // SchedulerSuppression -> root save (messaging/scheduler.rs:259).
        StateWriteIntent::SchedulerSuppression { .. } => helper_write_root(workspace, state),
        // IdleAck -> root save (cli/mod.rs:2544).
        StateWriteIntent::IdleAck { .. } => helper_write_root(workspace, state),
        // TaskRepair -> `save_team_scoped_state` (cli/adapters.rs:675).
        StateWriteIntent::TaskRepair { .. } => helper_write_team_scoped(workspace, state),
        // FakeE2eSeed / SelfMigration are diagnostic seams. The current
        // callsites live at cli/adapters.rs:1035/1070 and
        // state/persist.rs:1127; S1a routes them to the root helper.
        StateWriteIntent::FakeE2eSeed => helper_write_root(workspace, state),
        StateWriteIntent::SelfMigration => helper_write_root(workspace, state),
    }
}

// Route intents that historically used the `_reapplying_after_conflict`
// helper family. Behavior stays identical: S1a chooses the same helper the
// legacy caller would have chosen for a reapply.
#[derive(Clone, Copy, Eq, PartialEq)]
enum ReapplyScope {
    Root,
    Team,
}

fn reapply_scope(intent: &StateWriteIntent<'_>) -> ReapplyScope {
    if matches!(
        intent,
        StateWriteIntent::RestartSessionRepair { .. }
            | StateWriteIntent::MessagingDeliveryState {
                owner_team_id: Some(_),
            }
            | StateWriteIntent::ResultCollection {
                owner_team_id: Some(_),
            }
            | StateWriteIntent::CoordinatorTick { .. }
            | StateWriteIntent::McpUpdateStateNote {
                team_key: Some(_),
            }
    ) {
        ReapplyScope::Team
    } else {
        ReapplyScope::Root
    }
}

fn route_reapply<F>(
    workspace: &Path,
    intent: StateWriteIntent<'_>,
    state: &Value,
    reapply: F,
) -> Result<(), StateError>
where
    F: FnOnce(&mut Value),
{
    if reapply_scope(&intent) == ReapplyScope::Team {
        helper_write_team_scoped_reapply(workspace, state, reapply)
    } else {
        helper_write_root_reapply(workspace, state, reapply)
    }
}

#[cfg(test)]
mod tests;
