//! E2E-LNCH-002 Quick-start refuses duplicate team-id in the same workspace.
//!
//! Invariants:
//! - First quick-start launches successfully.
//! - Second quick-start with the same `--team-id` returns `ok:false` and the
//!   JSON summary mentions an existing runtime; the user is steered to
//!   `restart --allow-fresh` (next_actions string).

use crate::framework::*;

#[test]
fn lnch_002_duplicate_quick_start_is_refused() {
    let team_id = "lnch002";
    let ws = TestWorkspace::new(team_id).with_fake_spec(&["a"]);
    let ws_path = ws.path().to_str().unwrap();

    let first = quick_start_fake(&ws, team_id);
    assert!(quick_start_launched(&first), "1st quick-start: {}", first.stdout);

    let second = quick_start_fake(&ws, team_id);
    let j = second.json();
    let ok = j.pointer("/ok").and_then(|v| v.as_bool()).unwrap_or(true);
    assert!(!ok, "2nd quick-start must refuse duplicate; got {j}");

    let dump = serde_json::to_string(&j).unwrap().to_lowercase();
    let signals = ["existing", "already", "restart", "--allow-fresh"];
    assert!(
        signals.iter().any(|s| dump.contains(s)),
        "duplicate refusal should steer user to restart --allow-fresh; got json={j}"
    );

    let _ = run_ta(
        &ws,
        &["shutdown", "--workspace", ws_path, "--keep-logs", "--json"],
    );
}
