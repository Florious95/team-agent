//! Issue #432 RED (in-process, Claude argv): reset with `--discard-session` must reload the
//! seat's role file from disk, so the respawned Claude command carries the edited
//! `dangerously_skip_permissions` / `model` / `effort` and the seat row records them.
//!
//! Layout mirrors the public quick start: role files live in `<ws>/.team/current/agents/`,
//! the compiled spec in `<ws>/.team/runtime/<team>/team.spec.yaml`. The transport is the
//! offline recorder (no tmux, no provider process); launch and reset run the same
//! lifecycle code as `team-agent start` / `team-agent agent reset`.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[path = "../../../tests/support/hermetic.rs"]
mod hermetic_guard;

use std::path::{Path, PathBuf};

use serde_json::Value;
use serial_test::serial;

use crate::lifecycle::{quick_start_with_transport_in_workspace, reset_agent_with_transport, ResetAgentOutcome};
use crate::model::ids::AgentId;
use crate::transport::test_support::OfflineTransport;

const TEAM: &str = "resetcfg";
const AGENT: &str = "worker";
const OLD_MODEL: &str = "claude-sonnet-4-6";
const NEW_MODEL: &str = "claude-opus-4-1";
const OLD_EFFORT: &str = "low";
const NEW_EFFORT: &str = "high";
const BYPASS_FLAG: &str = "--dangerously-skip-permissions";

fn role(bypass: bool, model: &str, effort: &str) -> String {
    format!(
        "---\nname: {AGENT}\nrole: Claude Worker\nprovider: claude\nmodel: {model}\neffort: {effort}\nauth_mode: subscription\ndangerously_skip_permissions: {bypass}\ntools:\n  - mcp_team\n---\n\nHandle the assigned message.\n"
    )
}

fn write_team(ws: &Path) -> PathBuf {
    let team = ws.join(".team").join("current");
    std::fs::create_dir_all(team.join("agents")).unwrap();
    std::fs::write(
        team.join("TEAM.md"),
        format!("---\nname: {TEAM}\nobjective: Issue 432 reset reload.\nprovider: claude\n---\n\nTeam.\n"),
    )
    .unwrap();
    std::fs::write(team.join("agents").join(format!("{AGENT}.md")), role(false, OLD_MODEL, OLD_EFFORT)).unwrap();
    team
}

fn value_after<'a>(argv: &'a [String], flag: &str) -> Option<&'a str> {
    argv.windows(2).find(|pair| pair[0] == flag).map(|pair| pair[1].as_str())
}

fn count(argv: &[String], flag: &str) -> usize {
    argv.iter().filter(|arg| *arg == flag).count()
}

fn rows(ws: &Path) -> Vec<(String, Value)> {
    let state = crate::state::persist::load_runtime_state(ws).unwrap();
    [format!("/teams/{TEAM}/agents/{AGENT}"), format!("/agents/{AGENT}")]
        .into_iter()
        .filter_map(|pointer| state.pointer(&pointer).cloned().map(|row| (pointer, row)))
        .collect()
}

#[test]
#[serial(env)]
fn reset_discard_session_respawns_claude_with_role_file_edited_on_disk() {
    let env = hermetic_guard::HermeticTestEnv::enter_with_leader("i432-claude-reset");
    let ws = env.workspace("i432");
    super::launch_spawn::seed_healthy_coordinator(&ws);
    let team = write_team(&ws);
    let role_path = team.join("agents").join(format!("{AGENT}.md"));
    let transport = OfflineTransport::new();

    // Step 1: launch with bypass=false / model=OLD / effort=low.
    quick_start_with_transport_in_workspace(&ws, &team, None, true, Some(TEAM), &transport)
        .expect("[i432] fixture precondition: quick start must launch the Claude seat");
    let launches = transport.spawn_records();
    let launch_argv = launches.last().map(|(_, argv)| argv.clone()).unwrap_or_default();
    let launch_rows = rows(&ws);
    eprintln!("[i432] after start: spawns={launches:?}\n[i432] after start: rows={launch_rows:#?}");
    assert_eq!(launches.len(), 1, "[i432] fixture precondition: exactly one launch spawn; spawns={launches:?}");
    assert_eq!(
        (
            count(&launch_argv, BYPASS_FLAG),
            value_after(&launch_argv, "--permission-mode"),
            value_after(&launch_argv, "--model"),
            value_after(&launch_argv, "--effort"),
        ),
        (0, Some("default"), Some(OLD_MODEL), Some(OLD_EFFORT)),
        "[i432] fixture precondition: initial launch must honor the original role file; argv={launch_argv:?}"
    );

    // Step 2: edit the role file on disk.
    let edited = role(true, NEW_MODEL, NEW_EFFORT);
    std::fs::write(&role_path, &edited).unwrap();
    eprintln!("[i432] edited role file {}:\n{edited}", role_path.display());

    // Step 3: reset with --discard-session.
    let outcome = reset_agent_with_transport(&ws, &AgentId::new(AGENT), true, false, Some(TEAM), &transport);
    eprintln!("[i432] reset outcome: {outcome:?}");
    assert!(
        matches!(outcome, Ok(ResetAgentOutcome::Reset { .. })),
        "[i432] reset --discard-session must complete on an idle seat; got {outcome:?}"
    );

    // Step 4: the respawn and the seat row must reflect the edited role file.
    let spawns = transport.spawn_records();
    let reset_argv = spawns.last().map(|(_, argv)| argv.clone()).unwrap_or_default();
    let reset_rows = rows(&ws);
    let mut failures = Vec::new();
    if spawns.len() != 2 {
        failures.push(format!("reset must respawn exactly once; total spawns={}", spawns.len()));
    }
    if count(&reset_argv, BYPASS_FLAG) != 1 {
        failures.push(format!(
            "respawn argv must contain `{BYPASS_FLAG}` exactly once (role file now says dangerously_skip_permissions: true); found {}",
            count(&reset_argv, BYPASS_FLAG)
        ));
    }
    if let Some(mode) = value_after(&reset_argv, "--permission-mode") {
        failures.push(format!("respawn argv must not keep the restricted `--permission-mode {mode}`"));
    }
    for (flag, expected) in [("--model", NEW_MODEL), ("--effort", NEW_EFFORT)] {
        if value_after(&reset_argv, flag) != Some(expected) {
            failures.push(format!(
                "respawn argv must pass `{flag} {expected}` (edited role file); found {flag} {:?}",
                value_after(&reset_argv, flag)
            ));
        }
    }
    if reset_rows.is_empty() {
        failures.push(format!("no seat row for {AGENT} in state.json after reset"));
    }
    for (pointer, row) in &reset_rows {
        for (field, expected) in [
            ("/as_launched_dangerously_skip_permissions", Value::Bool(true)),
            ("/effective_approval_policy/enabled", Value::Bool(true)),
            ("/model", Value::String(NEW_MODEL.into())),
            ("/effort", Value::String(NEW_EFFORT.into())),
        ] {
            let actual = row.pointer(field).cloned().unwrap_or(Value::Null);
            if actual != expected {
                failures.push(format!("state.json {pointer}{field} must be {expected} after reset; found {actual}"));
            }
        }
    }
    let spec_path = crate::model::paths::runtime_spec_path(&ws, TEAM);
    let spec_text = std::fs::read_to_string(&spec_path).unwrap_or_else(|e| format!("(unreadable: {e})"));
    assert!(
        failures.is_empty(),
        "[i432] RED: reset --discard-session respawned Claude with the STALE launch-time declaration instead of the role file on disk.\n\
         failures:\n  - {}\n\
         role file ({}):\n{}\n\
         launch argv (before edit): {launch_argv:?}\n\
         reset argv  (after edit):  {reset_argv:?}\n\
         seat rows before reset: {launch_rows:#?}\n\
         seat rows after reset:  {reset_rows:#?}\n\
         compiled runtime spec after reset ({}):\n{spec_text}",
        failures.join("\n  - "),
        role_path.display(),
        std::fs::read_to_string(&role_path).unwrap(),
        spec_path.display(),
    );
}
