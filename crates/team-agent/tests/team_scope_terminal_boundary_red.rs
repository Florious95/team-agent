//! CONCERN boundary contract (r13): the terminal exact-projection edge.
//!
//! `resolve_runtime_team_scope` lets an EXACT canonical key project a
//! terminal/shutdown team (restart-after-shutdown compatibility). Because the
//! resolver is command-agnostic, that same terminal projection is reachable by
//! every consumer (status/send/lifecycle). This contract pins the semantic
//! boundary so the terminal edge cannot silently widen:
//!
//!   1. A terminal team is NOT a default alive candidate.
//!   2. A terminal team is reachable ONLY by its explicit exact canonical key;
//!      an invalid/non-active alias (including `current` when it does not
//!      resolve to that team) is refused, never widened into the terminal team.
//!
//! Written from the observed field shape + locate §6.1 ("terminal candidate
//! depends on explicit lifecycle status; owner binding is not liveness") — not
//! from the producer's fix reasoning.

use serde_json::json;
use std::path::Path;
use team_agent::state::persist::save_runtime_state;
use team_agent::state::projection::{resolve_runtime_team_scope, team_state_candidates};

/// active `alpha` (alive) + `oldshut` (an explicit shutdown/terminal team that
/// still has a teams entry).
fn seed(ws: &Path) {
    let state = json!({
        "active_team_key": "alpha",
        "session_name": "team-alpha",
        "agents": { "verifier": { "status": "running", "provider": "fake" } },
        "teams": {
            "alpha": { "status": "alive", "session_name": "team-alpha",
                       "agents": { "verifier": { "status": "running", "provider": "fake" } } },
            "oldshut": { "status": "shutdown", "session_name": "team-oldshut",
                         "agents": { "w1": { "status": "stopped", "provider": "fake" } } }
        }
    });
    std::fs::create_dir_all(ws.join(".team/runtime")).unwrap();
    save_runtime_state(ws, &state).unwrap();
    std::fs::write(ws.join("team.spec.yaml"), "name: alpha\n").unwrap();
}

fn fresh(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("tk-term-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    seed(&dir);
    dir
}

#[test]
fn terminal_team_is_not_a_default_alive_candidate() {
    let ws = fresh("cand");
    let state = team_agent::state::persist::load_runtime_state(&ws).unwrap();
    let candidates = team_state_candidates(&state);
    let _ = std::fs::remove_dir_all(&ws);
    assert!(
        !candidates.contains_key("oldshut"),
        "a shutdown/terminal team must not be a default alive candidate; candidates={:?}",
        candidates.keys().collect::<Vec<_>>()
    );
    assert!(
        candidates.contains_key("alpha"),
        "the alive team must remain a candidate; candidates={:?}",
        candidates.keys().collect::<Vec<_>>()
    );
}

#[test]
fn terminal_team_reachable_only_by_exact_key_not_by_widened_alias() {
    let ws = fresh("reach");

    // (a) explicit exact canonical key CAN address the terminal team
    //     (restart-after-shutdown compatibility).
    let exact = resolve_runtime_team_scope(&ws, Some("oldshut"))
        .expect("explicit exact terminal key must resolve for lifecycle ops");
    assert_eq!(
        exact.canonical_team_key, "oldshut",
        "explicit exact key must address the terminal team; got {}",
        exact.canonical_team_key
    );

    // (b) an invalid/non-active alias must NOT be widened into the terminal
    //     team — it fails closed. `current` here resolves to the ACTIVE team
    //     (alpha), never to the terminal `oldshut`.
    let via_current = resolve_runtime_team_scope(&ws, Some("current"))
        .expect("current resolves to the active team");
    assert_eq!(
        via_current.canonical_team_key, "alpha",
        "current must resolve to the active team, never inherit the terminal team; got {}",
        via_current.canonical_team_key
    );

    // (c) a bogus alias fails closed, is never widened into the terminal team.
    let bogus = resolve_runtime_team_scope(&ws, Some("oldshu"));
    let _ = std::fs::remove_dir_all(&ws);
    match bogus {
        Ok(sel) => panic!(
            "a near-miss alias must fail closed, not widen into the terminal team; got {}",
            sel.canonical_team_key
        ),
        Err(e) => assert!(
            !e.to_string().contains("oldshut")
                || e.to_string().to_lowercase().contains("not found"),
            "the refusal must not silently select the terminal team: {e}"
        ),
    }
}

#[test]
fn current_fails_closed_when_active_team_key_points_at_a_terminal_team() {
    // r14 boundary + leader policy: `current` means "the currently ACTIVE and
    // ALIVE team". If active_team_key itself names a terminal/shutdown team,
    // `current` must fail closed LOUDLY — never silently select the dead team,
    // and with NO restart/command special case. A shutdown team is reachable
    // only by its explicit exact key; the error must guide the user there.
    let dir = std::env::temp_dir().join(format!("tk-term-{}-activeterm", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join(".team/runtime")).unwrap();
    let state = json!({
        "active_team_key": "oldshut",
        "session_name": "team-oldshut",
        "teams": {
            "oldshut": { "status": "shutdown", "session_name": "team-oldshut",
                         "agents": { "w1": { "status": "stopped", "provider": "fake" } } }
        }
    });
    save_runtime_state(&dir, &state).unwrap();
    std::fs::write(dir.join("team.spec.yaml"), "name: oldshut\n").unwrap();

    let result = resolve_runtime_team_scope(&dir, Some("current"));
    let _ = std::fs::remove_dir_all(&dir);
    match result {
        Ok(sel) => panic!(
            "current must fail closed when active_team_key points at a terminal team, \
             not silently select it; got canonical={}",
            sel.canonical_team_key
        ),
        Err(e) => {
            let msg = e.to_string();
            let lowered = msg.to_lowercase();
            assert!(
                !msg.contains("canonical=oldshut"),
                "the refusal must not resolve to the terminal team: {msg}"
            );
            // Policy: the error must guide the user to address a shutdown team
            // by its explicit team key, not leave them stranded on `current`.
            assert!(
                lowered.contains("team key")
                    || lowered.contains("--team")
                    || lowered.contains("explicit"),
                "the fail-closed error must guide the user to use an explicit team key \
                 (a shutdown team is reachable only by exact key, no current/restart special case): {msg}"
            );
        }
    }
}
