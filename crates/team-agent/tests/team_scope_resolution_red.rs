//! RED contract: single-source team-scope resolution.
//!
//! Case: team-key-split-brain (baseline main a166ee19). Written from the §7
//! observation points and §6.1 target invariants — NOT from the locate's
//! line-level root-cause reasoning (information isolation: the verifier must
//! not inherit the producer's hypotheses).
//!
//! Two resolver-layer invariants that are DETERMINISTICALLY red at baseline,
//! each with a fixture reproducing the observed field shape from the live
//! 0.5.47 workspace (canonical `alpha` active team + an owner-only, zero-agent,
//! no-status stale `old-team` stub, mirroring the real `team-ta-rs-acceptance`
//! stub):
//!
//!   1. An invalid alias (0 matches) fails CLOSED — it returns an error, and
//!      must NOT be silently rescued by falling back to raw root state.
//!   2. An owner-only / 0-agent / no-status stub is NOT an alive team candidate.
//!
//! Teeth (verified, each independently): baseline (with selector's
//! `.or_else(load_runtime_state)`) -> (1) red; a fix removing that fallback ->
//! (1) green. Baseline alive predicate -> (2) red; excluding owner-only stubs
//! -> (2) green.
//!
//! The `current -> active_team_key` alias (locate §7.1) is deliberately NOT
//! forced red here: at this pure-resolver layer, when active_team_key is the
//! intended team, `--team current` resolves to the right key regardless of
//! whether a real alias exists. Its split-brain only manifests at the
//! CLI/lifecycle layer (re-invoked selector + stale active_team_key), so it is
//! contracted there — fabricating a red for it here would be a false red with
//! no real weakening behind it.

use serde_json::json;
use std::path::Path;
use team_agent::state::persist::save_runtime_state;
use team_agent::state::projection::team_state_candidates;
use team_agent::state::selector::{resolve_active_team, SelectorMode};

/// Field shape from the observed live workspace: canonical `alpha` is the
/// active team with a real agent; `old-team` is an owner-only, 0-agent,
/// no-status stub (the split-brain candidate).
fn seed(ws: &Path) {
    let state = json!({
        "active_team_key": "alpha",
        "session_name": "team-alpha",
        "agents": { "verifier": { "status": "running", "provider": "fake" } },
        "teams": {
            "alpha": {
                "status": "alive",
                "session_name": "team-alpha",
                "agents": { "verifier": { "status": "running", "provider": "fake" } }
            },
            "old-team": {
                "team_owner": {
                    "pane_id": "%517",
                    "leader_session_uuid": "4422569976dc3afb602f702361fd7ef7",
                    "claimed_at": "2026-06-03T05:29:23.806758+00:00"
                }
            }
        }
    });
    std::fs::create_dir_all(ws.join(".team/runtime")).unwrap();
    save_runtime_state(ws, &state).unwrap();
    // A team.spec.yaml so resolve_active_team takes the explicit-spec branch.
    std::fs::write(ws.join("team.spec.yaml"), "name: alpha\n").unwrap();
}

fn fresh_ws(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("teamscope-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    seed(&dir);
    dir
}

// NOTE (contract boundary, reported to owner-b): the `current -> active_team_key`
// alias defect (§7.1) is NOT independently red at this pure-resolver layer —
// when active_team_key is the intended team, `--team current` "resolves" to the
// right key regardless (whether by a real alias or by the raw fallback the other
// two invariants below pin). Its split-brain only manifests at the CLI/lifecycle
// layer where the SAME selector is re-invoked and a stale active_team_key is
// re-derived (positional send / start-agent / restart). It is therefore
// contracted in the CLI layer, not fabricated red here. Fabricating a red for it
// at this layer would be a false-green-hole in reverse (a red with no real
// weakening behind it).

#[test]
fn invalid_alias_fails_closed_without_raw_fallback() {
    let ws = fresh_ws("invalid");
    let result = resolve_active_team(&ws, Some("does-not-exist"), SelectorMode::RuntimeOnly);
    let _ = std::fs::remove_dir_all(&ws);
    // Fail-closed means: return an error, NOT an Ok(SelectedTeam) that silently
    // rescued the raw active team. (An error message that LISTS alpha as a valid
    // candidate is correct and expected — the failure is a rescued *selection*,
    // not a mention.)
    match result {
        Ok(selected) => panic!(
            "an unresolvable --team alias must fail closed, not fall back to raw root \
             state; got a rescued Ok(team_key={})",
            selected.team_key
        ),
        Err(e) => {
            let msg = e.to_string().to_lowercase();
            assert!(
                msg.contains("not found")
                    || msg.contains("unresolved")
                    || msg.contains("does-not-exist"),
                "the fail-closed error must name the unresolved target, not a generic \
                 fallback success: {e}"
            );
        }
    }
}

#[test]
fn owner_only_zero_agent_no_status_stub_is_not_an_alive_candidate() {
    let ws = fresh_ws("stub");
    let state = team_agent::state::persist::load_runtime_state(&ws).unwrap();
    let candidates = team_state_candidates(&state);
    let _ = std::fs::remove_dir_all(&ws);
    assert!(
        !candidates.contains_key("old-team"),
        "an owner-only, 0-agent, no-status team stub must NOT be an alive team \
         candidate (a stale team_owner binding is transport attachment, not team \
         liveness); candidates={:?}",
        candidates.keys().collect::<Vec<_>>()
    );
    assert!(
        candidates.contains_key("alpha"),
        "the real alive team must still be a candidate; candidates={:?}",
        candidates.keys().collect::<Vec<_>>()
    );
}
