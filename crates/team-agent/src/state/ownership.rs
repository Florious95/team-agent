//! Stage 2 of identity-boundary unified plan (architect direction 2026-06-23):
//! `state::ownership` — single read entry point for `team_owner` lookups.
//!
//! Today the project has ~30 hand-rolled `state.get("team_owner")` sites and
//! a separate `team_owner_value` helper in `cli/mod.rs` that re-implements the
//! teams.<key> → top-level precedence. Stage 2 collapses owner *reads* (not
//! writes) into a single resolver so:
//!
//! 1. The owner gate, restart/start/stop/reset/send/diagnose all see the
//!    SAME owner regardless of whether the caller passed a raw or projected
//!    state — Stage 5 will swap the data source to per-team canonical state
//!    without touching call sites.
//! 2. Stale `teams.<key>.team_owner` can no longer single-handedly decide
//!    the gate when the canonical truth lives elsewhere (architect §verdict).
//! 3. Read-only diagnostic CLI / status JSON can ask the repository for the
//!    `OwnershipSource` so legacy duplicates are visible as migration
//!    warnings, not hidden truth.
//!
//! What Stage 2 does NOT do (those land in Stage 3):
//! - Does NOT change writers. `lease.rs` / `start.rs` / `launch.rs` still
//!   write owner at multiple locations; Stage 3 migrates them.
//! - Does NOT remove duplicate persisted fields. The legacy top-level and
//!   `teams.<key>.team_owner` remain on disk.
//! - Does NOT add a new file path. Stage 5 introduces
//!   `.team/runtime/<team_key>/state.json` as the canonical source.
//!
//! Precedence rule (architect §3 migration precedence; today no canonical
//! file exists so steps 1+4 are inert):
//!
//! 1. canonical `.team/runtime/<team_key>/state.json` (Stage 5 — not wired)
//! 2. `state.teams.<team_key>.team_owner`
//! 3. top-level `state.team_owner` IFF `team_state_key(state) == team_key`
//! 4. snapshot is diagnostic only (not consulted)

use serde_json::Value;

use crate::state::projection::{read_owner as projection_read_owner, team_state_key};

/// Where the repository found the owner record. Diagnostic-only; the owner
/// gate and other gating callers do not branch on this — they only need the
/// owner value. CLI status / diagnose use it to surface legacy duplicate
/// warnings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OwnershipSource {
    /// Stage 5 canonical path: `.team/runtime/<team_key>/state.json` (not
    /// yet emitted — placeholder for forward compatibility).
    CanonicalPerTeamState,
    /// Legacy projection: `state.teams.<team_key>.team_owner` —
    /// the value the projection layer would have surfaced.
    LegacyTeamsProjection,
    /// Legacy top-level: `state.team_owner`, used when the state's
    /// `team_state_key` matches the requested `team_key`.
    LegacyTopLevel,
}

/// Result of a repository read. The `value` is the JSON object the gate /
/// caller consumes (same shape as `state.get("team_owner")` returned today);
/// the `source` is diagnostic.
#[derive(Debug, Clone)]
pub struct OwnershipRead<'a> {
    pub value: &'a Value,
    pub source: OwnershipSource,
}

/// Stage 2 entry point: read the team owner for the given `team_key` from an
/// in-memory state value, honouring the legacy precedence. Returns `None` if
/// no source carries a non-empty, valid owner record.
///
/// `team_key` may be empty — that path defers to the legacy top-level lookup
/// only (no teams-projection branch). Callers who already resolved a
/// `team_key` should pass it; arbitrary projected state can pass `""`.
///
/// This deliberately reuses `projection::read_owner` so the pane-id validity
/// check (`%N` or all-digits) stays in one place. The repository's value-add
/// is the precedence ordering and the `OwnershipSource` tag.
pub fn read_owner_for_team<'a>(state: &'a Value, team_key: &str) -> Option<OwnershipRead<'a>> {
    // Step 1 (Stage 5): canonical per-team state. Stage 2 does not consult
    // a separate file yet — owner truth still lives in the in-memory state.
    // Once Stage 5 wires `.team/runtime/<team_key>/state.json` as canonical,
    // a new top-level reader at `state::ownership::read_owner_for_team_at(
    // workspace, team_key)` will check that file first and only fall
    // through to this in-memory resolver for legacy migration.

    // Step 2: `state.teams.<team_key>.team_owner` (the projection branch).
    if !team_key.is_empty() {
        if let Some(owner) = projection_read_owner(state, Some(team_key)) {
            return Some(OwnershipRead {
                value: owner,
                source: OwnershipSource::LegacyTeamsProjection,
            });
        }
    }

    // Step 3: top-level `state.team_owner`, only when the state's
    // `team_state_key` agrees with the requested team_key (or when the
    // caller passed an empty key — they're explicitly asking for the
    // arbitrary-projection view, which is the pre-Stage-2 owner_gate
    // shape).
    let top_level_matches = team_key.is_empty() || team_state_key(state) == team_key;
    if top_level_matches {
        if let Some(owner) = projection_read_owner(state, None) {
            return Some(OwnershipRead {
                value: owner,
                source: OwnershipSource::LegacyTopLevel,
            });
        }
    }

    None
}

/// Convenience: just the JSON value, no diagnostic source. Returns `None`
/// when no owner is found. Stable across Stage 5 — only the lookup
/// implementation will move.
pub fn read_owner_value<'a>(state: &'a Value, team_key: &str) -> Option<&'a Value> {
    read_owner_for_team(state, team_key).map(|read| read.value)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use serde_json::json;

    fn owner_with_pane(pane: &str) -> Value {
        json!({
            "pane_id": pane,
            "leader_session_uuid": format!("uuid-for-{pane}"),
            "owner_epoch": 1,
        })
    }

    #[test]
    fn returns_none_when_no_owner_anywhere() {
        let state = json!({"active_team_key": "alpha"});
        assert!(read_owner_for_team(&state, "alpha").is_none());
    }

    #[test]
    fn reads_top_level_when_team_state_key_matches() {
        let state = json!({
            "team_key": "alpha",
            "team_owner": owner_with_pane("%top"),
        });
        let read = read_owner_for_team(&state, "alpha").expect("owner found");
        assert_eq!(read.source, OwnershipSource::LegacyTopLevel);
        assert_eq!(read.value["pane_id"], json!("%top"));
    }

    #[test]
    fn refuses_top_level_when_team_state_key_disagrees() {
        // Architect §verdict point 2: stale top-level owner from a different
        // team must not be returned for `read_owner_for_team(state, "beta")`.
        let state = json!({
            "team_key": "alpha",
            "team_owner": owner_with_pane("%alpha"),
        });
        assert!(read_owner_for_team(&state, "beta").is_none());
    }

    #[test]
    fn teams_projection_wins_over_top_level_when_both_present() {
        // Architect §3 migration precedence step 2 > step 3: legacy projection
        // outranks legacy top-level. This is the shape the existing
        // `cli::team_owner_value` helper already encoded; lifting it into
        // the repository preserves the behaviour for all callers.
        let state = json!({
            "team_key": "alpha",
            "team_owner": owner_with_pane("%top"),
            "teams": {"alpha": {"team_owner": owner_with_pane("%teams")}},
        });
        let read = read_owner_for_team(&state, "alpha").expect("owner found");
        assert_eq!(read.source, OwnershipSource::LegacyTeamsProjection);
        assert_eq!(read.value["pane_id"], json!("%teams"));
    }

    #[test]
    fn empty_team_key_only_reads_top_level() {
        // The owner-gate today calls `read_owner(state, None)` — that pre-Stage-2
        // shape maps to `read_owner_for_team(state, "")`. The empty-key path
        // must NOT branch into teams.<key> (the gate caller is feeding an
        // already-projected state in the legacy flow).
        let state = json!({
            "team_owner": owner_with_pane("%top"),
            "teams": {"alpha": {"team_owner": owner_with_pane("%teams")}},
        });
        let read = read_owner_for_team(&state, "").expect("owner found");
        assert_eq!(read.source, OwnershipSource::LegacyTopLevel);
        assert_eq!(read.value["pane_id"], json!("%top"));
    }

    #[test]
    fn rejects_invalid_pane_id_consistent_with_projection_read_owner() {
        // The pane-id validity check (must start with `%` or be all-digits)
        // is preserved — this is the legacy `valid_owner_pane_id` behaviour
        // we inherit from projection::read_owner.
        let state = json!({
            "team_key": "alpha",
            "team_owner": json!({"pane_id": "not-a-pane"}),
        });
        assert!(read_owner_for_team(&state, "alpha").is_none());
    }

    #[test]
    fn read_owner_value_returns_just_the_value() {
        let state = json!({
            "team_key": "alpha",
            "team_owner": owner_with_pane("%top"),
        });
        let value = read_owner_value(&state, "alpha").expect("owner found");
        assert_eq!(value["pane_id"], json!("%top"));
    }
}
