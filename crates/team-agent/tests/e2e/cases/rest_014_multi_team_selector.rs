//! E2E-REST-014 Multi-team selector.
//!
//! Two teams in the same workspace; `restart --team <id>` targets only the
//! named team.
//!
//! Invariants:
//! - Quick-starting two team-ids in the same workspace yields a state with
//!   `teams.<a>` and `teams.<b>` (or, less strictly, the `--team` selector
//!   round-trips through the JSON).
//! - `restart --team <id> --allow-fresh --json` returns ok:true and the
//!   `session_name` in the JSON corresponds to `team-<id>`.

use crate::framework::*;

#[test]
fn rest_014_restart_team_selector_targets_named_team() {
    let ws = TestWorkspace::new("rest014").with_fake_spec(&["a"]);
    let ws_path = ws.path().to_str().unwrap();
    let team_a = "rest014a";
    let team_b = "rest014b";

    let qs_a = quick_start_fake(&ws, team_a);
    assert!(quick_start_launched(&qs_a), "qs a: {}", qs_a.stdout);
    let qs_b = quick_start_fake(&ws, team_b);
    assert!(quick_start_launched(&qs_b), "qs b: {}", qs_b.stdout);

    // Shut both down so restart picks fresh launches.
    let _ = run_ta(
        &ws,
        &[
            "shutdown",
            "--workspace",
            ws_path,
            "--team",
            team_a,
            "--keep-logs",
            "--json",
        ],
    );
    let _ = run_ta(
        &ws,
        &[
            "shutdown",
            "--workspace",
            ws_path,
            "--team",
            team_b,
            "--keep-logs",
            "--json",
        ],
    );

    let out = run_ta(
        &ws,
        &[
            "restart",
            ws_path,
            "--team",
            team_a,
            "--allow-fresh",
            "--json",
        ],
    );
    let j = out.json();
    assert_json_field_eq_bool(&j, "/ok", true);
    let session_name = j
        .pointer("/session_name")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let expected = worker_session_name(team_a);
    assert_eq!(
        session_name, expected,
        "restart --team {team_a} should target session {expected}; got {session_name:?} (json={j})"
    );

    // Cleanup
    let _ = run_ta(
        &ws,
        &[
            "shutdown",
            "--workspace",
            ws_path,
            "--team",
            team_a,
            "--keep-logs",
            "--json",
        ],
    );
    let _ = run_ta(
        &ws,
        &[
            "shutdown",
            "--workspace",
            ws_path,
            "--team",
            team_b,
            "--keep-logs",
            "--json",
        ],
    );
}
