//! Windows portability Batch 0 CR C-2 fallback burndown tracker.
//!
//! Three legacy non-Unix fallbacks currently live in the tree and
//! carry `FIXME(portability)` comments pointing at this design:
//!
//!  A. `mcp_server/wire.rs`         — `parent_pid() = 0`
//!  B. `lifecycle/restart/agent.rs` — `pid_is_alive() = true`
//!  C. `state/persist.rs`           — `RuntimeLock = not_implemented`
//!
//! Batch 0 asserts:
//! - Each of the three sites carries an active `FIXME(portability)`
//!   marker pointing at the design + a target batch number.
//! - The marker text names the specific replacement API in
//!   `crate::platform::*`.
//!
//! Batch 2 will remove (C) and update this file to assert the fallback
//! is gone.
//! Batch 3 will remove (A) and (B) and update this file the same way.
//!
//! Any grep for the removed fallback substring after its target batch
//! is a regression signal.

use std::path::PathBuf;

fn src(rel: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("src")
        .join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("cannot read {rel}: {e}"))
}

#[test]
fn c2_wire_rs_fallback_carries_active_fixme_marker() {
    let body = src("mcp_server/wire.rs");
    // Fallback still present (Batch 3 removes it).
    assert!(
        body.contains("#[cfg(not(unix))]") && body.contains("fn parent_pid()"),
        "wire.rs non-Unix `parent_pid()` fallback missing — was Batch 3 landed \
         without also updating this burndown tracker?"
    );
    // FIXME marker present with target batch.
    assert!(
        body.contains("FIXME(portability)") && body.contains("Batch 3"),
        "wire.rs non-Unix fallback must carry `FIXME(portability)` + `Batch 3` \
         markers so the burndown target is grep-visible (CR C-2)."
    );
    // Names the replacement API.
    assert!(
        body.contains("crate::platform::process::current_parent_pid"),
        "wire.rs FIXME must name the Batch 3 replacement \
         `crate::platform::process::current_parent_pid` (CR C-2)."
    );
}

#[test]
fn c2_restart_agent_fallback_carries_active_fixme_marker() {
    let body = src("lifecycle/restart/agent.rs");
    assert!(
        body.contains("#[cfg(not(unix))]") && body.contains("true"),
        "restart/agent.rs non-Unix `pid_is_alive() = true` fallback missing \
         — was Batch 3 landed without also updating this burndown tracker?"
    );
    assert!(
        body.contains("FIXME(portability)") && body.contains("Batch 3"),
        "restart/agent.rs non-Unix fallback must carry `FIXME(portability)` \
         + `Batch 3` markers (CR C-2)."
    );
    assert!(
        body.contains("crate::platform::process::pid_liveness"),
        "restart/agent.rs FIXME must name the Batch 3 replacement \
         `crate::platform::process::pid_liveness` (CR C-2)."
    );
}

#[test]
fn c2_persist_rs_fallback_removed_after_batch_2() {
    // Batch 2 landed: `RuntimeLock` now routes through
    // `platform::file_lock::{try_lock_once_nonblocking, unlock}` on
    // both Unix and Windows. The old `not yet implemented on non-unix`
    // fallback is gone.
    let body = src("state/persist.rs");
    // Fallback string must be GONE — a regression that reintroduced
    // the not-implemented branch would fire this assertion.
    assert!(
        !body.contains("runtime lock not yet implemented on non-unix"),
        "CR C-2: persist.rs `not yet implemented on non-unix` fallback \
         must be REMOVED after Batch 2. Reintroduction would put Windows \
         back on the silent-lock-never-succeeds path."
    );
    // The FIXME(portability) + Batch 2 comment block should also be
    // gone (no more work at this site).
    assert!(
        !body.contains("FIXME(portability)"),
        "CR C-2: persist.rs no longer needs the FIXME marker after \
         Batch 2. If someone reintroduced a FIXME, they should also \
         remove or narrow it."
    );
    // Positive anchor: the platform migration marker is present.
    assert!(
        body.contains("crate::platform::file_lock::try_lock_once_nonblocking")
            && body.contains("crate::platform::file_lock::unlock"),
        "Batch 2 migration marker missing: `RuntimeLock` must call \
         `crate::platform::file_lock::try_lock_once_nonblocking` (acquire) \
         and `crate::platform::file_lock::unlock` (Drop)."
    );
    // Legacy Unix inline calls must be gone from the fn body — if the
    // cfg-gated `libc::flock` block reappears, someone reverted the
    // migration.
    assert!(
        !body.contains("libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB)"),
        "CR C-2: persist.rs must NOT call `libc::flock` directly after \
         Batch 2 — the whole polling loop goes through \
         `platform::file_lock`."
    );
}
