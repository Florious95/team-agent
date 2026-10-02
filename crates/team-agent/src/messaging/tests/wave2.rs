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

/// **Scenario**: state has no `leader_receiver.pane_id` AND no leader session
/// metadata. Leader is genuinely not attached.
///
/// **Expectation (unchanged by wave-2)**: still `leader_not_attached`. The
/// fallback chain MUST NOT swallow a real leader-not-attached into a synthetic
/// `SessionWindow{window:"leader"}` injection. Same invariant as `resolve_inject_target`
/// doc: "must not fall through to a synthetic SessionWindow{window=leader}".
#[test]
fn u1_a_leader_truly_not_attached_still_leader_not_attached_not_synthetic_target() {
    let ws = tmp_ws("u1a-truly-dead");
    let store = store_for(&ws);
    let log = EventLog::new(&ws);
    let state = serde_json::json!({
        // no session_name, no leader_receiver pane/window — truly unattached
        "leader_receiver": {},
    });
    crate::state::persist::save_runtime_state(&ws, &state).unwrap();
    let message_id = store
        .create_message(None, "coordinator", "leader", "hi", None, false, None)
        .unwrap();

    // Transport returns NO panes at all (no leader window anywhere).
    let transport = OfflineTransport::new()
        .with_targets(vec![])
        .with_session_present(false);

    let out = deliver_pending_message(&ws, &store, &transport, &message_id, &log, &state).unwrap();

    assert_eq!(
        out.reason,
        Some(DeliveryRefusal::LeaderNotAttached),
        "U1-A: truly unattached leader must still terminal-fail as \
         leader_not_attached (regression guard)"
    );
    assert!(
        transport.inject_targets().is_empty(),
        "U1-A: truly unattached leader must NOT attempt a synthetic inject; \
         got {:?}",
        transport.inject_targets()
    );
}

// ═════════════════════════════════════════════════════════════════════════════
// U1-B — list_targets Err is server jitter, NOT empty
// ═════════════════════════════════════════════════════════════════════════════

/// **Scenario**: a WORKER recipient has `pane_id=%cached` (last-known) in state.
/// On this tick, tmux server jitters and `list_targets()` returns Err. The
/// cached pane has NOT been validated this tick.
///
/// **Pre-fix behaviour**: `list_targets().unwrap_or_default()` → empty vec →
/// resolver falls through to `if live_targets.is_empty() && !cached_pane_known_dead`
/// → REUSES `%cached` blindly → inject into an unverified (possibly dead) pane.
///
/// **Post-fix expectation**: Err is observed as such → delivery DEFERs (status
/// stays at `target_resolved`, NOT `delivered`, NOT `failed`), with a
/// jitter-named verification reason, so the next tick reattempts on a fresh
/// `list_targets`.
#[test]
fn u1_b_list_targets_err_defers_does_not_reuse_unvalidated_cached_pane() {
    let ws = tmp_ws("u1b-jitter");
    let store = store_for(&ws);
    let log = EventLog::new(&ws);
    let state = serde_json::json!({
        "session_name": "team-jitter",
        "agents": {
            "w1": {
                "provider": "fake",
                "pane_id": "%cached",
                "window": "w1",
            }
        }
    });
    crate::state::persist::save_runtime_state(&ws, &state).unwrap();
    let message_id = store
        .create_message(None, "leader", "w1", "ping", None, false, None)
        .unwrap();

    let transport = OfflineTransport::new()
        .with_list_targets_error("tmux server connection refused (jitter)")
        .with_session_present(true)
        .with_default_liveness(PaneLiveness::Live); // cached looks alive in isolation

    let out = deliver_pending_message(&ws, &store, &transport, &message_id, &log, &state).unwrap();

    assert!(
        !out.ok,
        "U1-B: jitter must not declare delivered (false-green guard)"
    );
    assert_ne!(
        out.message_status.0, "delivered",
        "U1-B: jitter must not mark delivered; got status={}",
        out.message_status.0
    );
    assert_ne!(
        out.message_status.0, "failed",
        "U1-B: jitter must not terminal-fail this tick (defer); got status={}",
        out.message_status.0
    );
    assert!(
        transport.inject_targets().is_empty(),
        "U1-B: jitter must NOT inject into the unvalidated cached_pane; \
         injected={:?}",
        transport.inject_targets()
    );
    let events = log.tail(0).unwrap();
    assert!(
        events.iter().any(|event| {
            let reason = event
                .get("reason")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("");
            let verif = event
                .get("verification")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("");
            reason.contains("list_targets")
                || verif.contains("list_targets")
                || reason.contains("server_jitter")
                || verif.contains("server_jitter")
        }),
        "U1-B: defer must carry a named reason (list_targets / server_jitter), \
         not a silent skip; got events={events:?}"
    );
}

/// **Scenario**: Ok(empty vec) — list_targets succeeded but the session is
/// genuinely empty (no panes at all). This is NOT jitter, it's authoritative
/// evidence the session has nothing.
///
/// **Expectation (unchanged)**: existing fallthrough behaviour. The point is
/// that wave-2 must DISTINGUISH Err from Ok(empty); this guard test asserts the
/// Ok(empty) branch still flows to the pre-existing logic.
#[test]
fn u1_b_list_targets_ok_empty_is_authoritative_not_deferred() {
    let ws = tmp_ws("u1b-emptyok");
    let store = store_for(&ws);
    let log = EventLog::new(&ws);
    let state = serde_json::json!({
        "session_name": "team-emptyok",
        "agents": {
            "w1": {
                "provider": "fake",
                "pane_id": "%cached",
                "window": "w1",
            }
        }
    });
    crate::state::persist::save_runtime_state(&ws, &state).unwrap();
    let message_id = store
        .create_message(None, "leader", "w1", "ping", None, false, None)
        .unwrap();

    // Ok(empty) + cached pane explicitly NOT known dead (has_pane=None, liveness=Unknown)
    // → resolver should reuse cached_pane (pre-existing path). The point of this guard:
    // wave-2 must NOT degrade the Ok(empty) branch into a defer.
    let transport = OfflineTransport::new()
        .with_targets(vec![])
        .with_session_present(true)
        .with_default_liveness(PaneLiveness::Unknown);

    let out = deliver_pending_message(&ws, &store, &transport, &message_id, &log, &state).unwrap();

    // We don't assert ok=true (the fake provider may or may not roundtrip);
    // we assert the pre-existing inject ATTEMPT happened (i.e. NOT deferred
    // silently), proving wave-2 did not over-tighten.
    assert!(
        !transport.inject_targets().is_empty(),
        "U1-B regression guard: Ok(empty) is authoritative — resolver must \
         still attempt the existing fallback (cached_pane reuse); got no \
         injections (status={}, reason={:?})",
        out.message_status.0,
        out.reason
    );
}

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

/// **Pre-fix behaviour**: `delivery.rs:960` calls `capture(target, Full)` for
/// the startup-prompt peek site. Full reads the entire pane scrollback (up to
/// the configured limit), which on an idle codex worker can carry the
/// already-answered trust modal text long after dismissal.
///
/// **Post-fix expectation**: the peek site uses `CaptureRange::Tail(80)`. The
/// peek is a delivery-time pre-check, NOT the startup-prompts dismissal phase
/// (that path needs Full to anchor recency on its own terms). We verify the
/// `CaptureRange` the gate passed to the transport.
#[test]
fn u1_c_delivery_peek_uses_tail_eighty_not_full() {
    use crate::transport::CaptureRange;

    let ws = tmp_ws("u1c-tail-range");
    let store = store_for(&ws);
    let log = EventLog::new(&ws);
    let state = serde_json::json!({
        "session_name": "team-tail-range",
        "agents": {
            "w1": {
                "provider": "codex",
                "pane_id": "%cdx",
                "window": "w1",
                "startup_prompts": "pending",  // NOT handled — triggers peek path
            }
        }
    });
    crate::state::persist::save_runtime_state(&ws, &state).unwrap();
    let message_id = store
        .create_message(None, "leader", "w1", "ping", None, false, None)
        .unwrap();

    // Empty capture text → classify returns KeepPolling → peek returns false → inject
    // proceeds. We only care about WHICH range the peek site asked for.
    let transport = OfflineTransport::new()
        .with_targets(vec![pane_info_w("%cdx", "team-tail-range", "w1")])
        .with_session_present(true)
        .with_default_liveness(PaneLiveness::Live)
        .with_capture_for_pane("%cdx", "OpenAI Codex\ncodex>\n");

    let _ = deliver_pending_message(&ws, &store, &transport, &message_id, &log, &state).unwrap();

    let ranges = transport.capture_ranges();
    assert!(
        !ranges.is_empty(),
        "U1-C: delivery peek must call capture() for codex worker with non-terminal \
         startup_prompts; got no capture calls"
    );
    assert!(
        ranges
            .iter()
            .any(|r| matches!(r, CaptureRange::Tail(n) if *n == 80)),
        "U1-C: delivery peek site must request Tail(80), not Full. ranges={ranges:?}. \
         Full was the pre-wave-2 default and caused scrolled-off answered-trust \
         residue to be classified as actionable, parking idle codex panes forever."
    );
    assert!(
        !ranges.iter().any(|r| matches!(r, CaptureRange::Full)),
        "U1-C: delivery peek must NOT request Full (residue-trap). ranges={ranges:?}"
    );
}

/// **Regression guard**: a TRULY actionable codex trust modal — startup_prompts
/// pending, Tail(80) capture shows the live trust modal in the visible region.
///
/// **Expectation (unchanged)**: still parks as `queued_until_trust`. Wave-2 must
/// not regress the legitimate trust-defer path: a live modal in Tail(80) is
/// still detected by the unchanged `has_actionable_trust_shape` early-return.
#[test]
fn u1_c_delivery_real_trust_modal_still_parks_queued_until_trust() {
    let ws = tmp_ws("u1c-realtrust");
    let store = store_for(&ws);
    let log = EventLog::new(&ws);
    let state = serde_json::json!({
        "session_name": "team-realtrust",
        "agents": {
            "w1": {
                "provider": "codex",
                "pane_id": "%cdx",
                "window": "w1",
                "startup_prompts": "pending",
            }
        }
    });
    crate::state::persist::save_runtime_state(&ws, &state).unwrap();
    let message_id = store
        .create_message(None, "leader", "w1", "ping", None, false, None)
        .unwrap();

    // Live trust modal — fits well inside Tail(80).
    let live_trust = "\
Loading codex...
Do you trust the contents of this directory?
› 1. Yes, continue
› 2. No, exit
";

    let transport = OfflineTransport::new()
        .with_targets(vec![pane_info_w("%cdx", "team-realtrust", "w1")])
        .with_session_present(true)
        .with_default_liveness(PaneLiveness::Live)
        .with_capture_for_pane("%cdx", live_trust);

    let out = deliver_pending_message(&ws, &store, &transport, &message_id, &log, &state).unwrap();

    assert_eq!(
        out.message_status.0, "queued_until_trust",
        "U1-C regression guard: a live trust modal in Tail(80) must still park as \
         queued_until_trust; got status={} reason={:?}",
        out.message_status.0, out.reason
    );
}

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
