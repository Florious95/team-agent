//! U1 波2 contracts (RED-first → GREEN after fix).
//!
//! Wave-2 covers the U1-B and U1-C hardening gaps the wave-1 7-acute audit
//! identified, while retaining U1-A fail-closed regression guards:
//!
//!   * U1-B — `transport.list_targets().unwrap_or_default()` SWALLOWED server
//!     jitter (`Err` ≡ subprocess fork failed) as an empty vec. With the cache
//!     codepath at :513-516 (`if live_targets.is_empty() && !known_dead { reuse }`)
//!     a true jitter Err would reuse a never-validated cached_pane → injects
//!     into a possibly-dead pane. The wave-2 fix DEFERs (degraded outcome,
//!     `target_resolved` status) on Err and only treats Ok(empty) as authoritative
//!     evidence that the session has no panes.
//!   * U1-C — The delivery peek SITE at `delivery.rs:960` read `CaptureRange::Full`,
//!     so the WHOLE scrollback (including answered-trust residue that scrolled off
//!     screens ago) was classified — keeping a `Do you trust …` + `› 1.` glyph tail
//!     alive forever and parking idle codex panes in `queued_until_trust`. Wave-2
//!     narrows the peek to `Tail(80)`: a LIVE trust modal Codex pins to the visible
//!     region (still answered), but residual scrolled-off modals no longer match.
//!     **Recognizer recency** (the codex actionable-shape early-return at
//!     `startup_prompt.rs:86`) is NOT changed in wave-2 — the rfind-position recency
//!     model claude/copilot use is invalidated on Codex by CR-063 pre-render
//!     (Update box + banner + `› Find…` input prompt pre-render BELOW a LIVE trust
//!     modal — real-machine fixture `SUBROOT_FULL_TRUST_PANE_AFTER_QUICKSTART`
//!     proves it). The narrower "scrollback residue idle-pane" problem needs a
//!     Codex-specific shape (not rfind), tracked as U1-C-1 follow-up.

use super::*;
use crate::messaging::deliver_pending_message;
use crate::transport::test_support::OfflineTransport;
use crate::transport::{PaneId, PaneLiveness, SessionName, WindowName};

fn pane_info_w(pane_id: &str, session: &str, window: &str) -> PaneInfo {
    PaneInfo {
        pane_id: PaneId::new(pane_id),
        session: SessionName::new(session),
        window_index: None,
        window_name: Some(WindowName::new(window)),
        pane_index: None,
        tty: None,
        current_command: None,
        current_path: None,
        active: true,
        pane_pid: None,
        leader_env: BTreeMap::new(),
    }
}

// ═════════════════════════════════════════════════════════════════════════════
// U1-A — LEADER pane_id drift fallback chain
// ═════════════════════════════════════════════════════════════════════════════


// ═════════════════════════════════════════════════════════════════════════════
// U1-B — list_targets Err is server jitter, NOT empty
// ═════════════════════════════════════════════════════════════════════════════



// ═════════════════════════════════════════════════════════════════════════════
// U1-C — delivery peek site narrows Full → Tail(80)
//
// NOTE on scope vs the wave-2 brief: the brief also called for an rfind-recency
// guard inside `has_actionable_trust_shape` mirroring claude/copilot. The
// real-machine fixture `SUBROOT_FULL_TRUST_PANE_AFTER_QUICKSTART` (CR-063 pre-
// render: Update box + banner + `› Find…` input prompt all rendered BELOW a
// LIVE trust modal in the SAME capture) proves rfind-position recency does NOT
// transfer to codex — banner-position > trust-position is a routine pre-render
// artefact, not a "trust is stale" signal. That sub-item is deferred to a
// follow-up (U1-C-1) that needs a codex-specific shape, not rfind. Wave-2
// keeps the narrower, defensible fix: delivery's pre-inject peek uses Tail(80)
// instead of Full, so scrolled-off answered-trust residue can no longer trick
// the gate into parking idle codex panes forever. A LIVE trust modal is pinned
// by codex to the visible region, so it still gets answered.
// ═════════════════════════════════════════════════════════════════════════════



/// **Regression guard**: when NEITHER the team entry NOR the root state has
/// `session_name`, projection must still produce a `session_name` key with
/// JSON null (Python `setdefault(null)` contract — keeps the existing
/// `project_top_level_view_does_not_fall_to_toplevel_owner_when_teams_exist`
/// behaviour intact at `state/projection.rs:706`).
#[test]
fn u1_a_project_top_level_view_inserts_null_session_name_when_neither_root_nor_entry_has_it() {
    let state = serde_json::json!({
        // NO root session_name.
        "teams": {
            "t1": {
                "agents": {},
                // NO entry session_name.
            }
        }
    });
    let projected = crate::state::projection::project_top_level_view(&state, "t1");
    assert!(
        projected.get("session_name").is_some(),
        "regression guard: projection must always insert session_name key \
         (Python setdefault contract); got {projected:?}"
    );
    assert_eq!(
        projected.get("session_name"),
        Some(&serde_json::Value::Null),
        "regression guard: when neither root nor entry has session_name, the \
         projected value is JSON null (unchanged from main HEAD behaviour); \
         got {:?}",
        projected.get("session_name")
    );
}
