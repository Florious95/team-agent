//! 0.5.61 gate-coverage-hole RED: the documented fresh-team command and first
//! message/result loop belong to the existing CLI E2E hard smoke.
//!
//! Requirement anchors:
//! - `skills/team-agent/SKILL.md` "Minimal Copy-Paste Team" and "Commands"
//! - F1 one-entry startup / stable team identity
//! - F4 end-to-end delivery truth and unique recipient
//! - F10 requirement-to-RED and anti-vacuous controls
//!
//! Reanchor:
//! - `collect` must contain both the spawned fake-worker's original message-scoped result and
//!   the independent stdio MCP supplemental result; the latter cannot mask loss of the former.
//! - command coverage is an honest A-covered / B-declared-gap / C-last-resort-exemption catalog.
//!   A requires exact documented argv plus an assertion bound to that invocation. Provider
//!   launchers may graduate to A only through a per-call hermetic PATH shim with exact argv-log
//!   and CLI exit/output evidence; invoke-and-ignore and unshimmed launchers are forbidden.

use std::collections::BTreeSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use crate::framework::*;
use crate::support::source_walker::source_tree;
use rusqlite::Connection;
use serde_json::Value;

const LNCH_CASE: &str = "lnch_001_quick_start_basic";
const SEND_CASE: &str = "send_001_delivers_to_fake_worker";
const COVERAGE_MANIFEST: &str = "skills/team-agent/command-coverage.json";

#[test]
fn tooth_1_existing_launch_smoke_runs_documented_quick_start_verbatim() {
    assert_documented_argv_canary();

    let launch = source_tree(&["tests/e2e/cases/lnch_001_quick_start_basic.rs"]);
    assert!(
        launch.contains(&format!("fn {LNCH_CASE}(")),
        "TOOTH-1 RED: existing hard-smoke test `{LNCH_CASE}` is missing"
    );
    let compact = launch.split_whitespace().collect::<String>();
    assert!(
        compact.contains(r#"["quick-start",".team/current"]"#),
        "TOOTH-1 RED: `{LNCH_CASE}` never executes the SKILL.md argv verbatim: \
         `team-agent quick-start .team/current` (no --workspace/--team-id)."
    );
    assert!(
        !compact.contains("quick_start_fake("),
        "TOOTH-1 RED: `{LNCH_CASE}` still routes through the convenience helper that adds \
         --workspace/--team-id instead of the documented public argv."
    );

    // Independent assembly canary: after the existing hard smoke is repaired,
    // this contract executes the same real CLI process and verifies the
    // project-root runtime, canonical identity, and worker addressing.
    let ws = documented_fake_team("gate061-doc", "coder");
    let out = run_ta(&ws, &["quick-start", ".team/current"]);
    assert_eq!(
        out.argv,
        vec!["team-agent", "quick-start", ".team/current"],
        "TOOTH-1: runner changed the documented argv"
    );
    assert!(
        out.stdout.contains("status: leader_receiver_unbound")
            && out.stdout.contains("\"all_workers_spawned\": true"),
        "TOOTH-1: hermetic documented quick-start must reach the expected post-spawn \
         leader-binding boundary; exit={} stdout={} stderr={}",
        out.exit_code,
        out.stdout,
        out.stderr
    );

    assert_file_exists(&ws.state_json_path());
    assert_file_absent(&ws.path().join(".team/current/.team/runtime/state.json"));
    let state = ws.read_state();
    let team_key = state["active_team_key"]
        .as_str()
        .expect("TOOTH-1: active_team_key must be a string");
    assert!(
        team_key != "current" && team_key != ".team/current",
        "TOOTH-1: canonical team identity must come from TEAM.md, not the directory literal; \
         active_team_key={team_key:?}"
    );
    assert_eq!(
        state["session_name"],
        Value::String(format!("team-{team_key}")),
        "TOOTH-1: session identity must be derived from the one canonical team key"
    );
    let teams = state["teams"]
        .as_object()
        .expect("TOOTH-1: teams projection must be an object");
    assert_eq!(
        teams.keys().collect::<Vec<_>>(),
        vec![&team_key.to_string()],
        "TOOTH-1: documented launch must create exactly one canonical team identity"
    );
    assert_file_exists(
        &ws.path()
            .join(".team/runtime")
            .join(team_key)
            .join("team.spec.yaml"),
    );

    let canary = format!("gate061-doc-address-{}", std::process::id());
    let send = run_ta(
        &ws,
        &[
            "send",
            "coder",
            &canary,
            "--workspace",
            ws.path().to_str().expect("workspace utf8"),
            "--json",
        ],
    );
    assert!(
        send.is_success(),
        "TOOTH-1: documented team could not address coder; stdout={} stderr={}",
        send.stdout,
        send.stderr
    );
    let message_id = send.json()["message_id"]
        .as_str()
        .expect("TOOTH-1: send must return message_id")
        .to_string();
    wait_for_or_panic(
        "documented-path message delivered to coder",
        || {
            message_truth(ws.path(), &message_id)
                .is_some_and(|truth| truth.recipient == "coder" && truth.delivered())
        },
        Duration::from_secs(10),
    );

    shutdown(&ws);
}

#[test]
fn tooth_2_existing_send_smoke_proves_worker_receive_report_and_collect() {
    assert_worker_truth_negative_canary();
    assert_original_result_collect_negative_canary();

    let code = source_tree(&["tests/e2e/cases/send_001_fake_worker.rs"]);
    assert!(
        code.contains(&format!("fn {SEND_CASE}(")),
        "TOOTH-2 RED: existing send hard-smoke test `{SEND_CASE}` is missing"
    );
    for required in [
        "recipient",
        "delivered_at",
        "report_result",
        "\"collect\"",
        "collected_results",
        "result_id",
        "task_id",
        "agent_id",
        "\"scope\"",
        "Fake worker handled message",
    ] {
        assert!(
            code.contains(required),
            "TOOTH-2 RED: `{SEND_CASE}` still stops at accepted/DB existence; missing \
             worker-receive→report_result→collect evidence token {required:?}. Delivery truth \
             must use recipient + delivered fields, not a presentation view."
        );
    }

    // Independent real-CLI closure canary. The built-in fake worker is a real
    // spawned `team-agent fake-worker` process: it parses the injected token,
    // reports a result, and the leader consumes it through the public collect
    // command.
    let ws = TestWorkspace::new("gate061-send").with_fake_spec(&["a"]);
    let qs = quick_start_fake(&ws, "gate061-send");
    assert!(
        quick_start_launched(&qs),
        "TOOTH-2 setup quick-start failed: stdout={} stderr={}",
        qs.stdout,
        qs.stderr
    );
    let canary = format!("gate061-worker-canary-{}", std::process::id());
    let send = run_ta(
        &ws,
        &[
            "send",
            "a",
            &canary,
            "--workspace",
            ws.path().to_str().expect("workspace utf8"),
            "--json",
        ],
    );
    assert!(
        send.is_success(),
        "TOOTH-2 send failed: stdout={} stderr={}",
        send.stdout,
        send.stderr
    );
    let send_json = send.json();
    let message_id = send_json["message_id"]
        .as_str()
        .expect("TOOTH-2 send must return message_id")
        .to_string();
    let fake_worker_summary = format!("Fake worker handled message {message_id}");

    wait_for_or_panic(
        "message row recipient=a with delivered truth",
        || {
            message_truth(ws.path(), &message_id)
                .is_some_and(|truth| truth.recipient == "a" && truth.delivered())
        },
        Duration::from_secs(10),
    );
    wait_for_or_panic(
        "fake worker received message token and reported a result",
        || {
            result_truth_for_message(ws.path(), &message_id).is_some_and(|truth| {
                truth.task_id == message_id
                    && truth.agent_id == "a"
                    && truth.summary == fake_worker_summary
            })
        },
        Duration::from_secs(10),
    );
    let fake_worker_result = result_truth_for_message(ws.path(), &message_id)
        .expect("TOOTH-2 spawned fake-worker result must remain queryable before collect");
    let owner_team_id = ws.read_state()["active_team_key"]
        .as_str()
        .expect("TOOTH-2 active team key")
        .to_string();
    let report_summary = format!("worker a MCP received {message_id}: {canary}");
    run_worker_mcp_report_result(ws.path(), "a", &owner_team_id, &report_summary);

    let collect = run_ta(
        &ws,
        &[
            "collect",
            "--workspace",
            ws.path().to_str().expect("workspace utf8"),
            "--json",
        ],
    );
    assert!(
        collect.is_success(),
        "TOOTH-2 collect failed: stdout={} stderr={}",
        collect.stdout,
        collect.stderr
    );
    let collected = collect.json();
    let rows = collected["collected_results"]
        .as_array()
        .expect("TOOTH-2 collect must expose collected_results");
    assert!(
        collected_rows_include(rows, &fake_worker_result, "message"),
        "TOOTH-2 RED: collect omitted the spawned fake-worker result or changed its \
         result_id/task_id/agent_id/scope/exact summary; expected={fake_worker_result:?} \
         output={collected}"
    );
    assert!(
        rows.iter().any(|row| {
            row["agent_id"] == Value::String("a".to_string())
                && row["summary"]
                    .as_str()
                    .is_some_and(|summary| summary == report_summary)
        }),
        "TOOTH-2: collect did not return the result produced after worker `a` received \
         message_id={message_id}; output={collected}"
    );

    shutdown(&ws);
}

#[test]
fn tooth_3a_every_skill_command_is_recorded_losslessly() {
    assert_command_extractor_canary();
    assert_coverage_closed_world_canary();

    let skill = std::fs::read_to_string(repo_root().join("skills/team-agent/SKILL.md"))
        .expect("read product Team Agent SKILL.md");
    let documented = extract_team_agent_commands(&skill);
    assert!(
        documented.contains("team-agent quick-start .team/current"),
        "TOOTH-3 harness canary: extractor missed the canonical quick-start command"
    );

    let manifest = load_coverage_manifest("TOOTH-3A");
    let listed = unique_manifest_commands(&manifest)
        .unwrap_or_else(|failure| panic!("TOOTH-3A EXACTLY-ONE-BUCKET RED: {failure}"));
    if let Some(drift) = command_set_drift(&documented, &listed) {
        panic!(
            "TOOTH-3A UNASSIGNED-COMMAND RED: SKILL.md executable commands and the \
             three-bucket coverage manifest drifted; commands must be recorded byte-for-byte \
             after whitespace normalization, including a legal final argv token `.`; {drift}"
        );
    }
}

#[test]
fn tooth_3b_three_bucket_claims_are_honest_and_launcher_safe() {
    assert_three_bucket_validator_canary();

    let manifest = load_coverage_manifest("TOOTH-3B");
    let e2e_tests = source_tree(&["tests/e2e/main.rs", "tests/e2e/cases"]);
    validate_bucket_fields(&manifest)
        .and_then(|_| validate_covered_case_registration(&manifest))
        .and_then(|_| validate_covered_evidence(&manifest, &e2e_tests))
        .and_then(|_| validate_no_unshimmed_launcher_calls(&manifest, &e2e_tests))
        .unwrap_or_else(|failure| panic!("{failure}"));
}

#[derive(Debug)]
struct MessageTruth {
    recipient: String,
    status: String,
    delivered_at: Option<String>,
}

#[derive(Debug)]
struct ResultTruth {
    result_id: String,
    task_id: String,
    agent_id: String,
    summary: String,
}

impl MessageTruth {
    fn delivered(&self) -> bool {
        self.status == "delivered" && self.delivered_at.is_some()
    }
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct CoverageManifest {
    schema_version: String,
    commands: Vec<CoverageEntry>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(tag = "bucket", rename_all = "snake_case", deny_unknown_fields)]
enum CoverageEntry {
    Covered {
        command: String,
        #[serde(default)]
        cases: Vec<String>,
        #[serde(default)]
        launcher_shim_evidence: Option<LauncherShimEvidence>,
    },
    DeclaredGap {
        command: String,
        #[serde(default)]
        covered: Option<bool>,
        #[serde(default)]
        owner: String,
        #[serde(default)]
        plan: String,
    },
    Exempt {
        command: String,
        #[serde(default)]
        category: String,
        #[serde(default)]
        reason: String,
        #[serde(default)]
        owner: String,
        #[serde(default)]
        shim_or_isolation_infeasible: Option<bool>,
    },
}

impl CoverageEntry {
    fn command(&self) -> &str {
        match self {
            Self::Covered { command, .. }
            | Self::DeclaredGap { command, .. }
            | Self::Exempt { command, .. } => command,
        }
    }
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct LauncherShimEvidence {
    case: String,
    provider: String,
    argv_log_binding: String,
    cli_result_binding: String,
}

#[derive(Debug)]
struct RunTaCall {
    runner: String,
    binding: Option<String>,
    argv: Vec<String>,
    source: String,
}

fn documented_fake_team(tag: &str, agent_id: &str) -> TestWorkspace {
    let ws = TestWorkspace::new(tag).with_fake_spec(&[agent_id]);
    let team_dir = ws.path().join(".team/current");
    std::fs::create_dir_all(&team_dir).expect("create documented .team/current fixture");
    std::fs::rename(ws.path().join("TEAM.md"), team_dir.join("TEAM.md"))
        .expect("move TEAM.md into documented team dir");
    std::fs::rename(ws.path().join("agents"), team_dir.join("agents"))
        .expect("move agents into documented team dir");
    ws
}

fn run_worker_mcp_report_result(
    workspace: &Path,
    agent_id: &str,
    owner_team_id: &str,
    summary: &str,
) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_team-agent"))
        .arg("mcp-server")
        .arg("--workspace")
        .arg(workspace)
        .env("TEAM_AGENT_WORKSPACE", workspace)
        .env("TEAM_AGENT_ID", agent_id)
        .env("TEAM_AGENT_OWNER_TEAM_ID", owner_team_id)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("TOOTH-2 spawn real stdio MCP server");
    {
        let stdin = child.stdin.as_mut().expect("TOOTH-2 MCP stdin");
        writeln!(
            stdin,
            r#"{{"jsonrpc":"2.0","id":1,"method":"initialize","params":{{"protocolVersion":"2024-11-05"}}}}"#
        )
        .expect("TOOTH-2 write MCP initialize");
        writeln!(
            stdin,
            "{}",
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "tools/call",
                "params": {
                    "name": "report_result",
                    "arguments": {
                        "summary": summary,
                        "status": "success",
                        "tests": [{"command": "gate061-fake-worker-receipt", "status": "passed"}]
                    }
                }
            })
        )
        .expect("TOOTH-2 write MCP report_result");
    }
    let output = child
        .wait_with_output()
        .expect("TOOTH-2 wait for MCP report_result");
    assert!(
        output.status.success(),
        "TOOTH-2 MCP server failed: status={:?} stderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let response = stdout
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .find(|frame| frame["id"] == Value::from(2))
        .unwrap_or_else(|| panic!("TOOTH-2 missing report_result response; stdout={stdout}"));
    assert_ne!(
        response["result"]["isError"],
        Value::Bool(true),
        "TOOTH-2 worker MCP report_result failed: {response}"
    );
}

fn shutdown(ws: &TestWorkspace) {
    let _ = run_ta(
        ws,
        &[
            "shutdown",
            "--workspace",
            ws.path().to_str().expect("workspace utf8"),
            "--keep-logs",
            "--json",
        ],
    );
}

fn message_truth(workspace: &Path, message_id: &str) -> Option<MessageTruth> {
    let conn = Connection::open(workspace.join(".team/runtime/team.db")).ok()?;
    conn.query_row(
        "select recipient, status, delivered_at from messages where message_id = ?1",
        [message_id],
        |row| {
            Ok(MessageTruth {
                recipient: row.get(0)?,
                status: row.get(1)?,
                delivered_at: row.get(2)?,
            })
        },
    )
    .ok()
}

fn result_truth_for_message(workspace: &Path, message_id: &str) -> Option<ResultTruth> {
    let conn = Connection::open(workspace.join(".team/runtime/team.db")).ok()?;
    let mut stmt = conn
        .prepare(
            "select result_id, task_id, agent_id, envelope \
             from results order by created_at desc",
        )
        .ok()?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })
        .ok()?;
    let found = rows
        .filter_map(Result::ok)
        .find_map(|(result_id, task_id, agent_id, raw)| {
            let envelope: Value = serde_json::from_str(&raw).ok()?;
            let summary = envelope["summary"].as_str()?;
            (task_id == message_id && summary.contains(message_id)).then(|| ResultTruth {
                result_id,
                task_id,
                agent_id,
                summary: summary.to_string(),
            })
        });
    found
}

fn collected_rows_include(rows: &[Value], expected: &ResultTruth, scope: &str) -> bool {
    rows.iter().any(|row| {
        row["result_id"] == Value::String(expected.result_id.clone())
            && row["task_id"] == Value::String(expected.task_id.clone())
            && row["agent_id"] == Value::String(expected.agent_id.clone())
            && row["scope"] == Value::String(scope.to_string())
            && row["summary"] == Value::String(expected.summary.clone())
    })
}

fn worker_delivery_truth_matches(expected: &str, truth: &MessageTruth) -> bool {
    truth.recipient == expected && truth.delivered()
}

fn assert_worker_truth_negative_canary() {
    let leader_row = MessageTruth {
        recipient: "leader".to_string(),
        status: "delivered".to_string(),
        delivered_at: Some("2026-07-26T00:00:00Z".to_string()),
    };
    assert!(
        !worker_delivery_truth_matches("a", &leader_row),
        "TOOTH-2 harness canary: a delivered row addressed to leader must fail the worker-receipt tooth"
    );
}

fn assert_original_result_collect_negative_canary() {
    let original = ResultTruth {
        result_id: "res-original".to_string(),
        task_id: "msg-original".to_string(),
        agent_id: "a".to_string(),
        summary: "Fake worker handled message msg-original".to_string(),
    };
    let supplement_only = vec![serde_json::json!({
        "result_id": "res-supplement",
        "task_id": "manual",
        "agent_id": "a",
        "scope": "manual",
        "summary": "worker a MCP received msg-original: canary"
    })];
    assert!(
        !collected_rows_include(&supplement_only, &original, "message"),
        "TOOTH-2 harness canary: a supplemental stdio MCP result must not satisfy the \
         spawned fake-worker result assertion"
    );
}

fn assert_documented_argv_canary() {
    let accepts = |argv: &[&str]| argv == ["quick-start", ".team/current"];
    assert!(accepts(&["quick-start", ".team/current"]));
    assert!(!accepts(&[
        "quick-start",
        ".team/current",
        "--workspace",
        ".",
    ]));
    assert!(!accepts(&[
        "quick-start",
        ".team/current",
        "--team-id",
        "current",
    ]));
}

fn assert_command_extractor_canary() {
    let markdown = r#"
```bash
team-agent quick-start .team/current
team-agent profile show codex-default --workspace .
```
Use `team-agent status --json`; prose only.
The prose sentence team-agent verifier-prose-canary --json. is not executable Markdown.
"#;
    let commands = extract_team_agent_commands(markdown);
    assert_eq!(
        commands,
        BTreeSet::from([
            "team-agent profile show codex-default --workspace .".to_string(),
            "team-agent quick-start .team/current".to_string(),
            "team-agent status --json".to_string(),
        ]),
        "TOOTH-3A harness canary: command extraction must preserve a legal final `.` argv \
         while excluding prose outside executable Markdown"
    );
    assert_eq!(
        normalize_team_agent_command("team-agent status --json."),
        Some("team-agent status --json".to_string()),
        "TOOTH-3A harness canary: prose punctuation attached to the final argv token must be removed"
    );
}

fn assert_coverage_closed_world_canary() {
    let base = BTreeSet::from(["team-agent status --json".to_string()]);
    assert!(command_set_drift(&base, &base).is_none());

    let mut documented = base.clone();
    documented.insert("team-agent verifier-meta-canary --json".to_string());
    let drift = command_set_drift(&documented, &base)
        .expect("TOOTH-3A harness canary: an added SKILL command must make coverage drift red");
    assert!(
        drift.contains("team-agent verifier-meta-canary --json"),
        "TOOTH-3A harness canary: drift must name the uncovered SKILL command; drift={drift}"
    );

    let mut restored = base;
    restored.insert("team-agent verifier-meta-canary --json".to_string());
    assert!(
        command_set_drift(&documented, &restored).is_none(),
        "TOOTH-3A harness canary: adding the matching manifest entry must restore closure"
    );
}

fn assert_three_bucket_validator_canary() {
    let honest = CoverageManifest {
        schema_version: "team-agent-skill-command-coverage-v2".to_string(),
        commands: vec![
            CoverageEntry::Covered {
                command: "team-agent status --json".to_string(),
                cases: vec!["verifier_covered_canary".to_string()],
                launcher_shim_evidence: None,
            },
            CoverageEntry::DeclaredGap {
                command: "team-agent doctor".to_string(),
                covered: Some(false),
                owner: "runtime-owner".to_string(),
                plan: "execute the documented argv and assert its diagnostic result".to_string(),
            },
        ],
    };
    let honest_source = r#"
fn verifier_covered_canary() {
    let out = run_ta(&ws, &["status", "--json"]);
    assert!(out.is_success(), "status failed: {}", out.stderr);
}
"#;
    assert!(
        validate_bucket_fields(&honest).is_ok()
            && validate_covered_evidence(&honest, honest_source).is_ok()
            && validate_no_unshimmed_launcher_calls(&honest, honest_source).is_ok(),
        "TOOTH-3B harness canary: an honest A=1/B=1/C=0 catalog must be green"
    );

    let missing_owner = CoverageManifest {
        schema_version: honest.schema_version.clone(),
        commands: vec![CoverageEntry::DeclaredGap {
            command: "team-agent doctor".to_string(),
            covered: Some(false),
            owner: String::new(),
            plan: "assert diagnostic output".to_string(),
        }],
    };
    assert_failure_signature(validate_bucket_fields(&missing_owner), "DECLARED-GAP-OWNER");

    let invalid_exemption = CoverageManifest {
        schema_version: honest.schema_version.clone(),
        commands: vec![CoverageEntry::Exempt {
            command: "team-agent verifier-exempt".to_string(),
            category: "convenience_skip".to_string(),
            reason: "not implemented".to_string(),
            owner: "runtime-owner".to_string(),
            shim_or_isolation_infeasible: Some(true),
        }],
    };
    assert_failure_signature(
        validate_bucket_fields(&invalid_exemption),
        "EXEMPT-CATEGORY",
    );

    let discarded_source = r#"
fn verifier_covered_canary() {
    let _ = run_ta(&ws, &["status", "--json"]);
}
"#;
    assert_failure_signature(
        validate_covered_evidence(&honest, discarded_source),
        "MAPPED-DISCARDED-RUN",
    );

    let mapped_launcher_source = r#"
fn verifier_covered_canary() {
    let out = run_ta(&ws, &["status", "--json"]);
    assert!(out.is_success());
    let launcher = run_ta_env(
        &ws,
        &["claude"],
        &[("PATH", shim_path.as_str())],
    );
    assert!(launcher.is_success());
}
"#;
    assert_failure_signature(
        validate_covered_evidence(&honest, mapped_launcher_source),
        "MAPPED-LAUNCHER-ARGV",
    );

    let missing_bound_assertion = r#"
fn verifier_covered_canary() {
    let out = run_ta(&ws, &["status", "--json"]);
    assert_eq!(out.argv, vec!["team-agent", "status", "--json"]);
}
"#;
    assert_failure_signature(
        validate_covered_evidence(&honest, missing_bound_assertion),
        "COVERED-BINDING-ASSERTION",
    );

    let unshimmed_launcher = r#"
fn verifier_unmapped_launcher_canary() {
    let out = run_ta(&ws, &["codex"]);
    assert!(out.is_success());
}
"#;
    assert_failure_signature(
        validate_no_unshimmed_launcher_calls(&honest, unshimmed_launcher),
        "UNSHIMMED-LAUNCHER-EXECUTION",
    );

    let undeclared_path_launcher = r#"
fn verifier_undeclared_path_launcher_canary() {
    let out = run_ta_env(
        &ws,
        &["claude"],
        &[("PATH", shim_path.as_str())],
    );
    assert!(out.is_success());
}
"#;
    assert_failure_signature(
        validate_no_unshimmed_launcher_calls(&honest, undeclared_path_launcher),
        "UNSHIMMED-LAUNCHER-EXECUTION",
    );

    let shimmed_launcher = CoverageManifest {
        schema_version: honest.schema_version,
        commands: vec![CoverageEntry::Covered {
            command: "team-agent codex".to_string(),
            cases: vec!["verifier_shimmed_launcher_canary".to_string()],
            launcher_shim_evidence: Some(LauncherShimEvidence {
                case: "verifier_shimmed_launcher_canary".to_string(),
                provider: "codex".to_string(),
                argv_log_binding: "shim_argv".to_string(),
                cli_result_binding: "out".to_string(),
            }),
        }],
    };
    let shimmed_source = r#"
fn verifier_shimmed_launcher_canary() {
    let shim_argv = std::fs::read_to_string(&shim_log).unwrap();
    let out = run_ta_env(
        &ws,
        &["codex"],
        &[("PATH", shim_path.as_str())],
    );
    assert_eq!(shim_argv, "codex\n");
    assert!(out.is_success() && out.stdout.contains("shim"));
}
"#;
    assert!(
        validate_bucket_fields(&shimmed_launcher).is_ok()
            && validate_covered_evidence(&shimmed_launcher, shimmed_source).is_ok()
            && validate_no_unshimmed_launcher_calls(&shimmed_launcher, shimmed_source).is_ok(),
        "TOOTH-3B harness canary: a launcher entry with hermetic PATH shim argv evidence \
         and bound CLI behavior assertions must be eligible to graduate into A"
    );
}

fn assert_failure_signature(result: Result<(), String>, signature: &str) {
    let failure = result.expect_err("TOOTH-3B harness canary: invalid catalog must be red");
    assert!(
        failure.contains(signature),
        "TOOTH-3B harness canary: expected red signature {signature:?}; got {failure}"
    );
}

fn load_coverage_manifest(tooth: &str) -> CoverageManifest {
    let manifest_path = repo_root().join(COVERAGE_MANIFEST);
    let raw = std::fs::read_to_string(&manifest_path).unwrap_or_else(|_| {
        panic!(
            "{tooth} RED: machine-readable documented-command coverage manifest is missing: \
             {COVERAGE_MANIFEST}"
        )
    });
    let value: Value = serde_json::from_str(&raw)
        .unwrap_or_else(|e| panic!("{tooth} RED: invalid {COVERAGE_MANIFEST}: {e}"));
    assert_eq!(
        value["schema_version"],
        Value::String("team-agent-skill-command-coverage-v2".to_string()),
        "{tooth} THREE-BUCKET-SCHEMA RED: expected \
         team-agent-skill-command-coverage-v2"
    );
    serde_json::from_value(value).unwrap_or_else(|e| panic!("{tooth} THREE-BUCKET-SCHEMA RED: {e}"))
}

fn unique_manifest_commands(manifest: &CoverageManifest) -> Result<BTreeSet<String>, String> {
    let listed = manifest
        .commands
        .iter()
        .map(|entry| entry.command().to_string())
        .collect::<BTreeSet<_>>();
    if listed.len() != manifest.commands.len() {
        return Err(
            "a documented command occurs in more than one A/B/C entry; every command must \
             belong to exactly one bucket"
                .to_string(),
        );
    }
    Ok(listed)
}

fn validate_bucket_fields(manifest: &CoverageManifest) -> Result<(), String> {
    for entry in &manifest.commands {
        match entry {
            CoverageEntry::Covered {
                command,
                cases,
                launcher_shim_evidence,
            } => {
                if cases.is_empty() {
                    return Err(format!(
                        "TOOTH-3B COVERED-CASE RED: A entry {command:?} has no E2E case"
                    ));
                }
                if launcher_provider_from_command(command).is_some()
                    != launcher_shim_evidence.is_some()
                {
                    return Err(format!(
                        "TOOTH-3B LAUNCHER-SHIM-DECLARATION RED: A launcher entries must declare \
                         launcher_shim_evidence and non-launcher entries must not; command={command:?}"
                    ));
                }
            }
            CoverageEntry::DeclaredGap {
                command,
                covered,
                owner,
                plan,
            } => {
                if *covered != Some(false) {
                    return Err(format!(
                        "TOOTH-3B DECLARED-GAP-COVERED RED: B entry {command:?} must explicitly \
                         declare covered=false"
                    ));
                }
                if owner.trim().is_empty() {
                    return Err(format!(
                        "TOOTH-3B DECLARED-GAP-OWNER RED: B entry {command:?} has no owner"
                    ));
                }
                if plan.trim().is_empty() {
                    return Err(format!(
                        "TOOTH-3B DECLARED-GAP-PLAN RED: B entry {command:?} has no remediation plan"
                    ));
                }
            }
            CoverageEntry::Exempt {
                command,
                category,
                reason,
                owner,
                shim_or_isolation_infeasible,
            } => {
                if !matches!(
                    category.as_str(),
                    "provider_launcher_ci_forbidden" | "real_subscription_session_required"
                ) {
                    return Err(format!(
                        "TOOTH-3B EXEMPT-CATEGORY RED: C entry {command:?} uses category \
                         {category:?}, outside the leader-approved closed set"
                    ));
                }
                if reason.trim().is_empty() {
                    return Err(format!(
                        "TOOTH-3B EXEMPT-REASON RED: C entry {command:?} has no reason"
                    ));
                }
                if owner.trim().is_empty() {
                    return Err(format!(
                        "TOOTH-3B EXEMPT-OWNER RED: C entry {command:?} has no owner"
                    ));
                }
                if *shim_or_isolation_infeasible != Some(true) {
                    return Err(format!(
                        "TOOTH-3B EXEMPT-LAST-RESORT RED: C entry {command:?} must explicitly \
                         attest shim_or_isolation_infeasible=true; exemption is not a convenience \
                         substitute for a hermetic shim or isolated fixture"
                    ));
                }
            }
        }
    }
    Ok(())
}

fn validate_covered_evidence(manifest: &CoverageManifest, e2e_tests: &str) -> Result<(), String> {
    for entry in &manifest.commands {
        let CoverageEntry::Covered {
            command,
            cases,
            launcher_shim_evidence,
        } = entry
        else {
            continue;
        };
        let launcher_provider = launcher_provider_from_command(command);
        for case in cases {
            if case.starts_with("tooth_") {
                return Err(format!(
                    "TOOTH-3B COVERED-CASE RED: A entry {command:?} self-maps to verifier \
                     case {case:?}"
                ));
            }
            let body = test_function_body(e2e_tests, case).ok_or_else(|| {
                format!(
                    "TOOTH-3B COVERED-CASE RED: A entry {command:?} maps to missing/non-test \
                     E2E case {case:?}"
                )
            })?;
            let calls = literal_run_ta_calls(body);
            if let Some(call) = calls
                .iter()
                .find(|call| call.binding.as_deref() == Some("_"))
            {
                return Err(format!(
                    "TOOTH-3B MAPPED-DISCARDED-RUN RED: mapped case {case:?} contains \
                     `let _ = {}(...)` for argv {:?}; invoke-and-ignore cannot prove behavior",
                    call.runner, call.argv
                ));
            }
            let launcher_calls = calls
                .iter()
                .filter(|call| launcher_provider_from_argv(&call.argv).is_some())
                .collect::<Vec<_>>();
            if launcher_provider.is_none() && !launcher_calls.is_empty() {
                return Err(format!(
                    "TOOTH-3B MAPPED-LAUNCHER-ARGV RED: non-launcher A entry {command:?} maps \
                     to case {case:?}, which also executes provider launcher argv {:?}; launcher \
                     coverage must use its own declared hermetic-shim evidence",
                    launcher_calls
                        .iter()
                        .map(|call| &call.argv)
                        .collect::<Vec<_>>()
                ));
            }
            let matching = calls
                .iter()
                .filter(|call| documented_command_matches_argv(command, &call.argv))
                .collect::<Vec<_>>();
            if matching.is_empty() {
                return Err(format!(
                    "TOOTH-3B COVERED-EXACT-ARGV RED: A entry {command:?} case {case:?} has no \
                     token-equivalent literal run_ta invocation; observed={:?}",
                    calls.iter().map(|call| &call.argv).collect::<Vec<_>>()
                ));
            }
            if !matching.iter().any(|call| {
                call.binding.as_deref().is_some_and(|binding| {
                    binding != "_" && binding_has_behavior_assertion(body, binding)
                })
            }) {
                return Err(format!(
                    "TOOTH-3B COVERED-BINDING-ASSERTION RED: A entry {command:?} case {case:?} \
                     does not bind the matching run_ta return value to a behavior assertion; \
                     argv-only or adjacent state assertions do not prove that invocation"
                ));
            }
        }
        if let (Some(provider), Some(evidence)) =
            (launcher_provider, launcher_shim_evidence.as_ref())
        {
            validate_launcher_shim_evidence(command, &provider, cases, evidence, e2e_tests)?;
        }
    }
    Ok(())
}

fn validate_covered_case_registration(manifest: &CoverageManifest) -> Result<(), String> {
    let e2e_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/e2e");
    let main = std::fs::read_to_string(e2e_dir.join("main.rs"))
        .map_err(|error| format!("TOOTH-3B COVERED-CASE-REGISTRATION RED: {error}"))?;
    let case_files = std::fs::read_dir(e2e_dir.join("cases"))
        .map_err(|error| format!("TOOTH-3B COVERED-CASE-REGISTRATION RED: {error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("TOOTH-3B COVERED-CASE-REGISTRATION RED: {error}"))?;
    for entry in &manifest.commands {
        let CoverageEntry::Covered { command, cases, .. } = entry else {
            continue;
        };
        for case in cases {
            let signature = format!("fn {case}(");
            let mut modules = case_files
                .iter()
                .filter_map(|entry| {
                    let path = entry.path();
                    (path.extension().is_some_and(|extension| extension == "rs")
                        && std::fs::read_to_string(&path)
                            .is_ok_and(|source| source.contains(&signature)))
                    .then(|| {
                        path.file_stem()
                            .expect("Rust case file has stem")
                            .to_string_lossy()
                            .to_string()
                    })
                })
                .collect::<Vec<_>>();
            modules.sort();
            modules.dedup();
            let registered = modules
                .first()
                .is_some_and(|module| main.contains(&format!("mod {module};")));
            if modules.len() != 1 || !registered {
                return Err(format!(
                    "TOOTH-3B COVERED-CASE-REGISTRATION RED: A entry {command:?} case {case:?} \
                     must occur in exactly one `tests/e2e/cases/*.rs` module registered by \
                     tests/e2e/main.rs; observed_modules={modules:?}"
                ));
            }
        }
    }
    Ok(())
}

fn validate_launcher_shim_evidence(
    command: &str,
    provider: &str,
    cases: &[String],
    evidence: &LauncherShimEvidence,
    e2e_tests: &str,
) -> Result<(), String> {
    if evidence.provider != provider || !cases.contains(&evidence.case) {
        return Err(format!(
            "TOOTH-3B LAUNCHER-SHIM-DECLARATION RED: launcher {command:?} evidence must name \
             provider {provider:?} and one of its mapped cases; evidence={evidence:?}"
        ));
    }
    if evidence.argv_log_binding.trim().is_empty() || evidence.cli_result_binding.trim().is_empty()
    {
        return Err(format!(
            "TOOTH-3B LAUNCHER-SHIM-EVIDENCE RED: launcher {command:?} has empty evidence bindings"
        ));
    }
    let body = test_function_body(e2e_tests, &evidence.case).ok_or_else(|| {
        format!(
            "TOOTH-3B LAUNCHER-SHIM-EVIDENCE RED: missing evidence case {:?}",
            evidence.case
        )
    })?;
    let matching = literal_run_ta_calls(body)
        .into_iter()
        .filter(|call| documented_command_matches_argv(command, &call.argv))
        .collect::<Vec<_>>();
    let Some(call) = matching.iter().find(|call| {
        call.runner == "run_ta_env"
            && call.binding.as_deref() == Some(evidence.cli_result_binding.as_str())
            && compact(&call.source).contains("(\"PATH\",")
    }) else {
        return Err(format!(
            "TOOTH-3B UNSHIMMED-LAUNCHER-EXECUTION RED: launcher {command:?} must execute via \
             bound run_ta_env with a per-call PATH override; observed={matching:?}"
        ));
    };
    let body_compact = compact(body);
    let log_read_prefix = format!("let{}=", evidence.argv_log_binding);
    if !body_compact.contains(&log_read_prefix) || !body_compact.contains("read_to_string(") {
        return Err(format!(
            "TOOTH-3B LAUNCHER-SHIM-ARGV RED: launcher {command:?} does not read the hermetic \
             shim argv log into binding {:?}",
            evidence.argv_log_binding
        ));
    }
    let assertions = assertion_macros(body);
    let expected = shell_tokens(command).unwrap_or_default();
    let provider_argv = expected.into_iter().skip(1).collect::<Vec<_>>();
    let exact_log_asserted = assertions.iter().any(|(kind, assertion)| {
        *kind == "assert_eq!"
            && identifier_occurs(assertion, &evidence.argv_log_binding)
            && provider_argv.iter().all(|token| assertion.contains(token))
    });
    if !exact_log_asserted {
        return Err(format!(
            "TOOTH-3B LAUNCHER-SHIM-ARGV RED: launcher {command:?} must use assert_eq! on shim \
             log binding {:?} and name the full provider argv {:?}",
            evidence.argv_log_binding, provider_argv
        ));
    }
    if !binding_has_behavior_assertion(body, &evidence.cli_result_binding) {
        return Err(format!(
            "TOOTH-3B LAUNCHER-SHIM-CLI-RESULT RED: launcher {command:?} must assert exit/output \
             from CLI result binding {:?}",
            evidence.cli_result_binding
        ));
    }
    debug_assert_eq!(
        call.binding.as_deref(),
        Some(evidence.cli_result_binding.as_str())
    );
    Ok(())
}

fn validate_no_unshimmed_launcher_calls(
    manifest: &CoverageManifest,
    e2e_tests: &str,
) -> Result<(), String> {
    let launcher_calls = literal_run_ta_calls(e2e_tests)
        .into_iter()
        .filter(|call| launcher_provider_from_argv(&call.argv).is_some())
        .collect::<Vec<_>>();
    for call in &launcher_calls {
        if call.runner != "run_ta_env" || !compact(&call.source).contains("(\"PATH\",") {
            return Err(format!(
                "TOOTH-3B UNSHIMMED-LAUNCHER-EXECUTION RED: E2E source executes provider \
                 launcher argv {:?} via {} without an inline per-call hermetic PATH shim",
                call.argv, call.runner
            ));
        }
    }
    let mut authorized = 0usize;
    for entry in &manifest.commands {
        let CoverageEntry::Covered {
            command,
            launcher_shim_evidence: Some(evidence),
            ..
        } = entry
        else {
            continue;
        };
        let Some(body) = test_function_body(e2e_tests, &evidence.case) else {
            continue;
        };
        authorized += literal_run_ta_calls(body)
            .iter()
            .filter(|call| {
                call.runner == "run_ta_env"
                    && compact(&call.source).contains("(\"PATH\",")
                    && documented_command_matches_argv(command, &call.argv)
            })
            .count();
    }
    if launcher_calls.len() != authorized {
        return Err(format!(
            "TOOTH-3B UNSHIMMED-LAUNCHER-EXECUTION RED: observed {} launcher execution(s) but \
             only {authorized} are named by A-bucket hermetic launcher_shim_evidence; every \
             E2E launcher execution must be declared and machine-checked",
            launcher_calls.len()
        ));
    }
    Ok(())
}

fn command_set_drift(documented: &BTreeSet<String>, listed: &BTreeSet<String>) -> Option<String> {
    if documented == listed {
        return None;
    }
    let missing = documented.difference(listed).cloned().collect::<Vec<_>>();
    let stale = listed.difference(documented).cloned().collect::<Vec<_>>();
    Some(format!("missing={missing:?} stale={stale:?}"))
}

fn extract_team_agent_commands(markdown: &str) -> BTreeSet<String> {
    let mut commands = BTreeSet::new();
    let mut in_bash = false;
    for line in markdown.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("```") {
            if in_bash {
                in_bash = false;
            } else {
                in_bash = trimmed == "```bash" || trimmed == "```sh" || trimmed == "```shell";
            }
            continue;
        }
        if in_bash {
            if let Some(command) = normalize_team_agent_command(trimmed) {
                commands.insert(command);
            }
        }
        let mut rest = line;
        while let Some(open) = rest.find('`') {
            rest = &rest[open + 1..];
            let Some(close) = rest.find('`') else {
                break;
            };
            if let Some(command) = normalize_team_agent_command(&rest[..close]) {
                commands.insert(command);
            }
            rest = &rest[close + 1..];
        }
    }
    commands
}

fn normalize_team_agent_command(raw: &str) -> Option<String> {
    let raw = raw.trim();
    let start = raw.find("team-agent ")?;
    let command = raw[start..].trim();
    let command = if command.ends_with(" .") {
        command
    } else {
        command.trim_end_matches(|c: char| matches!(c, '.' | ',' | ';' | ':'))
    };
    let command = command.split_whitespace().collect::<Vec<_>>().join(" ");
    Some(command)
}

fn documented_command_matches_argv(command: &str, actual: &[String]) -> bool {
    let Some(mut expected) = shell_tokens(command) else {
        return false;
    };
    if expected.first().is_none_or(|token| token != "team-agent") {
        return false;
    }
    expected.remove(0);
    expected.len() == actual.len()
        && expected.iter().zip(actual).all(|(expected, actual)| {
            if expected.starts_with('<') && expected.ends_with('>') {
                !actual.is_empty()
            } else if expected.starts_with('[') && expected.ends_with(']') {
                actual == &expected[1..expected.len() - 1]
            } else {
                expected == actual
            }
        })
}

fn launcher_provider_from_command(command: &str) -> Option<String> {
    let tokens = shell_tokens(command)?;
    if tokens.first().is_none_or(|token| token != "team-agent") {
        return None;
    }
    tokens
        .get(1)
        .filter(|provider| matches!(provider.as_str(), "claude" | "codex"))
        .cloned()
}

fn launcher_provider_from_argv(argv: &[String]) -> Option<&str> {
    argv.first()
        .map(String::as_str)
        .filter(|provider| matches!(*provider, "claude" | "codex"))
}

fn shell_tokens(command: &str) -> Option<Vec<String>> {
    let mut tokens = Vec::new();
    let mut token = String::new();
    let mut quote = None;
    let mut escaped = false;
    for ch in command.chars() {
        if escaped {
            token.push(ch);
            escaped = false;
            continue;
        }
        if ch == '\\' && quote != Some('\'') {
            escaped = true;
            continue;
        }
        if let Some(active) = quote {
            if ch == active {
                quote = None;
            } else {
                token.push(ch);
            }
        } else if ch == '\'' || ch == '"' {
            quote = Some(ch);
        } else if ch.is_whitespace() {
            if !token.is_empty() {
                tokens.push(std::mem::take(&mut token));
            }
        } else {
            token.push(ch);
        }
    }
    if escaped || quote.is_some() {
        return None;
    }
    if !token.is_empty() {
        tokens.push(token);
    }
    Some(tokens)
}

fn test_function_body<'a>(source: &'a str, case: &str) -> Option<&'a str> {
    let signature = format!("fn {case}(");
    let start = source.find(&signature)?;
    let open = source[start..].find('{')? + start;
    let close = matching_delimiter(source, open, b'{', b'}')?;
    Some(&source[open + 1..close])
}

fn literal_run_ta_calls(body: &str) -> Vec<RunTaCall> {
    let mut calls = Vec::new();
    let mut positions = code_token_positions(body, "run_ta");
    positions.extend(code_token_positions(body, "run_ta_env"));
    positions.sort_unstable();
    for call_start in positions {
        let suffix = &body[call_start..];
        let runner = if suffix.starts_with("run_ta_env(") {
            "run_ta_env"
        } else if suffix.starts_with("run_ta(") {
            "run_ta"
        } else {
            continue;
        };
        let Some(open) = body[call_start..].find('(').map(|i| call_start + i) else {
            continue;
        };
        let Some(close) = matching_delimiter(body, open, b'(', b')') else {
            continue;
        };
        let call = &body[open + 1..close];
        if let Some(array_start) = call.find("&[").map(|i| i + 1) {
            if let Some(array_end) = matching_delimiter(call, array_start, b'[', b']') {
                if let Some(parsed) = parse_literal_string_array(&call[array_start + 1..array_end])
                {
                    calls.push(RunTaCall {
                        runner: runner.to_string(),
                        binding: run_ta_binding(&body[..call_start]),
                        argv: parsed,
                        source: body[call_start..=close].to_string(),
                    });
                }
            }
        }
    }
    calls
}

fn code_token_positions(source: &str, token: &str) -> Vec<usize> {
    let bytes = source.as_bytes();
    let mut positions = Vec::new();
    let mut index = 0;
    let mut line_comment = false;
    let mut block_comment = 0usize;
    let mut string = false;
    let mut escaped = false;
    let mut raw_hashes = None;
    while index < bytes.len() {
        let byte = bytes[index];
        let next = bytes.get(index + 1).copied();
        if line_comment {
            if byte == b'\n' {
                line_comment = false;
            }
        } else if block_comment > 0 {
            if byte == b'/' && next == Some(b'*') {
                block_comment += 1;
                index += 1;
            } else if byte == b'*' && next == Some(b'/') {
                block_comment -= 1;
                index += 1;
            }
        } else if let Some(hashes) = raw_hashes {
            if byte == b'"'
                && bytes
                    .get(index + 1..index + 1 + hashes)
                    .is_some_and(|tail| tail.iter().all(|byte| *byte == b'#'))
            {
                raw_hashes = None;
                index += hashes;
            }
        } else if string {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                string = false;
            }
        } else if byte == b'/' && next == Some(b'/') {
            line_comment = true;
            index += 1;
        } else if byte == b'/' && next == Some(b'*') {
            block_comment = 1;
            index += 1;
        } else if byte == b'r' {
            let mut cursor = index + 1;
            while bytes.get(cursor) == Some(&b'#') {
                cursor += 1;
            }
            if bytes.get(cursor) == Some(&b'"') {
                raw_hashes = Some(cursor - index - 1);
                index = cursor;
            } else if source[index..].starts_with(token)
                && identifier_boundary(source, index, token.len())
            {
                positions.push(index);
                index += token.len() - 1;
            }
        } else if byte == b'"' {
            string = true;
        } else if source[index..].starts_with(token)
            && identifier_boundary(source, index, token.len())
        {
            positions.push(index);
            index += token.len() - 1;
        }
        index += 1;
    }
    positions
}

fn identifier_boundary(source: &str, start: usize, len: usize) -> bool {
    let before = source[..start].chars().next_back();
    let after = source[start + len..].chars().next();
    before.is_none_or(|ch| !(ch.is_ascii_alphanumeric() || ch == '_'))
        && after.is_none_or(|ch| !(ch.is_ascii_alphanumeric() || ch == '_'))
}

fn run_ta_binding(prefix: &str) -> Option<String> {
    let statement = prefix
        .rsplit_once(';')
        .map_or(prefix, |(_, statement)| statement)
        .trim();
    let declaration = statement.strip_prefix("let ")?.trim();
    let (binding, _) = declaration.split_once('=')?;
    let binding = binding
        .trim()
        .strip_prefix("mut ")
        .unwrap_or(binding.trim());
    (!binding.is_empty()).then(|| binding.to_string())
}

fn binding_has_behavior_assertion(body: &str, binding: &str) -> bool {
    assertion_macros(body).iter().any(|(_, assertion)| {
        if !identifier_occurs(assertion, binding) {
            return false;
        }
        let compact = compact(assertion);
        [
            format!("{binding}.is_success("),
            format!("{binding}.exit_code"),
            format!("{binding}.stdout"),
            format!("{binding}.stderr"),
            format!("{binding}.json("),
            format!("quick_start_launched(&{binding})"),
        ]
        .iter()
        .any(|marker| compact.contains(marker))
    })
}

fn assertion_macros(body: &str) -> Vec<(&'static str, String)> {
    let mut assertions = Vec::new();
    for kind in ["assert!", "assert_eq!", "assert_ne!"] {
        let needle = format!("{kind}(");
        let mut offset = 0;
        while let Some(found) = body[offset..].find(&needle) {
            let start = offset + found;
            let open = start + kind.len();
            let Some(close) = matching_delimiter(body, open, b'(', b')') else {
                break;
            };
            assertions.push((kind, body[start..=close].to_string()));
            offset = close + 1;
        }
    }
    assertions
}

fn identifier_occurs(source: &str, identifier: &str) -> bool {
    source.match_indices(identifier).any(|(index, _)| {
        let before = source[..index].chars().next_back();
        let after = source[index + identifier.len()..].chars().next();
        before.is_none_or(|ch| !(ch.is_ascii_alphanumeric() || ch == '_'))
            && after.is_none_or(|ch| !(ch.is_ascii_alphanumeric() || ch == '_'))
    })
}

fn compact(source: &str) -> String {
    source.split_whitespace().collect()
}

fn parse_literal_string_array(raw: &str) -> Option<Vec<String>> {
    let mut values = Vec::new();
    let mut rest = raw.trim();
    while !rest.is_empty() {
        if !rest.starts_with('"') {
            return None;
        }
        let bytes = rest.as_bytes();
        let mut escaped = false;
        let mut close = None;
        for (index, byte) in bytes.iter().enumerate().skip(1) {
            if escaped {
                escaped = false;
            } else if *byte == b'\\' {
                escaped = true;
            } else if *byte == b'"' {
                close = Some(index);
                break;
            }
        }
        let close = close?;
        values.push(serde_json::from_str::<String>(&rest[..=close]).ok()?);
        rest = rest[close + 1..].trim_start();
        if rest.is_empty() {
            break;
        }
        rest = rest.strip_prefix(',')?.trim_start();
    }
    Some(values)
}

fn matching_delimiter(source: &str, open: usize, left: u8, right: u8) -> Option<usize> {
    let bytes = source.as_bytes();
    let mut depth = 0usize;
    let mut index = open;
    let mut string = false;
    let mut escaped = false;
    let mut line_comment = false;
    let mut block_comment = 0usize;
    while index < bytes.len() {
        let byte = bytes[index];
        let next = bytes.get(index + 1).copied();
        if line_comment {
            if byte == b'\n' {
                line_comment = false;
            }
        } else if block_comment > 0 {
            if byte == b'/' && next == Some(b'*') {
                block_comment += 1;
                index += 1;
            } else if byte == b'*' && next == Some(b'/') {
                block_comment -= 1;
                index += 1;
            }
        } else if string {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                string = false;
            }
        } else if byte == b'/' && next == Some(b'/') {
            line_comment = true;
            index += 1;
        } else if byte == b'/' && next == Some(b'*') {
            block_comment = 1;
            index += 1;
        } else if byte == b'"' {
            string = true;
        } else if byte == left {
            depth += 1;
        } else if byte == right {
            depth = depth.checked_sub(1)?;
            if depth == 0 {
                return Some(index);
            }
        }
        index += 1;
    }
    None
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("workspace root")
        .to_path_buf()
}
