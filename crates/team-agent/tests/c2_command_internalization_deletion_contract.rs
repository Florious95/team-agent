//! C2 Command Internalization / Deletion RED contracts.
//!
//! References:
//! - `.team/artifacts/c2-command-internalization-deletion-design.md` §3
//!   deletion plan, §5 worker reference cleanup, and §6 RED1-RED8.
//!
//! User story: workers and users should use the normal send/report_result,
//! collect, diagnose, and doctor paths. C2 deletes the old user-invoked repair
//! commands instead of keeping them as hidden compatibility escape hatches.

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

#[path = "support/hermetic.rs"]
mod hermetic_guard;
#[allow(dead_code)]
fn _hermetic_boundary_marker(_: &hermetic_guard::HermeticTestEnv) {}

use std::collections::{BTreeSet, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

use rusqlite::params;
use serde_json::{json, Value};
use team_agent::message_store::MessageStore;
use team_agent::state::persist::{load_runtime_state, save_runtime_state};

const DELETED_COMMANDS: &[&str] = &[
    "fallback-send-leader",
    "fallback-report-result",
    "settle",
    "validate-result",
    "repair-state",
];
const FALLBACK_COMMANDS: &[&str] = &["fallback-send-leader", "fallback-report-result"];
const VISIBLE_COMMAND_LIMIT: usize = 15;

static COUNTER: AtomicU64 = AtomicU64::new(0);

#[test]
fn red1_deleted_repair_commands_absent_from_command_surface() {
    let case = Case::new("red1-surface");
    let spec = read_repo_file("crates/team-agent/src/cli/spec.rs");
    let emit = read_repo_file("crates/team-agent/src/cli/emit.rs");
    let default_help = stdout(&case.run_ta(&["--help"]));
    let visible = visible_default_commands(&default_help);
    let mut failures = Vec::new();

    for command in DELETED_COMMANDS {
        if spec_block_for(&spec, command).is_some() {
            failures.push(format!("COMMAND_SPECS still contains `{command}`"));
        }
        if emit_const_contains(&emit, "DISPATCH_COMMANDS", command) {
            failures.push(format!("DISPATCH_COMMANDS still lists `{command}`"));
        }
        if dispatch_contains_command_arm(&emit, command) {
            failures.push(format!("dispatch still has a match arm for `{command}`"));
        }
        if visible.contains(*command) || default_help.contains(command) {
            failures.push(format!("default help still exposes `{command}`"));
        }

        let help = case.run_ta(&[command, "--help"]);
        let help_text = output_text(&help);
        if help.status.success()
            || help_text.contains("status: hidden compatibility command")
            || help_text.contains(&format!("usage: team-agent {command}"))
        {
            failures.push(format!(
                "`team-agent {command} --help` must be unknown/removed, not hidden-compatible; status={} text={help_text}",
                help.status
            ));
        }

        let invoke = case.run_ta(&[command, "--json"]);
        let invoke_text = output_text(&invoke);
        if invoke.status.success() || invoke_text.contains("hidden compatibility command") {
            failures.push(format!(
                "`team-agent {command}` must not execute as a compat command; status={} text={invoke_text}",
                invoke.status
            ));
        }
    }

    assert!(
        failures.is_empty(),
        "RED1: C2 must hard-delete fallback-send-leader/fallback-report-result/settle/validate-result/repair-state from registry, help, dispatch, and exact invocation.\n{}",
        failures.join("\n")
    );
}

#[test]
fn red2_worker_prompts_and_skills_do_not_teach_fallback_commands() {
    let mut offenders = Vec::new();
    for path in packaged_worker_reference_files() {
        let text = read_repo_file(&path);
        for command in FALLBACK_COMMANDS {
            if text.contains(command) {
                offenders.push(format!("{path} contains literal `{command}`"));
            }
        }
    }

    let prompt_source = read_repo_file("crates/team-agent/src/lifecycle/worker_command_context.rs");
    for command in FALLBACK_COMMANDS {
        if prompt_source.contains(command) {
            offenders.push(format!(
                "generated worker prompt source contains literal `{command}`"
            ));
        }
    }

    let runbook = read_repo_file("skills/team-agent/references/recovery-runbook.md");
    let normalized = normalize(&runbook);
    if !(normalized.contains("payload") && normalized.contains("handoff")) {
        offenders.push(
            "recovery runbook must still preserve the MCP-transport-dead payload handoff contingency"
                .to_string(),
        );
    }

    assert!(
        offenders.is_empty(),
        "RED2: packaged worker prompts/skills/runbooks must not teach fallback CLI names after C2; workers preserve payloads and recover through normal MCP send/report paths.\n{}",
        offenders.join("\n")
    );
}

#[test]
fn red3_normal_send_and_report_paths_replace_fallback_commands() {
    let case = Case::new("red3-normal-paths");
    case.write_rebind_required_state();

    let send = case.run_ta(&[
        "send",
        "--workspace",
        case.workspace_str(),
        "--to-leader",
        "c2-missing-leader",
        "leader unavailable C2 token",
        "--json",
    ]);
    let send_text = output_text(&send);
    let send_lower = send_text.to_lowercase();
    let mut failures = Vec::new();
    for required in ["leaders", "inbox"] {
        if !send_lower.contains(required) {
            failures.push(format!(
                "send leader-unavailable output must guide to `{required}` when actionable; text={send_text}"
            ));
        }
    }
    for state in ["queued", "blocked", "rebind", "refused"] {
        if send_lower.contains(state) {
            break;
        }
        if state == "refused" {
            failures.push(format!(
                "send leader-unavailable output must state an honest queued/blocked/rebind/refused outcome; text={send_text}"
            ));
        }
    }
    assert_no_fallback_command_text("send leader-unavailable output", &send_text, &mut failures);

    let diagnose = case.run_ta(&["diagnose", "--workspace", case.workspace_str(), "--json"]);
    let diagnose_text = output_text(&diagnose);
    let diagnose_lower = diagnose_text.to_lowercase();
    for required in ["claim-leader", "takeover", "attach-leader"] {
        if !diagnose_lower.contains(required) {
            failures.push(format!(
                "diagnose rebind_required output must guide to `{required}`; text={diagnose_text}"
            ));
        }
    }
    assert_no_fallback_command_text(
        "diagnose rebind_required output",
        &diagnose_text,
        &mut failures,
    );

    let report = case.run_ta(&["mcp-server", "--workspace", case.workspace_str(), "--help"]);
    let report_text = output_text(&report);
    assert_no_fallback_command_text("mcp/report_result public help", &report_text, &mut failures);

    let command_count = visible_default_commands(&stdout(&case.run_ta(&["--help"]))).len();
    if command_count > VISIBLE_COMMAND_LIMIT {
        failures.push(format!(
            "normal path must not introduce a replacement command; visible_count={command_count}"
        ));
    }

    assert!(
        failures.is_empty(),
        "RED3: send/report_result replacement paths must return honest queued/blocked/rebind state with visible recovery anchors and no fallback command names.\n{}",
        failures.join("\n")
    );
}

#[test]
fn red4_collect_replaces_settle() {
    let case = Case::new("red4-collect");
    case.seed_collect_workspace();

    let collect = case.run_ta(&["collect", "--workspace", case.workspace_str(), "--json"]);
    let collect_json = json_stdout("collect --json", &collect);
    let status = case.run_ta(&["status", "--workspace", case.workspace_str(), "--json"]);
    let status_text = output_text(&status);
    let settle = case.run_ta(&["settle", "--workspace", case.workspace_str(), "--json"]);
    let mut failures = Vec::new();

    if collect_json
        .get("collected_results")
        .and_then(Value::as_array)
        .is_none()
    {
        failures.push(format!(
            "collect --json must expose collected_results; output={collect_json}"
        ));
    }
    if collect_json.get("results").is_none() {
        failures.push(format!(
            "collect --json must expose result counts; output={collect_json}"
        ));
    }
    if !status.status.success() || !status_text.contains("worker") {
        failures.push(format!(
            "status --json must remain the observation half that replaces settle; status={} text={status_text}",
            status.status
        ));
    }
    if settle.status.success() || output_text(&settle).contains("details_log") {
        failures.push(format!(
            "`settle` must be removed; collect/status are the normal path. status={} text={}",
            settle.status,
            output_text(&settle)
        ));
    }
    let settle_logs = glob_log_names(case.workspace(), "settle-");
    if !settle_logs.is_empty() {
        failures.push(format!(
            "normal collect/status success must not require settle-*.json artifacts; found={settle_logs:?}"
        ));
    }

    assert!(
        failures.is_empty(),
        "RED4: C2 replaces settle with collect + status and removes the settle command/artifact contract.\n{}",
        failures.join("\n")
    );
}

#[test]
fn red5_result_validation_is_in_normal_ingestion() {
    let case = Case::new("red5-ingestion");
    case.seed_collect_workspace();
    let mut failures = Vec::new();

    let spec = read_repo_file("crates/team-agent/src/cli/spec.rs");
    let emit = read_repo_file("crates/team-agent/src/cli/emit.rs");
    if spec_block_for(&spec, "validate-result").is_some()
        || emit_const_contains(&emit, "DISPATCH_COMMANDS", "validate-result")
        || dispatch_contains_command_arm(&emit, "validate-result")
    {
        failures.push(
            "`validate-result` must be removed from CommandSpec, dispatch, and command lists"
                .to_string(),
        );
    }
    let validate = case.run_ta(&["validate-result", "--help"]);
    if validate.status.success()
        || output_text(&validate).contains("usage: team-agent validate-result")
    {
        failures.push(format!(
            "`validate-result` must be removed; status={} text={}",
            validate.status,
            output_text(&validate)
        ));
    }

    case.insert_result_row(
        "res_c2_invalid",
        "task_c2",
        "worker",
        json!({
            "schema_version": "result_envelope_v1",
            "result_id": "res_c2_invalid",
            "task_id": "task_c2",
            "agent_id": "worker",
            "summary": "missing status is invalid"
        }),
        "success",
    );
    let collect = case.run_ta(&["collect", "--workspace", case.workspace_str(), "--json"]);
    let collected = json_output("collect invalid stored row", &collect);
    if !collected["invalid_results"].as_array().is_some_and(|rows| {
        rows.iter()
            .any(|row| row["result_id"] == json!("res_c2_invalid"))
    }) {
        failures.push(format!(
            "collect must surface invalid stored envelopes through invalid_results; output={collected}"
        ));
    }

    case.insert_result_row(
        "res_c2_valid",
        "task_c2",
        "worker",
        json!({
            "schema_version": "result_envelope_v1",
            "result_id": "res_c2_valid",
            "task_id": "task_c2",
            "agent_id": "worker",
            "status": "success",
            "summary": "valid result still collects",
            "changes": [],
            "tests": [{"command": "cargo test", "status": "passed"}],
            "risks": [],
            "artifacts": [],
            "next_actions": []
        }),
        "success",
    );
    let collect_valid = case.run_ta(&["collect", "--workspace", case.workspace_str(), "--json"]);
    let valid = json_stdout("collect valid stored row", &collect_valid);
    if !valid["collected_results"].as_array().is_some_and(|rows| {
        rows.iter()
            .any(|row| row["result_id"] == json!("res_c2_valid"))
    }) {
        failures.push(format!(
            "valid envelopes must still collect through the normal result path; output={valid}"
        ));
    }

    assert!(
        failures.is_empty(),
        "RED5: result validation must live in report_result/collect ingestion, not a standalone validate-result CLI.\n{}",
        failures.join("\n")
    );
}

#[test]
fn red6_repair_state_daily_path_removed_and_schema_hint_points_to_doctor() {
    let case = Case::new("red6-repair-state");
    case.write_schema_issue_state();
    let mut failures = Vec::new();

    let repair_help = case.run_ta(&["repair-state", "--help"]);
    if repair_help.status.success()
        || output_text(&repair_help).contains("usage: team-agent repair-state")
    {
        failures.push(format!(
            "`repair-state` daily task/status path must be removed; status={} text={}",
            repair_help.status,
            output_text(&repair_help)
        ));
    }

    for path in [
        "crates/team-agent/src/cli/diagnose.rs",
        "crates/team-agent/src/coordinator/health.rs",
    ] {
        let text = read_repo_file(path);
        if text.contains("repair-state --schema") {
            failures.push(format!(
                "{path} still suggests nonexistent schema repair via `repair-state --schema`"
            ));
        }
        if !text.contains("doctor --fix-schema") {
            failures.push(format!(
                "{path} must redirect schema repair hints to `team-agent doctor --fix-schema --json`"
            ));
        }
    }

    for (label, output) in [
        (
            "status",
            case.run_ta(&["status", "--workspace", case.workspace_str(), "--json"]),
        ),
        (
            "collect",
            case.run_ta(&["collect", "--workspace", case.workspace_str(), "--json"]),
        ),
        (
            "diagnose",
            case.run_ta(&["diagnose", "--workspace", case.workspace_str(), "--json"]),
        ),
    ] {
        let text = output_text(&output);
        if text.contains("repair-state --task") || text.contains("repair-state --status") {
            failures.push(format!(
                "{label} output suggests daily repair-state path; text={text}"
            ));
        }
    }

    assert!(
        failures.is_empty(),
        "RED6: C2 removes repair-state --task/--status from daily operations and schema repair guidance must point to doctor --fix-schema.\n{}",
        failures.join("\n")
    );
}

#[test]
fn red7_g0_visible_command_metric_is_hard_fail() {
    let case = Case::new("red7-g0");
    let help = stdout(&case.run_ta(&["--help"]));
    let visible = visible_default_commands(&help);
    let source = read_repo_file("crates/team-agent/tests/governance_g0_metrics.rs");
    let mut failures = Vec::new();

    if visible.len() > VISIBLE_COMMAND_LIMIT {
        failures.push(format!(
            "visible command count must hard-fail above {VISIBLE_COMMAND_LIMIT}; visible={visible:?}"
        ));
    }
    for forbidden in [
        "report_only=true",
        "report-only",
        "does not hard-fail",
        "warning",
    ] {
        if source.to_lowercase().contains(&forbidden.to_lowercase()) {
            failures.push(format!(
                "G0 visible-command metric must no longer use report-only wording `{forbidden}` after C1 acceptance"
            ));
        }
    }
    if !(source.contains("assert!") && source.contains("VISIBLE_COMMAND_TARGET")) {
        failures.push(
            "G0 visible command metric must assert commands.len() <= VISIBLE_COMMAND_TARGET"
                .to_string(),
        );
    }

    assert!(
        failures.is_empty(),
        "RED7: G0 visible-command metric is a hard governance gate in C2, not report-only text.\n{}",
        failures.join("\n")
    );
}

#[test]
fn red8_net_negative_loc_verdict_gate_documented() {
    let source =
        read_repo_file("crates/team-agent/tests/c2_command_internalization_deletion_contract.rs");
    for required in [
        "C2_VERDICT_REQUIRES_SHORTSTAT",
        "C2_VERDICT_REQUIRES_NET_NEGATIVE_PRODUCT_LOC",
        "C2_VERDICT_TARGET_MINUS_400_LOC",
        "C2_VERDICT_REQUIRES_ZERO_NEW_COMMANDS",
    ] {
        assert!(
            source.contains(required),
            "RED8: C2 verdict-layer net LOC gate marker `{required}` must stay documented in this contract"
        );
    }
}

// RED8 verdict-layer markers:
// C2_VERDICT_REQUIRES_SHORTSTAT: implementation report must attach `git diff --shortstat`.
// C2_VERDICT_REQUIRES_NET_NEGATIVE_PRODUCT_LOC: product code net LOC must be negative.
// C2_VERDICT_TARGET_MINUS_400_LOC: target is at least -400 LOC, stretch target -800 LOC.
// C2_VERDICT_REQUIRES_ZERO_NEW_COMMANDS:新增命令数 must be 0.

struct Case {
    root: PathBuf,
    home: PathBuf,
    workspace: PathBuf,
    workspace_str: String,
}

impl Case {
    fn new(tag: &str) -> Self {
        let base = std::env::var_os("TEAM_AGENT_TEST_TMP")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        fs::create_dir_all(&base).expect("create TEAM_AGENT_TEST_TMP base");
        let root = base.join(format!(
            "ta-c2-{tag}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("create c2 case root");
        let root = fs::canonicalize(root).expect("canonical c2 root");
        let home = root.join("home");
        let workspace = root.join("workspace");
        fs::create_dir_all(home.join(".team-agent/leaders")).expect("create home registry");
        fs::create_dir_all(&workspace).expect("create workspace");
        let workspace = fs::canonicalize(workspace).expect("canonical workspace");
        let workspace_str = workspace.to_string_lossy().to_string();
        Self {
            root,
            home,
            workspace,
            workspace_str,
        }
    }

    fn workspace(&self) -> &Path {
        &self.workspace
    }

    fn workspace_str(&self) -> &str {
        &self.workspace_str
    }

    fn run_ta(&self, args: &[&str]) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_team-agent"));
        command
            .args(args)
            .env("HOME", &self.home)
            .current_dir(&self.workspace);
        let _ = std::env::var_os("TEAM_AGENT_TEST_TMP");
        for key in [
            "TMUX",
            "TMUX_PANE",
            "TEAM_AGENT_LEADER_PANE_ID",
            "TEAM_AGENT_LEADER_SESSION_UUID",
            "TEAM_AGENT_LEADER_SESSION_UUID_OVERRIDE",
            "TEAM_AGENT_LEADER_PROVIDER",
            "TEAM_AGENT_MACHINE_FINGERPRINT",
            "TEAM_AGENT_WORKSPACE",
            "TEAM_AGENT_TEAM_ID",
            "TEAM_AGENT_OWNER_TEAM_ID",
            "TEAM_AGENT_ACTIVE_TEAM",
            "TEAM_AGENT_ID",
            "TEAM_AGENT_AGENT_ID",
        ] {
            command.env_remove(key);
        }
        command.output().expect("run team-agent")
    }

    fn seed_collect_workspace(&self) {
        let team_dir = self.workspace.join(".team/runtime/teams/current");
        fs::create_dir_all(team_dir.join("agents")).expect("create team spec dirs");
        fs::write(
            team_dir.join("team.spec.yaml"),
            "team:\n  name: current\n  objective: C2 collect fixture.\nleader:\n  provider: codex\nagents:\n  - id: worker\n    provider: codex\n    role: Worker\n",
        )
        .expect("write team spec");
        fs::write(
            team_dir.join("agents/worker.md"),
            "---\nname: worker\nrole: Worker\nprovider: codex\n---\n\nWorker.\n",
        )
        .expect("write role");
        let state = json!({
            "active_team_key": "current",
            "team_key": "current",
            "teams": {
                "current": {
                    "team_key": "current",
                    "workspace": self.workspace,
                    "team_dir": team_dir,
                    "spec_path": team_dir.join("team.spec.yaml"),
                    "session_name": "team-c2-collect",
                    "agents": {
                        "worker": {
                            "agent_id": "worker",
                            "status": "stopped",
                            "provider": "codex",
                            "window": "worker"
                        }
                    },
                    "tasks": [{
                        "id": "task_c2",
                        "title": "C2 collect task",
                        "assignee": "worker",
                        "status": "pending"
                    }]
                }
            },
            "workspace": self.workspace,
            "team_dir": team_dir,
            "spec_path": team_dir.join("team.spec.yaml"),
            "session_name": "team-c2-collect",
            "agents": {
                "worker": {
                    "agent_id": "worker",
                    "status": "stopped",
                    "provider": "codex",
                    "window": "worker"
                }
            },
            "tasks": [{
                "id": "task_c2",
                "title": "C2 collect task",
                "assignee": "worker",
                "status": "pending"
            }]
        });
        save_runtime_state(&self.workspace, &state).expect("seed collect state");
        let _ = MessageStore::open(&self.workspace).expect("open message store");
    }

    fn write_rebind_required_state(&self) {
        let state = json!({
            "active_team_key": "current",
            "team_key": "current",
            "teams": {
                "current": {
                    "team_key": "current",
                    "workspace": self.workspace,
                    "team_dir": self.workspace.join(".team/runtime/teams/current"),
                    "session_name": "team-c2-rebind",
                    "agents": {}
                }
            },
            "leader_receiver": {
                "status": "missing"
            }
        });
        save_runtime_state(&self.workspace, &state).expect("write rebind state");
        let _ = MessageStore::open(&self.workspace).expect("open message store");
    }

    fn write_schema_issue_state(&self) {
        self.seed_collect_workspace();
        let mut state = load_runtime_state(&self.workspace).expect("load seeded state");
        state["coordinator"] = json!({
            "status": "unhealthy",
            "metadata_mismatch_reason": "schema_version_mismatch"
        });
        save_runtime_state(&self.workspace, &state).expect("write schema issue state");
    }

    fn insert_result_row(
        &self,
        result_id: &str,
        task_id: &str,
        agent_id: &str,
        envelope: Value,
        status: &str,
    ) {
        let store = MessageStore::open(&self.workspace).expect("open message store");
        let conn = team_agent::db::schema::open_db(store.db_path()).expect("open team.db");
        conn.execute(
            "insert into results(result_id, owner_team_id, task_id, agent_id, envelope, status, created_at)
             values (?1, 'current', ?2, ?3, ?4, ?5, ?6)",
            params![
                result_id,
                task_id,
                agent_id,
                envelope.to_string(),
                status,
                "2026-07-12T00:00:00Z"
            ],
        )
        .expect("insert result row");
    }
}

impl Drop for Case {
    fn drop(&mut self) {
        if std::env::var("TEAM_AGENT_KEEP_TEST_TMP").as_deref() != Ok("1") {
            let _ = fs::remove_dir_all(&self.root);
        }
    }
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("team-agent crate should live under crates/team-agent")
        .to_path_buf()
}

fn read_repo_file(path: &str) -> String {
    fs::read_to_string(repo_root().join(path))
        .unwrap_or_else(|error| panic!("read {path}: {error}"))
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn output_text(output: &Output) -> String {
    format!("{}{}", stdout(output), stderr(output))
}

fn json_stdout(label: &str, output: &Output) -> Value {
    assert!(
        output.status.success(),
        "{label} must exit 0 before JSON assertion; status={} stdout={} stderr={}",
        output.status,
        stdout(output),
        stderr(output)
    );
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "{label} stdout must be JSON: {error}; stdout={} stderr={}",
            stdout(output),
            stderr(output)
        )
    })
}

fn json_output(label: &str, output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "{label} stdout must be JSON even when the command exits nonzero: {error}; status={} stdout={} stderr={}",
            output.status,
            stdout(output),
            stderr(output)
        )
    })
}

fn visible_default_commands(help: &str) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    if let Some((_, rest)) = help.split_once("Commands:") {
        let section = rest.split("\n\n").next().unwrap_or(rest);
        for part in section.split(',') {
            let command = part.trim();
            if is_command_name(command) {
                names.insert(command.to_string());
            }
        }
    } else {
        for line in help.lines() {
            let trimmed = line.trim_start();
            if !line.starts_with("  ") || trimmed.starts_with("team-agent ") {
                continue;
            }
            let command = trimmed.split_whitespace().next().unwrap_or_default();
            if is_command_name(command) {
                names.insert(command.to_string());
            }
        }
    }
    for provider in ["codex", "claude", "copilot"] {
        names.remove(provider);
    }
    names
}

fn is_command_name(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-')
}

fn spec_block_for(spec: &str, command: &str) -> Option<String> {
    let needle = format!("\"{command}\"");
    let index = spec.find(&needle)?;
    let start = spec[..index]
        .rfind("CommandSpec")
        .unwrap_or(index.saturating_sub(400));
    let end = spec[index + needle.len()..]
        .find("CommandSpec")
        .map(|offset| index + needle.len() + offset)
        .unwrap_or_else(|| spec.len().min(index + 1200));
    Some(spec[start..end].to_string())
}

fn emit_const_contains(emit: &str, const_name: &str, command: &str) -> bool {
    parse_const_str_array(emit, const_name)
        .iter()
        .any(|name| name == command)
}

fn parse_const_str_array(source: &str, const_name: &str) -> Vec<String> {
    let marker = format!("const {const_name}:");
    let Some(start) = source.find(&marker) else {
        return Vec::new();
    };
    let after = &source[start..];
    let Some(array_start) = after.find("&[") else {
        return Vec::new();
    };
    let after_array = &after[array_start..];
    let Some(end) = after_array.find("];") else {
        return Vec::new();
    };
    let array = &after_array[..end];
    let mut values = Vec::new();
    let mut parts = array.split('"');
    let _ = parts.next();
    while let Some(value) = parts.next() {
        values.push(value.to_string());
        let _ = parts.next();
    }
    values
}

fn dispatch_contains_command_arm(emit: &str, command: &str) -> bool {
    let Some(start) = emit.find("pub(crate) fn dispatch") else {
        return false;
    };
    let end = emit[start..]
        .find("const DISPATCH_COMMANDS")
        .map(|offset| start + offset)
        .unwrap_or(emit.len());
    emit[start..end].contains(&format!("\"{command}\""))
}

fn packaged_worker_reference_files() -> Vec<String> {
    let mut files = Vec::new();
    for root in [
        "skills/team-agent",
        "crates/team-agent/src/lifecycle",
        "crates/team-agent/src/mcp_server",
    ] {
        collect_files(&repo_root().join(root), &mut files);
    }
    files
        .into_iter()
        .filter_map(|path| {
            let rel = path
                .strip_prefix(repo_root())
                .ok()?
                .to_string_lossy()
                .to_string();
            if rel.contains(".team/artifacts") || rel.contains(".team/evidence") {
                None
            } else {
                Some(rel)
            }
        })
        .collect()
}

fn collect_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        if path.is_dir() {
            collect_files(&path, out);
        } else if matches!(
            path.extension().and_then(|ext| ext.to_str()),
            Some("rs" | "md" | "toml" | "json" | "yaml" | "yml")
        ) {
            out.push(path);
        }
    }
}

fn assert_no_fallback_command_text(label: &str, text: &str, failures: &mut Vec<String>) {
    for command in FALLBACK_COMMANDS {
        if text.contains(command) {
            failures.push(format!("{label} mentions deleted `{command}`; text={text}"));
        }
    }
}

fn glob_log_names(workspace: &Path, prefix: &str) -> Vec<String> {
    let logs = workspace.join(".team/logs");
    let Ok(entries) = fs::read_dir(logs) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().to_string();
            name.starts_with(prefix).then_some(name)
        })
        .collect()
}

fn normalize(value: &str) -> String {
    value
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}
