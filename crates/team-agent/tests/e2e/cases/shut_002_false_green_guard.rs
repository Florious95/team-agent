//! E2E-SHUT-002 Shutdown False-Green Guard.
//!
//! Known bug (0.3.39): shutdown returned `ok:true` with `session_killed:false`
//! when state was dirty enough that the topology decision punted but JSON
//! still claimed success.
//!
//! Black-box invariant:
//! - If `session_killed == false`, the JSON must NOT be a plain green.
//!   Acceptable: `ok==false`, or `status` in {failed, partial, dirty_state,
//!   refused_*, blocked}, or per-pane explicit kill proof.
//!
//! We reproduce a dirty state by booting a real quick-started team and then
//! pointing `leader_receiver.session_name` to the worker session (collision
//! between leader anchor and worker session) before calling shutdown.

use crate::framework::*;
use serde_json::json;

#[test]
fn shut_002_false_green_guard() {
    let team_id = "shut002";
    let ws = TestWorkspace::new(team_id).with_fake_spec(&["a"]);
    let qs = quick_start_fake(&ws, team_id);
    assert!(
        quick_start_launched(&qs),
        "quick-start did not launch: {}",
        qs.stdout
    );

    let worker_session = worker_session_name(team_id);

    // Make leader_receiver collide with the worker session — classic dirty
    // topology that triggered the 0.3.39 false-green.
    ws.inject_state(
        "leader_receiver",
        json!({
            "session_name": worker_session,
            "pane_id": "%9999",
            "status": "attached"
        }),
    );

    let out = run_ta(
        &ws,
        &[
            "shutdown",
            "--workspace",
            ws.path().to_str().unwrap(),
            "--keep-logs",
            "--json",
        ],
    );

    let j = out.json();
    let session_killed = j
        .pointer("/session_killed")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let ok = j.pointer("/ok").and_then(|v| v.as_bool()).unwrap_or(false);
    let status = j
        .pointer("/status")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    if !session_killed {
        // False-green guard: must NOT be plain green.
        assert!(
            !ok || status != "ok",
            "FALSE-GREEN regression: shutdown returned ok=true status='ok' but session_killed=false. JSON: {}",
            j
        );
    }
    // We don't assert the inverse direction (session_killed==true) here —
    // either outcome is acceptable so long as the JSON faithfully reports it.
    // The bug being guarded is specifically the false-green disagreement.
}
