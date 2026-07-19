//! Batch 8 F7 RED: daemon-shim lifecycle contracts.
//!
//! Leader msg_590b4dce0f68, referencing §Shim Lifecycle:
//!
//! > shim 是 per-(workspace,team) 长寿进程,coordinator 可死可重连,
//! > shim 死=transport session 死(诚实上报,restart 恢复)
//!
//! These are STRUCTURAL / API-shape contracts that lock the coordinator-
//! owned shim spawn path and the reconnect-on-restart semantics.
//!
//! Real-machine verification lives in the batch report; these tests
//! guard the code shape so a future refactor cannot silently regress
//! back to "quick-start owns the shim" (Batch 6/7 shape).
//!
//! Compile-time contracts (both platforms):
//! - `coordinator::conpty_shim::ensure_shim_running` exists and takes
//!   `(workspace, team_key, workspace_hash)`.
//! - `coordinator::conpty_shim::reconnect_recorded_shim` exists and
//!   takes `(workspace, team_key, workspace_hash)`.
//! - `coordinator::conpty_shim::mark_transport_unavailable` exists
//!   and emits the C-3 stale-family typed event.
//! - `lifecycle::launch.rs` no longer references
//!   `spawn_shim_and_handshake` directly (it goes through
//!   `ensure_coordinator_running` which internally calls into the
//!   coordinator ensure-shim path).

#[path = "support/composite_source.rs"]
mod composite_source;
use std::path::PathBuf;

fn read_src(rel: &str) -> String {
    composite_source::composite_source(&format!("src/{rel}"))
}

/// Strip `//` comment lines so grep guards don't fire on doc-comments
/// that reference the API for context.
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
fn coordinator_owns_shim_spawn_not_quick_start() {
    // Batch 8 F7 lock: `lifecycle/launch.rs` must NOT call
    // `spawn_shim_and_handshake` directly. quick-start ensures the
    // COORDINATOR is running; the coordinator ensures the SHIM is
    // running. This preserves the "coord can die, shim survives"
    // property in the leader's design.
    let launch = read_src("lifecycle/launch.rs");
    let code = strip_comments(&launch);
    assert!(
        !code.contains("spawn_shim_and_handshake"),
        "Batch 8 lock: lifecycle/launch.rs must not call \
         `spawn_shim_and_handshake` directly. Coordinator owns the shim; \
         quick-start ensures coordinator, coordinator ensures shim."
    );
}

#[test]
fn coordinator_ensure_shim_api_shape_present() {
    // Batch 8 F7 lock: coordinator MUST expose an `ensure_shim_running`
    // fn that is idempotent (spawns if not running, reconnects if
    // running-but-not-yet-wired). This is the ONE entry point
    // quick-start/restart/tick call.
    let shim_mod = read_src("coordinator/conpty_shim.rs");
    let code = strip_comments(&shim_mod);
    assert!(
        code.contains("pub fn ensure_shim_running"),
        "coordinator::conpty_shim::ensure_shim_running must be a \
         public fn — this is quick-start's Batch 8 entry point."
    );
}

#[test]
fn coordinator_reconnect_api_shape_present() {
    // Batch 8 F7 lock: coordinator MUST expose a
    // `reconnect_recorded_shim` fn that reads
    // `state.transport.shim.pipe_name`, opens a new `NamedPipeClient`,
    // and performs Hello. This is what a fresh coord (post-crash)
    // calls to restore the shim link.
    let shim_mod = read_src("coordinator/conpty_shim.rs");
    let code = strip_comments(&shim_mod);
    assert!(
        code.contains("pub fn reconnect_recorded_shim"),
        "coordinator::conpty_shim::reconnect_recorded_shim must be a \
         public fn — this is the post-crash restart reconnect path."
    );
}

#[test]
fn transport_unavailable_event_family_named_stale() {
    // Batch 8 F7 lock: shim death detection emits an event whose
    // name lives in the C-3 stale event family so the audit log
    // groups shim-death alongside other stale-transport signals.
    let shim_mod = read_src("coordinator/conpty_shim.rs");
    let code = strip_comments(&shim_mod);
    assert!(
        code.contains("transport.conpty_shim_unavailable")
            || code.contains("transport.stale.conpty_shim"),
        "shim-death event must be in the C-3 stale family. Expected \
         `transport.conpty_shim_unavailable` or \
         `transport.stale.conpty_shim`."
    );
}

#[test]
fn shim_spawn_uses_detached_process_flag_on_windows() {
    // Batch 8 F7 anchor (Windows-specific): shim spawn should
    // set the DETACHED_PROCESS + CREATE_BREAKAWAY_FROM_JOB
    // creation flags so the shim truly outlives its parent
    // (coord). Without this, Windows job-object semantics can
    // still kill the child when the parent exits (depending on
    // whether the parent was launched under a job object — a
    // common quick-start scenario).
    //
    // On Unix this constraint is a no-op (the shim isn't spawned
    // on Unix), so the test just checks the source code shape.
    let shim_mod = read_src("coordinator/conpty_shim.rs");
    let code = strip_comments(&shim_mod);
    // Accept either the raw flag names or the CommandExt trait
    // usage that sets them.
    assert!(
        code.contains("DETACHED_PROCESS")
            || code.contains("CREATE_BREAKAWAY_FROM_JOB")
            || code.contains("creation_flags"),
        "shim spawn on Windows must set detached-process flags so \
         the shim survives coord exit. Expected one of \
         `DETACHED_PROCESS`, `CREATE_BREAKAWAY_FROM_JOB`, or \
         `creation_flags` in `coordinator::conpty_shim`."
    );
}
