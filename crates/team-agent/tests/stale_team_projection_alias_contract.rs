//! 0.5.28 RED contract: restart-family projection saves must not clone one
//! team's compact state into a dead sibling named `current`.
//!
//! References:
//! - `.team/artifacts/0527-realfail-layer2-leader-locate.md` §1-§4.
//! - architect result `res_1681f6dd4e72`.
//! - 0528 leader addendum: production `restart --team research` is mandatory;
//!   start-agent/restart-agent noop coverage is intentionally deferred here to
//!   keep this contract focused on the fresh add-agent chain plus rebuild chain.
//!
//! User story: after I shut down a team called `current` and then operate on a
//! live sibling called `research`, the retained `current` tombstone remains
//! diagnostic history. Only a true legacy single-team `current` alias may update
//! `teams.current`.

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

#[path = "support/hermetic.rs"]
mod hermetic_guard;

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use serde_json::Value;
use serial_test::serial;
use team_agent::state::persist::load_runtime_state;

const CURRENT: &str = "current";
const RESEARCH: &str = "research";
const ADMINWEB: &str = "adminweb";
const RESEARCHER: &str = "researcher";
const STANDARDS: &str = "standards";

#[test]
#[serial(env)]
fn add_agent_on_research_preserves_shutdown_current_tombstone() {
    let case = SupermarketCase::new("projection-add-agent");
    case.quick_start(CURRENT);
    case.shutdown(CURRENT);
    case.assert_current_tombstone("setup after current shutdown");

    case.quick_start(RESEARCH);
    case.add_agent(RESEARCH, STANDARDS);

    let state = case.state();
    assert_current_tombstone(
        &state,
        "RED main: add-agent --team research must not clone research into teams.current",
    );
    assert!(
        state.pointer("/teams/research/agents/standards").is_some(),
        "RED main: add-agent still must persist the new research worker; state={state}"
    );
}

#[test]
#[serial(env)]
fn restart_research_preserves_shutdown_current_tombstone() {
    let case = SupermarketCase::new("projection-restart");
    case.quick_start(CURRENT);
    case.shutdown(CURRENT);
    case.quick_start(RESEARCH);
    case.assert_current_tombstone("setup before restart");

    case.restart(RESEARCH);

    let state = case.state();
    assert_current_tombstone(
        &state,
        "RED5: production restart --team research must not rewrite the shutdown current tombstone",
    );
    assert!(
        state.pointer("/teams/research/agents/researcher").is_some(),
        "RED5 setup guard: research team must remain present after restart; state={state}"
    );
}

#[test]
#[serial(env)]
fn legacy_single_current_team_alias_still_updates_current_entry() {
    let case = SupermarketCase::new("projection-legacy-current");
    case.quick_start(CURRENT);
    case.add_agent(CURRENT, STANDARDS);

    let state = case.state();
    assert_eq!(
        state.pointer("/teams/current/team_key").and_then(Value::as_str),
        Some(CURRENT),
        "legacy guard: teams.current must remain the operation team when current is the only team; state={state}"
    );
    assert!(
        state.pointer("/teams/current/agents/standards").is_some(),
        "legacy guard: removing current alias sync would strand the new worker outside teams.current; state={state}"
    );
    let team_count = state
        .pointer("/teams")
        .and_then(Value::as_object)
        .map(|teams| teams.len())
        .unwrap_or(0);
    assert_eq!(
        team_count, 1,
        "legacy guard: fixture must remain a single-team legacy current shape; state={state}"
    );
}

struct SupermarketCase {
    env: hermetic_guard::HermeticTestEnv,
    workspace: PathBuf,
    current_dir: PathBuf,
    research_dir: PathBuf,
}

impl SupermarketCase {
    fn new(tag: &str) -> Self {
        let env = hermetic_guard::HermeticTestEnv::enter(tag);
        let workspace = env.workspace(tag);
        let current_dir = workspace.join(CURRENT);
        let research_dir = workspace.join(RESEARCH);
        write_team(&current_dir, CURRENT, ADMINWEB);
        write_team(&research_dir, RESEARCH, RESEARCHER);
        Self {
            env,
            workspace,
            current_dir,
            research_dir,
        }
    }

    fn quick_start(&self, team: &str) {
        let source = self.team_dir(team);
        let out = self.run([
            "quick-start",
            source,
            "--workspace",
            self.ws(),
            "--team-id",
            team,
            "--yes",
            "--no-display",
            "--json",
        ]);
        let json = json_output(&out);
        let ok = json
            .pointer("/ok")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let all_workers_spawned = json
            .pointer("/readiness/all_workers_spawned")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let status = json
            .pointer("/status")
            .and_then(Value::as_str)
            .unwrap_or("");
        let acceptable_degraded = matches!(
            status,
            "leader_receiver_unbound" | "pending_tool_load" | "pending_session_capture"
        );
        assert!(
            ok || (all_workers_spawned && acceptable_degraded),
            "quick-start {team} must launch enough to create realistic state; stdout={} stderr={}",
            text(&out.stdout),
            text(&out.stderr)
        );
    }

    fn shutdown(&self, team: &str) {
        let out = self.run([
            "shutdown",
            "--workspace",
            self.ws(),
            "--team",
            team,
            "--keep-logs",
            "--json",
        ]);
        let json = json_output(&out);
        assert!(
            out.status.success() && json.pointer("/ok").and_then(Value::as_bool) == Some(true),
            "shutdown {team} must cleanly retain logs before this contract can assert tombstone preservation; stdout={} stderr={}",
            text(&out.stdout),
            text(&out.stderr)
        );
    }

    fn add_agent(&self, team: &str, agent: &str) {
        let role = self.team_path(team).join(format!("{agent}.md"));
        write_role(&role, agent);
        let out = self.run([
            "add-agent",
            agent,
            "--role-file",
            path(&role),
            "--workspace",
            self.ws(),
            "--team",
            team,
            "--no-display",
            "--json",
        ]);
        let json = json_output(&out);
        assert!(
            out.status.success() && json.pointer("/ok").and_then(Value::as_bool) == Some(true),
            "add-agent {agent} --team {team} must succeed before checking projection alias safety; stdout={} stderr={}",
            text(&out.stdout),
            text(&out.stderr)
        );
    }

    fn restart(&self, team: &str) {
        let out = self.run(["restart", self.ws(), "--team", team, "--json"]);
        let json = json_output(&out);
        assert!(
            out.status.success() && json.pointer("/ok").and_then(Value::as_bool) == Some(true),
            "restart --team {team} must reach the production rebuild save path before checking tombstone preservation; stdout={} stderr={}",
            text(&out.stdout),
            text(&out.stderr)
        );
    }

    fn assert_current_tombstone(&self, label: &str) {
        assert_current_tombstone(&self.state(), label);
    }

    fn state(&self) -> Value {
        load_runtime_state(&self.workspace).expect("load runtime state")
    }

    fn run<const N: usize>(&self, args: [&str; N]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_team-agent"))
            .args(args)
            .current_dir(&self.workspace)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env("HOME", self.env.home())
            .env_remove("TMUX")
            .env_remove("TMUX_PANE")
            .output()
            .expect("run team-agent")
    }

    fn team_dir(&self, team: &str) -> &str {
        path(self.team_path(team))
    }

    fn team_path(&self, team: &str) -> &Path {
        match team {
            CURRENT => &self.current_dir,
            RESEARCH => &self.research_dir,
            other => panic!("unknown fixture team {other}"),
        }
    }

    fn ws(&self) -> &str {
        path(&self.workspace)
    }
}

impl Drop for SupermarketCase {
    fn drop(&mut self) {
        for team in [RESEARCH, CURRENT] {
            let _ = Command::new(env!("CARGO_BIN_EXE_team-agent"))
                .args([
                    "shutdown",
                    "--workspace",
                    path(&self.workspace),
                    "--team",
                    team,
                    "--keep-logs",
                    "--json",
                ])
                .current_dir(&self.workspace)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .env("HOME", self.env.home())
                .env_remove("TMUX")
                .env_remove("TMUX_PANE")
                .output();
        }
    }
}

fn write_team(dir: &Path, team: &str, agent: &str) {
    std::fs::create_dir_all(dir.join("agents")).expect("create team fixture");
    std::fs::write(
        dir.join("TEAM.md"),
        format!(
            "---\nname: {team}\nobjective: {team} fixture.\nprovider: fake\ndisplay_backend: none\n---\n\n{team}.\n"
        ),
    )
    .expect("write TEAM.md");
    write_role(&dir.join("agents").join(format!("{agent}.md")), agent);
}

fn write_role(path: &Path, id: &str) {
    std::fs::write(
        path,
        format!(
            "---\nname: {id}\nrole: {id}\nprovider: fake\nmodel: fake\nauth_mode: subscription\ntools:\n  - mcp_team\n---\n\n{id}.\n"
        ),
    )
    .expect("write role file");
}

fn assert_current_tombstone(state: &Value, label: &str) {
    assert_eq!(
        state
            .pointer("/teams/current/team_key")
            .and_then(Value::as_str),
        Some(CURRENT),
        "{label}: teams.current must keep its own payload identity; state={state}"
    );
    assert_eq!(
        state
            .pointer("/teams/current/status")
            .and_then(Value::as_str),
        Some("shutdown"),
        "{label}: teams.current.status must remain shutdown; state={state}"
    );
    assert_eq!(
        state
            .pointer("/teams/current/agents/adminweb/status")
            .and_then(Value::as_str),
        Some("stopped"),
        "{label}: teams.current adminweb must remain stopped diagnostic history; state={state}"
    );
}

fn json_output(output: &Output) -> Value {
    let raw = text(&output.stdout);
    serde_json::from_str(raw.trim()).unwrap_or_else(|error| {
        panic!(
            "parse json stdout failed: {error}; stdout={} stderr={}",
            raw,
            text(&output.stderr)
        )
    })
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn path(path: &Path) -> &str {
    path.to_str().expect("fixture path must be utf8")
}
