//! Foundation-0 F0-2 RED contract: B0 legacy per-session snapshot non-authority.
//!
//! References:
//! - `.team/artifacts/foundation-0-slice-design.md` §4 B0 target semantics.
//! - `.team/artifacts/foundation-0-slice-design.md` §5 F0-2 RED design.
//!
//! User story: legacy `.team/runtime/teams/<session_name>/state.json`
//! snapshots are either gone or visibly diagnostic. Root/projection remains the
//! runtime authority for route/readiness/ok decisions when the legacy snapshot
//! diverges.

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

#[path = "support/hermetic.rs"]
mod hermetic_guard;

use std::path::{Path, PathBuf};

use serde_json::{json, Value};
use serial_test::serial;
use team_agent::state::persist::{runtime_state_path, save_runtime_state};

const TEAM: &str = "team-a";
const SESSION: &str = "b0-legacy-session";
const WORKER: &str = "worker";
const DIAGNOSTIC_READ_MARKER: &str = "B0_DIAGNOSTIC_LEGACY_SNAPSHOT_READ";
const DIAGNOSTIC_WRITE_MARKER: &str = "B0_DIAGNOSTIC_LEGACY_SNAPSHOT_WRITE";

#[test]
#[serial(env)]
fn lifecycle_snapshot_after_operation_is_absent_or_marked_nonauthoritative() {
    let case = B0Case::new("snapshot-after-operation");
    let state = case.state_with_worker("%new", 4201, 1);

    let path = team_agent::lifecycle::save_team_runtime_snapshot(&case.workspace, &state)
        .expect("F0-2 fixture: lifecycle snapshot save should return a path or no-op cleanly");
    case.env.assert_path_under_root(&path);
    if !path.exists() {
        return;
    }

    let snapshot = read_json(&path);
    let canonical = runtime_state_path(&case.workspace);
    assert_eq!(
        snapshot
            .get("_not_authoritative")
            .and_then(Value::as_bool),
        Some(true),
        "F0-2 RED1: a retained legacy per-session snapshot must carry `_not_authoritative:true`; raw dual-write snapshots look authoritative. path={} snapshot={snapshot}",
        path.display()
    );
    assert_eq!(
        snapshot
            .get("_canonical_state_path")
            .and_then(Value::as_str),
        Some(canonical.to_string_lossy().as_ref()),
        "F0-2 RED1: retained snapshot must point readers at the canonical root/projection state via `_canonical_state_path`; path={} snapshot={snapshot}",
        path.display()
    );
    for required in ["_derived_from", "_generated_at"] {
        assert!(
            snapshot.get(required).is_some(),
            "F0-2 RED1: retained legacy snapshot must include diagnostic metadata `{required}`; path={} snapshot={snapshot}",
            path.display()
        );
    }
}

#[test]
fn legacy_snapshot_writes_are_not_reachable_from_authority_save_paths() {
    let offenders = rs_source_lines("src")
        .into_iter()
        .filter(|(rel, line_no, line)| legacy_snapshot_write_reference(rel, *line_no, line))
        .collect::<Vec<_>>();

    assert!(
        offenders.is_empty(),
        "F0-2 RED2: legacy per-session snapshot writes are allowed only for diagnostic/migration/test paths marked `{DIAGNOSTIC_WRITE_MARKER}`. Product authority saves must not call save_team_runtime_snapshot/write_team_snapshot_atomic or recreate dual-write promises. Offenders (file:line): {offenders:#?}"
    );
}

#[test]
#[serial(env)]
fn stale_legacy_snapshot_does_not_drive_route_readiness_or_ok_authority() {
    let case = B0Case::new("stale-snapshot-nonauthority");
    let root = case.state_with_worker("%new", 5101, 1);
    save_runtime_state(&case.workspace, &root).expect("seed canonical root state");

    let legacy = case.state_with_worker("%old", 9101, 0);
    let legacy_path = team_agent::lifecycle::helpers::team_snapshot_path(&case.workspace, SESSION);
    write_json(&legacy_path, &legacy);

    let selected = team_agent::state::projection::select_runtime_state(&case.workspace, Some(TEAM))
        .expect("select current team from canonical root/projection state");
    assert_eq!(
        selected.pointer("/agents/worker/pane_id").and_then(Value::as_str),
        Some("%new"),
        "F0-2 RED3: route/readiness selection must use canonical root/projection worker tuple, not divergent legacy snapshot tuple; selected={selected} legacy={legacy}"
    );
    assert_eq!(
        selected
            .pointer("/agents/worker/spawn_epoch")
            .and_then(Value::as_u64),
        Some(1),
        "F0-2 RED3: stale snapshot fields must not lower readiness/route epoch decisions; selected={selected} legacy={legacy}"
    );

    let offenders = rs_source_lines("src")
        .into_iter()
        .filter(|(rel, line_no, line)| {
            legacy_snapshot_authority_read_reference(rel, *line_no, line)
        })
        .collect::<Vec<_>>();
    assert!(
        offenders.is_empty(),
        "F0-2 RED3: legacy snapshot reads may only be diagnostic/stale display reads marked `{DIAGNOSTIC_READ_MARKER}` and must not feed route/readiness/ok/repair decisions. Offenders (file:line): {offenders:#?}"
    );
}

struct B0Case {
    env: hermetic_guard::HermeticTestEnv,
    workspace: PathBuf,
}

impl B0Case {
    fn new(tag: &str) -> Self {
        let env = hermetic_guard::HermeticTestEnv::enter(tag);
        let workspace = env.workspace(tag);
        Self { env, workspace }
    }

    fn state_with_worker(&self, pane_id: &str, pane_pid: i64, spawn_epoch: u64) -> Value {
        let worker = json!({
            "id": WORKER,
            "pane_id": pane_id,
            "pane_pid": pane_pid,
            "tmux_session": SESSION,
            "tmux_window": format!("{WORKER}-win"),
            "spawn_epoch": spawn_epoch,
            "status": "running"
        });
        let team = json!({
            "session_name": SESSION,
            "team_dir": self.workspace.to_string_lossy().to_string(),
            "status": "alive",
            "agents": {
                WORKER: worker.clone()
            }
        });
        json!({
            "schema_version": 1,
            "session_name": SESSION,
            "team_dir": self.workspace.to_string_lossy().to_string(),
            "active_team_key": TEAM,
            "agents": {
                WORKER: worker
            },
            "teams": {
                TEAM: team
            }
        })
    }
}

fn write_json(path: &Path, value: &Value) {
    std::fs::create_dir_all(path.parent().expect("json parent")).expect("create json parent");
    std::fs::write(
        path,
        serde_json::to_vec_pretty(value).expect("serialize json"),
    )
    .unwrap_or_else(|error| panic!("write {}: {error}", path.display()));
}

fn read_json(path: &Path) -> Value {
    serde_json::from_slice(
        &std::fs::read(path).unwrap_or_else(|error| panic!("read {}: {error}", path.display())),
    )
    .unwrap_or_else(|error| panic!("parse {}: {error}", path.display()))
}

fn legacy_snapshot_write_reference(rel: &str, line_no: usize, line: &str) -> bool {
    let trimmed = line.trim_start();
    if trimmed.starts_with("//") || trimmed.starts_with("///") || trimmed.starts_with("//!") {
        return false;
    }
    if allowed_test_source(rel) || allowed_diagnostic_or_migration_source(rel, line) {
        return false;
    }
    if rel == "src/lifecycle/helpers.rs"
        && (line.contains("pub fn save_team_runtime_snapshot")
            || line.contains("pub fn team_snapshot_path"))
    {
        return false;
    }
    let _ = line_no;
    (line.contains("save_team_runtime_snapshot(")
        && !line.contains("pub fn save_team_runtime_snapshot"))
        || line.contains("write_team_snapshot_atomic(")
        || line.contains("fn write_team_snapshot_atomic")
}

fn legacy_snapshot_authority_read_reference(rel: &str, line_no: usize, line: &str) -> bool {
    let trimmed = line.trim_start();
    if trimmed.starts_with("//") || trimmed.starts_with("///") || trimmed.starts_with("//!") {
        return false;
    }
    if allowed_test_source(rel) || allowed_diagnostic_or_migration_source(rel, line) {
        return false;
    }
    if rel == "src/lifecycle/helpers.rs" {
        return false;
    }
    let _ = line_no;
    line.contains("team_snapshot_path(")
        || line.contains("readable_team_snapshot_path(")
        || line.contains("detect_dual_state_divergence(")
        || line.contains("\".team/runtime/teams\"")
        || line.contains("runtime/teams")
}

fn allowed_test_source(rel: &str) -> bool {
    rel.contains("/tests/") || rel.ends_with("/tests.rs")
}

fn allowed_diagnostic_or_migration_source(rel: &str, line: &str) -> bool {
    rel.contains("/migration")
        || rel.contains("/diagnose")
        || rel.contains("diagnose")
        || line.contains(DIAGNOSTIC_READ_MARKER)
        || line.contains(DIAGNOSTIC_WRITE_MARKER)
}

fn rs_source_lines(rel: &str) -> Vec<(String, usize, String)> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join(rel);
    let mut out = Vec::new();
    append_rs_source_lines(&root, Path::new(env!("CARGO_MANIFEST_DIR")), &mut out);
    out
}

fn append_rs_source_lines(path: &Path, base: &Path, out: &mut Vec<(String, usize, String)>) {
    if path.is_dir() {
        let mut entries = std::fs::read_dir(path)
            .expect("read source dir")
            .map(|entry| entry.expect("read source entry").path())
            .collect::<Vec<PathBuf>>();
        entries.sort();
        for entry in entries {
            append_rs_source_lines(&entry, base, out);
        }
        return;
    }
    if path.extension().and_then(|v| v.to_str()) != Some("rs") {
        return;
    }
    let rel = path
        .strip_prefix(base)
        .unwrap_or(path)
        .to_string_lossy()
        .to_string();
    let text = std::fs::read_to_string(path).expect("read source file");
    for (idx, line) in text.lines().enumerate() {
        out.push((rel.clone(), idx + 1, line.trim().to_string()));
    }
}
