//! S1a governance-gate hardening (frozen by verifier).
//!
//! Successor teeth for the three escape probes from arch delta-car-a:
//! 1. ratchet: the allowlist is pinned to the CURRENT 29-row snapshot -
//!    resurrecting any deleted row or raising the baseline goes red;
//! 2. scanner anti-alias: renaming or importing the direct-save family
//!    outside the state authority goes red;
//! 3. raw-read enumeration: any NEW file mentioning the runtime state.json
//!    surface goes red until classified here.

#![allow(clippy::expect_used, clippy::panic)]

#[path = "support/state_save_allowlist.rs"]
mod state_save_allowlist;

use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;

use state_save_allowlist::{ALLOWED_STATE_SAVE_CALLS, BASELINE_DIRECT_SAVE_COUNT};

/// Post-car-A snapshot (external = 0, 30 authority-internal rows). The live
/// allowlist may only DELETE relative to this set; the historical G0 67-row
/// set is deliberately NOT used here - it would allow deleted debt back in.
///
/// Snapshot extension (verifier single-point sign-off, leader msg_f9bf8320c5d0):
/// this snapshot was 29 rows; the leader-inbound
/// `save_runtime_state_with_receiver_authority` row was ADDED under the same
/// extension predicate documented at the RED1 `FROZEN_G0_ROW_KEYS` header —
/// a repository-internal authority helper (`is_external_writer == false`,
/// reached only via `StateWriteIntent` dispatch, path in `AUTHORITY_PATHS`).
/// External writers are already at zero here and can NEVER be added — this
/// snapshot is authority-only by construction, so the extension does not touch
/// external protection. Whether this per-row hand-extension should be replaced by a
/// structural external-frozen-vs-internal-extensible split is an arch-delta item.
const FROZEN_CURRENT_ROW_KEYS: &[&str] = &[
    "state/persist.rs::load_runtime_state::save_runtime_state",
    "state/persist.rs::save_runtime_state::save_runtime_state_with_merge_options",
    "state/persist.rs::save_runtime_state_reapplying_after_conflict::save_runtime_state",
    "state/persist.rs::save_runtime_state_reapplying_after_conflict::save_runtime_state",
    "state/persist.rs::save_runtime_state_with_deleted_agents::save_runtime_state_with_merge_options",
    "state/persist.rs::save_runtime_state_with_receiver_authority::save_runtime_state_with_merge_options",
    "state/persist.rs::save_runtime_state_with_lifecycle_topology_authority::save_runtime_state_with_merge_options",
    "state/persist.rs::save_runtime_state_with_lifecycle_topology_authority_and_capture_backfill_skip::save_runtime_state_with_merge_options",
    "state/persist.rs::save_runtime_state_with_team_tombstone_lifecycle_topology_authority::save_runtime_state_with_merge_options",
    "state/persist.rs::save_runtime_state_with_team_tombstoned_agents::save_runtime_state_with_merge_options",
    "state/projection.rs::save_team_scoped_state::save_team_scoped_state_with_deleted_agents",
    "state/projection.rs::save_team_scoped_state_reapplying_after_conflict::save_team_scoped_state",
    "state/projection.rs::save_team_scoped_state_reapplying_after_conflict::save_team_scoped_state",
    "state/projection.rs::save_team_scoped_state_with_deleted_agents::save_team_scoped_state_with_merge_exceptions",
    "state/projection.rs::save_team_scoped_state_with_lifecycle_topology_authority::save_team_scoped_state_with_merge_options",
    "state/projection.rs::save_team_scoped_state_with_lifecycle_topology_authority_and_capture_backfill_skip::save_team_scoped_state_with_merge_options",
    "state/projection.rs::save_team_scoped_state_with_merge_exceptions::save_team_scoped_state_with_merge_options",
    "state/projection.rs::save_team_scoped_state_with_merge_options::save_runtime_state_with_deleted_agents",
    "state/projection.rs::save_team_scoped_state_with_merge_options::save_runtime_state_with_deleted_agents",
    "state/projection.rs::save_team_scoped_state_with_merge_options::save_runtime_state_with_lifecycle_topology_authority",
    "state/projection.rs::save_team_scoped_state_with_merge_options::save_runtime_state_with_lifecycle_topology_authority",
    "state/projection.rs::save_team_scoped_state_with_merge_options::save_runtime_state_with_lifecycle_topology_authority_and_capture_backfill_skip",
    "state/projection.rs::save_team_scoped_state_with_merge_options::save_runtime_state_with_lifecycle_topology_authority_and_capture_backfill_skip",
    "state/projection.rs::save_team_scoped_state_with_merge_options::save_runtime_state_with_lifecycle_topology_authority_and_capture_backfill_skip",
    "state/projection.rs::save_team_scoped_state_with_merge_options::save_runtime_state_with_lifecycle_topology_authority_and_capture_backfill_skip",
    "state/projection.rs::save_team_scoped_state_with_merge_options::save_runtime_state_with_team_tombstone_lifecycle_topology_authority",
    "state/projection.rs::save_team_scoped_state_with_merge_options::save_runtime_state_with_team_tombstone_lifecycle_topology_authority",
    "state/projection.rs::save_team_scoped_state_with_merge_options::save_runtime_state_with_team_tombstoned_agents",
    "state/projection.rs::save_team_scoped_state_with_merge_options::save_runtime_state_with_team_tombstoned_agents",
    "state/projection.rs::save_team_scoped_state_with_tombstone_lifecycle_topology_authority::save_team_scoped_state_with_merge_options",
];

const AUTHORITY_PATHS: &[&str] = &[
    "state/persist.rs",
    "state/projection.rs",
    "state/repository.rs",
];

/// Every non-test src file that today references the runtime `state.json`
/// surface. A new file mentioning it must be classified here first.
const STATE_JSON_SURFACE_FILES: &[&str] = &[
    "cli/diagnose.rs",
    "cli/mod.rs",
    "cli/named_address.rs",
    "conpty/backend.rs",
    "coordinator/conpty_shim.rs",
    "coordinator/steps/abnormal.rs",
    "coordinator/tick.rs",
    "layout/runtime_sessions.rs",
    "layout/tmux_endpoint.rs",
    "leader/lease.rs",
    "leader/mod.rs",
    "leader/rediscover/tests.rs",
    "leader/types.rs",
    "lib.rs",
    "lifecycle/helpers.rs",
    "lifecycle/launch.rs",
    "lifecycle/launch/quick_start.rs",
    "lifecycle/launch/spec_state.rs",
    "lifecycle/restart/preflight.rs",
    "lifecycle/restart/rebuild.rs",
    "lifecycle/restart/remove.rs",
    "lifecycle/types.rs",
    "packaging/install.rs",
    "packaging/mod.rs",
    "packaging/tests.rs",
    "provider/adapter.rs",
    "state/mod.rs",
    "state/owner_gate.rs",
    "state/ownership.rs",
    "state/paths.rs",
    "state/persist.rs",
    "state/projection.rs",
];

fn src_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src")
}

fn walk_src(files: &mut Vec<(String, String)>, dir: &PathBuf) {
    for entry in fs::read_dir(dir).expect("read src dir") {
        let entry = entry.expect("dir entry");
        let path = entry.path();
        if path.is_dir() {
            walk_src(files, &path);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            let rel = path
                .strip_prefix(src_root())
                .expect("rel path")
                .to_string_lossy()
                .replace('\\', "/");
            let source = fs::read_to_string(&path).expect("read src file");
            files.push((rel, source));
        }
    }
}

#[test]
fn hard1_ratchet_pins_current_snapshot_and_monotone_baseline() {
    let frozen: BTreeSet<&str> = FROZEN_CURRENT_ROW_KEYS.iter().copied().collect();
    let mut failures = Vec::new();
    if ALLOWED_STATE_SAVE_CALLS.len() > FROZEN_CURRENT_ROW_KEYS.len() {
        failures.push(format!(
            "allowlist grew beyond the current snapshot: {} > {}",
            ALLOWED_STATE_SAVE_CALLS.len(),
            FROZEN_CURRENT_ROW_KEYS.len()
        ));
    }
    if BASELINE_DIRECT_SAVE_COUNT > FROZEN_CURRENT_ROW_KEYS.len() {
        failures.push(format!(
            "baseline raised above the frozen snapshot: {BASELINE_DIRECT_SAVE_COUNT} > {}",
            FROZEN_CURRENT_ROW_KEYS.len()
        ));
    }
    for row in ALLOWED_STATE_SAVE_CALLS {
        let key = format!("{}::{}::{}", row.path, row.containing_fn, row.callee_family);
        if !frozen.contains(key.as_str()) {
            failures.push(format!(
                "row resurrected or renamed outside the snapshot: {key}"
            ));
        }
        if !AUTHORITY_PATHS.contains(&row.path) {
            failures.push(format!(
                "non-authority row present after external reached zero: {key}"
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "HARD1: allowlist must only shrink from the frozen 29-row authority snapshot.\n{}",
        failures.join("\n")
    );
}

#[test]
fn hard2_direct_save_family_cannot_be_aliased_or_imported_outside_authority() {
    let mut files = Vec::new();
    walk_src(&mut files, &src_root());
    let mut failures = Vec::new();
    for (rel, source) in &files {
        let authority = AUTHORITY_PATHS.contains(&rel.as_str())
            || rel.contains("/tests/")
            || rel.ends_with("/tests.rs");
        for line in source.lines() {
            let trimmed = line.trim_start();
            let mentions_family = trimmed.contains("save_runtime_state")
                || trimmed.contains("save_team_scoped_state");
            if !mentions_family {
                continue;
            }
            if trimmed.starts_with("use ") && trimmed.contains(" as ") {
                failures.push(format!(
                    "{rel}: aliased import of the direct-save family: {trimmed}"
                ));
            } else if trimmed.starts_with("use ") && !authority {
                failures.push(format!(
                    "{rel}: direct-save family imported outside authority: {trimmed}"
                ));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "HARD2: the direct-save family must not be aliased anywhere nor imported outside the state authority (test modules exempt).\n{}",
        failures.join("\n")
    );
}

#[test]
fn hard3_runtime_state_surface_files_are_enumerated() {
    let mut files = Vec::new();
    walk_src(&mut files, &src_root());
    let allowed: BTreeSet<&str> = STATE_JSON_SURFACE_FILES.iter().copied().collect();
    let mut failures = Vec::new();
    for (rel, source) in &files {
        if rel.contains("/tests/") || rel.ends_with("/tests.rs") {
            continue;
        }
        if source.contains("state.json") && !allowed.contains(rel.as_str()) {
            failures.push(format!(
                "{rel}: new reference to the runtime state.json surface; classify it in STATE_JSON_SURFACE_FILES only after verifier review"
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "HARD3: every src file touching the state.json surface must be enumerated.\n{}",
        failures.join("\n")
    );
}
