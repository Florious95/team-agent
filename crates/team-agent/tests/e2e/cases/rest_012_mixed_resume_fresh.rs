//! E2E-REST-012 Restart mixed never-captured workers auto-fresh.
//!
//! 0.4.7 partial-resume note: like rest_002, this fixture's top-level
//! `/agents` injection is silently overwritten by `project_top_level_view`
//! before restart classification reads state. Both workers stay as the
//! original fake-team shape (provider=fake, session_id=null, first_send_at=
//! null) — i.e. both are NEVER_CAPTURED, and partial-resume correctly
//! auto-freshes both without --allow-fresh (no context to lose).
//!
//! This guards the multi-worker variant of the partial-resume policy:
//! restart must not refuse a multi-worker team where ALL workers are
//! never-captured, because there's nothing to recover.

use crate::framework::*;

#[test]
fn rest_012_restart_mixed_never_captured_auto_fresh_partial_resume() {
    let team_id = "rest012";
    let ws = TestWorkspace::new(team_id).with_fake_spec(&["a", "b"]);
    let qs = quick_start_fake(&ws, team_id);
    assert!(quick_start_launched(&qs), "quick-start: {}", qs.stdout);

    let ws_path = ws.path().to_str().unwrap();
    let _ = run_ta(
        &ws,
        &["shutdown", "--workspace", ws_path, "--keep-logs", "--json"],
    );

    let out = run_ta(&ws, &["restart", ws_path, "--json"]);
    let j = out.json();
    assert!(
        !out.stderr.contains("panicked"),
        "restart stderr contains panic: {}",
        out.stderr
    );

    let ok = j.pointer("/ok").and_then(|v| v.as_bool()).unwrap_or(false);
    assert!(
        ok,
        "0.4.7 partial-resume: multi-worker never-captured team must auto-fresh; got {j}"
    );
    let status = j.pointer("/status").and_then(|v| v.as_str()).unwrap_or("");
    assert_eq!(
        status, "restarted",
        "0.4.7: all workers never-captured → status=restarted; got {status:?}; json={j}"
    );

    let dump = serde_json::to_string(&j).unwrap();
    assert!(
        dump.contains("\"a\"") && dump.contains("\"b\""),
        "restart JSON should mention both worker ids; got {dump}"
    );
}
