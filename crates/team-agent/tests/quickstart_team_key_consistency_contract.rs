//! Independent-verifier successor contract (supersedes the archived RED
//! contract `contract-quickstart-team-key-divergence-verifier.rs`, frozen sha
//! `2a2759381f180e0d3ec650e7af189103279f4447ab7bd63be3cd9683b96041d8`, whose
//! trigger shape — quick-start manufacturing a diverged team — is no longer
//! constructible after the unified-identity fix).
//!
//! Permanent addressing-consistency lock (third recurrence of the
//! selector-divergence family ⇒ permanent black-box lock, leader ruling):
//!
//!  1. A new `quick-start` (no `--team-id`, `TEAM.md name:` different from the
//!     workspace dir basename) must produce exactly ONE addressable identity:
//!     `active_team_key == team_key == the single teams-map key == the
//!     compiled display name`, session `team-<displayname>`.
//!  2. For every user-visible selector value (display name AND legacy dir
//!     basename), the three addressing entries — `status --team`, positional
//!     `send --team`, qualified `send --to-name '<ws>::<team>/<agent>'` —
//!     must agree (all accept or all refuse), any selector that status
//!     accepts must make positional send actually deliverable, and message DB
//!     `owner_team_id` rows must only ever contain the canonical key.
//!
//! Coverage split (constitution review R2, MUST-15 canonical-only): this
//! black-box contract covers only shapes canonical commands can produce.
//! LEGACY pre-fix diverged workspaces (canonical key = dir basename, session
//! = `team-<displayname>`) are deliberately NOT reconstructed here — seeding
//! them requires mutating runtime intermediates, which MUST-15 forbids in
//! top-level acceptance contracts. That face is covered by (a) the lib unit
//! regressions `cmd_send_canonicalizes_legacy_session_alias_before_membership_projection`
//! and `resolve_qualified_name_canonicalizes_legacy_session_alias` (RED→GREEN
//! in this slice) and (b) the archived real-machine evidence in
//! `.team/artifacts/pipeline-runs/quickstart-team-key-divergence/verdict.md`
//! (a genuine 0.5.48-binary-created diverged team driven by the fixed binary,
//! 12/12 consistent). A hermetic canonical trigger for that shape does not
//! exist on this platform — deferred honestly per MUST-15.
//!
//! Harness: real binary + real tmux on the team's own socket, fake provider
//! (zero tokens), canonical commands only: quick-start / status / send /
//! send --to-name / shutdown.

#![cfg(unix)]
#![allow(clippy::expect_used, clippy::panic)]

#[path = "support/hermetic.rs"]
mod hermetic_guard;
use hermetic_guard::HermeticTestEnv;

use std::path::PathBuf;
use std::process::Output;

use serde_json::Value;
use serial_test::serial;

const DISPLAY_NAME: &str = "displayname";
const WORKER: &str = "base";

struct ConsistencyCase {
    env: HermeticTestEnv,
    workspace: PathBuf,
    socket: PathBuf,
}

impl ConsistencyCase {
    fn start(tag: &str) -> Self {
        let env = HermeticTestEnv::enter(tag);
        env.scrub_tmux();
        let workspace = env.workspace(tag);
        std::fs::create_dir_all(workspace.join("agents")).expect("create agents dir");
        std::fs::write(
            workspace.join("TEAM.md"),
            format!(
                "---\nname: {DISPLAY_NAME}\nobjective: team-key consistency contract.\nprovider: fake\n---\n"
            ),
        )
        .expect("write TEAM.md");
        std::fs::write(
            workspace.join("agents").join(format!("{WORKER}.md")),
            format!(
                "---\nname: {WORKER}\nrole: {WORKER}\nprovider: fake\nmodel: fake\nauth_mode: subscription\ntools:\n  - mcp_team\n---\n\n{WORKER}.\n"
            ),
        )
        .expect("write worker role doc");
        let mut case = Self {
            env,
            workspace,
            socket: PathBuf::new(),
        };
        let output = case.run_cli(&[
            "quick-start",
            "--workspace",
            case.workspace_str(),
            "--yes",
            "--no-display",
            "--json",
        ]);
        let value = json_stdout(&output, "quick-start");
        let all_spawned = value
            .get("worker_readiness")
            .and_then(|node| node.get("all_workers_spawned"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        assert!(
            all_spawned,
            "fixture: quick-start must spawn the worker; stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        case.socket = case.discover_socket();
        case
    }

    fn workspace_str(&self) -> &str {
        self.workspace.to_str().expect("workspace utf8")
    }

    fn dir_basename(&self) -> String {
        self.workspace
            .file_name()
            .expect("workspace basename")
            .to_string_lossy()
            .to_string()
    }

    fn run_cli(&self, args: &[&str]) -> Output {
        self.env.run_cli(&self.workspace, args)
    }

    fn discover_socket(&self) -> PathBuf {
        let output = self.run_cli(&["status", "--workspace", self.workspace_str(), "--json"]);
        let value = json_stdout(&output, "status (socket discovery)");
        let attach = value
            .get("leader_attach_command")
            .and_then(Value::as_str)
            .expect("status must expose leader_attach_command");
        let socket = attach
            .split_whitespace()
            .skip_while(|token| *token != "-S")
            .nth(1)
            .expect("leader_attach_command must carry -S <socket>");
        PathBuf::from(socket)
    }

    fn state_path(&self) -> PathBuf {
        self.workspace
            .join(".team")
            .join("runtime")
            .join("state.json")
    }

    fn state(&self) -> Value {
        let raw = std::fs::read_to_string(self.state_path()).expect("read state.json");
        serde_json::from_str(&raw).expect("parse state.json")
    }

    fn status_accepts(&self, selector: &str) -> (bool, Value) {
        let output = self.run_cli(&[
            "status",
            "--workspace",
            self.workspace_str(),
            "--team",
            selector,
            "--json",
        ]);
        let value = json_stdout(&output, "status --team");
        let accepted = value.get("ok").and_then(Value::as_bool) == Some(true)
            && value
                .get("agents")
                .and_then(Value::as_object)
                .is_some_and(|agents| agents.contains_key(WORKER));
        (accepted, value)
    }

    fn send_accepts(&self, selector: &str, probe: &str) -> (bool, Value) {
        let output = self.run_cli(&[
            "send",
            WORKER,
            probe,
            "--workspace",
            self.workspace_str(),
            "--team",
            selector,
            "--no-wait",
            "--json",
        ]);
        let value = json_stdout(&output, "send --team");
        let accepted = value.get("ok").and_then(Value::as_bool) == Some(true);
        (accepted, value)
    }

    fn qualified_accepts(&self, selector: &str, probe: &str) -> (bool, Value) {
        let qualified = format!("{}::{}/{}", self.workspace_str(), selector, WORKER);
        let output = self.run_cli(&[
            "send",
            "--to-name",
            qualified.as_str(),
            probe,
            "--workspace",
            self.workspace_str(),
            "--no-wait",
            "--json",
        ]);
        let value = json_stdout(&output, "send --to-name");
        let accepted = value.get("ok").and_then(Value::as_bool) == Some(true);
        (accepted, value)
    }

    /// The permanent lock: for one selector value, the three addressing
    /// entries must agree; a status-accepted selector must be send-deliverable.
    fn assert_matrix_consistent(&self, selector: &str, context: &str) -> bool {
        let (status_ok, status_value) = self.status_accepts(selector);
        let (send_ok, send_value) =
            self.send_accepts(selector, &format!("{context} send probe {selector}"));
        let (name_ok, name_value) =
            self.qualified_accepts(selector, &format!("{context} name probe {selector}"));
        assert!(
            status_ok == send_ok && status_ok == name_ok,
            "{context}: selector `{selector}` must resolve identically across status / positional \
             send / qualified --to-name (all accept or all refuse); status={status_ok} \
             send={send_ok} name={name_ok}\nstatus={status_value}\nsend={send_value}\nname={name_value}"
        );
        status_ok
    }

    /// Message DB owner scope must only ever contain the canonical key.
    fn assert_db_owner_scope_only(&self, canonical: &str, context: &str) {
        let db_path = self.workspace.join(".team").join("runtime").join("team.db");
        if !db_path.exists() {
            panic!(
                "{context}: message DB must exist after accepted sends: {}",
                db_path.display()
            );
        }
        let connection = rusqlite::Connection::open(&db_path).expect("open team.db");
        let mut statement = connection
            .prepare("SELECT DISTINCT owner_team_id FROM messages WHERE owner_team_id IS NOT NULL")
            .expect("prepare owner_team_id query");
        let owners: Vec<String> = statement
            .query_map([], |row| row.get::<_, String>(0))
            .expect("query owner_team_id")
            .filter_map(Result::ok)
            .collect();
        assert!(
            owners.iter().all(|owner| owner == canonical),
            "{context}: every messages.owner_team_id must be the canonical key `{canonical}` — a \
             display alias leaking into owner scope poisons DB scoping; owners={owners:?}"
        );
        assert!(
            !owners.is_empty(),
            "{context}: at least one accepted send must have produced a message row"
        );
    }

    fn shutdown(&self) {
        let _ = self.run_cli(&[
            "shutdown",
            "--workspace",
            self.workspace_str(),
            "--yes",
            "--json",
        ]);
        let _ = std::process::Command::new("tmux")
            .args([
                "-S",
                self.socket.to_str().expect("socket utf8"),
                "kill-server",
            ])
            .output();
    }
}

fn json_stdout(output: &Output, context: &str) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|_| {
        panic!(
            "{context}: expected JSON stdout; stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

/// Lock 1 — a fresh quick-start (no `--team-id`, display name != dir
/// basename) must yield exactly one addressable identity, and both
/// user-visible selector values must pass the three-entry consistency matrix
/// with the canonical selector reachable.
#[test]
#[serial(env)]
fn new_quick_start_has_single_identity_and_consistent_addressing_matrix() {
    let case = ConsistencyCase::start("qtc-new");
    let state = case.state();
    let teams: Vec<String> = state
        .get("teams")
        .and_then(Value::as_object)
        .map(|teams| teams.keys().cloned().collect())
        .unwrap_or_default();
    assert_eq!(
        state.get("active_team_key").and_then(Value::as_str),
        Some(DISPLAY_NAME),
        "new team: active_team_key must be the compiled display name; state={state}"
    );
    assert_eq!(
        teams,
        vec![DISPLAY_NAME.to_string()],
        "new team: the teams map must hold exactly the canonical display-name key"
    );
    assert_eq!(
        state.get("session_name").and_then(Value::as_str),
        Some(format!("team-{DISPLAY_NAME}").as_str()),
        "new team: session derives from the same single identity"
    );

    let display_accepted = case.assert_matrix_consistent(DISPLAY_NAME, "new-team");
    assert!(
        display_accepted,
        "new team: the canonical display-name selector must be accepted and deliverable"
    );
    // The dir basename is a legacy alias; it may be accepted (canonicalized)
    // or refused, but never split across entries.
    let basename = case.dir_basename();
    case.assert_matrix_consistent(basename.as_str(), "new-team-dir-alias");
    case.assert_db_owner_scope_only(DISPLAY_NAME, "new-team");
    case.shutdown();
}
