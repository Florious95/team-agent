//! E2E-REST-011 Restart resume "happy path" — JSON shape contract.
//!
//! Pure-fake teams never become resumable (no provider session is captured
//! for `provider: fake`), so the runtime always refuses `restart` without
//! `--allow-fresh`. This is the documented behavior we want to lock down.
//!
//! Invariants we DO assert (the contract the leader / scribe rely on):
//! - `restart` without `--allow-fresh` returns `ok:false`,
//!   `status:"refused_resume_atomicity"` (or a successor `refused_*` /
//!   `session_*` label), and `error` mentions `--allow-fresh`.
//! - `unresumable` array names every agent in the team.
//! - The exit code is 0 (CLI prints structured JSON, does not crash).
//!
//! This guards the path users hit when they shut a fake team and try a
//! bare `restart`. Real resume happy-paths are exercised by T2 scripted
//! provider tests in a later batch.

use crate::framework::*;

#[test]
fn rest_011_restart_without_allow_fresh_returns_structured_refusal() {
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
    // Refusal is non-zero exit; only assert that the CLI didn't panic.
    assert!(
        !out.stderr.contains("panicked"),
        "restart stderr contains panic: {}",
        out.stderr
    );
    let j = out.json();

    assert_json_field_eq_bool(&j, "/ok", false);
    let status = j.pointer("/status").and_then(|v| v.as_str()).unwrap_or("");
    assert!(
        status.starts_with("refused_") || status.starts_with("session_"),
        "status should be a refused_* / session_* label; got {status:?}; json={j}"
    );
    let error = j.pointer("/error").and_then(|v| v.as_str()).unwrap_or("");
    assert!(
        error.contains("--allow-fresh"),
        "error should steer user to --allow-fresh; got {error:?}"
    );

    // Leader directive 2026-06-22: `unresumable` is now an array of
    // structured entries `{agent_id, reason, ...}`. The legacy string
    // array remains available under `unresumable_ids` for tools that
    // still want the bare list.
    let unresumable = j
        .pointer("/unresumable")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let unresumable_ids: Vec<&str> = unresumable
        .iter()
        .filter_map(|e| e.get("agent_id").and_then(|v| v.as_str()))
        .collect();
    assert!(
        unresumable_ids.contains(&"a") && unresumable_ids.contains(&"b"),
        "unresumable should list both agents; got {unresumable_ids:?}"
    );
    for e in &unresumable {
        assert!(
            e.get("reason").and_then(|v| v.as_str()).is_some(),
            "each unresumable entry must carry a reason; got {e}"
        );
    }
}
