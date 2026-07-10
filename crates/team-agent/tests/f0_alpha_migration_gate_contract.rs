//! Foundation-0 F0-4 RED contract: 0.6.0-alpha migration and observability gate.
//!
//! References:
//! - `.team/artifacts/foundation-0-slice-design.md` §5 F0-4.
//! - `.team/artifacts/foundation-0-slice-design.md` §7 compatibility and migration strategy.
//! - F0-2/F0-3 B0 boundary: legacy snapshots are diagnostic only; authority reads
//!   require `B0_DIAGNOSTIC_LEGACY_SNAPSHOT_READ` if they touch the old path.
//!
//! User story: a 0.5.x workspace can enter the Foundation-0 alpha without being
//! rewritten into the future B1 repository shape, while A0/B0 observability makes
//! the transition explicit instead of hiding task/message or root/snapshot drift.

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

#[path = "support/hermetic.rs"]
mod hermetic_guard;

use std::path::{Path, PathBuf};

use serde_json::{json, Value};
use serial_test::serial;
use team_agent::mcp_server::TeamOrchestratorTools;
use team_agent::message_store::MessageStore;
use team_agent::model::enums::ResultStatus;
use team_agent::model::ids::{AgentId, TeamKey};
use team_agent::state::persist::{runtime_state_path, save_runtime_state};

const TEAM: &str = "team-alpha";
const SESSION: &str = "legacy-05-session";
const WORKER: &str = "worker";
const MSG_ALPHA: &str = "msg_alpha_current_turn";
const NEW_ENDPOINT: &str = "/private/tmp/tmux-501/ta-alpha-new";
const OLD_ENDPOINT: &str = "/private/tmp/tmux-501/ta-alpha-old";
const READ_MARKER: &str = "B0_DIAGNOSTIC_LEGACY_SNAPSHOT_READ";

#[test]
#[serial(env)]
fn legacy_05_workspace_loads_without_b1_destructive_conversion() {
    let case = AlphaCase::new("legacy-05-load");
    case.write_raw_root_state(&case.legacy_05_state());
    case.write_raw_stale_snapshot(false);

    let status = case.run_json(&[
        "status",
        "--workspace",
        case.workspace_str(),
        "--team",
        TEAM,
        "--detail",
        "--json",
    ]);
    assert_eq!(
        status.pointer("/agents/worker/pane_id").and_then(Value::as_str),
        Some("%new"),
        "F0-4 RED1 setup: alpha status must load the 0.5.x root/projection fixture and display the current root worker; status={status}"
    );

    let after = read_json(&runtime_state_path(&case.workspace));
    assert_eq!(
        after.get("legacy_05_marker").and_then(Value::as_str),
        Some("preserve-me"),
        "F0-4 RED1: loading a 0.5.x workspace must not destructively rewrite unknown legacy root fields into a B1-only shape; after={after}"
    );
    assert!(
        after.get("agents").is_some()
            && after.get("tasks").is_some()
            && after.get("teams").and_then(Value::as_object).is_some(),
        "F0-4 RED1: alpha load must keep 0.5.x root/projection fields available; no raw B1 workspace-index-only assumption is allowed; after={after}"
    );
    assert!(
        !case
            .workspace
            .join(".team/runtime/workspace-index.json")
            .exists(),
        "F0-4 RED1: alpha load must not create a B1 workspace index as a prerequisite"
    );
    assert!(
        !case.workspace.join(".team/runtime/teams").join(TEAM).exists(),
        "F0-4 RED1: alpha load must not convert root state into the future B1 team-key directory during a read-only status command"
    );
    assert!(
        case.legacy_snapshot_path().exists(),
        "F0-4 RED1: read-only alpha load must not delete a legacy 0.5.x snapshot as a hidden migration side effect"
    );
}

#[test]
#[serial(env)]
fn stale_legacy_snapshot_is_marked_or_reported_and_never_consumed_by_product_readers() {
    let case = AlphaCase::new("stale-snapshot-observability");
    save_runtime_state(&case.workspace, &case.legacy_05_state()).expect("seed root state");
    case.write_raw_stale_snapshot(false);

    let status = case.run_json(&[
        "status",
        "--workspace",
        case.workspace_str(),
        "--team",
        TEAM,
        "--detail",
        "--json",
    ]);
    let diagnose = case.run_json(&[
        "diagnose",
        "--workspace",
        case.workspace_str(),
        "--team",
        TEAM,
        "--json",
    ]);

    assert_eq!(
        status.pointer("/agents/worker/pane_id").and_then(Value::as_str),
        Some("%new"),
        "F0-4 RED2: stale legacy snapshot pane must be ignored by status/readiness authority; status={status}"
    );
    assert!(
        issue_ids(&diagnose)
            .iter()
            .any(|issue| issue == "legacy_snapshot_stale"),
        "F0-4 RED2: a divergent legacy snapshot must be visibly marked/reported as `legacy_snapshot_stale` instead of silently ignored; diagnose={diagnose}"
    );

    let offenders = source_lines_under("src")
        .into_iter()
        .filter(|(rel, line_no, line)| legacy_snapshot_authority_read(rel, *line_no, line))
        .collect::<Vec<_>>();
    assert!(
        offenders.is_empty(),
        "F0-4 RED2: product readers must not consume legacy snapshot paths. Only migration/diagnostic/test code, or lines marked `{READ_MARKER}`, may read them. Offenders (file:line): {offenders:#?}"
    );
}

#[test]
#[serial(env)]
fn a0_transition_response_names_message_identity_distinctly_from_task_identity() {
    let case = AlphaCase::new("a0-response-naming");
    save_runtime_state(&case.workspace, &case.a0_state()).expect("seed A0 state");
    let store = MessageStore::open(&case.workspace).expect("open message store");
    store
        .create_message_with_id(
            MSG_ALPHA,
            None,
            "leader",
            WORKER,
            "alpha current-turn report",
            None,
            false,
            Some(TEAM),
        )
        .expect("insert current-turn message");
    store
        .mark(MSG_ALPHA, "delivered", None)
        .expect("mark current-turn message delivered");

    let tools = TeamOrchestratorTools::with_identity(
        &case.workspace,
        Some(AgentId::new(WORKER)),
        Some(TeamKey::new(TEAM)),
    );
    let body = tools
        .report_result(
            None,
            Some("F0_ALPHA_A0_NAMING"),
            ResultStatus::Success,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .map(|ok| Value::Object(ok.fields))
        .unwrap_or_else(|err| err.to_envelope());

    assert_eq!(
        body.get("task_id").and_then(Value::as_str),
        Some(MSG_ALPHA),
        "F0-4 RED3 setup: A0 transition still allows task_id == message_id for the current direct turn; body={body}"
    );
    assert_eq!(
        body.get("attributed_message_id").and_then(Value::as_str),
        Some(MSG_ALPHA),
        "F0-4 RED3: response shape must expose the message identity under a distinct field, not only echo it as task_id; body={body}"
    );
    assert_eq!(
        body.get("attribution_scope").and_then(Value::as_str),
        Some("message"),
        "F0-4 RED3: response/docs fields must name this as message-scoped attribution so clients do not learn that task_id and message_id are permanently identical; body={body}"
    );
    assert_eq!(
        body.get("task_id_source").and_then(Value::as_str),
        Some("current_turn_message"),
        "F0-4 RED3: task_id provenance must be structured during the A0 bridge; body={body}"
    );
}

#[test]
fn alpha_gate_keeps_a0_b0_contract_families_present_and_ci_executed() {
    let manifest = read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml"));
    assert!(
        !manifest.contains("autotests = false"),
        "F0-4 RED4: integration contracts under crates/team-agent/tests must stay auto-discovered by cargo test; Cargo.toml must not disable autotests"
    );

    for file in [
        "a0_current_turn_attribution_contract.rs",
        "result_attribution_race_contract.rs",
        "b0_legacy_snapshot_nonauthority_contract.rs",
        "b0_reader_hideone_audit_contract.rs",
        "f0_alpha_migration_gate_contract.rs",
    ] {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join(file);
        assert!(
            path.exists(),
            "F0-4 RED4: alpha gate requires A0/B0 contract family file `{file}` to be present so it cannot be silently removed"
        );
    }

    let workflow = read_to_string(repo_root().join(".github/workflows/publish-npm.yml"));
    assert!(
        workflow.contains("cargo test --workspace --locked --no-fail-fast"),
        "F0-4 RED4: npm/release CI must run cargo test for the workspace so standalone A0/B0 integration contracts execute"
    );
}

struct AlphaCase {
    _env: hermetic_guard::HermeticTestEnv,
    workspace: PathBuf,
    workspace_str: String,
}

impl AlphaCase {
    fn new(tag: &str) -> Self {
        let env = hermetic_guard::HermeticTestEnv::enter(tag);
        let workspace = env.workspace(tag);
        let workspace_str = workspace.to_string_lossy().to_string();
        Self {
            _env: env,
            workspace,
            workspace_str,
        }
    }

    fn workspace_str(&self) -> &str {
        &self.workspace_str
    }

    fn legacy_05_state(&self) -> Value {
        let worker = self.worker("%new", 24001, 7, NEW_ENDPOINT);
        let team = json!({
            "team_key": TEAM,
            "session_name": SESSION,
            "team_dir": self.workspace_str,
            "workspace": self.workspace_str,
            "status": "alive",
            "tmux_endpoint": NEW_ENDPOINT,
            "tmux_socket": NEW_ENDPOINT,
            "agents": {
                WORKER: worker.clone()
            },
            "tasks": [{
                "id": "task_initial",
                "assignee": WORKER,
                "status": "pending"
            }]
        });
        json!({
            "schema_version": 1,
            "runtime_version": "0.5.x-fixture",
            "legacy_05_marker": "preserve-me",
            "active_team_key": TEAM,
            "team_key": TEAM,
            "session_name": SESSION,
            "team_dir": self.workspace_str,
            "workspace": self.workspace_str,
            "status": "alive",
            "tmux_endpoint": NEW_ENDPOINT,
            "tmux_socket": NEW_ENDPOINT,
            "agents": {
                WORKER: worker
            },
            "tasks": [{
                "id": "task_initial",
                "assignee": WORKER,
                "status": "pending"
            }],
            "teams": {
                TEAM: team
            }
        })
    }

    fn a0_state(&self) -> Value {
        let mut state = self.legacy_05_state();
        state["agents"][WORKER]["current_turn_message_id"] = json!(MSG_ALPHA);
        state["teams"][TEAM]["agents"][WORKER]["current_turn_message_id"] = json!(MSG_ALPHA);
        state
    }

    fn stale_snapshot_state(&self, marked: bool) -> Value {
        let mut state = self.legacy_05_state();
        state["tmux_endpoint"] = json!(OLD_ENDPOINT);
        state["tmux_socket"] = json!(OLD_ENDPOINT);
        state["agents"][WORKER] = self.worker("%old", 12001, 1, OLD_ENDPOINT);
        state["teams"][TEAM]["tmux_endpoint"] = json!(OLD_ENDPOINT);
        state["teams"][TEAM]["tmux_socket"] = json!(OLD_ENDPOINT);
        state["teams"][TEAM]["agents"][WORKER] = self.worker("%old", 12001, 1, OLD_ENDPOINT);
        if marked {
            let canonical = runtime_state_path(&self.workspace);
            let obj = state.as_object_mut().expect("state object");
            obj.insert("_not_authoritative".to_string(), json!(true));
            obj.insert(
                "_canonical_state_path".to_string(),
                json!(canonical.to_string_lossy().to_string()),
            );
            obj.insert("_derived_from".to_string(), json!("f0-4-test"));
            obj.insert("_generated_at".to_string(), json!("2026-07-11T00:00:00Z"));
        }
        state
    }

    fn worker(&self, pane_id: &str, pane_pid: i64, spawn_epoch: u64, endpoint: &str) -> Value {
        json!({
            "id": WORKER,
            "agent_id": WORKER,
            "provider": "fake",
            "model": "fake",
            "pane_id": pane_id,
            "pane_pid": pane_pid,
            "tmux_session": SESSION,
            "window": WORKER,
            "spawn_epoch": spawn_epoch,
            "spawn_cwd": self.workspace_str,
            "tmux_endpoint": endpoint,
            "status": "running",
            "auth_mode": "subscription"
        })
    }

    fn write_raw_root_state(&self, state: &Value) {
        write_json(&runtime_state_path(&self.workspace), state);
    }

    fn write_raw_stale_snapshot(&self, marked: bool) {
        write_json(
            &self.legacy_snapshot_path(),
            &self.stale_snapshot_state(marked),
        );
    }

    fn legacy_snapshot_path(&self) -> PathBuf {
        team_agent::lifecycle::helpers::team_snapshot_path(&self.workspace, SESSION)
    }

    fn run_json(&self, args: &[&str]) -> Value {
        let output = self._env.run_cli(&self.workspace, args);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        parse_json_object(&format!("{stdout}\n{stderr}")).unwrap_or_else(|| {
            panic!("command did not emit JSON: args={args:?} stdout={stdout} stderr={stderr}")
        })
    }
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

fn read_json(path: &Path) -> Value {
    serde_json::from_slice(
        &std::fs::read(path).unwrap_or_else(|error| panic!("read {}: {error}", path.display())),
    )
    .unwrap_or_else(|error| panic!("parse {}: {error}", path.display()))
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

fn source_lines_under(rel: &str) -> Vec<(String, usize, String)> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join(rel);
    let mut out = Vec::new();
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
    let text = read_to_string(path);
    for (idx, line) in text.lines().enumerate() {
        out.push((rel.clone(), idx + 1, line.trim().to_string()));
    }
}

fn read_to_string(path: impl AsRef<Path>) -> String {
    std::fs::read_to_string(path.as_ref())
        .unwrap_or_else(|error| panic!("read {}: {error}", path.as_ref().display()))
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("repo root")
        .to_path_buf()
}
