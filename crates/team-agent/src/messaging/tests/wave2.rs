//! U1 波2 contracts (RED-first → GREEN after fix).
//!
//! Wave-2 closes three hardening gaps the wave-1 7-acute audit identified but
//! left untouched:
//!
//!   * U1-A — `resolve_inject_target` LEADER branch had no liveness/fallback
//!     gate: a stale `leader_receiver.pane_id` would be returned verbatim, and
//!     the upstream `leader_receiver_pane_is_usable` would also fail (pane_id
//!     no longer in `list_targets`, liveness Dead) → message goes terminal
//!     `failed:leader_not_attached` on a mere pane-id drift. The wave-2 fix
//!     widens "usable" to include "leader session+window exists" and lets the
//!     resolver fall back to that live pane.
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
use crate::transport::{PaneLiveness, PaneId, SessionName, WindowName};

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

/// **Scenario**: state binds leader to `%old`, but tmux replaced it with `%new`
/// in the same `leader-session`/`leader` window (pane drift — leader is ALIVE,
/// just at a new id; this happens on tmux respawn-pane, `kill-pane` of an
/// inner shell, panel resize, etc.).
///
/// **Pre-fix behaviour**: `leader_receiver_pane_is_usable` returns false (old
/// pane gone from list, liveness Dead) → mark failed leader_not_attached.
/// We never even reach `resolve_inject_target`.
///
/// **Post-fix expectation**: usable widens to "leader session+window exists";
/// resolver falls back to the new live pane; delivery either delivers or fails
/// for an unrelated reason — but NOT `leader_not_attached`.
#[test]
#[ignore = "U1-A drift fallback excised for 0.3.24 (macmini RED v2, res_e4b40473d36f). \
            Mock contract preserved verbatim for v0.3.25 fix-forward — the real-machine \
            fix requires writer-shape + projection + rediscover-writer triad, NOT the \
            wave-2 single-fn fallback. See .team/artifacts/u1-a-realmachine-v2-fix-or-excise.md."]
fn u1_a_leader_pane_drift_falls_back_to_live_session_window_not_failed() {
    let ws = tmp_ws("u1a-drift");
    let store = store_for(&ws);
    let log = EventLog::new(&ws);
    let state = serde_json::json!({
        "session_name": "leader-session",
        "leader_receiver": {
            "pane_id": "%old",
            "session_name": "leader-session",
            "window_name": "leader",
        },
    });
    crate::state::persist::save_runtime_state(&ws, &state).unwrap();
    let message_id = store
        .create_message(None, "coordinator", "leader", "hi", None, false, None)
        .unwrap();

    // Drift: %old is GONE; %new took its place under the same session/window.
    let transport = OfflineTransport::new()
        .with_targets(vec![pane_info_w("%new", "leader-session", "leader")])
        .with_session_present(true)
        .with_liveness("%old", PaneLiveness::Dead)
        .with_liveness("%new", PaneLiveness::Live);

    let out = deliver_pending_message(&ws, &store, &transport, &message_id, &log, &state)
        .unwrap();

    assert!(
        out.reason != Some(DeliveryRefusal::LeaderNotAttached),
        "U1-A: pane DRIFT (not leader death) must not terminal-fail as \
         leader_not_attached; got reason={:?} status={:?}",
        out.reason,
        out.message_status.0
    );
    assert_ne!(
        out.message_status.0,
        "failed",
        "U1-A: drift must not be terminal; got status={}",
        out.message_status.0
    );
    let inject_targets = transport.inject_targets();
    assert!(
        inject_targets.iter().any(|t| matches!(t, Target::Pane(p) if p.as_str() == "%new")),
        "U1-A: fallback chain must inject into the LIVE pane (%new), not the stale \
         %old; injected targets={inject_targets:?}"
    );
}

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

    let out = deliver_pending_message(&ws, &store, &transport, &message_id, &log, &state)
        .unwrap();

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

    let out = deliver_pending_message(&ws, &store, &transport, &message_id, &log, &state)
        .unwrap();

    assert!(
        !out.ok,
        "U1-B: jitter must not declare delivered (false-green guard)"
    );
    assert_ne!(
        out.message_status.0,
        "delivered",
        "U1-B: jitter must not mark delivered; got status={}",
        out.message_status.0
    );
    assert_ne!(
        out.message_status.0,
        "failed",
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
            let reason = event.get("reason").and_then(serde_json::Value::as_str).unwrap_or("");
            let verif = event.get("verification").and_then(serde_json::Value::as_str).unwrap_or("");
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

    let out = deliver_pending_message(&ws, &store, &transport, &message_id, &log, &state)
        .unwrap();

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

    let _ = deliver_pending_message(&ws, &store, &transport, &message_id, &log, &state)
        .unwrap();

    let ranges = transport.capture_ranges();
    assert!(
        !ranges.is_empty(),
        "U1-C: delivery peek must call capture() for codex worker with non-terminal \
         startup_prompts; got no capture calls"
    );
    assert!(
        ranges.iter().any(|r| matches!(r, CaptureRange::Tail(n) if *n == 80)),
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

    let out = deliver_pending_message(&ws, &store, &transport, &message_id, &log, &state)
        .unwrap();

    assert_eq!(
        out.message_status.0,
        "queued_until_trust",
        "U1-C regression guard: a live trust modal in Tail(80) must still park as \
         queued_until_trust; got status={} reason={:?}",
        out.message_status.0,
        out.reason
    );
}

// ═════════════════════════════════════════════════════════════════════════════
// U1-A wave-2 follow-on — REAL-MACHINE-FAITHFUL contracts for the projection-null
// regression that escaped the original wave-2 fix (macmini fixture
// `res_dc8f8c8e20f6`, see `.team/artifacts/u1-a-realmachine-fail-fix-spec.md`).
//
// The original U1-A test at line 72 of this file uses a SYNTHETIC state shape
// that pre-populates BOTH `state.session_name` AND `leader_receiver.session_name`
// AND omits `owner_team_id` on the message. All three are simultaneously WRONG
// for the real-machine isolated-team scenario, which is exactly why the wave-2
// fix shipped GREEN here but the bug reproduced on the device.
//
// The real-machine writer
// (`lifecycle/launch.rs::seed_launched_owner_from_caller_with_provider_lookup`,
// lines 4440-4468) builds the `leader_receiver` JSON WITHOUT `session_name`
// and `window_name`. Real-machine messages from worker_b → leader DO carry
// `owner_team_id` on the row at send time, so the delivery path at
// `messaging/delivery.rs:140-162` re-projects the runtime state through
// `project_state_for_owner_team` → `project_top_level_view`
// (`state/projection.rs:301-306`). The projection's `setdefault(null)` contract
// converts a missing `state.session_name` on the team entry to a JSON null at
// the projected top level. From there the fallback chain at `delivery.rs:604-606`
// (`leader_receiver_session_name`) hits null on both the receiver AND the
// projected root, returns None, the `!session.is_empty()` gate at
// `delivery.rs:542` is skipped, and the U1-A drift fallback in
// `leader_receiver_pane_is_usable` ALSO short-circuits with empty session —
// back to `LeaderNotAttached` on a live %new.
// ═════════════════════════════════════════════════════════════════════════════

/// **Real-machine scenario** (macmini fixture `res_dc8f8c8e20f6`):
///   * Isolated team `t1` launched via quick-start; tmux session = `team-iso`.
///   * `leader_receiver` written by the isolated-team seed path carries
///     `pane_id` / `mode` / `status` / `discovery` / `tmux_socket` but
///     **NO** `session_name` and **NO** `window_name` (writer never sets them
///     — see `lifecycle/launch.rs:4440-4468`).
///   * worker_b → leader message carries `owner_team_id = "t1"` on the SQLite
///     row (real-machine sender always populates this for isolated teams).
///   * On delivery tick, `tmux respawn-pane -k -t %old` had already drifted
///     the leader pane: `%old` is Dead, `%new` lives at session `team-iso`,
///     window `leader`.
///
/// **Pre-fix behaviour (the bug that shipped in dd85ba7)**:
///   delivery.rs:140 projects state via `project_top_level_view` → projection
///   inserts `session_name: null` at projected top level
///   (`state/projection.rs:301-302` `unwrap_or(Value::Null)` branch).
///   Receiver lacks session_name → fallback at `delivery.rs:606` reads
///   projected `state.session_name` → null → `.as_str()` → None.
///   `leader_receiver_pane_is_usable` sees session=`""` → skips drift probe →
///   returns false → message terminal-fails `LeaderNotAttached`, even though
///   %new is alive at the right session+window.
///
/// **Post-fix expectation**: projection.rs:300-306 falls back to the ROOT
/// `state.session_name` before inserting null. The drift probe then succeeds
/// and %new is selected as inject target.
///
/// **0.3.24 status**: IGNORED. The wave-2 single-fn fallback (whose mock this
/// asserted) was excised after macmini RED v2 (res_e4b40473d36f) proved that
/// projection+drift-fallback alone do not close the real-machine gap. The
/// v0.3.25 fix-forward will re-enable this contract with a real-tmux macmini
/// counterpart — see `.team/artifacts/u1-a-realmachine-v2-fix-or-excise.md`.
#[test]
#[ignore = "U1-A deferred to v0.3.25 — wave-2 single-fn fallback excised for 0.3.24 ship"]
fn u1_a_projected_team_state_with_null_session_still_finds_drifted_leader_pane() {
    let ws = tmp_ws("u1a-proj-null-session");
    let store = store_for(&ws);
    let log = EventLog::new(&ws);

    // Real-machine state shape: nested `teams.t1` entry, receiver as the writer
    // at lifecycle/launch.rs:4440-4468 produces it (NO session_name, NO window_name).
    // Top-level `session_name` is the canonical isolated-team session; the team
    // entry does NOT carry it (matches real-machine `state.json` snapshots).
    let state = serde_json::json!({
        "active_team_key": "t1",
        "session_name": "team-iso",
        "teams": {
            "t1": {
                // NOTE: no session_name on the team entry — this is exactly the
                // projection-trigger condition. Real machine relies on the root
                // session_name; the projection setdefault(null) destroys it.
                "leader_receiver": {
                    "mode": "direct_tmux",
                    "status": "attached",
                    "pane_id": "%old",
                    "pane": "%old",
                    "leader_session_uuid": "00000000-0000-0000-0000-000000000001",
                    "owner_epoch": 1,
                    "discovery": "quick_start",
                    // NOTE: no tmux_socket (the isolated-team seed path at
                    // lifecycle/launch.rs:4440-4468 marks it OPTIONAL; setting a
                    // foreign socket here would route us through
                    // `delivery_transport_for_recipient`'s real-tmux endpoint
                    // backend, which is correct production behaviour but bypasses
                    // OfflineTransport and is asserted on by the real-machine
                    // gated stub at the end of this section).
                    // NOTE: no session_name. No window_name. Faithful to
                    // seed_launched_owner_from_caller_with_provider_lookup
                    // (lifecycle/launch.rs:4440-4468) which never writes them.
                },
                "team_owner": {
                    "pane_id": "%old",
                    "leader_session_uuid": "00000000-0000-0000-0000-000000000001",
                    "owner_epoch": 1,
                    "claimed_via": "quick-start",
                },
                "owner_epoch": 1,
                "agents": {},
            }
        }
    });
    crate::state::persist::save_runtime_state(&ws, &state).unwrap();

    // Critical faithfulness knob: owner_team_id IS set on the row, exactly as
    // the real-machine sender (worker_b → leader on isolated team) writes it.
    // This is what triggers the projection path at delivery.rs:140-162 that
    // the pre-existing u1_a test above silently skipped by passing None.
    let message_id = store
        .create_message(None, "worker_b", "leader", "hi", None, false, Some("t1"))
        .unwrap();

    // Drift: %old is Dead; %new took its place under session "team-iso", window "leader".
    let transport = OfflineTransport::new()
        .with_targets(vec![pane_info_w("%new", "team-iso", "leader")])
        .with_session_present(true)
        .with_liveness("%old", PaneLiveness::Dead)
        .with_liveness("%new", PaneLiveness::Live);

    let out = deliver_pending_message(&ws, &store, &transport, &message_id, &log, &state)
        .unwrap();

    assert!(
        out.reason != Some(DeliveryRefusal::LeaderNotAttached),
        "U1-A projection-null escape (real-machine fixture res_dc8f8c8e20f6): \
         worker_b → leader on isolated team with owner_team_id projects state \
         through project_top_level_view, which setdefault(null)s session_name \
         when the team entry lacks it; the U1-A fallback chain then sees empty \
         session and terminal-fails. Got reason={:?} status={:?} — must NOT be \
         LeaderNotAttached when %new is live at session+window.",
        out.reason,
        out.message_status.0
    );
    assert_ne!(
        out.message_status.0,
        "failed",
        "U1-A projection-null escape: drift via projected state must not be \
         terminal; got status={}",
        out.message_status.0
    );
    let inject_targets = transport.inject_targets();
    assert!(
        inject_targets.iter().any(|t| matches!(t, Target::Pane(p) if p.as_str() == "%new")),
        "U1-A projection-null escape: fallback chain must inject into the LIVE \
         pane (%new) reachable via session+window probe; injected={inject_targets:?}"
    );
}

/// **Projection-primitive contract**: direct test on
/// `state::projection::project_top_level_view` that exposes the
/// `unwrap_or(Value::Null)` bug at `state/projection.rs:301-306` in isolation.
///
/// **Pre-fix behaviour**: when the team entry lacks `session_name`, the
/// projection inserts JSON null at the projected top level, regardless of
/// whether the ROOT state has a valid `session_name`. Every downstream consumer
/// uses `.and_then(Value::as_str)` which treats null and missing identically,
/// so the bug is silent at projection time and only manifests at delivery time.
///
/// **Post-fix expectation**: when the team entry lacks `session_name`, fall
/// back to the ROOT `state.session_name` before inserting null. This is the
/// minimal-blast-radius fix — when the team entry HAS session_name the
/// behaviour is bit-identical (entry branch wins), and when both lack it the
/// projected value is still null (unchanged → `project_top_level_view_golden`
/// + `project_top_level_view_does_not_fall_to_toplevel_owner_when_teams_exist`
/// in `state/projection.rs` stay green).
///
/// **0.3.24 status**: IGNORED. The projection or_else fallback was reverted
/// along with the wave-2 U1-A drift fallback because it had no other consumer.
/// v0.3.25 will re-introduce it as part of the writer-shape + projection +
/// rediscover-writer triad.
#[test]
#[ignore = "U1-A deferred to v0.3.25 — projection or_else fallback reverted with the wave-2 excision"]
fn u1_a_project_top_level_view_falls_back_to_root_session_name_when_entry_lacks_it() {
    let state = serde_json::json!({
        "session_name": "root-sess",
        "teams": {
            "t1": {
                "agents": {},
                // NO session_name on the entry — projection must fall back to root.
            }
        }
    });

    let projected = crate::state::projection::project_top_level_view(&state, "t1");

    let session = projected.get("session_name");
    assert!(
        session.is_some(),
        "projection must produce a session_name key (matches Python setdefault \
         contract); got {projected:?}"
    );
    assert_eq!(
        session.and_then(serde_json::Value::as_str),
        Some("root-sess"),
        "U1-A projection contract: when team entry lacks session_name, the \
         projection MUST fall back to ROOT state.session_name; instead it \
         inserted {:?}. This is the root cause of the U1-A real-machine FAIL — \
         every delivery.rs fallback that calls .and_then(Value::as_str) treats \
         null as missing.",
        session
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

// Real-machine integration stub (#[ignore]) — macmini scenario res_dc8f8c8e20f6.
// The deterministic source of truth is the three OfflineTransport contracts above.
// This stub claims the test name so the macmini harness can link it.
#[test]
#[ignore = "real-machine: macmini fixture res_dc8f8c8e20f6 — requires real tmux + isolated team launch + respawn-pane drift"]
fn u1_a_macmini_res_dc8f8c8e20f6_isolated_team_respawn_pane_drift_delivers() {
    if std::env::var("TEAMAGENT_MACMINI_E2E").as_deref() != Ok("1") {
        eprintln!("skipping: TEAMAGENT_MACMINI_E2E != 1");
        return;
    }
    panic!(
        "stub: implement against tools/macmini-ssh/run_macmini_gap32_u1a_drift_e2e.sh \
         (follow-up). The OfflineTransport unit contracts above are the deterministic \
         source of truth; a real-machine replay was observed manually on the macmini \
         host (worker_b → leader, %old dead, %new alive, projection nulls \
         session_name, terminal-fails leader_not_attached pre-fix)."
    );
}
