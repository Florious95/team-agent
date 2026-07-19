//! Foundation-0 F0-3 RED contract: B0 legacy snapshot reader/path hide-one audit.
//!
//! References:
//! - `.team/artifacts/foundation-0-slice-design.md` §5 F0-3.
//! - F0-2 boundary: retained legacy snapshots are diagnostic only via
//!   `_not_authoritative`, `_canonical_state_path`, `_derived_from`,
//!   `_generated_at`, and `B0_DIAGNOSTIC_LEGACY_SNAPSHOT_READ` allowlisted reads.
//!
//! User story: when a legacy per-session snapshot disagrees with the current
//! root/projection state, every runtime reader keeps routing, restart, delivery,
//! status, and diagnose decisions on the current authority. The snapshot may be
//! shown only as a labeled diagnostic, never as hidden truth.

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

#[path = "support/hermetic.rs"]
mod hermetic_guard;

#[path = "support/composite_source.rs"]
mod composite_source;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};
use serial_test::serial;
use team_agent::state::persist::{runtime_state_path, save_runtime_state};

const TEAM: &str = "team-a";
const SESSION: &str = "team-b0-hideone";
const WORKER: &str = "worker";
const NEW_ENDPOINT: &str = "/private/tmp/tmux-501/ta-b0-new";
const OLD_ENDPOINT: &str = "/private/tmp/tmux-501/ta-b0-old";
const READ_MARKER: &str = "B0_DIAGNOSTIC_LEGACY_SNAPSHOT_READ";

#[test]
#[serial(env)]
fn stale_snapshot_endpoint_does_not_reach_restart_preflight_spawn_attach_or_events() {
    let case = B0HideOneCase::new("restart-endpoint-hide-one");
    case.seed_current_root_and_stale_snapshot();

    let selected = team_agent::state::projection::select_runtime_state(&case.workspace, Some(TEAM))
        .expect("select runtime state from current authority");
    assert_eq!(
        selected.get("tmux_endpoint").and_then(Value::as_str),
        Some(NEW_ENDPOINT),
        "F0-3 RED1: restart selection must read current root/projection endpoint, not stale legacy snapshot endpoint; selected={selected}"
    );

    let candidate = team_agent::lifecycle::select_restart_state(&case.workspace, Some(TEAM))
        .expect("restart preflight should select the current team");
    assert_eq!(
        candidate.state_path,
        runtime_state_path(&case.workspace),
        "F0-3 RED1: restart preflight must be rooted at the canonical runtime state file, not a legacy per-session snapshot; candidate={candidate:?}"
    );

    let offenders = source_lines_under("src/lifecycle/restart")
        .into_iter()
        .filter(|(rel, line_no, line)| legacy_snapshot_authority_read(rel, *line_no, line))
        .collect::<Vec<_>>();
    assert!(
        offenders.is_empty(),
        "F0-3 RED1: lifecycle/restart preflight, spawn metadata, attach_commands, and restart events must not read legacy snapshot paths. Offenders (file:line): {offenders:#?}"
    );
}

#[test]
#[serial(env)]
fn stale_snapshot_pane_does_not_reach_delivery_target_resolution() {
    let case = B0HideOneCase::new("delivery-pane-hide-one");
    case.seed_current_root_and_stale_snapshot();

    let selected = team_agent::state::projection::select_runtime_state(&case.workspace, Some(TEAM))
        .expect("select runtime state from current authority");
    assert_eq!(
        selected.pointer("/agents/worker/pane_id").and_then(Value::as_str),
        Some("%new"),
        "F0-3 RED2: delivery target selection must see the current root/projection worker pane, not stale legacy snapshot pane; selected={selected}"
    );

    let offenders = source_lines_for(&[
        "src/messaging",
        "src/cli/send.rs",
        "src/cli/named_address.rs",
    ])
    .into_iter()
    .filter(|(rel, line_no, line)| legacy_snapshot_authority_read(rel, *line_no, line))
    .collect::<Vec<_>>();
    assert!(
        offenders.is_empty(),
        "F0-3 RED2: delivery and named-send target resolution must not read legacy snapshot paths, so stale snapshot panes cannot be selected. Offenders (file:line): {offenders:#?}"
    );
}

#[test]
#[serial(env)]
fn stale_snapshot_cannot_flip_status_or_diagnose_ok_readiness() {
    let case = B0HideOneCase::new("status-diagnose-hide-one");
    let root = case.current_state();
    save_runtime_state(&case.workspace, &root).expect("seed current root state");

    let status_before = case.run_json(&[
        "status",
        "--workspace",
        case.workspace_str(),
        "--team",
        TEAM,
        "--detail",
        "--json",
    ]);
    let diagnose_before = case.run_json(&[
        "diagnose",
        "--workspace",
        case.workspace_str(),
        "--team",
        TEAM,
        "--json",
    ]);

    case.write_stale_snapshot();

    let status_after = case.run_json(&[
        "status",
        "--workspace",
        case.workspace_str(),
        "--team",
        TEAM,
        "--detail",
        "--json",
    ]);
    let diagnose_after = case.run_json(&[
        "diagnose",
        "--workspace",
        case.workspace_str(),
        "--team",
        TEAM,
        "--json",
    ]);

    assert_eq!(
        status_after.pointer("/agents/worker/pane_id").and_then(Value::as_str),
        Some("%new"),
        "F0-3 RED3: status must not display the stale snapshot pane as the worker authority; before={status_before} after={status_after}"
    );
    assert_eq!(
        worker_status_tuple(&status_before),
        worker_status_tuple(&status_after),
        "F0-3 RED3: stale legacy snapshot must not flip status/readiness fields except for a labeled diagnostic; before={status_before} after={status_after}"
    );
    assert_eq!(
        diagnose_before.get("ok"),
        diagnose_after.get("ok"),
        "F0-3 RED3: stale legacy snapshot must not flip diagnose ok; before={diagnose_before} after={diagnose_after}"
    );
    let before_issues = issue_ids(&diagnose_before);
    let after_issues = issue_ids(&diagnose_after);
    let new_issues = after_issues
        .iter()
        .filter(|issue| !before_issues.contains(*issue))
        .cloned()
        .collect::<Vec<_>>();
    let bad = new_issues
        .iter()
        .filter(|issue| issue.as_str() != "legacy_snapshot_stale")
        .collect::<Vec<_>>();
    assert!(
        bad.is_empty(),
        "F0-3 RED3: status/diagnose may surface only labeled `legacy_snapshot_stale` for a divergent snapshot; new_issues={new_issues:?} before={diagnose_before} after={diagnose_after}"
    );
}

#[test]
fn product_authority_paths_do_not_read_legacy_snapshot_without_diagnostic_marker() {
    let offenders = source_lines_under("src")
        .into_iter()
        .filter(|(rel, line_no, line)| legacy_snapshot_authority_read(rel, *line_no, line))
        .collect::<Vec<_>>();

    assert!(
        offenders.is_empty(),
        "F0-3 RED4: product authority paths must not read legacy per-session snapshots. Only migration/diagnostic/test code, or lines explicitly marked `{READ_MARKER}`, may touch `.team/runtime/teams/<session_name>/state.json`. Offenders (file:line): {offenders:#?}"
    );
}

#[test]
fn team_runtime_state_path_uses_b3_team_key_layout_not_legacy_runtime_root_layout() {
    let paths = team_agent::state::paths::TeamRuntimePaths::new(PathBuf::from("/ws/proj"), "alpha");

    assert_eq!(
        paths.state_path(),
        PathBuf::from("/ws/proj/.team/runtime/teams/alpha/state.json"),
        "F0-3 RED5: state/paths.rs must pre-announce the B3 canonical team state path as `.team/runtime/teams/<team_key>/state.json`; `.team/runtime/<team_key>/state.json` collides with runtime spec/coordinator sidecar layout and keeps the old hide-two ambiguity alive"
    );
}

struct B0HideOneCase {
    env: hermetic_guard::HermeticTestEnv,
    workspace: PathBuf,
    workspace_str: String,
}

impl B0HideOneCase {
    fn new(tag: &str) -> Self {
        let env = hermetic_guard::HermeticTestEnv::enter(tag);
        let workspace = env.workspace(tag);
        let workspace_str = workspace.to_string_lossy().to_string();
        Self {
            env,
            workspace,
            workspace_str,
        }
    }

    fn workspace_str(&self) -> &str {
        &self.workspace_str
    }

    fn seed_current_root_and_stale_snapshot(&self) {
        save_runtime_state(&self.workspace, &self.current_state())
            .expect("seed current root state");
        self.write_stale_snapshot();
    }

    fn current_state(&self) -> Value {
        self.state("%new", 24001, 3, NEW_ENDPOINT)
    }

    fn stale_snapshot_state(&self) -> Value {
        let mut state = self.state("%old", 12001, 1, OLD_ENDPOINT);
        let canonical = runtime_state_path(&self.workspace);
        let obj = state.as_object_mut().expect("state object");
        obj.insert("_not_authoritative".to_string(), json!(true));
        obj.insert(
            "_canonical_state_path".to_string(),
            json!(canonical.to_string_lossy().to_string()),
        );
        obj.insert(
            "_derived_from".to_string(),
            json!("f0-3-test-stale-legacy-snapshot"),
        );
        obj.insert("_generated_at".to_string(), json!("2026-07-11T00:00:00Z"));
        state
    }

    fn state(&self, pane_id: &str, pane_pid: i64, spawn_epoch: u64, endpoint: &str) -> Value {
        let worker = json!({
            "id": WORKER,
            "agent_id": WORKER,
            "provider": "fake",
            "model": "fake",
            "pane_id": pane_id,
            "pane_pid": pane_pid,
            "tmux_session": SESSION,
            "window": WORKER,
            "last_window": WORKER,
            "spawn_epoch": spawn_epoch,
            "status": "running",
            "auth_mode": "subscription"
        });
        let team = json!({
            "team_key": TEAM,
            "active_team_key": TEAM,
            "session_name": SESSION,
            "team_dir": self.workspace_str,
            "workspace": self.workspace_str,
            "status": "alive",
            "tmux_endpoint": endpoint,
            "tmux_socket": endpoint,
            "tmux_socket_source": "test",
            "transport": {"kind": "tmux", "source": "test"},
            "agents": {
                WORKER: worker.clone()
            },
            "leader_receiver": {
                "status": "attached",
                "pane_id": "%leader-new",
                "provider": "fake",
                "tmux_socket": endpoint,
                "owner_epoch": 3
            },
            "team_owner": {
                "pane_id": "%leader-new",
                "provider": "fake",
                "owner_epoch": 3,
                "leader_session_uuid": "leader-new"
            }
        });
        json!({
            "schema_version": 1,
            "active_team_key": TEAM,
            "team_key": TEAM,
            "session_name": SESSION,
            "team_dir": self.workspace_str,
            "workspace": self.workspace_str,
            "status": "alive",
            "tmux_endpoint": endpoint,
            "tmux_socket": endpoint,
            "tmux_socket_source": "test",
            "transport": {"kind": "tmux", "source": "test"},
            "agents": {
                WORKER: worker
            },
            "leader_receiver": {
                "status": "attached",
                "pane_id": "%leader-new",
                "provider": "fake",
                "tmux_socket": endpoint,
                "owner_epoch": 3
            },
            "team_owner": {
                "pane_id": "%leader-new",
                "provider": "fake",
                "owner_epoch": 3,
                "leader_session_uuid": "leader-new"
            },
            "teams": {
                TEAM: team
            }
        })
    }

    fn write_stale_snapshot(&self) {
        let path = team_agent::lifecycle::helpers::team_snapshot_path(&self.workspace, SESSION);
        write_json(&path, &self.stale_snapshot_state());
    }

    fn run_json(&self, args: &[&str]) -> Value {
        let output = self.env.run_cli(&self.workspace, args);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        parse_json_object(&format!("{stdout}\n{stderr}")).unwrap_or_else(|| {
            panic!("command did not emit JSON: args={args:?} stdout={stdout} stderr={stderr}")
        })
    }
}

fn worker_status_tuple(status: &Value) -> Value {
    let worker = status
        .get("agents")
        .and_then(Value::as_object)
        .and_then(|agents| agents.get(WORKER))
        .cloned()
        .unwrap_or(Value::Null);
    json!({
        "ok": status.get("ok").cloned().unwrap_or(Value::Null),
        "pane_id": worker.get("pane_id").cloned().unwrap_or(Value::Null),
        "status": worker.get("status").cloned().unwrap_or(Value::Null),
        "readiness": status.get("readiness").cloned().unwrap_or(Value::Null),
    })
}

fn issue_ids(value: &Value) -> Vec<String> {
    value
        .get("issues")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|issue| {
            issue.as_str().map(str::to_string).or_else(|| {
                issue
                    .get("id")
                    .or_else(|| issue.get("code"))
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
        })
        .collect()
}

fn write_json(path: &Path, value: &Value) {
    std::fs::create_dir_all(path.parent().expect("json parent")).expect("create json parent");
    std::fs::write(
        path,
        serde_json::to_vec_pretty(value).expect("serialize json"),
    )
    .unwrap_or_else(|error| panic!("write {}: {error}", path.display()));
}

fn parse_json_object(text: &str) -> Option<Value> {
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    serde_json::from_str(&text[start..=end]).ok()
}

fn legacy_snapshot_authority_read(rel: &str, _line_no: usize, line: &str) -> bool {
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
        || line.contains(READ_MARKER)
}

fn source_lines_for(paths: &[&str]) -> Vec<(String, usize, String)> {
    paths
        .iter()
        .flat_map(|rel| source_lines_under(rel))
        .collect()
}

fn source_lines_under(rel: &str) -> Vec<(String, usize, String)> {
    let mut out = Vec::new();
    if rel.ends_with(".rs") {
        for (part_rel, text) in composite_source::composite_files(rel) {
            for (index, line) in text.lines().enumerate() {
                out.push((part_rel.clone(), index + 1, line.to_string()));
            }
        }
        return out;
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join(rel);
    append_source_lines(&root, Path::new(env!("CARGO_MANIFEST_DIR")), &mut out);
    out
}

fn append_source_lines(path: &Path, base: &Path, out: &mut Vec<(String, usize, String)>) {
    if path.is_dir() {
        let mut entries = std::fs::read_dir(path)
            .expect("read source dir")
            .map(|entry| entry.expect("read source entry").path())
            .collect::<Vec<PathBuf>>();
        entries.sort();
        for entry in entries {
            append_source_lines(&entry, base, out);
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
