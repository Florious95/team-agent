//! 0.5.20 coordinator lifecycle forensics RED contracts.
//!
//! References:
//! - `.team/artifacts/coordinator-lifecycle-hardening-locate.md` §7.
//! - R1: startup failure must write `coordinator.exit {reason:"startup_error"}`
//!   and a heartbeat carrying pid / boot_id / phase.
//! - R2: non-tick uncaught panic must write `coordinator.exit {reason:"panic"}`
//!   with panic payload/backtrace.
//! - R3: recovered tick panic must update heartbeat `last_tick_status="panic"`
//!   while preserving the existing `coordinator.tick_panic` marker.
//! - R6/R7: mutating send against a dead coordinator must not silently queue, and
//!   read-only diagnose must not spawn or mutate coordinator runtime files.
//! - R4/R5 (SIGTERM/SIGKILL) are reserved for the real-machine gate because the
//!   signal delivery/window semantics are platform/runtime dependent.

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

#[path = "support/hermetic.rs"]
mod hermetic_guard;

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::{json, Value};
use serial_test::serial;
use team_agent::coordinator::{
    coordinator_meta_path, coordinator_pid_path, write_coordinator_metadata, MetadataSource, Pid,
    WorkspacePath,
};
use team_agent::event_log::EventLog;
use team_agent::message_store::MessageStore;
use team_agent::messaging::{
    send_message, DeliveryRefusal, DeliveryStatus, MessageTarget, SendOptions,
};
use team_agent::state::persist::{runtime_state_path, save_runtime_state};

#[test]
fn daemon_startup_error_writes_exit_and_heartbeat() {
    let backoff = repo_file("crates/team-agent/src/coordinator/backoff.rs");
    let tick = repo_file("crates/team-agent/src/coordinator/tick.rs");
    let missing = missing_requirements(&[
        (
            "coordinator.exit reason=startup_error event",
            backoff.contains("coordinator.exit")
                && backoff.contains("startup_error")
                && backoff.contains("reason"),
        ),
        (
            "heartbeat pid field",
            tick.contains("coordinator_tick.json") && tick.contains("\"pid\""),
        ),
        (
            "heartbeat boot_id field",
            tick.contains("coordinator_tick.json") && tick.contains("boot_id"),
        ),
        (
            "heartbeat phase/last_phase field",
            tick.contains("coordinator_tick.json")
                && (tick.contains("last_phase") || tick.contains("\"phase\"")),
        ),
    ]);

    assert!(
        missing.is_empty(),
        "R1 RED: daemon startup errors must leave a durable coordinator.exit \
         reason=startup_error plus heartbeat pid/boot_id/phase before the loop; \
         missing={missing:?}"
    );
}

#[test]
fn daemon_uncaught_panic_writes_exit_marker() {
    let backoff = repo_file("crates/team-agent/src/coordinator/backoff.rs");
    let missing = missing_requirements(&[
        (
            "daemon-wide catch_unwind outside tick closure",
            backoff.contains("run_daemon_body_with_panic_marker")
                || backoff.contains("daemon_panic")
                || backoff.contains("non_tick_panic"),
        ),
        (
            "coordinator.exit reason=panic event",
            backoff.contains("coordinator.exit")
                && backoff.contains("\"panic\"")
                && backoff.contains("reason"),
        ),
        (
            "panic payload/backtrace on exit event",
            backoff.contains("panic_payload")
                || (backoff.contains("panic") && backoff.contains("backtrace")),
        ),
    ]);

    assert!(
        missing.is_empty(),
        "R2 RED: panic outside coordinator.tick must be surfaced as \
         coordinator.exit reason=panic with payload/backtrace instead of only \
         terminating the process; missing={missing:?}"
    );
}

#[test]
fn tick_panic_updates_heartbeat_without_exiting() {
    let backoff = repo_file("crates/team-agent/src/coordinator/backoff.rs");
    let tick = repo_file("crates/team-agent/src/coordinator/tick.rs");
    let missing = missing_requirements(&[
        (
            "existing coordinator.tick_panic marker",
            backoff.contains("coordinator.tick_panic"),
        ),
        (
            "heartbeat last_tick_status=panic update",
            tick.contains("last_tick_status")
                && tick.contains("\"panic\"")
                && tick.contains("coordinator_tick.json"),
        ),
        (
            "tick panic remains recovered path, not forced coordinator.exit",
            !backoff.contains("coordinator.exit")
                || backoff.contains("TickError::Panic")
                || backoff.contains("tick_panic"),
        ),
    ]);

    assert!(
        missing.is_empty(),
        "R3 RED: recovered tick panic must keep coordinator.tick_panic and also \
         update heartbeat last_tick_status=panic without requiring daemon exit; \
         missing={missing:?}"
    );
}

#[test]
#[serial(env)]
fn mutating_send_loudly_ensures_missing_coordinator_or_fails_closed() {
    let fixture = ForensicsFixture::active_worker("send-dead-coordinator");
    fixture.write_stale_coordinator(Pid::new(99_999_999));

    let out = send_message(
        &fixture.root,
        &MessageTarget::Single("worker-1".to_string()),
        "deliver only if coordinator progress is possible",
        &SendOptions::default(),
    )
    .expect("send message");

    let fail_closed = !out.ok
        && out.status == DeliveryStatus::Degraded
        && out.reason == Some(DeliveryRefusal::CoordinatorUnavailable)
        && out.message_id.is_none()
        && fixture.accepted_message_count() == 0
        && fixture.has_event("send.coordinator_unavailable");
    let loud_ensure = out
        .verification
        .as_deref()
        .is_some_and(|text| text.contains("coordinator_auto_restarted"))
        || fixture.has_event("coordinator.auto_restarted")
        || fixture.has_event("send.coordinator_auto_restarted");

    assert!(
        fail_closed || loud_ensure,
        "R6: mutating send against a dead coordinator must either loudly \
         auto-ensure current coordinator progress or fail closed without \
         accepted/delivered queue state; out={out:?}"
    );
}

#[test]
#[serial(env)]
fn read_only_diagnose_does_not_spawn() {
    let fixture = ForensicsFixture::active_worker("diagnose-no-spawn");
    fixture.write_stale_coordinator(Pid::new(88_888_888));
    let before = fixture.snapshot_runtime_files();

    let out = fixture.diagnose_json();

    assert_eq!(
        out["ok"],
        json!(false),
        "R7: diagnose must surface dead coordinator as an issue instead of \
         starting a daemon; out={out}"
    );
    assert!(
        issue(&out, "coordinator_unavailable").is_some(),
        "R7: diagnose JSON must include issue id=coordinator_unavailable; out={out}"
    );
    fixture.assert_runtime_files_unchanged(before, "R7 diagnose read-only");
    assert!(
        !fixture.has_event("coordinator.boot"),
        "R7: read-only diagnose must not spawn a new coordinator or write \
         coordinator.boot; events={:?}",
        EventLog::new(&fixture.root).tail(20).expect("tail events")
    );
}

struct ForensicsFixture {
    _env: hermetic_guard::HermeticTestEnv,
    root: PathBuf,
    workspace: WorkspacePath,
}

impl ForensicsFixture {
    fn active_worker(tag: &str) -> Self {
        let env = hermetic_guard::HermeticTestEnv::enter(tag);
        let root = env.workspace(tag);
        std::fs::create_dir_all(team_agent::model::paths::runtime_dir(&root))
            .expect("create runtime dir");
        let _ = MessageStore::open(&root).expect("create message store");
        save_runtime_state(
            &root,
            &json!({
                "session_name": "forensics-team",
                "active_team_key": "forensics-team",
                "agents": {
                    "worker-1": {
                        "status": "running",
                        "agent_id": "worker-1",
                        "provider": "fake",
                        "window": "worker-1"
                    }
                }
            }),
        )
        .expect("save runtime state");
        Self {
            _env: env,
            workspace: WorkspacePath::new(root.clone()),
            root,
        }
    }

    fn write_stale_coordinator(&self, pid: Pid) {
        write_coordinator_metadata(&self.workspace, pid, MetadataSource::Boot)
            .expect("write coordinator metadata");
        std::fs::write(coordinator_pid_path(&self.workspace), pid.to_string())
            .expect("write coordinator pid");
    }

    fn diagnose_json(&self) -> Value {
        let output = self.run_ta(&[
            "diagnose",
            "--workspace",
            self.root.to_str().expect("workspace utf8"),
            "--json",
        ]);
        parse_json_stdout("diagnose", output)
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

    fn accepted_message_count(&self) -> i64 {
        let store = MessageStore::open(&self.root).expect("open message store");
        let conn = team_agent::db::schema::open_db(store.db_path()).expect("open db");
        conn.query_row(
            "select count(*) from messages where status = 'accepted'",
            [],
            |row| row.get(0),
        )
        .expect("count accepted messages")
    }

    fn has_event(&self, event_name: &str) -> bool {
        EventLog::new(&self.root)
            .tail(50)
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
                "{label}: read-only diagnose must not mutate {}",
                path.display()
            );
        }
    }
}

fn issue<'a>(out: &'a Value, id: &str) -> Option<&'a Value> {
    out.get("issues")
        .and_then(Value::as_array)?
        .iter()
        .find(|issue| issue.get("id").and_then(Value::as_str) == Some(id))
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

fn missing_requirements(requirements: &[(&'static str, bool)]) -> Vec<&'static str> {
    requirements
        .iter()
        .filter_map(|(label, ok)| (!ok).then_some(*label))
        .collect()
}

fn repo_file(relative: &str) -> String {
    std::fs::read_to_string(repo_root().join(relative))
        .unwrap_or_else(|error| panic!("read {relative}: {error}"))
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}
