//! E2E-AGENT-006 Fork-agent either creates the target or refuses explicitly.
//!
//! Fake provider workers have no native session fork. The guard accepts that
//! black-box refusal, but rejects silent green or half-created state.
//!
//! The shimmed native-fork success path is covered by the CI-runnable
//! `fork_team_scope_verifier_contract` integration test: fork -> short send ->
//! qualified-name send -> team status -> stop-agent. Keep this fake-provider
//! case focused on explicit refusal and absence of half-created state.

use crate::framework::*;

#[test]
fn agent_006_fork_agent_reports_success_or_explicit_refusal() {
    let team_id = "agent006";
    let ws = TestWorkspace::new(team_id).with_fake_spec(&["a"]);
    let qs = quick_start_fake(&ws, team_id);
    assert!(quick_start_launched(&qs), "quick-start: {}", qs.stdout);

    let out = run_ta(
        &ws,
        &[
            "fork-agent",
            "a",
            "--as",
            "forked",
            "--workspace",
            ws.path().to_str().unwrap(),
            "--no-display",
            "--json",
        ],
    );
    let j = out.json();
    let ok = j.pointer("/ok").and_then(|v| v.as_bool()).unwrap_or(false);
    let state = ws.read_state();
    if ok {
        assert_json_field_eq_str(&j, "/source_agent_id", "a");
        assert_json_field_eq_str(&j, "/new_agent_id", "forked");
        assert!(
            state_has_agent(&state, "forked"),
            "forked agent missing from state"
        );
    } else {
        let dump = serde_json::to_string(&j).unwrap();
        assert!(
            dump.contains("fork") || dump.contains("session") || dump.contains("provider"),
            "fork refusal should explain provider/session reason; got {dump}"
        );
        assert!(
            !state_has_agent(&state, "forked"),
            "failed fork must not leave forked agent in state"
        );
    }

    let _ = run_ta(
        &ws,
        &[
            "shutdown",
            "--workspace",
            ws.path().to_str().unwrap(),
            "--keep-logs",
            "--json",
        ],
    );
}
