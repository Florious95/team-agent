//! E2E-LNCH-003 Quick-start refuses spec missing required `profile` field.
//!
//! Reproduces the canonical validation error: `provider: claude` with
//! `auth_mode: compatible_api` requires `profile`. Without it, spec
//! compile must fail with a structured JSON error rather than panic.
//!
//! Invariants:
//! - ok == false
//! - JSON error mentions "profile" and "compatible_api".
//! - No `.team/runtime/state.json` is written (validation happens before
//!   runtime materialization).

use crate::framework::*;
use std::fs;

#[test]
fn lnch_003_quick_start_refuses_missing_profile() {
    let team_id = "lnch003";
    let ws = TestWorkspace::new(team_id);
    let _ws_path = ws.path().to_str().unwrap();

    // Hand-write a spec that needs `profile` but omits it.
    let team_md = "---\nname: lnch003\nobjective: missing-profile probe.\nprovider: fake\ndisplay_backend: none\n---\n\nTeam.\n";
    fs::write(ws.path().join("TEAM.md"), team_md).unwrap();
    fs::create_dir_all(ws.path().join("agents")).unwrap();
    let agent_md = "---\nname: c\nrole: Claude worker\nprovider: claude\nauth_mode: compatible_api\nmodel: null\ntools:\n  - mcp_team\n---\n\nWorker.\n";
    fs::write(ws.path().join("agents/c.md"), agent_md).unwrap();

    let out = quick_start_fake(&ws, team_id);
    let j = out.json();
    let ok = j.pointer("/ok").and_then(|v| v.as_bool()).unwrap_or(true);
    assert!(!ok, "quick-start must refuse missing profile; got {j}");

    let err = j
        .pointer("/error")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_lowercase();
    assert!(
        err.contains("profile"),
        "error should mention 'profile'; got {err:?}; json={j}"
    );
    assert!(
        err.contains("compatible_api") || err.contains("auth_mode"),
        "error should mention auth_mode/compatible_api; got {err:?}"
    );

    assert!(
        !ws.state_json_path().exists(),
        "state.json must not be created when validation fails"
    );
}
