#![allow(dead_code)]

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct AllowedStateSaveCall {
    pub path: &'static str,
    pub containing_fn: &'static str,
    pub callee_family: &'static str,
    pub intent: &'static str,
    pub migration_phase: &'static str,
    pub reason: &'static str,
    pub evidence_line: usize,
}

// Was 29 at G0. Bumped to 30 for the leader-inbound `save_runtime_state_with_
// receiver_authority` repository-internal authority helper (see its ALLOWED row).
// This count tracks ALLOWED rows; the external-writer subset is ratcheted DOWN
// separately (never up) — see the FROZEN_G0_ROW_KEYS extension predicate.
pub const BASELINE_DIRECT_SAVE_COUNT: usize = 30;

macro_rules! allow {
    ($path:literal, $fn:literal, $callee:literal, $intent:literal, $phase:literal, $line:literal) => {
        AllowedStateSaveCall {
            path: $path,
            containing_fn: $fn,
            callee_family: $callee,
            intent: $intent,
            migration_phase: $phase,
            reason: concat!($path, "::", $fn, " => ", $intent),
            evidence_line: $line,
        }
    };
}

pub const ALLOWED_STATE_SAVE_CALLS: &[AllowedStateSaveCall] = &[
    allow!(
        "state/persist.rs",
        "save_runtime_state",
        "save_runtime_state_with_merge_options",
        "repository_internal",
        "repository_internal",
        207
    ),
    allow!(
        "state/persist.rs",
        "save_runtime_state_reapplying_after_conflict",
        "save_runtime_state",
        "repository_internal",
        "repository_internal",
        218
    ),
    allow!(
        "state/persist.rs",
        "save_runtime_state_reapplying_after_conflict",
        "save_runtime_state",
        "repository_internal",
        "repository_internal",
        223
    ),
    allow!(
        "state/persist.rs",
        "save_runtime_state_with_lifecycle_topology_authority",
        "save_runtime_state_with_merge_options",
        "repository_internal",
        "repository_internal",
        234
    ),
    // leader-inbound receiver-authority merge (slice2b/leader-inbound evolution):
    // a repository-internal authority helper in the SAME family as
    // `save_runtime_state_with_lifecycle_topology_authority` above — called ONLY
    // via `StateWriteIntent::{ClaimLeader,LeaderStartBinding}` dispatch
    // (repository.rs:386/394), `is_external_writer("state/persist.rs") == false`.
    // Its G0 extension is governed by the extension predicate documented at the
    // FROZEN_G0_ROW_KEYS header: internal-extensible authority helpers may grow
    // with repository evolution; the external 38->0 ratchet is untouched.
    allow!(
        "state/persist.rs",
        "save_runtime_state_with_receiver_authority",
        "save_runtime_state_with_merge_options",
        "repository_internal",
        "repository_internal",
        216
    ),
    allow!(
        "state/persist.rs",
        "save_runtime_state_with_lifecycle_topology_authority_and_capture_backfill_skip",
        "save_runtime_state_with_merge_options",
        "repository_internal",
        "repository_internal",
        244
    ),
    allow!(
        "state/persist.rs",
        "save_runtime_state_with_deleted_agents",
        "save_runtime_state_with_merge_options",
        "repository_internal",
        "repository_internal",
        259
    ),
    allow!(
        "state/persist.rs",
        "save_runtime_state_with_team_tombstoned_agents",
        "save_runtime_state_with_merge_options",
        "repository_internal",
        "repository_internal",
        268
    ),
    allow!(
        "state/persist.rs",
        "save_runtime_state_with_team_tombstone_lifecycle_topology_authority",
        "save_runtime_state_with_merge_options",
        "repository_internal",
        "repository_internal",
        284
    ),
    allow!(
        "state/persist.rs",
        "load_runtime_state",
        "save_runtime_state",
        "SelfMigration",
        "repository_internal",
        1127
    ),
    allow!(
        "state/projection.rs",
        "save_team_scoped_state",
        "save_team_scoped_state_with_deleted_agents",
        "repository_internal",
        "repository_internal",
        647
    ),
    allow!(
        "state/projection.rs",
        "save_team_scoped_state_reapplying_after_conflict",
        "save_team_scoped_state",
        "repository_internal",
        "repository_internal",
        658
    ),
    allow!(
        "state/projection.rs",
        "save_team_scoped_state_reapplying_after_conflict",
        "save_team_scoped_state",
        "repository_internal",
        "repository_internal",
        664
    ),
    allow!(
        "state/projection.rs",
        "save_team_scoped_state_with_deleted_agents",
        "save_team_scoped_state_with_merge_exceptions",
        "repository_internal",
        "repository_internal",
        675
    ),
    allow!(
        "state/projection.rs",
        "save_team_scoped_state_with_tombstone_lifecycle_topology_authority",
        "save_team_scoped_state_with_merge_options",
        "repository_internal",
        "repository_internal",
        683
    ),
    allow!(
        "state/projection.rs",
        "save_team_scoped_state_with_lifecycle_topology_authority",
        "save_team_scoped_state_with_merge_options",
        "repository_internal",
        "repository_internal",
        691
    ),
    allow!(
        "state/projection.rs",
        "save_team_scoped_state_with_lifecycle_topology_authority_and_capture_backfill_skip",
        "save_team_scoped_state_with_merge_options",
        "repository_internal",
        "repository_internal",
        700
    ),
    allow!(
        "state/projection.rs",
        "save_team_scoped_state_with_merge_exceptions",
        "save_team_scoped_state_with_merge_options",
        "repository_internal",
        "repository_internal",
        716
    ),
    allow!(
        "state/projection.rs",
        "save_team_scoped_state_with_merge_options",
        "save_runtime_state_with_team_tombstone_lifecycle_topology_authority",
        "repository_internal",
        "repository_internal",
        768
    ),
    allow!(
        "state/projection.rs",
        "save_team_scoped_state_with_merge_options",
        "save_runtime_state_with_lifecycle_topology_authority_and_capture_backfill_skip",
        "repository_internal",
        "repository_internal",
        776
    ),
    allow!(
        "state/projection.rs",
        "save_team_scoped_state_with_merge_options",
        "save_runtime_state_with_lifecycle_topology_authority",
        "repository_internal",
        "repository_internal",
        784
    ),
    allow!(
        "state/projection.rs",
        "save_team_scoped_state_with_merge_options",
        "save_runtime_state_with_lifecycle_topology_authority_and_capture_backfill_skip",
        "repository_internal",
        "repository_internal",
        791
    ),
    allow!(
        "state/projection.rs",
        "save_team_scoped_state_with_merge_options",
        "save_runtime_state_with_deleted_agents",
        "repository_internal",
        "repository_internal",
        800
    ),
    allow!(
        "state/projection.rs",
        "save_team_scoped_state_with_merge_options",
        "save_runtime_state_with_team_tombstoned_agents",
        "repository_internal",
        "repository_internal",
        802
    ),
    allow!(
        "state/projection.rs",
        "save_team_scoped_state_with_merge_options",
        "save_runtime_state_with_lifecycle_topology_authority_and_capture_backfill_skip",
        "repository_internal",
        "repository_internal",
        834
    ),
    allow!(
        "state/projection.rs",
        "save_team_scoped_state_with_merge_options",
        "save_runtime_state_with_lifecycle_topology_authority",
        "repository_internal",
        "repository_internal",
        842
    ),
    allow!(
        "state/projection.rs",
        "save_team_scoped_state_with_merge_options",
        "save_runtime_state_with_lifecycle_topology_authority_and_capture_backfill_skip",
        "repository_internal",
        "repository_internal",
        849
    ),
    allow!(
        "state/projection.rs",
        "save_team_scoped_state_with_merge_options",
        "save_runtime_state_with_deleted_agents",
        "repository_internal",
        "repository_internal",
        857
    ),
    allow!(
        "state/projection.rs",
        "save_team_scoped_state_with_merge_options",
        "save_runtime_state_with_team_tombstone_lifecycle_topology_authority",
        "repository_internal",
        "repository_internal",
        865
    ),
    allow!(
        "state/projection.rs",
        "save_team_scoped_state_with_merge_options",
        "save_runtime_state_with_team_tombstoned_agents",
        "repository_internal",
        "repository_internal",
        872
    ),
];
