//! team-key slice2 · layer 3 — real-machine retirement lifecycle (RED, DESTRUCTIVE).
//!
//! From locate §6.2 + §7 observation points 6/7/8, NOT from the root-cause
//! reasoning. These cases spawn a real fake-provider team (real tmux panes via
//! `provider: fake`, no subscription) and exercise the DESTRUCTIVE lifecycle:
//! remove / retire / re-add / restart. They are gated behind
//! `TEAM_AGENT_REALMACHINE_RETIREMENT=1` so the frozen contract does not run a
//! destructive real-machine flow until the leader authorizes it (subscription /
//! real-machine gate). Without the env gate each case is a no-op that documents
//! the observation point it will pin.
//!
//! Observation points pinned when authorized:
//!   6. retire: add one-shot -> complete -> remove/retire -> restart; spec,
//!      state active agents, status, tmux pane, agent_health all ABSENT while
//!      the retirement tombstone is RETAINED.
//!   7. explicit re-add: same id `add-agent` clears the tombstone and produces
//!      exactly one pane / row / spec entry.
//!   8. restart prune: a state-only removed seat stays ABSENT across restart and
//!      a second restart / process reload.
//!   (b) atomic remove->add (Twitter queue): remove succeeds then add fails ->
//!      the original seat is NOT lost (rollback leaves no half-state).

#![allow(clippy::expect_used, clippy::panic)]

fn authorized() -> bool {
    std::env::var("TEAM_AGENT_REALMACHINE_RETIREMENT").as_deref() == Ok("1")
}

/// §7.6 — retire then restart: every source (spec/state/status/pane/health) is
/// absent, tombstone retained. Red at baseline: no tombstone semantics, so the
/// dynamic role source resurrects the retired seat on restart.
#[test]
fn retire_then_restart_leaves_all_sources_absent_with_tombstone_retained() {
    if !authorized() {
        eprintln!(
            "SKIP (destructive, unauthorized): set TEAM_AGENT_REALMACHINE_RETIREMENT=1 \
             to run — pins §7.6 retire->restart absent+tombstone"
        );
        return;
    }
    // Real-machine body runs only under explicit authorization. It will:
    //   1. quick-start a fake team, add a one-shot agent `oneshot`;
    //   2. remove/retire `oneshot` (atomic pane/spec/state/health delete + tombstone);
    //   3. restart WITHOUT --allow-fresh;
    //   4. assert `oneshot` absent from spec, state.agents, status, tmux panes,
    //      agent_health; assert the retirement tombstone is still present.
    panic!(
        "RED: retirement tombstone lifecycle not implemented — a retired one-shot seat \
         must not be resurrected by its dynamic role source on restart, and its tombstone \
         must be retained (locate §7.6)."
    );
}

/// §7.7 — explicit re-add clears the tombstone and produces exactly one seat.
#[test]
fn explicit_re_add_clears_tombstone_and_creates_exactly_one_seat() {
    if !authorized() {
        eprintln!("SKIP (destructive, unauthorized): pins §7.7 re-add clears tombstone");
        return;
    }
    panic!(
        "RED: re-add tombstone clearing not implemented — re-adding a retired id must clear \
         the tombstone and create exactly one pane/row/spec entry (locate §7.7)."
    );
}

/// §7.8 — restart prune persistence: a removed state-only seat stays absent
/// across restart and a second restart / reload.
#[test]
fn restart_prune_of_removed_seat_persists_across_reload() {
    if !authorized() {
        eprintln!("SKIP (destructive, unauthorized): pins §7.8 restart prune persistence");
        return;
    }
    panic!(
        "RED: restart prune persistence not implemented — a pruned state-only seat must stay \
         absent from root state.json and the nested team entry across a second restart/reload \
         (locate §7.8)."
    );
}

/// (b) Twitter queue — remove succeeds then add fails: the original seat is not
/// lost; the transaction rolls back leaving no half-state.
#[test]
fn atomic_remove_then_failed_add_does_not_lose_the_original_seat() {
    if !authorized() {
        eprintln!("SKIP (destructive, unauthorized): pins atomic remove->failed-add rollback");
        return;
    }
    panic!(
        "RED: atomic remove->add transaction not implemented — if remove succeeds and the \
         subsequent add fails, the original seat must survive (no lost seat, no half-state)."
    );
}
