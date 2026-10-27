//! Profile launch contracts for Batch A Lane 2.
//!
//! The Claude compatible-api worker launch these contracts were written for is gone with the
//! legacy Claude worker runtime; the provider-neutral resolver wiring and doctor contracts remain.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[path = "../../../tests/support/hermetic.rs"]
mod hermetic_guard;
#[allow(dead_code)]
fn _hermetic_boundary_marker(_: &hermetic_guard::HermeticTestEnv) {}

#[path = "../../../tests/support/composite_source.rs"]
mod composite_source;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{json, Value};

#[test]
fn launch_restart_start_add_and_fork_all_delegate_to_the_profile_launch_resolver() {
    let launch = source("src/lifecycle/launch.rs");
    let restart_common = source("src/lifecycle/restart/common.rs");
    let restart_agent = source("src/lifecycle/restart/agent.rs");

    let surfaces = [
        ("launch quick-start", &launch, "spawn_agents"),
        ("add-agent", &launch, "add_agent_with_transport"),
        ("fork-agent", &launch, "fork_agent_with_transport"),
        ("restart", &restart_common, "restart_with_transport"),
    ];
    for (surface, source, entry) in surfaces {
        let section = source
            .find(entry)
            .map(|idx| &source[idx..])
            .unwrap_or(source.as_str());
        assert!(
            section.contains("prepare_provider_profile_launch"),
            "{surface} must call the single profile resolver before building/spawning provider argv; entry={entry}"
        );
    }

    let start_agent = restart_agent
        .find("pub(crate) fn start_agent_at_paths(")
        .map(|idx| &restart_agent[idx..])
        .unwrap_or(restart_agent.as_str());
    assert!(
        start_agent.contains("spawn_agent_window("),
        "start-agent must delegate spawning through the shared spawn helper"
    );
    let spawn_agent_window = restart_common
        .find("pub(super) fn spawn_agent_window")
        .map(|idx| &restart_common[idx..])
        .unwrap_or(restart_common.as_str());
    assert!(
        spawn_agent_window.contains("prepare_provider_profile_launch"),
        "the shared start-agent spawn helper must call the single profile resolver before building/spawning provider argv"
    );
}

#[test]
#[serial_test::serial(env)]
fn diagnose_missing_profile_is_rc1_ok_false_and_zero_token() {
    let env = hermetic_guard::HermeticTestEnv::enter("diagnose-missing-profile");
    let ws = env.workspace("missing-profile");
    let team = write_compatible_profile_team(&ws, "diagteam", "copiloter");
    std::fs::remove_file(
        ws.join(".team")
            .join("current")
            .join("profiles")
            .join("local.env"),
    )
    .unwrap();

    let output = Command::new(crate::lifecycle::tests::test_binary_path())
        .args(["doctor", team.to_str().unwrap(), "--workspace", ws.to_str().unwrap(), "--json"])
        .current_dir(&team)
        .output()
        .expect("diagnose subprocess should run");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let json = serde_json::from_str::<Value>(stdout.trim()).unwrap_or_else(|_| json!({}));

    assert!(
        !output.status.success(),
        "diagnose with missing referenced profile must exit non-zero before any token/provider call; stdout={stdout} stderr={stderr}"
    );
    assert_eq!(
        json["ok"],
        json!(false),
        "diagnose missing profile must emit ok:false JSON; stdout={stdout} stderr={stderr}"
    );
    assert_eq!(json["profile_smoke"]["ok"], false, "{json}");
    assert!(
        json["profile_smoke"].to_string().contains("profile"),
        "diagnose missing profile must identify the scoped configuration failure: {json}"
    );
    assert!(json["providers"].as_object().unwrap().values().all(|provider| {
        provider["probe_status"] == "not_run"
    }));
    assert!(
        !stdout.contains("tokens") && !stderr.contains("tokens"),
        "diagnose dry-run must not consume provider tokens or report token usage; stdout={stdout} stderr={stderr}"
    );
}

fn write_compatible_profile_team(ws: &Path, team_key: &str, agent_id: &str) -> PathBuf {
    let team = ws.join(team_key);
    std::fs::create_dir_all(team.join("agents")).unwrap();
    std::fs::create_dir_all(ws.join(".team").join("current").join("profiles")).unwrap();
    std::fs::write(
        ws.join(".team").join("current").join("profiles").join("local.env"),
        "BASE_URL=https://llm.local/v1\nAPI_KEY=local-secret\nMODEL=profile-effective-model\n",
    )
    .unwrap();
    std::fs::write(
        team.join("TEAM.md"),
        format!(
            "---\nname: {team_key}\nobjective: Compatible profile doctor contract.\nprovider: copilot\nauth_mode: compatible_api\ndangerously_skip_permissions: false\n---\n\nTeam.\n"
        ),
    )
    .unwrap();
    std::fs::write(
        team.join("agents").join(format!("{agent_id}.md")),
        format!(
            "---\nname: {agent_id}\nrole: Copilot Worker\nprovider: copilot\nauth_mode: compatible_api\ndangerously_skip_permissions: false\nprofile: local\nmodel: profile-effective-model\ntools:\n  - mcp_team\n---\n\nWorker.\n"
        ),
    )
    .unwrap();
    team
}

fn source(path: &str) -> String {
    composite_source::composite_source(path)
}
