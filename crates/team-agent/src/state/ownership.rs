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

/// Stage 3a (identity-boundary unified plan, architect direction 2026-06-23):
/// single write entry point for owner mutations. Pre-Stage-3, owner writes
/// were spread across 13 sites (claim/attach/readopt/managed-leader/quick-
/// start/identity/send), each independently inserting `team_owner` +
/// `leader_receiver` + `owner_epoch` into the in-memory state. Stage 3a
/// collapses those writers into this API.
///
/// Architect §1.A: the three fields form ONE ownership record. They must
/// be written together (no half-writes that race in projection/persist).
///
/// 3a behaviour (this commit): writes the record to BOTH the top-level
/// (`state.{team_owner, leader_receiver, owner_epoch}`) AND
/// `state.teams.<team_key>.{team_owner, leader_receiver, owner_epoch}`.
/// Zero behavioural change vs the legacy hand-rolled inserts — only the
/// API is consolidated. Stage 3b/3c will remove the read-side projection
/// promote and the persist-side copy-back; Stage 3d's tests will assert
/// canonical-only behaviour and a final small change here will stop the
/// top-level write.
///
/// Each write field is optional so callers can update a subset (e.g.
/// `write_receiver_only`). The `epoch` value is duplicated in
/// `state.owner_epoch` to match the legacy shape every reader expects.
pub fn write_owner(state: &mut Value, team_key: &str, record: OwnershipWrite) {
    if !state.is_object() {
        *state = serde_json::json!({});
    }
    // Top-level write (3a preserves dual-write; 3d removes).
    if let Some(root) = state.as_object_mut() {
        if let Some(receiver) = record.leader_receiver.as_ref() {
            root.insert("leader_receiver".to_string(), receiver.clone());
        }
        if let Some(owner) = record.team_owner.as_ref() {
            root.insert("team_owner".to_string(), owner.clone());
        }
        if let Some(epoch) = record.owner_epoch {
            root.insert("owner_epoch".to_string(), serde_json::json!(epoch));
        }
    }
    // Teams-projection write (Stage 5 will move this to per-team state file).
    if team_key.is_empty() {
        return;
    }
    let teams = state
        .as_object_mut()
        .and_then(|root| {
            root.entry("teams")
                .or_insert_with(|| serde_json::json!({}))
                .as_object_mut()
        });
    let Some(teams) = teams else { return };
    let entry = teams
        .entry(team_key.to_string())
        .or_insert_with(|| serde_json::json!({}));
    let Some(entry_obj) = entry.as_object_mut() else {
        return;
    };
    if let Some(receiver) = record.leader_receiver.as_ref() {
        entry_obj.insert("leader_receiver".to_string(), receiver.clone());
    }
    if let Some(owner) = record.team_owner.as_ref() {
        entry_obj.insert("team_owner".to_string(), owner.clone());
    }
    if let Some(epoch) = record.owner_epoch {
        entry_obj.insert("owner_epoch".to_string(), serde_json::json!(epoch));
    }
}

/// The ownership write payload. All fields optional so callers can update a
/// subset — receiver-only attach paths don't need to re-emit team_owner.
#[derive(Debug, Clone, Default)]
pub struct OwnershipWrite {
    pub team_owner: Option<Value>,
    pub leader_receiver: Option<Value>,
    pub owner_epoch: Option<u64>,
}

impl OwnershipWrite {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_team_owner(mut self, owner: Value) -> Self {
        self.team_owner = Some(owner);
        self
    }

    pub fn with_leader_receiver(mut self, receiver: Value) -> Self {
        self.leader_receiver = Some(receiver);
        self
    }

    pub fn with_owner_epoch(mut self, epoch: u64) -> Self {
        self.owner_epoch = Some(epoch);
        self
    }
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

    // ───────────── Stage 3a: write_owner API tests ─────────────

    #[test]
    fn write_owner_writes_top_level_and_teams_projection() {
        // 3a contract: preserve legacy dual-write semantics so readers
        // that still scan top-level (pre-3b/3c) keep working.
        let mut state = json!({});
        let record = OwnershipWrite::new()
            .with_team_owner(owner_with_pane("%new"))
            .with_owner_epoch(7)
            .with_leader_receiver(json!({"pane_id": "%new", "mode": "direct_tmux"}));
        write_owner(&mut state, "alpha", record);
        assert_eq!(state["team_owner"]["pane_id"], json!("%new"));
        assert_eq!(state["owner_epoch"], json!(7));
        assert_eq!(state["leader_receiver"]["pane_id"], json!("%new"));
        assert_eq!(state["teams"]["alpha"]["team_owner"]["pane_id"], json!("%new"));
        assert_eq!(state["teams"]["alpha"]["owner_epoch"], json!(7));
        assert_eq!(
            state["teams"]["alpha"]["leader_receiver"]["pane_id"],
            json!("%new")
        );
    }

    #[test]
    fn write_owner_supports_partial_updates() {
        // Receiver-only attach path doesn't need to re-emit team_owner.
        let mut state = json!({
            "team_owner": owner_with_pane("%existing"),
            "teams": {"alpha": {"team_owner": owner_with_pane("%existing")}},
        });
        let record = OwnershipWrite::new()
            .with_leader_receiver(json!({"pane_id": "%existing", "mode": "direct_tmux"}));
        write_owner(&mut state, "alpha", record);
        // Owner unchanged.
        assert_eq!(state["team_owner"]["pane_id"], json!("%existing"));
        // Receiver written to both locations.
        assert_eq!(state["leader_receiver"]["pane_id"], json!("%existing"));
        assert_eq!(
            state["teams"]["alpha"]["leader_receiver"]["pane_id"],
            json!("%existing")
        );
    }

    #[test]
    fn write_owner_with_empty_team_key_only_writes_top_level() {
        // The owner-gate-style attach with no team scope: still preserve the
        // legacy single-source top-level write. Stage 5 will refuse this
        // path; for now it remains compatible.
        let mut state = json!({});
        let record = OwnershipWrite::new().with_team_owner(owner_with_pane("%top"));
        write_owner(&mut state, "", record);
        assert_eq!(state["team_owner"]["pane_id"], json!("%top"));
        assert!(
            state.get("teams").is_none_or(|teams| teams
                .as_object()
                .is_some_and(|map| map.is_empty())),
            "empty team_key must NOT touch teams.<key>"
        );
    }

    #[test]
    fn write_owner_then_read_round_trip() {
        // 3a write + Stage 2 read agree: after writing for "alpha", reading
        // for "alpha" returns the LegacyTeamsProjection branch (teams wins
        // over top-level per the precedence rule).
        let mut state = json!({"team_key": "alpha"});
        let record = OwnershipWrite::new().with_team_owner(owner_with_pane("%w1"));
        write_owner(&mut state, "alpha", record);
        let read = read_owner_for_team(&state, "alpha").expect("owner found");
        assert_eq!(read.source, OwnershipSource::LegacyTeamsProjection);
        assert_eq!(read.value["pane_id"], json!("%w1"));
    }
}
