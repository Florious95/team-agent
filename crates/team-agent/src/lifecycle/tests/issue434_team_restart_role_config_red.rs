//! Issue #434 RED (in-process, Claude): a whole-team restart must persist the role file
//! edited on disk into the seat row, not only into the respawned command line.
//!
//! Defect: team restart rebuilds the runtime spec from the role docs and spawns from it,
//! but the respawned seat row in state.json keeps the stale launch-time `model`, `effort`,
//! `profile` and `effective_approval_policy`.
//!
//! Layout mirrors the public quick start: role files in `<ws>/.team/current/agents/`, the
//! compiled spec in `<ws>/.team/runtime/<team>/team.spec.yaml`. The transport is the
//! offline recorder (no tmux, no provider process); launch and restart run the same
//! lifecycle code as `team-agent start` / `team-agent restart`.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[path = "../../../tests/support/hermetic.rs"]
mod hermetic_guard;

use std::path::{Path, PathBuf};

use serde_json::Value;
use serial_test::serial;

use crate::lifecycle::{quick_start_with_transport_in_workspace, restart_with_transport, RestartReport};
use crate::transport::test_support::OfflineTransport;

const TEAM: &str = "restartcfg";
const AGENT: &str = "worker";
const OLD_MODEL: &str = "claude-sonnet-4-6";
const NEW_MODEL: &str = "claude-opus-4-1";
const OLD_EFFORT: &str = "low";
const NEW_EFFORT: &str = "high";
const NEW_PROFILE: &str = "alt";
const BYPASS_FLAG: &str = "--dangerously-skip-permissions";

fn role(bypass: bool, model: &str, effort: &str, profile: Option<&str>) -> String {
    let profile = profile.map(|name| format!("profile: {name}\n")).unwrap_or_default();
    format!(
        "---\nname: {AGENT}\nrole: Claude Worker\nprovider: claude\nmodel: {model}\neffort: {effort}\nauth_mode: subscription\n{profile}dangerously_skip_permissions: {bypass}\ntools:\n  - mcp_team\n---\n\nHandle the assigned message.\n"
    )
}

fn write_team(ws: &Path) -> PathBuf {
    let team = ws.join(".team").join("current");
    std::fs::create_dir_all(team.join("agents")).unwrap();
    std::fs::create_dir_all(team.join("profiles")).unwrap();
    std::fs::write(
        team.join("TEAM.md"),
        format!("---\nname: {TEAM}\nobjective: Issue 434 team restart reload.\nprovider: claude\n---\n\nTeam.\n"),
    )
    .unwrap();
    // A subscription profile exports no provider env; only its selection is observable.
    std::fs::write(team.join("profiles").join(format!("{NEW_PROFILE}.env")), "AUTH_MODE=subscription\n").unwrap();
    std::fs::write(team.join("agents").join(format!("{AGENT}.md")), role(false, OLD_MODEL, OLD_EFFORT, None)).unwrap();
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
fn team_restart_persists_role_file_edited_on_disk_into_claude_seat_row() {
    let env = hermetic_guard::HermeticTestEnv::enter_with_leader("i434-claude-team-restart");
    let ws = env.workspace("i434");
    super::launch_spawn::seed_healthy_coordinator(&ws);
    let team = write_team(&ws);
    let role_path = team.join("agents").join(format!("{AGENT}.md"));
    let transport = OfflineTransport::new();

    // Step 1: launch with bypass=false / model=OLD / effort=low / no profile.
    quick_start_with_transport_in_workspace(&ws, &team, None, true, Some(TEAM), &transport)
        .expect("[i434] fixture precondition: quick start must launch the Claude seat");
    let launches = transport.spawn_records();
    let launch_argv = launches.last().map(|(_, argv)| argv.clone()).unwrap_or_default();
    let launch_rows = rows(&ws);
    eprintln!("[i434] after start: spawns={launches:?}\n[i434] after start: rows={launch_rows:#?}");
    assert_eq!(launches.len(), 1, "[i434] fixture precondition: exactly one launch spawn; spawns={launches:?}");
    assert_eq!(
        (
            count(&launch_argv, BYPASS_FLAG),
            value_after(&launch_argv, "--permission-mode"),
            value_after(&launch_argv, "--model"),
            value_after(&launch_argv, "--effort"),
        ),
        (0, Some("default"), Some(OLD_MODEL), Some(OLD_EFFORT)),
        "[i434] fixture precondition: initial launch must honor the original role file; argv={launch_argv:?}"
    );
    assert!(!launch_rows.is_empty(), "[i434] fixture precondition: seat row after start; rows={launch_rows:#?}");
    for (pointer, row) in &launch_rows {
        assert_eq!(
            (
                row.pointer("/model").cloned().unwrap_or(Value::Null),
                row.pointer("/effective_approval_policy/enabled").cloned().unwrap_or(Value::Null),
                row.pointer("/profile").cloned().unwrap_or(Value::Null),
            ),
            (Value::String(OLD_MODEL.into()), Value::Bool(false), Value::Null),
            "[i434] fixture precondition: {pointer} must record the original declaration; row={row:#}"
        );
    }

    // Step 2: edit the role file on disk.
    let edited = role(true, NEW_MODEL, NEW_EFFORT, Some(NEW_PROFILE));
    std::fs::write(&role_path, &edited).unwrap();
    eprintln!("[i434] edited role file {}:\n{edited}", role_path.display());

    // Step 3: whole-team restart (fresh sessions allowed: the offline seat has none).
    let report = restart_with_transport(&ws, true, Some(TEAM), &transport);
    eprintln!("[i434] restart report: {report:?}");
    assert!(
        matches!(report, Ok(RestartReport::Restarted { .. })),
        "[i434] team restart must complete for an idle team; got {report:?}"
    );

    // Step 4: the respawn AND the seat row must reflect the edited role file.
    let spawns = transport.spawn_records();
    let restart_argv = spawns.last().map(|(_, argv)| argv.clone()).unwrap_or_default();
    let restart_rows = rows(&ws);
    let mut failures = Vec::new();
    if spawns.len() != 2 {
        failures.push(format!("team restart must respawn the seat exactly once; total spawns={}", spawns.len()));
    }
    if count(&restart_argv, BYPASS_FLAG) != 1 {
        failures.push(format!(
            "respawn argv must contain `{BYPASS_FLAG}` exactly once (role file now says dangerously_skip_permissions: true); found {}",
            count(&restart_argv, BYPASS_FLAG)
        ));
    }
    if let Some(mode) = value_after(&restart_argv, "--permission-mode") {
        failures.push(format!("respawn argv must not keep the restricted `--permission-mode {mode}`"));
    }
    for (flag, expected) in [("--model", NEW_MODEL), ("--effort", NEW_EFFORT)] {
        if value_after(&restart_argv, flag) != Some(expected) {
            failures.push(format!(
                "respawn argv must pass `{flag} {expected}` (edited role file); found {flag} {:?}",
                value_after(&restart_argv, flag)
            ));
        }
    }
    if restart_rows.is_empty() {
        failures.push(format!("no seat row for {AGENT} in state.json after team restart"));
    }
    for (pointer, row) in &restart_rows {
        for (field, expected) in [
            ("/as_launched_dangerously_skip_permissions", Value::Bool(true)),
            ("/effective_approval_policy/enabled", Value::Bool(true)),
            ("/model", Value::String(NEW_MODEL.into())),
            ("/effort", Value::String(NEW_EFFORT.into())),
            ("/profile", Value::String(NEW_PROFILE.into())),
        ] {
            let actual = row.pointer(field).cloned().unwrap_or(Value::Null);
            if actual != expected {
                failures.push(format!("state.json {pointer}{field} must be {expected} after team restart; found {actual}"));
            }
        }
    }
    let spec_path = crate::model::paths::runtime_spec_path(&ws, TEAM);
    let spec_text = std::fs::read_to_string(&spec_path).unwrap_or_else(|e| format!("(unreadable: {e})"));
    assert!(
        failures.is_empty(),
        "[i434] RED: team restart did not carry the role file edited on disk into the respawned Claude seat.\n\
         failures:\n  - {}\n\
         role file ({}):\n{}\n\
         launch argv  (before edit): {launch_argv:?}\n\
         restart argv (after edit):  {restart_argv:?}\n\
         seat rows before restart: {launch_rows:#?}\n\
         seat rows after restart:  {restart_rows:#?}\n\
         compiled runtime spec after restart ({}):\n{spec_text}",
        failures.join("\n  - "),
        role_path.display(),
        std::fs::read_to_string(&role_path).unwrap(),
        spec_path.display(),
    );
}
