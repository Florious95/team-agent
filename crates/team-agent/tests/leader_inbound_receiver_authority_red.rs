//! leader-inbound-attach-split · successor RED batch 2 (verifier) — receiver
//! authority merge over the coordinator stale-save seam (②③ composite).
//!
//! From locate §6 point 3 / §6.6 race test 6 / §8 test
//! `coordinator_stale_tick_cannot_overwrite_equal_epoch_receiver_refresh`, NOT
//! the root-cause reasoning. locate:154/164: "Equal epoch + both attached allows
//! older tuple to overwrite newer" — a coordinator tick that loaded the OLD
//! receiver saves AFTER a lease refresh landed on disk, and the stale same-epoch
//! tuple overwrites the refresh.
//!
//! Deterministic save-hook equivalent (sleep-free, locate:218): the coordinator
//! stale save is the PUBLIC `save_team_scoped_state` (projection.rs), which
//! routes through `apply_persist_merge_contract` with NO receiver authority
//! (CoordinatorTick owns only the fields it wrote). We land the lease refresh on
//! disk first (`latest`), then run the coordinator stale save (`incoming`). The
//! merge's equal-epoch ownership rule decides who wins:
//!   - candidate: equal-epoch keeps the attached `latest` (refresh) unless the
//!     incoming itself carries receiver authority → refresh survives.
//!   - parent 6502789: equal-epoch preserves `latest` only when incoming is
//!     UNATTACHED; both-attached lets the stale incoming overwrite → refresh lost.
//! `save_team_scoped_state` is pub on BOTH lineages, so the RED is a BEHAVIOR
//! difference (not a compile error) — unlike the new-signature dead ends
//! (verifier-batch-freeze-plan.md «批2③深评»).
//!
//! Composite ②③ (leader-approved msg_1bad58e87120): the persisted post-sequence
//! receiver must be the LIVE (refreshed) tuple. The parent red cause spans both
//! faces — ② (AlreadyBound/refresh authority not honored) and ③ (equal-epoch
//! stale overwrite) — both this case's fix surface; verdict notes the dual cause.
//!
//! Discipline: canonical team-scoped read-back (no intervening probe); EXISTS
//! precondition pins the refresh is on disk before the stale save; POSITIVE
//! CONTROL (seventh 六查 check) proves a genuinely-newer (higher-epoch)
//! coordinator save DOES win — the candidate enforces epoch+authority order, it
//! does not blanket-keep whatever is on disk.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[path = "support/hermetic.rs"]
mod hermetic_guard;

use serde_json::{json, Value};
use team_agent::state::projection::save_team_scoped_state;

const TEAM: &str = "receiver-authority";
const PANE: &str = "%7";

/// A FLAT team-scoped state (top-level == the team entry, no `teams` wrapper —
/// this is what `save_team_scoped_state`/coordinator tick save consume; the
/// wrapper is stripped by `compact_team_state`). `team_key` locates the scope.
fn state_with_receiver(session: &str, tty: &str, epoch: u64) -> Value {
    json!({
        "session_name": TEAM,
        "active_team_key": TEAM,
        "team_key": TEAM,
        "owner_epoch": epoch,
        "team_owner": {"status": "attached", "owner_epoch": epoch, "pane_id": PANE},
        "leader_receiver": {
            "status": "attached",
            "owner_epoch": epoch,
            "pane_id": PANE,
            "session_name": session,
            "pane_tty": tty,
            "fingerprint": format!("{session}|0|0|{tty}")
        }
    })
}

/// The persisted receiver's session_name for the active team, via the canonical
/// team-scoped projection (never a hard-coded nested path).
fn persisted_receiver_session(ws: &std::path::Path) -> Option<String> {
    let scoped = team_agent::state::projection::select_runtime_state(ws, Some(TEAM))
        .expect("select_runtime_state must resolve the team scope");
    scoped
        .get("leader_receiver")
        .or_else(|| {
            scoped
                .get("teams")
                .and_then(|t| t.get(TEAM))
                .and_then(|e| e.get("leader_receiver"))
        })
        .and_then(|r| r.get("session_name"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// ②③ composite: a coordinator stale save must NOT overwrite an equal-epoch
/// receiver refresh already on disk. Sequence: lease refresh lands on disk
/// (`latest`), then the coordinator's stale same-epoch save (`incoming`) runs.
/// Parent 6502789 (equal-epoch preserves latest only when incoming is unattached)
/// lets the stale both-attached incoming overwrite the refresh. Candidate keeps
/// the refresh.
#[test]
fn coordinator_stale_save_must_not_overwrite_equal_epoch_receiver_refresh() {
    let env = hermetic_guard::HermeticTestEnv::enter("receiver-authority-stale");
    let ws = env.workspace("ws");
    let ws = ws.as_path();

    // 1. Lease refresh lands on disk first — this is the `latest` the coordinator
    //    save merges against.
    let refresh = state_with_receiver("refreshed-session", "/dev/refreshed", 10);
    save_team_scoped_state(ws, &refresh).unwrap();
    // EXISTS precondition: the refresh must be on disk before the stale save,
    // else "must not overwrite" is a vacuous pass.
    assert_eq!(
        persisted_receiver_session(ws).as_deref(),
        Some("refreshed-session"),
        "precondition: the lease refresh must be persisted before the coordinator stale save"
    );

    // 2. Coordinator stale save: it loaded the OLD receiver before the refresh
    //    landed, so its in-memory state carries the stale same-epoch tuple.
    let stale = state_with_receiver("stale-session", "/dev/stale", 10);
    save_team_scoped_state(ws, &stale).expect("coordinator scoped save");

    // 3. The on-disk receiver must remain the refreshed tuple.
    assert_eq!(
        persisted_receiver_session(ws).as_deref(),
        Some("refreshed-session"),
        "a coordinator stale same-epoch save must NOT overwrite the on-disk receiver refresh: the \
         candidate's equal-epoch ownership rule keeps the attached latest; the parent lets the \
         stale both-attached incoming overwrite it (② refresh authority / ③ equal-epoch overwrite \
         — both this case's fix face)"
    );
}

/// POSITIVE CONTROL (seventh 六查 check — anchors the verdict to epoch order, not
/// a blanket keep-disk). When the coordinator save is GENUINELY newer (higher
/// epoch), it MUST win over the on-disk receiver — the persisted receiver is the
/// higher-epoch one. If the candidate kept the disk tuple here too, the test
/// above would prove nothing (the candidate would just always keep disk).
#[test]
fn higher_epoch_coordinator_save_still_wins_over_disk() {
    let env = hermetic_guard::HermeticTestEnv::enter("receiver-authority-posctrl");
    let ws = env.workspace("ws");
    let ws = ws.as_path();

    // Disk holds an OLDER receiver (epoch 10); the coordinator save is NEWER (11).
    let older_disk = state_with_receiver("older-session", "/dev/older", 10);
    save_team_scoped_state(ws, &older_disk).unwrap();

    let newer_incoming = state_with_receiver("newer-session", "/dev/newer", 11);
    save_team_scoped_state(ws, &newer_incoming).expect("coordinator scoped save");

    assert_eq!(
        persisted_receiver_session(ws).as_deref(),
        Some("newer-session"),
        "a genuinely newer (higher-epoch) coordinator save must win over the older on-disk \
         receiver: the candidate enforces epoch order, it does not blanket-keep the disk tuple. If \
         this kept 'older-session', the equal-epoch verdict would not be attributable to authority"
    );
}
