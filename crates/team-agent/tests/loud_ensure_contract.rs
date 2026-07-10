//! 0.5.22 loud coordinator ensure RED contracts (te-owned).
//!
//! References:
//! - `.team/artifacts/coordinator-lifecycle-hardening-locate.md` §5.3.
//! - `.team/artifacts/foundation-0-slice-design.md` §8.
//! - R1/R2: coordinator-dependent mutating `send` must loudly ensure a
//!   missing/stale-identity daemon when it can start the current runtime.
//! - R3: read-only `diagnose` / `status` / `doctor` must report coordinator
//!   health without spawning or rotating a daemon.
//! - R4: loud ensure must not bypass dirty-topology refusal.
//! - R5: explicit restart/start coordinator semantics remain structured and
//!   idempotent.

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

#[path = "support/hermetic.rs"]
mod hermetic_guard;

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output};

use serde_json::{json, Value};
use serial_test::serial;
use team_agent::coordinator::{
    coordinator_health, coordinator_meta_path, coordinator_pid_path, stop_coordinator, Pid,
    WorkspacePath, PROTOCOL_VERSION,
};
use team_agent::db::schema::open_db;
use team_agent::event_log::EventLog;
use team_agent::message_store::MessageStore;
use team_agent::state::persist::{runtime_state_path, save_runtime_state};

const TEAM: &str = "loud-ensure-team";
const WORKER: &str = "worker-1";
const STALE_PID: u32 = 99_999_991;

#[test]
#[serial(env)]
fn r1_mutating_send_loudly_ensures_missing_active_coordinator() {
    let fixture = LoudEnsureFixture::active("r1-missing");

    let body = fixture.send_worker_json("R1_LOUD_MISSING");

    assert_loud_ensure_response(
        &body,
        "missing",
        None,
        "R1 RED: missing active coordinator on mutating send must auto-start loudly",
    );
    assert_not_reported_delivered(&body, "R1 RED");
    assert_no_delivered_rows(&fixture.root, "R1 RED");
    let ensured_pid = response_coordinator_pid(&body, "R1 RED");
    assert_ensure_event(&fixture.root, "missing", None, Some(ensured_pid), "R1 RED");
    assert!(
        coordinator_health(&fixture.workspace).ok,
        "R1 RED: successful loud ensure must leave current coordinator healthy; body={body}"
    );
}

#[test]
#[serial(env)]
fn r2_mutating_send_loudly_rotates_stale_identity_coordinator() {
    let mut fixture = LoudEnsureFixture::active("r2-stale-identity");
    let previous_pid = fixture.spawn_stale_identity_process();

    let body = fixture.send_worker_json("R2_LOUD_ROTATION");

    assert_loud_ensure_response(
        &body,
        "running",
        Some("started_after_rotation"),
        "R2 RED: live daemon with stale binary identity must rotate loudly",
    );
    assert_eq!(
        body.pointer("/coordinator/rotation_reason")
            .and_then(Value::as_str),
        Some("binary_version_mismatch"),
        "R2 RED: stale identity rotation must name binary_version_mismatch; body={body}"
    );
    assert_not_reported_delivered(&body, "R2 RED");
    assert_no_delivered_rows(&fixture.root, "R2 RED");
    let ensured_pid = response_coordinator_pid(&body, "R2 RED");
    assert_ne!(
        ensured_pid, previous_pid,
        "R2 RED: rotation must replace the stale daemon pid; body={body}"
    );
    assert_ensure_event(
        &fixture.root,
        "running",
        Some("started_after_rotation"),
        Some(ensured_pid),
        "R2 RED",
    );
    assert!(
        coordinator_health(&fixture.workspace).ok,
        "R2 RED: rotated coordinator must be healthy after send; body={body}"
    );
}

#[test]
#[serial(env)]
fn r3_read_only_commands_report_dead_coordinator_without_spawning() {
    let fixture = LoudEnsureFixture::active("r3-read-only");
    fixture.write_stale_coordinator(Pid::new(STALE_PID));
    let before = fixture.snapshot_runtime_files();

    let diagnose = fixture.diagnose_json();
    assert!(
        issue(&diagnose, "coordinator_unavailable").is_some(),
        "R3 guard: diagnose must report stale coordinator_unavailable; json={diagnose}"
    );
    let status = fixture.status_json();
    assert!(
        status
            .pointer("/coordinator/status")
            .and_then(Value::as_str)
            == Some("stale")
            || status_reason(&status, "coordinator_not_running"),
        "R3 guard: status must report stale/dead coordinator without spawning; json={status}"
    );
    let _doctor = fixture.doctor_json();

    fixture.assert_runtime_files_unchanged(before, "R3 guard");
    assert!(
        !fixture.has_event("coordinator.ensure_restarted")
            && !fixture.has_event("coordinator.started")
            && !fixture.has_event("coordinator.rotation_required"),
        "R3 guard: read-only commands must not spawn/rotate/ensure coordinator; events={:?}",
        EventLog::new(&fixture.root).tail(50).expect("tail events")
    );
}

#[test]
#[serial(env)]
fn r4_loud_ensure_does_not_bypass_dirty_topology_refusal() {
    let fixture = LoudEnsureFixture::dirty_topology("r4-dirty");

    let body = fixture.send_worker_json("R4_DIRTY_TOPOLOGY");

    assert_eq!(
        body.get("ok").and_then(Value::as_bool),
        Some(false),
        "R4 RED: dirty topology must fail closed before send ensure starts a daemon; body={body}"
    );
    assert_eq!(
        body.get("status").and_then(Value::as_str),
        Some("refused_dirty_topology"),
        "R4 RED: dirty topology refusal must preserve status=refused_dirty_topology; body={body}"
    );
    assert!(
        issue(&body, "tmux_endpoint_socket_conflict").is_some()
            || body
                .pointer("/issues/0/id")
                .and_then(Value::as_str)
                == Some("tmux_endpoint_socket_conflict"),
        "R4 RED: refusal must name tmux_endpoint_socket_conflict, not hide behind coordinator ensure; body={body}"
    );
    assert!(
        body.get("coordinator_auto_restarted").is_none(),
        "R4 RED: dirty-topology refusal must not include loud ensure success fields; body={body}"
    );
    assert!(
        !fixture.has_event("coordinator.ensure_restarted")
            && !coordinator_pid_path(&fixture.workspace).exists(),
        "R4 RED: dirty topology must not spawn a coordinator as a side-effect; events={:?}",
        EventLog::new(&fixture.root).tail(50).expect("tail events")
    );
}

#[test]
fn r5_explicit_restart_start_report_semantics_remain_structured_guard() {
    let common = repo_file("crates/team-agent/src/lifecycle/restart/common.rs");
    let types = repo_file("crates/team-agent/src/lifecycle/types.rs");

    assert!(
        !common.contains("Result<bool>"),
        "R5 guard: explicit restart/start coordinator path must not collapse StartReport into bool; common.rs={common}"
    );
    for token in [
        "CoordinatorStartSummary",
        "from_start_report",
        "already_running",
        "started",
        "started_after_rotation",
        "rotation_reason",
    ] {
        assert!(
            types.contains(token) || common.contains(token),
            "R5 guard: explicit restart/start must preserve structured coordinator field `{token}`; types.rs={types}"
        );
    }
}

struct LoudEnsureFixture {
    _env: hermetic_guard::HermeticTestEnv,
    _binary_match_env: hermetic_guard::EnvOverride,
    root: PathBuf,
    workspace: WorkspacePath,
    fixture_children: Vec<Child>,
}

impl LoudEnsureFixture {
    fn active(tag: &str) -> Self {
        Self::with_state(tag, active_runtime_state)
    }

    fn dirty_topology(tag: &str) -> Self {
        Self::with_state(tag, dirty_topology_state)
    }

    fn with_state(tag: &str, state: fn(&Path) -> Value) -> Self {
        let env = hermetic_guard::HermeticTestEnv::enter(tag);
        let binary_match = cli_binary_path();
        let binary_match_env =
            env.with_env("TEAM_AGENT_TEST_HARNESS_BINARY_PATH_MATCH", &binary_match);
        let root = env.workspace(tag);
        std::fs::create_dir_all(team_agent::model::paths::runtime_dir(&root))
            .expect("create runtime dir");
        let _ = MessageStore::open(&root).expect("create message store");
        save_runtime_state(&root, &state(&root)).expect("save runtime state");
        Self {
            _env: env,
            _binary_match_env: binary_match_env,
            workspace: WorkspacePath::new(root.clone()),
            root,
            fixture_children: Vec::new(),
        }
    }

    fn send_worker_json(&self, token: &str) -> Value {
        let output = self.run_ta(&[
            "send",
            WORKER,
            token,
            "--workspace",
            self.root.to_str().expect("workspace utf8"),
            "--team",
            TEAM,
            "--json",
        ]);
        parse_json_stdout("send", output)
    }

    fn diagnose_json(&self) -> Value {
        parse_json_stdout(
            "diagnose",
            self.run_ta(&[
                "diagnose",
                "--workspace",
                self.root.to_str().expect("workspace utf8"),
                "--json",
            ]),
        )
    }

    fn status_json(&self) -> Value {
        parse_json_stdout(
            "status",
            self.run_ta(&[
                "status",
                "--workspace",
                self.root.to_str().expect("workspace utf8"),
                "--json",
            ]),
        )
    }

    fn doctor_json(&self) -> Value {
        parse_json_stdout(
            "doctor",
            self.run_ta(&[
                "doctor",
                "--workspace",
                self.root.to_str().expect("workspace utf8"),
                "--json",
            ]),
        )
    }

    fn run_ta(&self, args: &[&str]) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_team-agent"));
        command
            .args(args)
            .env("HOME", self._env.home())
            .env(
                "TEAM_AGENT_TEST_HARNESS_BINARY_PATH_MATCH",
                cli_binary_path(),
            )
            .current_dir(&self.root);
        for key in hermetic_guard::CALLER_IDENTITY_ENVS {
            command.env_remove(key);
        }
        command.output().expect("run team-agent")
    }

    fn write_stale_coordinator(&self, pid: Pid) {
        std::fs::write(coordinator_pid_path(&self.workspace), pid.to_string())
            .expect("write coordinator pid");
        write_raw_metadata(
            &self.workspace,
            json!({
                "pid": pid.get(),
                "protocol_version": PROTOCOL_VERSION,
                "message_store_schema_version": team_agent::db::schema::SCHEMA_VERSION,
                "binary_path": cli_binary_path(),
                "binary_version": current_version(),
                "source": "boot",
                "updated_at": "2026-07-10T00:00:00Z"
            }),
        );
    }

    fn spawn_stale_identity_process(&mut self) -> u32 {
        let child = Command::new("sleep")
            .arg("60")
            .spawn()
            .expect("spawn fixture stale coordinator process");
        let pid = child.id();
        write_raw_metadata(
            &self.workspace,
            json!({
                "pid": pid,
                "protocol_version": PROTOCOL_VERSION,
                "message_store_schema_version": team_agent::db::schema::SCHEMA_VERSION,
                "binary_path": cli_binary_path(),
                "binary_version": "0.5.20",
                "source": "boot",
                "updated_at": "2026-07-10T00:00:00Z"
            }),
        );
        self.fixture_children.push(child);
        pid
    }

    fn has_event(&self, event_name: &str) -> bool {
        EventLog::new(&self.root)
            .tail(100)
            .expect("tail event log")
            .iter()
            .any(|event| event.get("event").and_then(Value::as_str) == Some(event_name))
    }

    fn snapshot_runtime_files(&self) -> Vec<(PathBuf, Option<Vec<u8>>)> {
        [
            runtime_state_path(&self.root),
            coordinator_pid_path(&self.workspace),
            coordinator_meta_path(&self.workspace),
            self.root.join(".team/runtime/team.db"),
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
                "{label}: read-only command mutated {}",
                path.display()
            );
        }
    }
}

impl Drop for LoudEnsureFixture {
    fn drop(&mut self) {
        for child in &mut self.fixture_children {
            let _ = child.kill();
            let _ = child.wait();
        }
        let health = coordinator_health(&self.workspace);
        if health.ok {
            let _ = stop_coordinator(&self.workspace);
        }
    }
}

fn active_runtime_state(root: &Path) -> Value {
    json!({
        "session_name": "loud-ensure-session",
        "active_team_key": TEAM,
        "team_key": TEAM,
        "tmux_socket": null,
        "tmux_endpoint": null,
        "agents": {
            "worker-1": worker_state()
        },
        "teams": {
            "loud-ensure-team": {
                "status": "alive",
                "team_dir": root.join("team-dir"),
                "session_name": "loud-ensure-session",
                "agents": {
                    "worker-1": worker_state()
                }
            }
        }
    })
}

fn dirty_topology_state(root: &Path) -> Value {
    let old_socket = root.join("old.sock").to_string_lossy().to_string();
    let new_socket = root.join("new.sock").to_string_lossy().to_string();
    let mut state = active_runtime_state(root);
    state["tmux_endpoint"] = json!(old_socket);
    state["tmux_socket"] = json!(new_socket);
    state["leader_receiver"] = json!({
        "mode": "direct_tmux",
        "status": "attached",
        "pane_id": "%1",
        "tmux_socket": new_socket,
        "session_name": "dirty-leader",
        "window_name": "leader"
    });
    state["teams"][TEAM]["tmux_endpoint"] = state["tmux_endpoint"].clone();
    state["teams"][TEAM]["tmux_socket"] = state["tmux_socket"].clone();
    state["teams"][TEAM]["leader_receiver"] = state["leader_receiver"].clone();
    state
}

fn worker_state() -> Value {
    json!({
        "status": "running",
        "agent_id": WORKER,
        "provider": "fake",
        "window": WORKER
    })
}

fn assert_loud_ensure_response(
    body: &Value,
    previous_status: &str,
    start_status: Option<&str>,
    label: &str,
) {
    assert_eq!(
        body.get("coordinator_auto_restarted")
            .and_then(Value::as_bool),
        Some(true),
        "{label}: response must include coordinator_auto_restarted=true; body={body}"
    );
    assert_eq!(
        body.get("coordinator_previous_status")
            .and_then(Value::as_str),
        Some(previous_status),
        "{label}: response must expose coordinator_previous_status={previous_status}; body={body}"
    );
    if let Some(start_status) = start_status {
        assert_eq!(
            body.pointer("/coordinator/status").and_then(Value::as_str),
            Some(start_status),
            "{label}: response coordinator.status must be {start_status}; body={body}"
        );
    }
    assert!(
        response_coordinator_pid(body, label) > 0,
        "{label}: response must expose new coordinator.pid; body={body}"
    );
}

fn assert_not_reported_delivered(body: &Value, label: &str) {
    assert_eq!(
        body.get("delivered").and_then(Value::as_bool),
        Some(false),
        "{label}: send must not report delivered solely because the message entered the queue; body={body}"
    );
    assert_ne!(
        body.get("delivery_status").and_then(Value::as_str),
        Some("delivered"),
        "{label}: delivery_status must not be delivered before physical delivery proof; body={body}"
    );
}

fn response_coordinator_pid(body: &Value, label: &str) -> u32 {
    body.pointer("/coordinator/pid")
        .and_then(Value::as_u64)
        .unwrap_or_else(|| panic!("{label}: missing numeric coordinator.pid; body={body}"))
        .try_into()
        .expect("coordinator pid fits u32")
}

fn assert_ensure_event(
    root: &Path,
    previous_status: &str,
    start_status: Option<&str>,
    pid: Option<u32>,
    label: &str,
) {
    let event = events(root)
        .into_iter()
        .find(|event| {
            event.get("event").and_then(Value::as_str) == Some("coordinator.ensure_restarted")
        })
        .unwrap_or_else(|| {
            panic!(
                "{label}: missing coordinator.ensure_restarted event; events={:?}",
                EventLog::new(root).tail(100).expect("tail events")
            )
        });
    assert_eq!(
        event
            .get("coordinator_previous_status")
            .and_then(Value::as_str),
        Some(previous_status),
        "{label}: ensure event must expose previous status; event={event}"
    );
    if let Some(start_status) = start_status {
        assert_eq!(
            event.get("status").and_then(Value::as_str),
            Some(start_status),
            "{label}: ensure event must expose start status; event={event}"
        );
    }
    if let Some(pid) = pid {
        assert_eq!(
            event.get("pid").and_then(Value::as_u64),
            Some(u64::from(pid)),
            "{label}: ensure event pid must match response coordinator.pid; event={event}"
        );
    }
}

fn assert_no_delivered_rows(root: &Path, label: &str) {
    let store = MessageStore::open(root).expect("open message store");
    let conn = open_db(store.db_path()).expect("open db");
    let delivered: i64 = conn
        .query_row(
            "select count(*) from messages where status = 'delivered'",
            [],
            |row| row.get(0),
        )
        .expect("count delivered messages");
    assert_eq!(
        delivered, 0,
        "{label}: no DB row may be marked delivered before physical delivery proof"
    );
}

fn issue<'a>(out: &'a Value, id: &str) -> Option<&'a Value> {
    out.get("issues")
        .and_then(Value::as_array)?
        .iter()
        .find(|issue| issue.get("id").and_then(Value::as_str) == Some(id))
}

fn status_reason(out: &Value, reason: &str) -> bool {
    out.pointer("/not_ready/reasons")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .any(|value| value.as_str() == Some(reason))
}

fn events(root: &Path) -> Vec<Value> {
    let path = root.join(".team/logs/events.jsonl");
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .collect()
}

fn parse_json_stdout(label: &str, output: Output) -> Value {
    assert!(
        !output.stdout.is_empty(),
        "{label} must emit JSON on stdout; status={} stderr={}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "parse {label} JSON: {error}; status={} stdout={} stderr={}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

fn write_raw_metadata(workspace: &WorkspacePath, value: Value) {
    let pid = value
        .get("pid")
        .and_then(Value::as_u64)
        .expect("metadata pid") as u32;
    std::fs::write(coordinator_pid_path(workspace), pid.to_string()).expect("write pid");
    std::fs::write(
        coordinator_meta_path(workspace),
        serde_json::to_string_pretty(&value).expect("serialize metadata"),
    )
    .expect("write metadata");
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

fn repo_file(relative: &str) -> String {
    std::fs::read_to_string(repo_root().join(relative))
        .unwrap_or_else(|error| panic!("read {relative}: {error}"))
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}
