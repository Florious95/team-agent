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

/// Strip `//` comment lines so grep guards don't fire on doc-comments
/// that explain what's forbidden.
fn strip_comments(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    for line in src.lines() {
        if line.trim_start().starts_with("//") {
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

#[test]
fn c2_wire_rs_fallback_removed_after_batch_3() {
    // Batch 3 landed: `parent_pid()` now routes through
    // `platform::process::current_parent_pid` on both Unix and Windows.
    let body = src("mcp_server/wire.rs");
    // Fallback SHOULD BE GONE — no cfg-gated non-Unix `parent_pid` fn.
    assert!(
        !(body.contains("#[cfg(not(unix))]") && body.contains("fn parent_pid()")),
        "CR C-2: wire.rs `parent_pid()` non-Unix cfg-gated fallback must \
         be REMOVED after Batch 3. Reintroduction returns Windows to \
         `0` (fake parent pid → MCP metadata + orphan detection break)."
    );
    // FIXME marker must be gone at this site.
    assert!(
        !body.contains("FIXME(portability)"),
        "CR C-2: wire.rs no longer needs the FIXME marker after Batch 3."
    );
    // Positive anchor: platform migration marker present.
    assert!(
        body.contains("crate::platform::process::current_parent_pid"),
        "Batch 3 migration marker missing: `parent_pid` must call \
         `crate::platform::process::current_parent_pid`."
    );
    // Legacy libc call gone from CODE (not from doc comments that
    // reference the API for context).
    let code = strip_comments(&body);
    assert!(
        !code.contains("libc::getppid"),
        "CR C-2: wire.rs must NOT call `libc::getppid` directly after \
         Batch 3 — the getppid goes through `platform::process`."
    );
}

#[test]
fn c2_restart_agent_fallback_removed_after_batch_3() {
    // Batch 3 landed: `pid_is_alive` now routes through
    // `platform::process::pid_is_alive` on both platforms.
    let body = src("lifecycle/restart/agent.rs");
    // The old cfg-gated non-Unix `true` fallback SHOULD BE GONE. A
    // pattern-based check: the specific comment marker used inside
    // the old fallback must not reappear.
    assert!(
        !body.contains("On other platforms, returns true conservatively"),
        "CR C-2: restart/agent.rs `pid_is_alive() = true` non-Unix \
         fallback must be REMOVED after Batch 3. Reintroduction \
         breaks drain by treating every stale pid as alive."
    );
    // FIXME marker must be gone.
    assert!(
        !body.contains("FIXME(portability)"),
        "CR C-2: restart/agent.rs no longer needs the FIXME marker \
         after Batch 3."
    );
    // Positive anchor: platform migration marker present.
    assert!(
        body.contains("crate::platform::process::pid_is_alive"),
        "Batch 3 migration marker missing: `pid_is_alive` must call \
         `crate::platform::process::pid_is_alive`."
    );
    // Legacy libc calls gone from CODE (not from doc comments).
    let code = strip_comments(&body);
    assert!(
        !code.contains("libc::kill(pid as i32, 0)"),
        "CR C-2: restart/agent.rs must NOT call `libc::kill(pid, 0)` \
         directly after Batch 3."
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
    // Legacy Unix inline calls must be gone from CODE (not from doc
    // comments).
    let code = strip_comments(&body);
    assert!(
        !code.contains("libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB)"),
        "CR C-2: persist.rs must NOT call `libc::flock` directly after \
         Batch 2 — the whole polling loop goes through \
         `platform::file_lock`."
    );
}
