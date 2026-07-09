//! 0.5.19 diagnose coordinator-health RED contracts.
//!
//! References:
//! - `.team/artifacts/diagnose-coordinator-health-locate.md` section 8.
//! - RED1: stale coordinator pid must surface as a diagnose issue and restart hint.
//! - RED2: healthy same-version coordinator must not produce coordinator issues.
//! - RED3: live coordinator with stale binary identity must surface the mismatch.
//! - RED4: incompatible message-store schema must surface a repair-state hint.
//! - Non-goal: `diagnose` stays read-only; it must not mutate pid/meta/state/db/events
//!   or start/stop/rotate the coordinator.

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

#[path = "support/hermetic.rs"]
mod hermetic_guard;

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::{json, Value};
use serial_test::serial;
use team_agent::coordinator::{
    coordinator_meta_path, coordinator_pid_path, Pid, WorkspacePath, PROTOCOL_VERSION,
};
use team_agent::state::persist::{load_runtime_state, runtime_state_path, save_runtime_state};

const COORDINATOR_ISSUES: [&str; 3] = [
    "coordinator_unavailable",
    "coordinator_stale_identity",
    "coordinator_schema_incompatible",
];

#[test]
#[serial(env)]
fn diagnose_reports_stale_coordinator_pid_without_mutating_runtime() {
    let fixture = DiagnoseFixture::active("stale-pid");
    let stale_pid = Pid::new(4_000_000);
    fixture.write_metadata(stale_pid, Some(cli_binary_path()), Some(current_version()));
    let before = fixture.snapshot_runtime_files();

    let out = fixture.diagnose_json();

    assert_eq!(
        out["ok"],
        json!(false),
        "RED1: diagnose must return ok=false when active runtime has stale coordinator pid; out={out}"
    );
    let issue = issue(&out, "coordinator_unavailable").unwrap_or_else(|| {
        panic!(
            "RED1: stale coordinator pid must produce object issue id=coordinator_unavailable; out={out}"
        )
    });
    assert_eq!(
        issue["status"],
        json!("stale"),
        "RED1: coordinator_unavailable must expose status=stale; issue={issue} out={out}"
    );
    assert_eq!(
        issue["pid"],
        json!(stale_pid.get()),
        "RED1: coordinator_unavailable must expose the stale pid; issue={issue} out={out}"
    );
    assert!(
        repair(&out, "coordinator_unavailable").is_some_and(|value| {
            value.get("hint_action").and_then(Value::as_str) == Some("team-agent restart")
        }),
        "RED1: stale pid must suggest hint_action=team-agent restart; out={out}"
    );
    fixture.assert_runtime_files_unchanged(before, "RED1 stale-pid diagnose");
}

#[test]
#[serial(env)]
fn diagnose_keeps_healthy_same_version_coordinator_clean_guard() {
    let fixture = DiagnoseFixture::quiet("healthy-same-version");
    let pid = Pid::new(std::process::id());
    fixture.write_metadata(pid, Some(cli_binary_path()), Some(current_version()));

    let out = fixture.diagnose_json();

    assert!(
        !has_any_coordinator_issue(&out),
        "RED2 guard: healthy same-version coordinator must not produce coordinator issues; out={out}"
    );
    assert_eq!(
        out["ok"],
        json!(true),
        "RED2 guard: quiet healthy fixture has no topology issues, so diagnose should remain ok=true; out={out}"
    );
}

#[test]
#[serial(env)]
fn diagnose_reports_live_coordinator_with_stale_binary_identity() {
    let fixture = DiagnoseFixture::active("stale-identity");
    let pid = Pid::new(std::process::id());
    fixture.write_metadata(pid, Some(cli_binary_path()), Some("0.5.16".to_string()));
    let before = fixture.snapshot_runtime_files();

    let out = fixture.diagnose_json();

    let issue = issue(&out, "coordinator_stale_identity").unwrap_or_else(|| {
        panic!(
            "RED3: live coordinator with stale binary_version must produce coordinator_stale_identity; out={out}"
        )
    });
    assert_eq!(
        issue["metadata_mismatch_reason"],
        json!("binary_version_mismatch"),
        "RED3: stale identity issue must expose metadata_mismatch_reason=binary_version_mismatch; issue={issue} out={out}"
    );
    assert_eq!(
        issue["binary_path"],
        json!(cli_binary_path()),
        "RED3: stale identity issue must expose current binary_path; issue={issue} out={out}"
    );
    assert_eq!(
        issue["binary_version"],
        json!(current_version()),
        "RED3: stale identity issue must expose current binary_version; issue={issue} out={out}"
    );
    assert!(
        repair(&out, "coordinator_stale_identity").is_some_and(|value| {
            value.get("hint_action").and_then(Value::as_str) == Some("team-agent restart")
        }),
        "RED3: stale identity must suggest hint_action=team-agent restart without diagnose rotating; out={out}"
    );
    fixture.assert_runtime_files_unchanged(before, "RED3 stale-identity diagnose");
}

#[test]
#[serial(env)]
fn diagnose_reports_schema_incompatible_as_repair_state_hint() {
    let fixture = DiagnoseFixture::active_without_schema("schema-incompatible");
    let pid = Pid::new(std::process::id());
    fixture.write_metadata(pid, Some(cli_binary_path()), Some(current_version()));
    std::fs::write(fixture.db_path(), b"not a sqlite database").expect("write corrupt team.db");
    let before = fixture.snapshot_runtime_files();

    let out = fixture.diagnose_json();

    let issue = issue(&out, "coordinator_schema_incompatible").unwrap_or_else(|| {
        panic!(
            "RED4: incompatible team.db schema must produce coordinator_schema_incompatible; out={out}"
        )
    });
    assert_eq!(
        issue["schema_ok"],
        json!(false),
        "RED4: schema issue must expose schema_ok=false; issue={issue} out={out}"
    );
    assert!(
        repair(&out, "coordinator_schema_incompatible").is_some_and(|value| {
            value.get("hint_action").and_then(Value::as_str)
                == Some("team-agent repair-state --schema")
        }),
        "RED4: schema incompatible must suggest hint_action=team-agent repair-state --schema; out={out}"
    );
    fixture.assert_runtime_files_unchanged(before, "RED4 schema-incompatible diagnose");
}

struct DiagnoseFixture {
    _env: hermetic_guard::HermeticTestEnv,
    root: PathBuf,
    workspace: WorkspacePath,
}

impl DiagnoseFixture {
    fn active(tag: &str) -> Self {
        Self::with_state(tag, active_runtime_state, true)
    }

    fn active_without_schema(tag: &str) -> Self {
        Self::with_state(tag, active_runtime_state, false)
    }

    fn quiet(tag: &str) -> Self {
        Self::with_state(tag, quiet_runtime_state, true)
    }

    fn with_state(tag: &str, state: fn(&Path) -> Value, create_schema: bool) -> Self {
        let env = hermetic_guard::HermeticTestEnv::enter(tag);
        let root = env.workspace(tag);
        std::fs::create_dir_all(team_agent::model::paths::runtime_dir(&root))
            .expect("create runtime dir");
        if create_schema {
            let _ = team_agent::message_store::MessageStore::open(&root).expect("create schema");
        }
        save_runtime_state(&root, &state(&root)).expect("save state");
        let _ = load_runtime_state(&root).expect("settle runtime state migrations before snapshot");
        Self {
            _env: env,
            workspace: WorkspacePath::new(root.clone()),
            root,
        }
    }

    fn write_metadata(
        &self,
        pid: Pid,
        binary_path: Option<String>,
        binary_version: Option<String>,
    ) {
        std::fs::write(coordinator_pid_path(&self.workspace), pid.to_string())
            .expect("write coordinator pid");
        std::fs::write(
            coordinator_meta_path(&self.workspace),
            serde_json::to_string_pretty(&json!({
                "pid": pid.get(),
                "protocol_version": PROTOCOL_VERSION,
                "message_store_schema_version": team_agent::db::schema::SCHEMA_VERSION,
                "binary_path": binary_path,
                "binary_version": binary_version,
                "source": "boot",
                "updated_at": "2026-07-10T00:00:00Z"
            }))
            .expect("serialize metadata"),
        )
        .expect("write coordinator metadata");
    }

    fn diagnose_json(&self) -> Value {
        let output = self.run_ta(&[
            "diagnose",
            "--workspace",
            self.root.to_str().expect("workspace utf8"),
            "--json",
        ]);
        parse_json_stdout(output)
    }

    fn run_ta(&self, args: &[&str]) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_team-agent"));
        command
            .args(args)
            .env("HOME", self._env.home())
            .current_dir(&self.root);
        for key in hermetic_guard::CALLER_IDENTITY_ENVS {
            command.env_remove(key);
        }
        command.output().expect("run team-agent")
    }

    fn db_path(&self) -> PathBuf {
        team_agent::model::paths::runtime_dir(&self.root).join("team.db")
    }

    fn snapshot_runtime_files(&self) -> Vec<(PathBuf, Option<Vec<u8>>)> {
        [
            runtime_state_path(&self.root),
            coordinator_pid_path(&self.workspace),
            coordinator_meta_path(&self.workspace),
            self.db_path(),
            self.root.join(".team/logs/events.jsonl"),
        ]
        .into_iter()
        .map(|path| {
            let bytes = std::fs::read(&path).ok();
            (path, bytes)
        })
        .collect()
    }

    fn assert_runtime_files_unchanged(&self, before: Vec<(PathBuf, Option<Vec<u8>>)>, label: &str) {
        for (path, expected) in before {
            let actual = std::fs::read(&path).ok();
            assert_eq!(
                actual,
                expected,
                "{label}: diagnose must be read-only; changed {}",
                path.display()
            );
        }
    }
}

fn active_runtime_state(root: &Path) -> Value {
    let team_dir = root.join("team-dir");
    json!({
        "active_team_key": "diagnose-team",
        "team_key": "diagnose-team",
        "teams": {
            "diagnose-team": {
                "status": "alive",
                "team_dir": team_dir,
                "session_name": "diagnose-coordinator-health",
                "leader_receiver": attached_leader_receiver(),
                "agents": {}
            }
        }
    })
}

fn quiet_runtime_state(_root: &Path) -> Value {
    json!({
        "leader": {"id": "leader"},
        "leader_receiver": attached_leader_receiver()
    })
}

fn attached_leader_receiver() -> Value {
    json!({
        "mode": "direct_tmux",
        "status": "attached",
        "pane_id": "%1",
        "provider": "codex"
    })
}

fn parse_json_stdout(output: Output) -> Value {
    assert!(
        !output.stdout.is_empty(),
        "diagnose must emit JSON on stdout; status={} stderr={}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "parse diagnose JSON: {error}; status={} stdout={} stderr={}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

fn issue<'a>(out: &'a Value, id: &str) -> Option<&'a Value> {
    out.get("issues")
        .and_then(Value::as_array)?
        .iter()
        .find(|issue| issue.get("id").and_then(Value::as_str) == Some(id))
}

fn repair<'a>(out: &'a Value, id: &str) -> Option<&'a Value> {
    out.get("suggested_repairs")
        .and_then(Value::as_array)?
        .iter()
        .find(|repair| repair.get("issue").and_then(Value::as_str) == Some(id))
}

fn has_any_coordinator_issue(out: &Value) -> bool {
    COORDINATOR_ISSUES.iter().any(|id| issue(out, id).is_some())
}

fn cli_binary_path() -> String {
    std::fs::canonicalize(env!("CARGO_BIN_EXE_team-agent"))
        .unwrap_or_else(|_| PathBuf::from(env!("CARGO_BIN_EXE_team-agent")))
        .to_string_lossy()
        .to_string()
}

fn current_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}
