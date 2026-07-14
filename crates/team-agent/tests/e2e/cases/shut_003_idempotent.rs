//! E2E-SHUT-003 Shutdown is idempotent.
//!
//! Black-box invariant:
//! - First shutdown after quick-start succeeds (ok:true, status:"ok").
//! - Second shutdown immediately after, on the same workspace whose
//!   sessions are already gone, must NOT return a hard error. It is
//!   acceptable to return ok:true with no killed sessions or a benign
//!   "already_stopped"/"missing" coordinator status. It must NOT panic,
//!   leak stderr "thread panicked", or return an exit non-zero.

use crate::framework::*;

#[test]
fn shut_003_shutdown_is_idempotent() {
    let team_id = "shut003";
    let ws = TestWorkspace::new(team_id).with_fake_spec(&["a"]);
    let qs = quick_start_fake(&ws, team_id);
    assert!(quick_start_launched(&qs), "quick-start: {}", qs.stdout);

    let ws_path = ws.path().to_str().unwrap();
    let first = run_ta(
        &ws,
        &["shutdown", "--workspace", ws_path, "--keep-logs", "--json"],
    );
    assert!(
        first.is_success(),
        "1st shutdown exit {}; stderr={}",
        first.exit_code,
        first.stderr
    );
    let j1 = first.json();
    assert_json_field_eq_bool(&j1, "/ok", true);

    // Second shutdown — workspace is already torn down. Should not error.
    let second = run_ta(
        &ws,
        &["shutdown", "--workspace", ws_path, "--keep-logs", "--json"],
    );
    assert!(
        second.is_success(),
        "2nd shutdown exit {}; stdout={} stderr={}",
        second.exit_code,
        second.stdout,
        second.stderr
    );
    let j2 = second.json();
    let ok2 = j2.pointer("/ok").and_then(|v| v.as_bool()).unwrap_or(false);
    assert!(ok2, "2nd shutdown must be ok:true; got {}", j2);
    assert!(
        !second.stderr.contains("panicked"),
        "2nd shutdown stderr contains panic: {}",
        second.stderr
    );
}
