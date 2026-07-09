//! 0.5.18 coordinator version-rotation RED contracts.
//!
//! References:
//! - `.team/artifacts/coordinator-version-rotation-locate.md` §9.1.
//! - RED 1-2: coordinator metadata must include current binary identity and
//!   classify legacy / old-version live daemons as unhealthy.
//! - RED 3,6: protection guards. Same-binary daemons stay idempotent, and
//!   live-sibling scoped shutdown protections remain in force.

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

#[path = "support/hermetic.rs"]
mod hermetic_guard;

use std::path::{Path, PathBuf};

use serde_json::{json, Value};
use serial_test::serial;
use team_agent::coordinator::{
    coordinator_health, coordinator_meta_path, coordinator_pid_path, start_coordinator,
    write_coordinator_metadata, MetadataSource, Pid, StartOutcome, WorkspacePath, PROTOCOL_VERSION,
};

#[test]
#[serial(env)]
fn coordinator_metadata_missing_binary_identity_is_unhealthy() {
    let (_env, workspace, _root) = coordinator_workspace("missing-binary-identity");
    let pid = Pid::new(std::process::id());
    write_raw_metadata(
        &workspace,
        json!({
            "pid": pid.get(),
            "protocol_version": PROTOCOL_VERSION,
            "message_store_schema_version": team_agent::db::schema::SCHEMA_VERSION,
            "source": "boot",
            "updated_at": "2026-07-09T00:00:00Z"
        }),
    );

    let health = coordinator_health(&workspace);

    assert!(
        !health.ok,
        "RED 1: legacy coordinator metadata without binary_path/binary_version must be unhealthy; expected reason=binary_identity_missing; health={health:?}"
    );
    assert!(
        !health.metadata_ok,
        "RED 1: metadata_ok must be false for legacy binary identity; expected reason=binary_identity_missing; health={health:?}"
    );
    assert!(
        format!("{health:?}").contains("binary_identity_missing"),
        "RED 1: health/diagnostic shape must expose reason=binary_identity_missing; health={health:?}"
    );
}

#[test]
#[serial(env)]
fn coordinator_metadata_old_version_is_unhealthy_without_killing_current_process() {
    let (_env, workspace, _root) = coordinator_workspace("old-version");
    let pid = Pid::new(std::process::id());
    write_raw_metadata(
        &workspace,
        json!({
            "pid": pid.get(),
            "protocol_version": PROTOCOL_VERSION,
            "message_store_schema_version": team_agent::db::schema::SCHEMA_VERSION,
            "binary_path": std::env::current_exe().expect("current exe").to_string_lossy(),
            "binary_version": "0.5.16",
            "source": "boot",
            "updated_at": "2026-07-09T00:00:00Z"
        }),
    );

    let health = coordinator_health(&workspace);

    assert!(
        !health.ok,
        "RED 2: live coordinator metadata with old binary_version must be unhealthy; expected reason=binary_version_mismatch; current_version={} health={health:?}",
        env!("CARGO_PKG_VERSION")
    );
    assert!(
        !health.metadata_ok,
        "RED 2: metadata_ok must be false for old binary_version; expected reason=binary_version_mismatch; health={health:?}"
    );
    assert!(
        format!("{health:?}").contains("binary_version_mismatch"),
        "RED 2: health/diagnostic shape must expose reason=binary_version_mismatch; health={health:?}"
    );
}

#[test]
#[serial(env)]
fn same_binary_identity_remains_already_running_guard() {
    let (_env, workspace, _root) = coordinator_workspace("same-identity-already-running");
    let pid = Pid::new(std::process::id());
    write_coordinator_metadata(&workspace, pid, MetadataSource::Boot).expect("write metadata");
    std::fs::write(coordinator_pid_path(&workspace), pid.to_string()).expect("write pid");

    let report = start_coordinator(&workspace).expect("start coordinator");

    assert_eq!(
        report.status,
        StartOutcome::AlreadyRunning,
        "RED 3 guard: a healthy same-identity daemon must stay idempotent AlreadyRunning, not spawn/rotate; report={report:?}"
    );
    assert!(
        report.ok,
        "RED 3 guard: AlreadyRunning remains ok; report={report:?}"
    );
    assert_eq!(report.pid, Some(pid), "RED 3 guard: pid is preserved");
}

#[test]
fn restart_report_distinguishes_already_running_from_spawned() {
    let common = repo_file("crates/team-agent/src/lifecycle/restart/common.rs");
    let types = repo_file("crates/team-agent/src/lifecycle/types.rs");

    assert!(
        !common.contains("pub(super) fn start_coordinator_for_workspace(workspace: &Path) -> Result<bool"),
        "RED 4: restart must not flatten StartReport into bool; return a structured coordinator summary with status=already_running|started|started_after_rotation"
    );
    assert!(
        !common.contains(".map(|report| report.ok)"),
        "RED 4: restart must preserve StartReport.status instead of mapping it to ok bool"
    );
    assert!(
        types.contains("already_running") || types.contains("StartReport") || types.contains("coordinator_status"),
        "RED 4: RestartReport/CLI JSON must expose structured coordinator.status, not only coordinator_started: bool; lifecycle/types.rs={types}"
    );
}

#[test]
fn scoped_shutdown_last_live_team_stops_coordinator_and_reports_reason() {
    let cli = repo_file("crates/team-agent/src/cli/mod.rs");

    assert!(
        !cli.contains("let stopped = if team.is_none()"),
        "RED 5: scoped shutdown must stop the coordinator when the selected team is the last live team; current code gates coordinator stop on team.is_none() only"
    );
    for reason in [
        "bare_shutdown",
        "scoped_last_live_team",
        "scoped_live_sibling_present",
    ] {
        assert!(
            cli.contains(reason),
            "RED 5: shutdown JSON/event must expose structured coordinator_stop_reason={reason}; cli/mod.rs lacks it"
        );
    }
}

#[test]
fn scoped_shutdown_with_live_sibling_keeps_coordinator_guard() {
    let tit16 = repo_file("crates/team-agent/tests/shipgate_033_final_red.rs");
    let team_scope = repo_file("crates/team-agent/tests/team_in_team_scope_commands_red.rs");

    assert!(
        tit16.contains("TIT-16: shutdown --team child must not kill parent tmux session")
            && tit16.contains("must not kill the parent/workspace coordinator"),
        "RED 6 guard: TIT-16 live-sibling real-machine protection must remain present"
    );
    assert!(
        team_scope.contains(
            "scoped_shutdown_kills_only_selected_team_session_and_preserves_sibling_state"
        ) && team_scope.contains("must not mark sibling teamB agents stopped"),
        "RED 6 guard: scoped shutdown must preserve sibling team state"
    );
}

fn coordinator_workspace(tag: &str) -> (hermetic_guard::HermeticTestEnv, WorkspacePath, PathBuf) {
    let env = hermetic_guard::HermeticTestEnv::enter(tag);
    let root = env.workspace(tag);
    std::fs::create_dir_all(team_agent::model::paths::runtime_dir(&root))
        .expect("create runtime dir");
    let _ = team_agent::message_store::MessageStore::open(&root).expect("create schema");
    (env, WorkspacePath::new(root.clone()), root)
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

fn repo_file(relative: &str) -> String {
    std::fs::read_to_string(repo_root().join(relative))
        .unwrap_or_else(|error| panic!("read {relative}: {error}"))
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}
