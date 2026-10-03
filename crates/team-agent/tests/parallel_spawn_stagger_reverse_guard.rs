//! 0.5.38 D-k (per 0538-cr-verdict.md observation item + 0539 leader
//! dispatch): reverse guard contract for the parallel worker spawn
//! stagger.
//!
//! Context — 0.5.38 introduced bounded parallel worker spawn in
//! `lifecycle/restart/rebuild.rs`. Pane-id assignment inside the fake
//! transport happens at the tail of each spawn call, so plan-order
//! determinism relies on plan-order transport ENTRY. The submission
//! Condvar guarantees that only one thread is inside the spawn call at
//! a time from the ENTRY side, but not the EXIT side — the sleep-inside-
//! spawn window overlaps across threads (that IS the parallelism win).
//! An explicit per-slot stagger (currently `slot * 10ms`) keeps the
//! plan-order entry from being reordered by OS scheduler jitter racing
//! the transport's internal state lock at the tail.
//!
//! D-k requirement: **prove the stagger is present and intentional** so
//! a future "cleanup" pass cannot silently drop it. Without the
//! stagger, R1 (startup_latency_contract::restart_records_parallel_spawn_overlap_and_deterministic_state)
//! flaked at 2-5ms stagger on shared macOS gate machines and 3-5ms
//! stagger on 0.5.38 CI — the 10ms value is the minimum that survived
//! 8 consecutive iterations locally.
//!
//! Design: text-shaped source contract (same shape as the RED1/RED4
//! gate declarations in `tmux_server_death_0539_contract.rs`). A
//! runtime-shaped reverse guard would need a stagger-off knob and
//! sample many runs to statistically prove races, which is exactly the
//! flake-prone shape 0.5.38 was fixing. Locking the source is faster,
//! deterministic, and just as defensive.
//!
//! Pending: te sign-off on the contract shape (per 0539 leader
//! dispatch — contract files are te-owned; developer proposes, te
//! signs).

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use std::fs;
use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn read_source() -> String {
    fs::read_to_string(repo_root().join("crates/team-agent/src/lifecycle/restart/rebuild.rs"))
        .expect("read rebuild.rs")
}

#[test]
fn parallel_spawn_stagger_sleep_is_present_and_plan_ordered() {
    let source = read_source();
    // The stagger sleep line must be present, plan-slot-indexed, and
    // in milliseconds. Anchoring on `slot as u64 * 10` (the current
    // value) turns any silent tweak into a review event.
    assert!(
        source.contains("std::thread::sleep(std::time::Duration::from_millis(slot as u64 * 10))"),
        "D-k reverse guard: rebuild.rs must keep the deterministic \
         per-slot stagger `std::thread::sleep(from_millis(slot as u64 * 10))` \
         in the parallel worker spawn loop. If a future pass wants to \
         reduce or remove it, that IS the review point — 3/5ms flaked \
         historically and no better primitive currently exists (Condvar \
         guards entry order but not tail lock order under jitter)."
    );
}
