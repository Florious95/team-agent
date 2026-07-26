//! E2E-REST-011 Restart resume "happy path" — JSON shape contract.
//!
//! 0.4.7 partial-resume: pure-fake teams never become resumable (no provider
//! session is captured for `provider: fake`), AND fake workers also have no
//! `first_send_at` because the leader doesn't message them in this fixture.
//! So both agents are NEVER_CAPTURED (session_id=null + first_send_at=null),
//! which partial-resume auto-freshes WITHOUT --allow-fresh — there's no
//! context to lose.
//!
//! Pre-0.4.7 behaviour: `restart` without `--allow-fresh` refused with
//! `refused_resume_atomicity`. That was the 1-blocks-N regression: a
//! never-captured worker shouldn't block sibling restart.
//!
//! Invariants asserted (the contract):
//! - `restart` without `--allow-fresh` on a never-captured fake team
//!   returns `ok:true`, `status:"restarted"`.
//! - The exit code is 0 (CLI prints structured JSON, does not crash).
//!
//! The "preserve never-silently-fresh guard" path (session_id=null +
//! first_send_at=Valid → still Refuse) is tested in
//! `upgrade_compat_0211_red::restart_refuses_interacted_claude_worker_without_session_id_partial_resume_preserves_guard`.

use crate::framework::*;

#[test]
fn rest_011_restart_never_captured_fake_team_auto_freshes_partial_resume() {
    let team_id = "rest011";
    let ws = TestWorkspace::new(team_id).with_fake_spec(&["a", "b"]);
    let ws_path = ws.path().to_str().unwrap();
    let qs = quick_start_fake(&ws, team_id);
    assert!(quick_start_launched(&qs), "quick-start: {}", qs.stdout);

    let _ = run_ta(
        &ws,
        &["shutdown", "--workspace", ws_path, "--keep-logs", "--json"],
    );

    let out = run_ta(&ws, &["restart", ws_path, "--json"]);
    assert!(
        !out.stderr.contains("panicked"),
        "restart stderr contains panic: {}",
        out.stderr
    );
    let j = out.json();

    // 0.4.7: never-captured workers auto-fresh without --allow-fresh.
    assert_json_field_eq_bool(&j, "/ok", true);
    let status = j.pointer("/status").and_then(|v| v.as_str()).unwrap_or("");
    assert_eq!(
        status, "restarted",
        "0.4.7 partial-resume: never-captured fake team must auto-fresh \
         (no context to lose); got status={status:?}; json={j}"
    );
}
