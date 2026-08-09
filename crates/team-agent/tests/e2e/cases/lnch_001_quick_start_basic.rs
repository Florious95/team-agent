//! E2E-LNCH-001 / E2E-LAUNCH-001 Quick-Start Writes Canonical Runtime Spec And State.
//!
//! Architecture: T1 §1 storage layers, T5 §1 runtime tree, T7 §2 quick-start.
//!
//! Black-box invariants:
//! - `ok == true` in JSON
//! - `.team/runtime/<team>/team.spec.yaml` exists
//! - `.team/runtime/state.json` exists
//! - state.active_team_key == team_id
//! - state.session_name == "team-<team_id>"
//! - state.tmux_endpoint and state.tmux_socket populated
//! - state.agents.<id> exists for every spec agent.

use crate::framework::*;
use serde_json::Value;

#[ignore = "flaky under parallel suite, tracked"]
#[test]
fn lnch_001_quick_start_basic() {
    let ws = TestWorkspace::new("lnch001").with_fake_spec(&["a"]);
    let documented_team_dir = ws.path().join(".team/current");
    std::fs::create_dir_all(&documented_team_dir).expect("create .team/current");
    std::fs::rename(
        ws.path().join("TEAM.md"),
        documented_team_dir.join("TEAM.md"),
    )
    .expect("move TEAM.md into .team/current");
    std::fs::rename(ws.path().join("agents"), documented_team_dir.join("agents"))
        .expect("move agents into .team/current");

    let mut out = run_ta(&ws, &["quick-start", ".team/current"]);
    let mut twin_observation = TwinObservation::from_manifest();
    // Diagnostics if it fails — quick-start can degrade for non-bug reasons
    // (no tmux, sandbox), but in our test env it should succeed.
    if std::env::var("TEAM_AGENT_COVERAGE_NEGATIVE_TWIN").as_deref() == Ok("quick-start-status") {
        out.stdout = out.stdout.replacen(
            "status: leader_receiver_unbound",
            "negative_twin_removed",
            1,
        );
    }
    assert!(twin_observation.observe(
        out.stdout.contains("status: leader_receiver_unbound")
            && out.stdout.contains("\"all_workers_spawned\": true")),
        "quick-start did not launch the team. exit={} stdout={} stderr={}",
        out.exit_code,
        out.stdout,
        out.stderr
    );
    // Spec written
    let state = ws.read_state();
    let team_id = state["active_team_key"]
        .as_str()
        .expect("active_team_key must be a string");
    assert_ne!(team_id, "current");
    assert_ne!(team_id, ".team/current");
    let spec_path = ws
        .path()
        .join(".team/runtime")
        .join(team_id)
        .join("team.spec.yaml");
    assert_file_exists(&spec_path);
    if std::env::var("TEAM_AGENT_COVERAGE_NEGATIVE_TWIN").as_deref()
        == Ok("quick-start-session-name")
    {
        out.stdout = out
            .stdout
            .replacen("session_name: ", "negative_twin_removed", 1);
    }
    assert!(twin_observation.observe(
        out.stdout
            .contains(&format!("session_name: {}", worker_session_name(team_id)))),
        "public output must expose the canonical session name: {}",
        out.stdout
    );

    // state.json written and shape correct
    assert_file_exists(&ws.state_json_path());
    assert_json_field_eq_str(&state, "/active_team_key", team_id);
    assert_json_field_eq_str(&state, "/session_name", &worker_session_name(team_id));
    assert_json_field_present(&state, "/tmux_endpoint");
    assert_json_field_present(&state, "/tmux_socket");
    assert_json_field_present(&state, "/agents/a");
    assert_file_absent(&documented_team_dir.join(".team/runtime/state.json"));
    let teams = state["teams"].as_object().expect("teams must be an object");
    assert_eq!(
        teams.keys().map(String::as_str).collect::<Vec<_>>(),
        vec![team_id],
        "documented launch must create one canonical team identity"
    );

    // Cleanup the worker session so we don't leak state into other tests.
    let cleanup = run_ta(
        &ws,
        &[
            "shutdown",
            "--workspace",
            ws.path().to_str().unwrap(),
            "--keep-logs",
            "--json",
        ],
    );
    assert!(
        cleanup.is_success(),
        "documented launch cleanup failed: exit={} stdout={} stderr={}",
        cleanup.exit_code,
        cleanup.stdout,
        cleanup.stderr
    );

    // Sanity: state must be valid JSON
    let _: Value = state;
}

const TWIN_OBSERVATION_NONCE_ENV: &str = "TEAM_AGENT_TWIN_OBSERVATION_NONCE";
const NEGATIVE_TWIN_ENV: &str = "TEAM_AGENT_COVERAGE_NEGATIVE_TWIN";

#[derive(Default)]
struct TwinObservation {
    nonce: Option<String>,
    cells: Vec<String>,
    results: Vec<bool>,
}

impl TwinObservation {
    fn from_manifest() -> Self {
        let Ok(nonce) = std::env::var(TWIN_OBSERVATION_NONCE_ENV) else {
            return Self::default();
        };
        assert!(
            nonce.len() >= 16
                && nonce.len() <= 128
                && nonce
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-')),
            "{TWIN_OBSERVATION_NONCE_ENV} must be a 16..=128 byte [A-Za-z0-9_-] validator nonce"
        );

        let active_cell =
            std::env::var(NEGATIVE_TWIN_ENV).expect("observation mode requires a negative twin");
        let cells = covered_twin_cells();
        assert!(
            cells.contains(&active_cell),
            "observation mode negative twin is not declared by the mapped manifest case"
        );
        Self {
            nonce: Some(nonce),
            cells,
            results: Vec::new(),
        }
    }

    fn observe(&mut self, result: bool) -> bool {
        let Some(nonce) = self.nonce.as_deref() else {
            return result;
        };
        let cell = self
            .cells
            .get(self.results.len())
            .expect("observation emitted more cells than the manifest declares");
        eprintln!(
            "TEAM_AGENT_TWIN-OBSERVATION-V1 nonce={} cell={} result={}",
            nonce,
            cell,
            if result { "PASS" } else { "FAIL" }
        );
        self.results.push(result);
        if self.results.len() == self.cells.len() {
            self.results.iter().all(|result| *result)
        } else {
            true
        }
    }
}

fn covered_twin_cells() -> Vec<String> {
    let manifest: Value = serde_json::from_str(include_str!(
        "../../../../../skills/team-agent/command-coverage.json"
    ))
    .expect("command coverage manifest must be valid JSON");
    let mut cells = manifest["commands"]
        .as_array()
        .expect("command coverage manifest must contain commands")
        .iter()
        .filter(|command| {
            command["bucket"].as_str() == Some("covered")
                && command["evidence"]["case"].as_str()
                    == Some(stringify!(lnch_001_quick_start_basic))
        })
        .map(|command| {
            (
                command["evidence"]["assertion"]["line"]
                    .as_u64()
                    .and_then(|line| u32::try_from(line).ok())
                    .expect("covered evidence must declare a u32 assertion line"),
                command["evidence"]["negative_twin"]["env_value"]
                    .as_str()
                    .expect("covered evidence must declare negative_twin.env_value")
                    .to_owned(),
            )
        })
        .collect::<Vec<_>>();
    cells.sort_by_key(|(line, _)| *line);
    assert_eq!(
        cells.len(),
        2,
        "mapped quick-start case must have exactly two covered cells"
    );
    cells.into_iter().map(|(_, cell)| cell).collect()
}
