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
//!   Each A entry explicitly declares one source/test function, literal invocation, binding,
//!   literal assertion node, behavior operand, and executable negative twin. The authority
//!   resolves those declarations through Rust token trees (never substring/character-position
//!   inference), requires every node exactly once, and admits A only when the normal mapped case
//!   passes while the one-field twin fails at that declared assertion. Cross-entry admission also
//!   checks observable twin-discrimination cells; nested diagnostic/control subtrees do not become
//!   behavior evidence merely by containing the binding tokens. Provider launchers retain their
//!   additional hermetic PATH shim and exact argv-log obligations.

use std::collections::{BTreeMap, BTreeSet};
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
const TOOTH_3B_VERDICT_ARTIFACT: &str = "gate-hole-061-tooth-3b-verdict.json";
const TWIN_OBSERVATION_NONCE_ENV: &str = "TEAM_AGENT_TWIN_OBSERVATION_NONCE";
const TWIN_OBSERVATION_PREFIX: &str = "TEAM_AGENT_TWIN-OBSERVATION-V1";
const TWIN_OBSERVATION_SCENARIO_ENV: &str = "TEAM_AGENT_TWIN_OBSERVATION_SCENARIO";

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
    let compact = extract_team_agent_commands(&skill);
    assert!(
        compact.contains("team-agent quick-start .team/current"),
        "TOOTH-3 harness canary: extractor missed the canonical quick-start command"
    );

    let manifest = load_coverage_manifest("TOOTH-3A");
    let listed = unique_manifest_commands(&manifest)
        .unwrap_or_else(|failure| panic!("TOOTH-3A EXACTLY-ONE-BUCKET RED: {failure}"));
    let authority = manifest
        .authority
        .as_ref()
        .expect("TOOTH-3A AUTHORITY-METADATA RED: manifest authority metadata is required");
    assert_eq!(
        authority.kind, "normative_handbook_plus_live_help",
        "TOOTH-3A AUTHORITY-METADATA RED: unsupported command authority kind"
    );
    assert_eq!(
        authority.handbook.path, "docs/reference/team-agent-operator.md",
        "TOOTH-3A AUTHORITY-METADATA RED: handbook path must be repository canonical"
    );
    assert_eq!(
        authority.compact_skill_smoke.path, "skills/team-agent/SKILL.md",
        "TOOTH-3A AUTHORITY-METADATA RED: compact skill path must be repository canonical"
    );
    assert_eq!(
        authority.compact_skill_smoke.policy, "quick_start_and_send_are_subset_only",
        "TOOTH-3A AUTHORITY-METADATA RED: compact skill must be subset-only"
    );
    let handbook = std::fs::read_to_string(repo_root().join(&authority.handbook.path))
        .expect("read normative command handbook");
    let normative = extract_normative_handbook_commands(
        &handbook,
        &authority.handbook.start_marker,
        &authority.handbook.end_marker,
    )
    .unwrap_or_else(|failure| panic!("TOOTH-3A NORMATIVE-HANDBOOK RED: {failure}"));
    let handbook_commands = extract_team_agent_commands(&handbook);
    let live_help = exact_live_help_roots(&authority.live_help, &normative, &handbook_commands);
    let stale_allowed = normative
        .iter()
        .chain(live_help.iter())
        .cloned()
        .collect::<BTreeSet<_>>();
    let compact_smoke = compact
        .into_iter()
        .filter(|command| {
            command.starts_with("team-agent quick-start") || command.starts_with("team-agent send")
        })
        .collect::<BTreeSet<_>>();
    let compact_missing = compact_smoke
        .difference(&listed)
        .cloned()
        .collect::<Vec<_>>();
    assert!(
        compact_missing.is_empty(),
        "TOOTH-3A COMPACT-SMOKE RED: quick-start/send promises from the compact skill \
         must remain a subset of the manifest; missing={compact_missing:?}"
    );
    let missing = normative.difference(&listed).cloned().collect::<Vec<_>>();
    assert!(
        missing.is_empty(),
        "TOOTH-3A COMMAND-AUTHORITY RED: normative handbook commands are missing from the \
         three-bucket coverage manifest; missing={missing:?}"
    );
    let stale = listed
        .difference(&stale_allowed)
        .cloned()
        .collect::<Vec<_>>();
    assert!(
        stale.is_empty(),
        "TOOTH-3A STALE-MANIFEST RED: manifest commands are absent from marked handbook \
         sections and exact live public roots: stale={stale:?}"
    );
}

#[ignore = "red-by-design: pending contract, tracked in private backlog"]
#[test]
fn tooth_3b_three_bucket_claims_are_honest_and_launcher_safe() {
    let verdict = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(evaluate_tooth_3b)) {
        Ok(result) => GateVerdict::from_validation(result),
        Err(payload) => GateVerdict::red(format!(
            "TOOTH-3B HARNESS RED: {}",
            panic_payload_message(payload.as_ref())
        )),
    };
    finalize_gate_verdict(verdict);
}

fn evaluate_tooth_3b() -> Result<TwinDiscriminationOutcome, String> {
    assert_three_bucket_validator_canary();
    assert_global_evidence_identity_canary();
    assert_negative_twin_executor_canary();
    assert_twin_discrimination_canary();

    let manifest = load_coverage_manifest("TOOTH-3B");
    let e2e_tests = source_tree(&["tests/e2e/main.rs", "tests/e2e/cases"]);
    validate_bucket_fields(&manifest)
        .and_then(|_| validate_expected_bucket_totals(&manifest, 2, 46, 0))
        .and_then(|_| validate_covered_case_registration(&manifest))
        .and_then(|_| validate_covered_evidence(&manifest, &e2e_tests))
        .and_then(|outcome| {
            validate_no_unshimmed_launcher_calls(&manifest, &e2e_tests).map(|_| outcome)
        })
}

#[test]
fn gate_hole_negative_twin_execution_canary_case() {
    match std::env::var("TEAM_AGENT_COVERAGE_NEGATIVE_TWIN_EXECUTOR_CANARY").as_deref() {
        Ok("target") => panic!("NEGATIVE-TWIN-TARGET-ASSERTION-CANARY"),
        Ok("setup") => panic!("NEGATIVE-TWIN-SETUP-CANARY"),
        _ => {}
    }
}

#[ignore = "red-by-design: pending contract, tracked in private backlog"]
#[test]
fn gate_hole_twin_discrimination_canary_case() {
    let selected = std::env::var("TEAM_AGENT_COVERAGE_NEGATIVE_TWIN").unwrap_or_default();
    assert_ne!(
        selected, "matrix-first",
        "NEGATIVE-TWIN-MATRIX-FIRST-CANARY"
    );
    eprintln!("NEGATIVE-TWIN-MATRIX-FIRST-PASS");
    assert_ne!(
        selected, "matrix-second",
        "NEGATIVE-TWIN-MATRIX-SECOND-CANARY"
    );
    eprintln!("NEGATIVE-TWIN-MATRIX-SECOND-PASS");
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
    #[serde(default)]
    authority: Option<CoverageAuthority>,
    commands: Vec<CoverageEntry>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct CoverageAuthority {
    kind: String,
    handbook: HandbookAuthority,
    live_help: LiveHelpAuthority,
    compact_skill_smoke: CompactSkillSmokeAuthority,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct HandbookAuthority {
    path: String,
    start_marker: String,
    end_marker: String,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct LiveHelpAuthority {
    argv: Vec<String>,
    source: String,
    root_command_policy: String,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct CompactSkillSmokeAuthority {
    path: String,
    policy: String,
}

#[derive(Debug, serde::Deserialize)]
#[serde(tag = "bucket", rename_all = "snake_case", deny_unknown_fields)]
enum CoverageEntry {
    Covered {
        command: String,
        #[serde(default)]
        cases: Vec<String>,
        #[serde(default)]
        evidence: Option<CoveredEvidenceDeclaration>,
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

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct CoveredEvidenceDeclaration {
    case: String,
    source_file: String,
    invocation: InvocationDeclaration,
    binding: BindingDeclaration,
    assertion: AssertionDeclaration,
    negative_twin: NegativeTwinDeclaration,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct InvocationDeclaration {
    runner: String,
    line: usize,
    documented_argv: Vec<String>,
    literal_argv: Vec<String>,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct BindingDeclaration {
    name: String,
    line: usize,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct AssertionDeclaration {
    macro_name: String,
    line: usize,
    operand: String,
    behavior_fact: String,
    failure_marker: String,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct NegativeTwinDeclaration {
    env_key: String,
    env_value: String,
    operation: String,
    remove_literal: String,
    replacement: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum RustTokenKind {
    Ident(String),
    StringLiteral(String),
    Number(String),
    CharLiteral,
    Punct(char),
    Group {
        delimiter: char,
        tokens: Vec<RustToken>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RustToken {
    kind: RustTokenKind,
    line: usize,
}

#[derive(Debug, Clone)]
struct FunctionNode {
    body: Vec<RustToken>,
}

#[derive(Debug, Clone)]
struct RunTaCall {
    runner: String,
    binding: Option<String>,
    binding_line: Option<usize>,
    line: usize,
    argv: Vec<String>,
    has_path_override: bool,
}

#[derive(Debug, Clone)]
struct AssertionNode {
    name: String,
    line: usize,
    path_qualified: bool,
    arguments: Vec<RustToken>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GateTerminalStatus {
    Green,
    Pending,
    Red,
}

impl GateTerminalStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Green => "GREEN",
            Self::Pending => "PENDING",
            Self::Red => "RED",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
struct PendingTwinCell {
    row: usize,
    column: usize,
    outcome: String,
    detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum TwinDiscriminationOutcome {
    Complete,
    Pending(Vec<PendingTwinCell>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TwinObservationResult {
    Pass,
    Fail,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct GateVerdict {
    status: GateTerminalStatus,
    reason: Option<String>,
    pending_cells: Vec<PendingTwinCell>,
}

impl GateVerdict {
    fn from_validation(result: Result<TwinDiscriminationOutcome, String>) -> Self {
        match result {
            Ok(TwinDiscriminationOutcome::Complete) => Self {
                status: GateTerminalStatus::Green,
                reason: None,
                pending_cells: Vec::new(),
            },
            Ok(TwinDiscriminationOutcome::Pending(pending_cells)) => Self {
                status: GateTerminalStatus::Pending,
                reason: Some(
                    "one or more twin-discrimination cells lack a positive observation".to_string(),
                ),
                pending_cells,
            },
            Err(reason) => Self::red(reason),
        }
    }

    fn red(reason: String) -> Self {
        Self {
            status: GateTerminalStatus::Red,
            reason: Some(reason),
            pending_cells: Vec::new(),
        }
    }

    fn allows_success(&self) -> bool {
        self.status == GateTerminalStatus::Green
    }
}

fn verdict_artifact_path() -> PathBuf {
    std::env::var_os("TEAM_AGENT_TEST_TMP")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("team-agent-gate-verdicts"))
        .join(TOOTH_3B_VERDICT_ARTIFACT)
}

fn gate_verdict_value(verdict: &GateVerdict) -> Value {
    serde_json::json!({
        "schema_version": "team-agent-gate-verdict-v1",
        "status": verdict.status.as_str(),
        "allows_success": verdict.allows_success(),
        "reason": verdict.reason,
        "pending_cells": verdict.pending_cells,
    })
}

fn write_gate_verdict(verdict: &GateVerdict) -> Result<PathBuf, String> {
    let path = verdict_artifact_path();
    let parent = path
        .parent()
        .ok_or_else(|| "TOOTH-3B VERDICT-ARTIFACT RED: artifact path has no parent".to_string())?;
    std::fs::create_dir_all(parent)
        .map_err(|error| format!("TOOTH-3B VERDICT-ARTIFACT RED: {error}"))?;
    let encoded = serde_json::to_vec_pretty(&gate_verdict_value(verdict))
        .map_err(|error| format!("TOOTH-3B VERDICT-ARTIFACT RED: {error}"))?;
    std::fs::write(&path, encoded)
        .map_err(|error| format!("TOOTH-3B VERDICT-ARTIFACT RED: {error}"))?;
    Ok(path)
}

fn finalize_gate_verdict(verdict: GateVerdict) {
    let path = write_gate_verdict(&verdict).unwrap_or_else(|failure| panic!("{failure}"));
    match verdict.status {
        GateTerminalStatus::Green => {}
        GateTerminalStatus::Pending => panic!(
            "TOOTH-3B GATE-PENDING NON-GREEN: unresolved observation cells remain; \
             verdict_artifact={}",
            path.display()
        ),
        GateTerminalStatus::Red => panic!(
            "{}\nTOOTH-3B GATE-RED verdict_artifact={}",
            verdict
                .reason
                .as_deref()
                .unwrap_or("TOOTH-3B RED: no reason supplied"),
            path.display()
        ),
    }
}

fn panic_payload_message(payload: &(dyn std::any::Any + Send)) -> String {
    payload
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| {
            payload
                .downcast_ref::<&str>()
                .map(|value| value.to_string())
        })
        .unwrap_or_else(|| "non-string panic payload".to_string())
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
    let honest_evidence = canary_evidence("assert", "condition", "stdout", 7);
    let honest = CoverageManifest {
        schema_version: "team-agent-skill-command-coverage-v3".to_string(),
        authority: None,
        commands: vec![
            CoverageEntry::Covered {
                command: "team-agent status --json".to_string(),
                cases: vec!["verifier_covered_canary".to_string()],
                evidence: Some(honest_evidence.clone()),
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
    let honest_source =
        canary_source(r#"assert!(out.stdout.contains("ready"), "status failed: {}", out.stderr);"#);
    assert!(
        validate_bucket_fields(&honest).is_ok()
            && validate_declared_evidence_syntax(
                "team-agent status --json",
                &honest_evidence,
                &honest_source,
            )
            .is_ok()
            && validate_no_unshimmed_launcher_calls(&honest, &honest_source).is_ok(),
        "TOOTH-3B harness canary: an honest A=1/B=1/C=0 catalog must be green"
    );

    let mut mismatched_argv = honest_evidence.clone();
    mismatched_argv.invocation.literal_argv = vec!["doctor".to_string()];
    assert_syntax_failure(
        "team-agent status --json",
        &mismatched_argv,
        &honest_source,
        "COVERED-EXACT-ARGV",
    );

    let mut trailing_dot = honest_evidence.clone();
    trailing_dot.invocation.documented_argv = vec![
        "team-agent".to_string(),
        "status".to_string(),
        ".".to_string(),
    ];
    trailing_dot.invocation.literal_argv = vec!["status".to_string(), ".".to_string()];
    let trailing_dot_source = canary_source_with_invocation(
        r#"let mut out = run_ta(&ws, &["status", "."]);"#,
        r#"assert!(out.stdout.contains("ready"), "status failed: {}", out.stderr);"#,
    );
    assert!(
        validate_declared_evidence_syntax(
            "team-agent status .",
            &trailing_dot,
            &trailing_dot_source
        )
        .is_ok(),
        "TOOTH-3B A1 canary: final `.` must remain an independent declared and literal argv token"
    );
    let mut missing_trailing_dot = trailing_dot;
    missing_trailing_dot.invocation.documented_argv.pop();
    assert_syntax_failure(
        "team-agent status .",
        &missing_trailing_dot,
        &trailing_dot_source,
        "COVERED-EXACT-ARGV",
    );
    assert_syntax_failure(
        "team-agent status --json",
        &honest_evidence,
        "#[test]\nfn verifier_covered_canary() {\n",
        "COVERED-SYNTAX-PARSE",
    );

    let missing_owner = CoverageManifest {
        schema_version: honest.schema_version.clone(),
        authority: None,
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
        authority: None,
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

    let discarded_source = canary_source_with_invocation(
        r#"let _ = run_ta(&ws, &["status", "--json"]);"#,
        r#"assert!(out.stdout.contains("ready"), "status failed: {}", out.stderr);"#,
    );
    assert_syntax_failure(
        "team-agent status --json",
        &honest_evidence,
        &discarded_source,
        "MAPPED-DISCARDED-RUN",
    );

    let mapped_launcher_source = format!(
        "{honest_source}\nfn verifier_extra() {{\n\
         let launcher = run_ta_env(&ws, &[\"claude\"], &[(\"PATH\", shim_path.as_str())]);\n\
         assert!(launcher.is_success());\n}}\n"
    );
    let mapped_case_launcher = canary_source_with_extra(
        r#"assert!(out.stdout.contains("ready"), "status failed: {}", out.stderr);"#,
        r#"let launcher = run_ta_env(&ws, &["claude"], &[("PATH", shim_path.as_str())]);
    assert!(launcher.is_success());"#,
    );
    assert_syntax_failure(
        "team-agent status --json",
        &honest_evidence,
        &mapped_case_launcher,
        "MAPPED-LAUNCHER-ARGV",
    );

    let missing_bound_assertion = canary_source(
        r#"assert_eq!(out.argv, vec!["team-agent", "status", "--json"], "argv only");"#,
    );
    let mut missing_bound_evidence = honest_evidence.clone();
    missing_bound_evidence.assertion.macro_name = "assert_eq".to_string();
    missing_bound_evidence.assertion.operand = "left".to_string();
    assert_red_then_restored_green(
        "team-agent status --json",
        &missing_bound_evidence,
        &missing_bound_assertion,
        &canary_source(r#"assert_eq!(out.stdout.contains("ready"), true, "status failed");"#),
        "COVERED-BINDING-ASSERTION",
    );

    let diagnostic_only_bound_output =
        canary_source(r#"assert!(true, "diagnostic-only stdout={}", out.stdout);"#);
    assert_red_then_restored_green(
        "team-agent status --json",
        &honest_evidence,
        &diagnostic_only_bound_output,
        &honest_source,
        "COVERED-BINDING-ASSERTION",
    );

    let mut comparison_evidence = honest_evidence.clone();
    comparison_evidence.assertion.macro_name = "assert_eq".to_string();
    comparison_evidence.assertion.operand = "left".to_string();
    comparison_evidence.assertion.behavior_fact = "exit_code".to_string();
    comparison_evidence.assertion.failure_marker = "status failed".to_string();
    let comparison_operand_bound_output =
        canary_source(r#"assert_eq!(out.exit_code, 0, "status failed");"#);
    assert!(
        validate_declared_evidence_nodes(
            "team-agent status --json",
            &comparison_evidence,
            &comparison_operand_bound_output,
        )
        .is_ok(),
        "TOOTH-3B harness canary: a run_ta result referenced by a comparison operand must count"
    );

    for (assertion, signature) in [
        (
            r#"assert!(true || panic!("diagnostic stdout={}", out.stdout), "status failed");"#,
            "COVERED-BINDING-NESTED-PANIC",
        ),
        (
            r#"assert!(true || format!("{}", out.stdout).is_empty(), "status failed");"#,
            "COVERED-BINDING-NESTED-FORMAT",
        ),
        (
            r#"assert!(true || write!(&mut String::new(), "{}", out.stdout).is_ok(), "status failed");"#,
            "COVERED-BINDING-NESTED-WRITE",
        ),
        (
            r#"assert!(true || { debug_assert!(out.stdout.contains("ready")); false }, "status failed");"#,
            "COVERED-BINDING-NESTED-DEBUG-ASSERT",
        ),
        (
            r#"assert!(true || std::panic::catch_unwind(|| out.stdout.contains("ready")).is_ok(), "status failed");"#,
            "COVERED-BINDING-NESTED-CLOSURE",
        ),
    ] {
        assert_red_then_restored_green(
            "team-agent status --json",
            &honest_evidence,
            &canary_source(assertion),
            &honest_source,
            signature,
        );
    }

    for (assertion, macro_name, signature) in [
        (
            r#"assert_eq!(1, 1, "diagnostic-only stdout={}", out.stdout);"#,
            "assert_eq",
            "COVERED-BINDING-ASSERTION",
        ),
        (
            r#"panic!("diagnostic-only stdout={}", out.stdout);"#,
            "assert",
            "COVERED-ASSERTION-MACRO",
        ),
        (
            r#"assert!(true, "diagnostic-only nested={}", (((out.stdout))));"#,
            "assert",
            "COVERED-BINDING-ASSERTION",
        ),
        (
            r#"check!(out.stdout.contains("ready"));"#,
            "assert",
            "COVERED-ASSERTION-MACRO",
        ),
        (
            r#"custom_assert!(out.stdout.contains("ready"));"#,
            "assert",
            "COVERED-ASSERTION-MACRO",
        ),
        (
            r#"custom_assert_eq!(out.stdout, "ready");"#,
            "assert_eq",
            "COVERED-ASSERTION-MACRO",
        ),
        (
            r#"std::assert!(out.stdout.contains("ready"));"#,
            "assert",
            "COVERED-ASSERTION-MACRO",
        ),
    ] {
        let mut evidence = honest_evidence.clone();
        evidence.assertion.macro_name = macro_name.to_string();
        if macro_name == "assert_eq" {
            evidence.assertion.operand = "left".to_string();
        }
        let invalid = canary_source(assertion);
        let restored = if macro_name == "assert_eq" {
            canary_source(r#"assert_eq!(out.stdout.contains("ready"), true, "status failed");"#)
        } else {
            honest_source.clone()
        };
        assert_red_then_restored_green(
            "team-agent status --json",
            &evidence,
            &invalid,
            &restored,
            signature,
        );
    }

    let mut function_zero = honest_evidence.clone();
    function_zero.case = "missing_verifier_case".to_string();
    assert_syntax_failure(
        "team-agent status --json",
        &function_zero,
        &honest_source,
        "COVERED-FUNCTION-NODE-ZERO",
    );
    let function_multiple = format!("{honest_source}\n{honest_source}");
    assert_syntax_failure(
        "team-agent status --json",
        &honest_evidence,
        &function_multiple,
        "COVERED-FUNCTION-NODE-MULTIPLE",
    );

    let mut invocation_zero = honest_evidence.clone();
    invocation_zero.invocation.line = 99;
    assert_syntax_failure(
        "team-agent status --json",
        &invocation_zero,
        &honest_source,
        "COVERED-INVOCATION-NODE-ZERO",
    );
    let invocation_multiple = canary_source_with_invocation(
        r#"let mut out = run_ta(&ws, &["status", "--json"]); let other = run_ta(&ws, &["status", "--json"]);"#,
        r#"assert!(out.stdout.contains("ready"), "status failed: {}", out.stderr);"#,
    );
    assert_syntax_failure(
        "team-agent status --json",
        &honest_evidence,
        &invocation_multiple,
        "COVERED-INVOCATION-NODE-MULTIPLE",
    );

    let mut binding_zero = honest_evidence.clone();
    binding_zero.binding.name = "other".to_string();
    assert_syntax_failure(
        "team-agent status --json",
        &binding_zero,
        &honest_source,
        "COVERED-BINDING-NODE-ZERO",
    );
    let binding_multiple = canary_source_with_invocation(
        r#"let mut out = run_ta(&ws, &["status", "--json"]); let out = 1;"#,
        r#"assert!(out.stdout.contains("ready"), "status failed: {}", out.stderr);"#,
    );
    assert_syntax_failure(
        "team-agent status --json",
        &honest_evidence,
        &binding_multiple,
        "COVERED-BINDING-NODE-MULTIPLE",
    );

    let mut assertion_zero = honest_evidence.clone();
    assertion_zero.assertion.line = 99;
    assert_syntax_failure(
        "team-agent status --json",
        &assertion_zero,
        &honest_source,
        "COVERED-ASSERTION-NODE-ZERO",
    );
    let assertion_multiple = canary_source(
        r#"assert!(out.stdout.contains("ready"), "status failed: {}", out.stderr); assert!(out.stdout.contains("ready"), "status failed: {}", out.stderr);"#,
    );
    assert_syntax_failure(
        "team-agent status --json",
        &honest_evidence,
        &assertion_multiple,
        "COVERED-ASSERTION-NODE-MULTIPLE",
    );

    assert_failure_signature(
        validate_negative_twin_hook(&comparison_evidence, &comparison_operand_bound_output),
        "NEGATIVE-TWIN-MUTATION",
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

    assert!(
        mapped_launcher_source.contains("verifier_extra"),
        "TOOTH-3B harness canary: mapped launcher fixture must remain independent"
    );
    eprintln!(
        "TOOTH-3B AUTHORITY CANARIES GREEN: exact-one function/invocation/binding/assertion; \
         diagnostic-only assert/assert_eq; panic; nested diagnostic; check/custom_assert/\
         custom_assert_eq/path-qualified macros all conservative RED; literal assert_eq \
         comparison operand GREEN; unsupported exit-code twin conservative RED"
    );
}

fn canary_evidence(
    macro_name: &str,
    operand: &str,
    behavior_fact: &str,
    assertion_line: usize,
) -> CoveredEvidenceDeclaration {
    CoveredEvidenceDeclaration {
        case: "verifier_covered_canary".to_string(),
        source_file: "__synthetic_canary__.rs".to_string(),
        invocation: InvocationDeclaration {
            runner: "run_ta".to_string(),
            line: 3,
            documented_argv: vec![
                "team-agent".to_string(),
                "status".to_string(),
                "--json".to_string(),
            ],
            literal_argv: vec!["status".to_string(), "--json".to_string()],
        },
        binding: BindingDeclaration {
            name: "out".to_string(),
            line: 3,
        },
        assertion: AssertionDeclaration {
            macro_name: macro_name.to_string(),
            line: assertion_line,
            operand: operand.to_string(),
            behavior_fact: behavior_fact.to_string(),
            failure_marker: "status failed".to_string(),
        },
        negative_twin: NegativeTwinDeclaration {
            env_key: "TEAM_AGENT_COVERAGE_NEGATIVE_TWIN".to_string(),
            env_value: "verifier-status-ready".to_string(),
            operation: "remove_text_literal".to_string(),
            remove_literal: "ready".to_string(),
            replacement: "__negative_twin_removed__".to_string(),
        },
    }
}

fn covered_canary_entry(command: &str, evidence: CoveredEvidenceDeclaration) -> CoverageEntry {
    CoverageEntry::Covered {
        command: command.to_string(),
        cases: vec![evidence.case.clone()],
        evidence: Some(evidence),
        launcher_shim_evidence: None,
    }
}

fn assert_global_evidence_identity_canary() {
    let first = canary_evidence("assert", "condition", "stdout", 7);
    let duplicate_pair = CoverageManifest {
        schema_version: "team-agent-skill-command-coverage-v3".to_string(),
        authority: None,
        commands: vec![
            covered_canary_entry("team-agent status --json", first.clone()),
            covered_canary_entry("team-agent status", first.clone()),
        ],
    };
    assert!(
        unique_manifest_commands(&duplicate_pair).is_ok(),
        "TOOTH-3B global-identity canary: command identity must remain independent from \
         assertion+twin evidence identity"
    );
    assert_failure_signature(
        validate_assertion_twin_pair_uniqueness(&duplicate_pair),
        "COVERED-ASSERTION-TWIN-PAIR-DUPLICATE",
    );

    let mut distinct_pair = first;
    distinct_pair.assertion.line += 1;
    distinct_pair.negative_twin.env_value = "verifier-status-distinct".to_string();
    let restored = CoverageManifest {
        schema_version: duplicate_pair.schema_version.clone(),
        authority: None,
        commands: vec![
            covered_canary_entry(
                "team-agent status --json",
                canary_evidence("assert", "condition", "stdout", 7),
            ),
            covered_canary_entry("team-agent status", distinct_pair),
        ],
    };
    assert!(
        validate_assertion_twin_pair_uniqueness(&restored).is_ok(),
        "TOOTH-3B global-identity restore canary: distinct assertion+twin pairs must be green"
    );
}

fn canary_source(assertion: &str) -> String {
    canary_source_with_invocation(
        r#"let mut out = run_ta(&ws, &["status", "--json"]);"#,
        assertion,
    )
}

fn canary_source_with_invocation(invocation: &str, assertion: &str) -> String {
    format!(
        "#[test]\nfn verifier_covered_canary() {{\n    {invocation}\n\
         if std::env::var(\"TEAM_AGENT_COVERAGE_NEGATIVE_TWIN\").as_deref() == \
         Ok(\"verifier-status-ready\") {{\n\
         out.stdout = out.stdout.replacen(\"ready\", \"__negative_twin_removed__\", 1);\n\
         }}\n    {assertion}\n}}\n"
    )
}

fn canary_source_with_extra(assertion: &str, extra: &str) -> String {
    let mut source = canary_source(assertion);
    source.insert_str(
        source.rfind('}').expect("canary closing brace"),
        &format!("    {extra}\n"),
    );
    source
}

fn assert_syntax_failure(
    command: &str,
    evidence: &CoveredEvidenceDeclaration,
    source: &str,
    signature: &str,
) {
    assert_failure_signature(
        validate_declared_evidence_syntax(command, evidence, source),
        signature,
    );
}

fn assert_red_then_restored_green(
    command: &str,
    evidence: &CoveredEvidenceDeclaration,
    invalid: &str,
    restored: &str,
    signature: &str,
) {
    let failure = validate_declared_evidence_syntax(command, evidence, invalid)
        .expect_err("TOOTH-3B harness canary: invalid syntax shape must be red");
    assert!(
        failure.contains(signature),
        "TOOTH-3B harness canary: expected red signature {signature:?}; got {failure}"
    );
    eprintln!(
        "TOOTH-3B RED-THEN-RESTORE signature={signature} INVALID-RAW-BEGIN\n{failure}\n\
         TOOTH-3B RED-THEN-RESTORE signature={signature} INVALID-RAW-END"
    );
    if let Err(failure) = validate_declared_evidence_syntax(command, evidence, restored) {
        panic!(
            "TOOTH-3B harness restore canary: {signature} negative must turn green after \
             restoring the supported literal assertion; got {failure}"
        );
    }
}

fn assert_failure_signature(result: Result<(), String>, signature: &str) {
    let failure = result.expect_err("TOOTH-3B harness canary: invalid catalog must be red");
    assert!(
        failure.contains(signature),
        "TOOTH-3B harness canary: expected red signature {signature:?}; got {failure}"
    );
    eprintln!(
        "TOOTH-3B FAILURE-SIGNATURE signature={signature} RAW-BEGIN\n{failure}\n\
         TOOTH-3B FAILURE-SIGNATURE signature={signature} RAW-END"
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

fn validate_assertion_twin_pair_uniqueness(manifest: &CoverageManifest) -> Result<(), String> {
    // Cheap structural screening only. Admission still depends on the executable
    // discrimination cells in `validate_observable_twin_discrimination`.
    let mut declared_pairs = BTreeSet::new();
    for entry in &manifest.commands {
        let CoverageEntry::Covered {
            evidence: Some(evidence),
            ..
        } = entry
        else {
            continue;
        };
        let pair = (
            evidence.source_file.as_str(),
            evidence.case.as_str(),
            evidence.assertion.macro_name.as_str(),
            evidence.assertion.line,
            evidence.assertion.operand.as_str(),
            evidence.negative_twin.env_key.as_str(),
            evidence.negative_twin.env_value.as_str(),
            evidence.negative_twin.operation.as_str(),
            evidence.negative_twin.remove_literal.as_str(),
            evidence.negative_twin.replacement.as_str(),
        );
        if !declared_pairs.insert(pair) {
            return Err(
                "TOOTH-3B COVERED-ASSERTION-TWIN-PAIR-DUPLICATE RED: legal A evidence \
                 requires each (assertion node, negative twin) pair to be distinct"
                    .to_string(),
            );
        }
    }
    Ok(())
}

fn validate_bucket_fields(manifest: &CoverageManifest) -> Result<(), String> {
    for entry in &manifest.commands {
        match entry {
            CoverageEntry::Covered {
                command,
                cases,
                evidence,
                launcher_shim_evidence,
            } => {
                let Some(evidence) = evidence else {
                    return Err(format!(
                        "TOOTH-3B COVERED-EVIDENCE-DECLARATION RED: A entry {command:?} must \
                         explicitly declare its unique function, invocation, binding, assertion \
                         node, and executable negative twin"
                    ));
                };
                if cases != &[evidence.case.clone()] {
                    return Err(format!(
                        "TOOTH-3B COVERED-CASE RED: A entry {command:?} must name exactly the \
                         one evidence case {:?}",
                        evidence.case
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
    if manifest.schema_version != "team-agent-skill-command-coverage-v3" {
        return Err(
            "TOOTH-3B THREE-BUCKET-SCHEMA RED: syntax-declared evidence + executable negative \
             twin require team-agent-skill-command-coverage-v3"
                .to_string(),
        );
    }
    Ok(())
}

fn validate_expected_bucket_totals(
    manifest: &CoverageManifest,
    expected_a: usize,
    expected_b: usize,
    expected_c: usize,
) -> Result<(), String> {
    let (mut actual_a, mut actual_b, mut actual_c) = (0, 0, 0);
    for entry in &manifest.commands {
        match entry {
            CoverageEntry::Covered { .. } => actual_a += 1,
            CoverageEntry::DeclaredGap { .. } => actual_b += 1,
            CoverageEntry::Exempt { .. } => actual_c += 1,
        }
    }
    if (actual_a, actual_b, actual_c) != (expected_a, expected_b, expected_c) {
        return Err(format!(
            "TOOTH-3B THREE-BUCKET-TOTALS RED: manifest must explicitly remain \
             A={expected_a}/B={expected_b}/C={expected_c}; observed \
             A={actual_a}/B={actual_b}/C={actual_c}"
        ));
    }
    Ok(())
}

fn validate_covered_evidence(
    manifest: &CoverageManifest,
    _e2e_tests: &str,
) -> Result<TwinDiscriminationOutcome, String> {
    validate_assertion_twin_pair_uniqueness(manifest)?;
    let mut positive_cases = BTreeSet::new();
    for entry in &manifest.commands {
        let CoverageEntry::Covered {
            command,
            evidence: Some(evidence),
            launcher_shim_evidence,
            ..
        } = entry
        else {
            continue;
        };
        if evidence.case.starts_with("tooth_") {
            return Err(format!(
                "TOOTH-3B COVERED-CASE RED: A entry {command:?} self-maps to verifier case {:?}",
                evidence.case
            ));
        }
        let source =
            std::fs::read_to_string(repo_root().join(&evidence.source_file)).map_err(|error| {
                format!(
                    "TOOTH-3B COVERED-SOURCE-FILE RED: declared source {:?} is unreadable: \
                     {error}",
                    evidence.source_file
                )
            })?;
        validate_declared_evidence_syntax(command, evidence, &source)?;
        positive_cases.insert((evidence.source_file.clone(), evidence.case.clone()));
        if let (Some(provider), Some(launcher_evidence)) = (
            launcher_provider_from_command(command),
            launcher_shim_evidence.as_ref(),
        ) {
            validate_launcher_shim_evidence(
                command,
                &provider,
                evidence,
                launcher_evidence,
                &source,
            )?;
        }
    }
    for (source_file, case) in positive_cases {
        assert_mapped_case_positive(&source_file, &case)?;
    }
    validate_observable_twin_discrimination(manifest)
}

fn validate_covered_case_registration(manifest: &CoverageManifest) -> Result<(), String> {
    let main_path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/e2e/main.rs");
    let main = std::fs::read_to_string(&main_path)
        .map_err(|error| format!("TOOTH-3B COVERED-CASE-REGISTRATION RED: {error}"))?;
    let main_tokens = rust_syntax_tokens(&main)?;
    for entry in &manifest.commands {
        let CoverageEntry::Covered {
            command,
            evidence: Some(evidence),
            ..
        } = entry
        else {
            continue;
        };
        let source_path = Path::new(&evidence.source_file);
        let safe_components = source_path.components().all(|component| {
            matches!(
                component,
                std::path::Component::Normal(_) | std::path::Component::CurDir
            )
        });
        let expected_prefix = Path::new("crates/team-agent/tests/e2e/cases");
        if !safe_components
            || !source_path.starts_with(expected_prefix)
            || source_path
                .extension()
                .is_none_or(|extension| extension != "rs")
        {
            return Err(format!(
                "TOOTH-3B COVERED-SOURCE-FILE RED: A entry {command:?} must declare one \
                 repository-relative tests/e2e/cases/*.rs source file"
            ));
        }
        let source = std::fs::read_to_string(repo_root().join(source_path)).map_err(|error| {
            format!("TOOTH-3B COVERED-SOURCE-FILE RED: declared source is unreadable: {error}")
        })?;
        let functions = test_function_nodes(&rust_syntax_tokens(&source)?, &evidence.case);
        if functions.len() != 1 {
            return Err(node_count_failure(
                "FUNCTION",
                functions.len(),
                &evidence.case,
            ));
        }
        let module = source_path
            .file_stem()
            .expect("validated source file stem")
            .to_string_lossy();
        if module_declaration_count(&main_tokens, &module) != 1 {
            return Err(format!(
                "TOOTH-3B COVERED-CASE-REGISTRATION RED: A entry {command:?} source module \
                 must be registered exactly once by tests/e2e/main.rs"
            ));
        }
    }
    Ok(())
}

fn validate_launcher_shim_evidence(
    command: &str,
    provider: &str,
    covered: &CoveredEvidenceDeclaration,
    evidence: &LauncherShimEvidence,
    source: &str,
) -> Result<(), String> {
    if evidence.provider != provider || evidence.case != covered.case {
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
    let functions = test_function_nodes(&rust_syntax_tokens(source)?, &evidence.case);
    let function = functions.first().ok_or_else(|| {
        format!("TOOTH-3B LAUNCHER-SHIM-EVIDENCE RED: missing declared evidence case")
    })?;
    let calls = run_ta_calls(&function.body);
    let matching = calls
        .iter()
        .filter(|call| documented_command_matches_argv(command, &call.argv))
        .collect::<Vec<_>>();
    let Some(call) = matching.iter().find(|call| {
        call.runner == "run_ta_env"
            && call.binding.as_deref() == Some(evidence.cli_result_binding.as_str())
            && call.has_path_override
    }) else {
        return Err(format!(
            "TOOTH-3B UNSHIMMED-LAUNCHER-EXECUTION RED: launcher {command:?} must execute via \
             bound run_ta_env with a per-call PATH override; observed={matching:?}"
        ));
    };
    if !binding_assigned_named_call(&function.body, &evidence.argv_log_binding, "read_to_string") {
        return Err(format!(
            "TOOTH-3B LAUNCHER-SHIM-ARGV RED: launcher {command:?} does not read the hermetic \
             shim argv log into binding {:?}",
            evidence.argv_log_binding
        ));
    }
    let assertions = assertion_nodes(&function.body);
    let provider_argv = shell_tokens(command)
        .unwrap_or_default()
        .into_iter()
        .skip(1)
        .collect::<Vec<_>>();
    let exact_log_asserted = assertions.iter().any(|assertion| {
        assertion.name == "assert_eq"
            && !assertion.path_qualified
            && assertion_operands(assertion).is_some_and(|operands| {
                operands
                    .iter()
                    .take(2)
                    .any(|operand| identifier_in_tokens(operand, &evidence.argv_log_binding))
                    && provider_argv.iter().all(|expected| {
                        operands
                            .iter()
                            .take(2)
                            .any(|operand| string_literal_in_tokens(operand, expected))
                    })
            })
    });
    if !exact_log_asserted {
        return Err(format!(
            "TOOTH-3B LAUNCHER-SHIM-ARGV RED: launcher {command:?} must use assert_eq! on shim \
             log binding {:?} and name the full provider argv {:?}",
            evidence.argv_log_binding, provider_argv
        ));
    }
    if evidence.cli_result_binding != covered.binding.name {
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
    let tokens = rust_syntax_tokens(e2e_tests)?;
    let launcher_calls = run_ta_calls(&tokens)
        .into_iter()
        .filter(|call| launcher_provider_from_argv(&call.argv).is_some())
        .collect::<Vec<_>>();
    for call in &launcher_calls {
        if call.runner != "run_ta_env" || !call.has_path_override {
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
            evidence: Some(covered),
            ..
        } = entry
        else {
            continue;
        };
        let source = std::fs::read_to_string(repo_root().join(&covered.source_file))
            .unwrap_or_else(|_| String::new());
        let functions = rust_syntax_tokens(&source)
            .ok()
            .map(|tokens| test_function_nodes(&tokens, &evidence.case))
            .unwrap_or_default();
        let Some(function) = functions.first() else {
            continue;
        };
        authorized += run_ta_calls(&function.body)
            .into_iter()
            .filter(|call| {
                call.runner == "run_ta_env"
                    && call.has_path_override
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

fn extract_normative_handbook_commands(
    markdown: &str,
    start_marker: &str,
    end_marker: &str,
) -> Result<BTreeSet<String>, String> {
    let starts = markdown.match_indices(start_marker).collect::<Vec<_>>();
    let ends = markdown.match_indices(end_marker).collect::<Vec<_>>();
    if starts.len() != 1 || ends.len() != 1 {
        return Err(format!(
            "normative markers must occur exactly once (start={}, end={})",
            starts.len(),
            ends.len()
        ));
    }
    let start = starts[0].0 + start_marker.len();
    let end = ends[0].0;
    if start >= end {
        return Err("normative end marker precedes start marker".to_string());
    }
    let commands = markdown[start..end]
        .lines()
        .filter_map(|line| {
            let trimmed = line.trim();
            trimmed
                .strip_prefix("team-agent ")
                .and_then(|_| normalize_team_agent_command(trimmed))
        })
        .collect::<BTreeSet<_>>();
    if commands.is_empty() {
        return Err("normative section contains no canonical command lines".to_string());
    }
    Ok(commands)
}

fn exact_live_help_roots(
    authority: &LiveHelpAuthority,
    normative: &BTreeSet<String>,
    handbook_commands: &BTreeSet<String>,
) -> BTreeSet<String> {
    assert_eq!(
        authority.argv,
        vec!["team-agent".to_string(), "--help".to_string()],
        "TOOTH-3A LIVE-HELP-AUTHORITY RED: live help argv must be exact root help"
    );
    assert_eq!(
        authority.source, "exact_test_binary",
        "TOOTH-3A LIVE-HELP-AUTHORITY RED: live help must come from the test binary"
    );
    assert_eq!(
        authority.root_command_policy, "canonical_handbook_commands_may_use_compatibility_forms",
        "TOOTH-3A LIVE-HELP-AUTHORITY RED: unsupported live help policy"
    );
    let ws = TestWorkspace::new("gate061-live-help");
    let out = run_ta(&ws, &["--help"]);
    assert_eq!(
        out.exit_code, 0,
        "TOOTH-3A LIVE-HELP RED: exact CLI root help failed: stdout={} stderr={}",
        out.stdout, out.stderr
    );
    out.stdout
        .lines()
        .filter_map(|line| {
            let trimmed = line.trim();
            let command = if let Some(rest) = trimmed.strip_prefix("team-agent ") {
                rest.split_whitespace().next()?
            } else if line.starts_with("  ") {
                trimmed.split_whitespace().next()?
            } else {
                return None;
            };
            if command.is_empty() || command.contains('|') || command.contains('<') {
                return None;
            }
            let root = format!("team-agent {command}");
            if normative
                .iter()
                .any(|canonical| canonical == &root || canonical.starts_with(&format!("{root} ")))
            {
                return None;
            }
            if !handbook_commands.iter().any(|documented| {
                documented == &root || documented.starts_with(&format!("{root} "))
            })
            {
                return None;
            }
            Some(root)
        })
        .collect()
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

fn validate_declared_evidence_syntax(
    command: &str,
    evidence: &CoveredEvidenceDeclaration,
    source: &str,
) -> Result<(), String> {
    validate_declared_evidence_nodes(command, evidence, source)?;
    validate_negative_twin_hook(evidence, source)
}

fn validate_declared_evidence_nodes(
    command: &str,
    evidence: &CoveredEvidenceDeclaration,
    source: &str,
) -> Result<(), String> {
    let syntax = rust_syntax_tokens(source)?;
    let functions = test_function_nodes(&syntax, &evidence.case);
    if functions.len() != 1 {
        return Err(node_count_failure(
            "FUNCTION",
            functions.len(),
            &evidence.case,
        ));
    }
    let function = &functions[0];
    let calls = run_ta_calls(&function.body);
    if let Some(discarded) = calls
        .iter()
        .find(|call| call.binding.as_deref() == Some("_"))
    {
        return Err(format!(
            "TOOTH-3B MAPPED-DISCARDED-RUN RED: mapped case {:?} contains discarded {} argv {:?}",
            evidence.case, discarded.runner, discarded.argv
        ));
    }
    let launcher_calls = calls
        .iter()
        .filter(|call| launcher_provider_from_argv(&call.argv).is_some())
        .collect::<Vec<_>>();
    if launcher_provider_from_command(command).is_none() && !launcher_calls.is_empty() {
        return Err(format!(
            "TOOTH-3B MAPPED-LAUNCHER-ARGV RED: non-launcher A entry maps a case that also \
             executes provider launcher argv"
        ));
    }

    let expected_documented = shell_tokens(command).ok_or_else(|| {
        "TOOTH-3B COVERED-EXACT-ARGV RED: documented command is not valid shell tokens".to_string()
    })?;
    if expected_documented != evidence.invocation.documented_argv {
        return Err(
            "TOOTH-3B COVERED-EXACT-ARGV RED: normalized documented argv and the explicit \
             declaration are not token-identical"
                .to_string(),
        );
    }
    if evidence
        .invocation
        .documented_argv
        .last()
        .is_some_and(|token| token == ".")
        != shell_tokens(command)
            .and_then(|tokens| tokens.last().cloned())
            .is_some_and(|token| token == ".")
    {
        return Err(
            "TOOTH-3B COVERED-EXACT-ARGV RED: final `.` must remain an independent argv token"
                .to_string(),
        );
    }
    if !documented_command_matches_argv(command, &evidence.invocation.literal_argv) {
        return Err(
            "TOOTH-3B COVERED-EXACT-ARGV RED: declared literal invocation is not a valid \
             token-for-token instance of the documented argv"
                .to_string(),
        );
    }

    let invocations = calls
        .iter()
        .filter(|call| {
            call.runner == evidence.invocation.runner
                && call.line == evidence.invocation.line
                && call.argv == evidence.invocation.literal_argv
        })
        .collect::<Vec<_>>();
    if invocations.len() != 1 {
        return Err(node_count_failure(
            "INVOCATION",
            invocations.len(),
            &evidence.case,
        ));
    }
    let bindings = binding_nodes(&function.body)
        .into_iter()
        .filter(|(name, line)| name == &evidence.binding.name && *line == evidence.binding.line)
        .collect::<Vec<_>>();
    if bindings.len() != 1 {
        return Err(node_count_failure(
            "BINDING",
            bindings.len(),
            &evidence.binding.name,
        ));
    }
    let invocation = invocations[0];
    if invocation.binding.as_deref() != Some(evidence.binding.name.as_str())
        || invocation.binding_line != Some(evidence.binding.line)
    {
        return Err(
            "TOOTH-3B COVERED-BINDING-NODE-ZERO RED: declared binding does not receive the \
             declared invocation"
                .to_string(),
        );
    }

    let allowed_macros = ["assert", "assert_eq", "assert_ne"];
    if !allowed_macros.contains(&evidence.assertion.macro_name.as_str()) {
        return Err(
            "TOOTH-3B COVERED-ASSERTION-MACRO RED: legal literal macros are \
             assert|assert_eq|assert_ne"
                .to_string(),
        );
    }
    let assertions = assertion_nodes(&function.body);
    let at_line = assertions
        .iter()
        .filter(|node| node.line == evidence.assertion.line)
        .collect::<Vec<_>>();
    let exact = at_line
        .iter()
        .filter(|node| node.name == evidence.assertion.macro_name && !node.path_qualified)
        .collect::<Vec<_>>();
    if exact.is_empty() && !at_line.is_empty() {
        return Err(
            "TOOTH-3B COVERED-ASSERTION-MACRO RED: declared node is not one literal \
             assert|assert_eq|assert_ne macro; path/suffix/custom wrappers are unsupported"
                .to_string(),
        );
    }
    if exact.len() != 1 {
        return Err(node_count_failure(
            "ASSERTION",
            exact.len(),
            &evidence.assertion.macro_name,
        ));
    }
    let assertion = exact[0];
    let operands = assertion_operands(assertion).ok_or_else(|| {
        "TOOTH-3B COVERED-ASSERTION-MACRO RED: assertion token tree has no supported operand shape"
            .to_string()
    })?;
    let operand_index = match (
        evidence.assertion.macro_name.as_str(),
        evidence.assertion.operand.as_str(),
    ) {
        ("assert", "condition") | ("assert_eq" | "assert_ne", "left") => 0,
        ("assert_eq" | "assert_ne", "right") => 1,
        _ => {
            return Err(
                "TOOTH-3B COVERED-ASSERTION-OPERAND RED: legal operands are \
                 assert.condition|assert_eq.left|assert_eq.right|assert_ne.left|assert_ne.right"
                    .to_string(),
            )
        }
    };
    let operand = operands.get(operand_index).ok_or_else(|| {
        "TOOTH-3B COVERED-ASSERTION-OPERAND RED: declared comparison operand is missing".to_string()
    })?;
    if !behavior_fact_in_tokens(
        operand,
        &evidence.binding.name,
        &evidence.assertion.behavior_fact,
    )? {
        return Err(
            "TOOTH-3B COVERED-BINDING-ASSERTION RED: declared binding behavior fact is absent \
             from the literal assertion condition/comparison operand"
                .to_string(),
        );
    }
    Ok(())
}

fn validate_negative_twin_hook(
    evidence: &CoveredEvidenceDeclaration,
    source: &str,
) -> Result<(), String> {
    if evidence.negative_twin.operation != "remove_text_literal"
        || !matches!(
            evidence.assertion.behavior_fact.as_str(),
            "stdout" | "stderr"
        )
        || evidence.negative_twin.remove_literal.is_empty()
        || evidence
            .negative_twin
            .replacement
            .contains(&evidence.negative_twin.remove_literal)
    {
        return Err(
            "TOOTH-3B NEGATIVE-TWIN-MUTATION RED: v3 legal mutation set is \
             remove_text_literal on stdout|stderr with a non-empty removed literal and \
             non-preserving replacement"
                .to_string(),
        );
    }
    if evidence.negative_twin.env_key != "TEAM_AGENT_COVERAGE_NEGATIVE_TWIN"
        || evidence.negative_twin.env_value.trim().is_empty()
    {
        return Err(
            "TOOTH-3B NEGATIVE-TWIN-ENV RED: the isolated twin must use \
             TEAM_AGENT_COVERAGE_NEGATIVE_TWIN and a non-empty entry-specific value"
                .to_string(),
        );
    }
    let syntax = rust_syntax_tokens(source)?;
    let functions = test_function_nodes(&syntax, &evidence.case);
    if functions.len() != 1 {
        return Err(node_count_failure(
            "FUNCTION",
            functions.len(),
            &evidence.case,
        ));
    }
    let assertions = assertion_nodes(&functions[0].body);
    let target = assertions
        .iter()
        .filter(|node| {
            node.line == evidence.assertion.line
                && node.name == evidence.assertion.macro_name
                && !node.path_qualified
        })
        .collect::<Vec<_>>();
    if target.len() != 1 {
        return Err(node_count_failure(
            "ASSERTION",
            target.len(),
            &evidence.assertion.macro_name,
        ));
    }
    let operands = assertion_operands(target[0]).ok_or_else(|| {
        "TOOTH-3B NEGATIVE-TWIN-TARGET RED: target assertion has no supported operand tree"
            .to_string()
    })?;
    let operand_index = match (
        evidence.assertion.macro_name.as_str(),
        evidence.assertion.operand.as_str(),
    ) {
        ("assert", "condition") | ("assert_eq" | "assert_ne", "left") => 0,
        ("assert_eq" | "assert_ne", "right") => 1,
        _ => {
            return Err(
                "TOOTH-3B NEGATIVE-TWIN-TARGET RED: target assertion operand declaration is \
                 unsupported"
                    .to_string(),
            )
        }
    };
    if !operands.get(operand_index).is_some_and(|operand| {
        string_literal_contains(operand, &evidence.negative_twin.remove_literal)
    }) {
        return Err(
            "TOOTH-3B NEGATIVE-TWIN-FACT RED: the declared mutation literal is not part of \
             the target behavior operand"
                .to_string(),
        );
    }
    if !string_literal_contains(&target[0].arguments, &evidence.assertion.failure_marker) {
        return Err(
            "TOOTH-3B NEGATIVE-TWIN-TARGET RED: target assertion does not contain its declared \
             failure marker"
                .to_string(),
        );
    }
    let hooks = negative_twin_hook_lines(&functions[0].body, evidence);
    if hooks.len() != 1 {
        return Err(node_count_failure(
            "NEGATIVE-TWIN-HOOK",
            hooks.len(),
            &evidence.negative_twin.env_value,
        ));
    }
    if hooks[0] >= evidence.assertion.line {
        return Err(
            "TOOTH-3B NEGATIVE-TWIN-HOOK-ORDER RED: the one-field mutation must occur before \
             the declared target assertion"
                .to_string(),
        );
    }
    Ok(())
}

fn node_count_failure(kind: &str, count: usize, _declared: &str) -> String {
    let cardinality = if count == 0 { "ZERO" } else { "MULTIPLE" };
    format!(
        "TOOTH-3B COVERED-{kind}-NODE-{cardinality} RED: explicit declaration must resolve \
         to exactly one Rust syntax node"
    )
}

fn covered_evidence_entries(
    manifest: &CoverageManifest,
) -> Vec<(&str, &CoveredEvidenceDeclaration)> {
    manifest
        .commands
        .iter()
        .filter_map(|entry| match entry {
            CoverageEntry::Covered {
                command,
                evidence: Some(evidence),
                ..
            } => Some((command.as_str(), evidence)),
            _ => None,
        })
        .collect()
}

fn same_assertion_node(
    left: &CoveredEvidenceDeclaration,
    right: &CoveredEvidenceDeclaration,
) -> bool {
    left.source_file == right.source_file
        && left.case == right.case
        && left.assertion.macro_name == right.assertion.macro_name
        && left.assertion.line == right.assertion.line
}

fn declared_assertion_is_top_level(evidence: &CoveredEvidenceDeclaration) -> Result<bool, String> {
    let source =
        std::fs::read_to_string(repo_root().join(&evidence.source_file)).map_err(|_| {
            "TOOTH-3B NEGATIVE-TWIN-SEQUENCE-OBSERVABILITY RED: legal sequence evidence \
             requires a readable declared source"
                .to_string()
        })?;
    let syntax = rust_syntax_tokens(&source)?;
    let functions = test_function_nodes(&syntax, &evidence.case);
    if functions.len() != 1 {
        return Ok(false);
    }
    Ok(top_level_assertion_nodes(&functions[0].body)
        .iter()
        .filter(|node| {
            node.line == evidence.assertion.line
                && node.name == evidence.assertion.macro_name
                && !node.path_qualified
        })
        .count()
        == 1)
}

fn emit_twin_cell_raw(row: usize, column: usize, outcome: &str, observed: &str) {
    eprintln!(
        "TOOTH-3B TWIN-DISCRIMINATION-CELL row={row} column={column} outcome={outcome} \
         RAW-BEGIN\n{observed}\nTOOTH-3B TWIN-DISCRIMINATION-CELL \
         row={row} column={column} RAW-END"
    );
}

fn parse_twin_observations(
    observed: &str,
    expected_nonce: &str,
    expected_cells: &BTreeSet<String>,
) -> Result<BTreeMap<String, TwinObservationResult>, String> {
    let mut parsed = BTreeMap::new();
    for line in observed.lines() {
        let trimmed = line.trim();
        if !trimmed.contains(TWIN_OBSERVATION_PREFIX) {
            continue;
        }
        let payload = trimmed
            .strip_prefix(TWIN_OBSERVATION_PREFIX)
            .ok_or_else(|| {
                "TOOTH-3B NEGATIVE-TWIN-OBSERVATION-PROTOCOL RED: marker prefix must begin \
                 the machine observation line"
                    .to_string()
            })?
            .trim();
        let fields = payload.split_whitespace().collect::<Vec<_>>();
        if fields.len() != 3 {
            return Err(
                "TOOTH-3B NEGATIVE-TWIN-OBSERVATION-PROTOCOL RED: marker must contain exactly \
                 nonce, cell, and result fields"
                    .to_string(),
            );
        }
        let nonce = fields[0].strip_prefix("nonce=").ok_or_else(|| {
            "TOOTH-3B NEGATIVE-TWIN-OBSERVATION-PROTOCOL RED: first marker field must be nonce"
                .to_string()
        })?;
        let cell = fields[1].strip_prefix("cell=").ok_or_else(|| {
            "TOOTH-3B NEGATIVE-TWIN-OBSERVATION-PROTOCOL RED: second marker field must be cell"
                .to_string()
        })?;
        let result = fields[2].strip_prefix("result=").ok_or_else(|| {
            "TOOTH-3B NEGATIVE-TWIN-OBSERVATION-PROTOCOL RED: third marker field must be result"
                .to_string()
        })?;
        if nonce != expected_nonce {
            return Err(format!(
                "TOOTH-3B NEGATIVE-TWIN-OBSERVATION-NONCE RED: stale or unrelated marker \
                 nonce {nonce:?} does not match this validator execution"
            ));
        }
        if !expected_cells.contains(cell) {
            return Err(format!(
                "TOOTH-3B NEGATIVE-TWIN-OBSERVATION-CELL RED: marker names undeclared cell \
                 {cell:?}; cells must come from the coverage manifest negative-twin catalog"
            ));
        }
        let result = match result {
            "PASS" => TwinObservationResult::Pass,
            "FAIL" => TwinObservationResult::Fail,
            other => {
                return Err(format!(
                    "TOOTH-3B NEGATIVE-TWIN-OBSERVATION-PROTOCOL RED: marker result \
                     {other:?} is neither PASS nor FAIL"
                ));
            }
        };
        if parsed.insert(cell.to_string(), result).is_some() {
            return Err(format!(
                "TOOTH-3B NEGATIVE-TWIN-OBSERVATION-DUPLICATE RED: cell {cell:?} emitted more \
                 than one marker for the same validator execution"
            ));
        }
    }
    Ok(parsed)
}

fn complete_twin_observations(
    observed: &str,
    expected_nonce: &str,
    expected_cells: &BTreeSet<String>,
) -> Result<Option<BTreeMap<String, TwinObservationResult>>, String> {
    let parsed = parse_twin_observations(observed, expected_nonce, expected_cells)?;
    if parsed.is_empty() {
        return Ok(None);
    }
    if parsed.len() != expected_cells.len() {
        let missing = expected_cells
            .iter()
            .filter(|cell| !parsed.contains_key(cell.as_str()))
            .cloned()
            .collect::<Vec<_>>();
        return Err(format!(
            "TOOTH-3B NEGATIVE-TWIN-OBSERVATION-CELLSET RED: observation emitted only a partial \
             declared cell set; missing={missing:?}"
        ));
    }
    Ok(Some(parsed))
}

fn twin_observation_nonce(row: usize) -> String {
    format!("tooth-3b-{}-{row}", std::process::id())
}

fn validate_observable_twin_discrimination(
    manifest: &CoverageManifest,
) -> Result<TwinDiscriminationOutcome, String> {
    // Observable closure:
    // - diagonal: the applied twin fails at its declared assertion;
    // - same node: that positive failure coordinate also proves a non-diagonal collision;
    // - earlier top-level assertion: reaching the later failure coordinate proves sequence
    //   advanced through the earlier assertion without panic.
    // A later assertion is not observable after an earlier panic unless the mapped case
    // implements the nonce-bound per-assertion observation protocol. Silence stays explicitly
    // pending rather than being inferred as a pass.
    let entries = covered_evidence_entries(manifest);
    let expected_cells = entries
        .iter()
        .map(|(_, evidence)| evidence.negative_twin.env_value.clone())
        .collect::<BTreeSet<_>>();
    let mut pending_cells = Vec::new();
    for (row, (command, applied)) in entries.iter().enumerate() {
        let (success, observed) = run_negative_twin_raw(applied)?;
        if success {
            return Err(format!(
                "TOOTH-3B NEGATIVE-TWIN-NOT-RED RED: A entry {command:?} stayed green after \
                 its declared key behavior fact was removed"
            ));
        }
        if !observed_at_declared_assertion(applied, &observed) {
            if let Some((column, _)) = entries
                .iter()
                .enumerate()
                .find(|(_, (_, other))| observed_at_declared_assertion(other, &observed))
            {
                emit_twin_cell_raw(
                    row,
                    column,
                    "OFF-DIAGONAL-FAILED-BEFORE-DECLARED-NODE",
                    &observed,
                );
                return Err(
                    "TOOTH-3B NEGATIVE-TWIN-NONINDEPENDENT-ASSERTION RED: legal observable \
                     cells require the applied twin to fail only at its own declared node; \
                     a non-diagonal declared assertion failed instead"
                        .to_string(),
                );
            }
            return Err(
                "TOOTH-3B NEGATIVE-TWIN-WRONG-FAILURE-SITE RED: twin must fail at the declared \
                 assertion line and marker, never in setup/parse/launcher"
                    .to_string(),
            );
        }
        emit_twin_cell_raw(row, row, "DIAGONAL-FAILED-AT-DECLARED-NODE", &observed);
        let observation_nonce = twin_observation_nonce(row);
        let observation_raw = run_twin_observation_raw(applied, &observation_nonce)?;
        let observations =
            complete_twin_observations(&observation_raw, &observation_nonce, &expected_cells)?;
        if let Some(cells) = observations.as_ref() {
            match cells.get(&applied.negative_twin.env_value) {
                Some(TwinObservationResult::Fail) => {}
                Some(TwinObservationResult::Pass) => {
                    return Err(format!(
                        "TOOTH-3B NEGATIVE-TWIN-OBSERVATION-APPLIED-CELL RED: A entry \
                         {command:?} reports PASS for its own applied twin cell"
                    ));
                }
                None => unreachable!("complete observation set contains every declared cell"),
            }
        }

        for (column, (_, other)) in entries.iter().enumerate() {
            if row == column {
                continue;
            }
            if same_assertion_node(applied, other) {
                emit_twin_cell_raw(row, column, "OFF-DIAGONAL-FAILED-AT-SHARED-NODE", &observed);
                return Err(
                    "TOOTH-3B NEGATIVE-TWIN-NONINDEPENDENT-ASSERTION RED: legal observable \
                     cells are diagonal failure at one declared node or prior top-level \
                     assertion pass before that node; one failure coordinate resolved to \
                     both the applied and a non-diagonal declaration"
                        .to_string(),
                );
            }
            if let Some(cells) = observations.as_ref() {
                match cells.get(&other.negative_twin.env_value) {
                    Some(TwinObservationResult::Pass) => {
                        emit_twin_cell_raw(
                            row,
                            column,
                            "OFF-DIAGONAL-PASSED-BY-OBSERVATION-PROTOCOL",
                            &observation_raw,
                        );
                        continue;
                    }
                    Some(TwinObservationResult::Fail) => {
                        emit_twin_cell_raw(
                            row,
                            column,
                            "OFF-DIAGONAL-FAILED-BY-OBSERVATION-PROTOCOL",
                            &observation_raw,
                        );
                        return Err("TOOTH-3B NEGATIVE-TWIN-NONINDEPENDENT-ASSERTION RED: \
                             nonce-bound observation reports that the applied twin also failed \
                             a non-diagonal declared assertion"
                            .to_string());
                    }
                    None => unreachable!("complete observation set contains every declared cell"),
                }
            }
            if applied.source_file != other.source_file || applied.case != other.case {
                return Err(
                    "TOOTH-3B NEGATIVE-TWIN-SEQUENCE-OBSERVABILITY RED: legal non-diagonal \
                     evidence across exact cases requires the nonce-bound observation protocol"
                        .to_string(),
                );
            }
            if other.assertion.line < applied.assertion.line {
                if !declared_assertion_is_top_level(applied)?
                    || !declared_assertion_is_top_level(other)?
                {
                    return Err(
                        "TOOTH-3B NEGATIVE-TWIN-SEQUENCE-OBSERVABILITY RED: legal sequence \
                         advancement requires both declarations to resolve to top-level \
                         assertions in the same exact case"
                            .to_string(),
                    );
                }
                emit_twin_cell_raw(
                    row,
                    column,
                    "OFF-DIAGONAL-PASSED-BY-SEQUENCE-ADVANCEMENT",
                    &observed,
                );
            } else if other.assertion.line > applied.assertion.line {
                eprintln!(
                    "TOOTH-3B TWIN-DISCRIMINATION-CELL row={row} column={column} \
                     outcome=PENDING-TWIN-OVERFLOW-INTO-LATER-ASSERTION: panic at the applied \
                     node stops the case before the later assertion can emit positive evidence"
                );
                pending_cells.push(PendingTwinCell {
                    row,
                    column,
                    outcome: "PENDING-TWIN-OVERFLOW-INTO-LATER-ASSERTION".to_string(),
                    detail: "panic at the applied node stops the case before the later assertion \
                             can emit positive evidence"
                        .to_string(),
                });
            } else {
                return Err(
                    "TOOTH-3B NEGATIVE-TWIN-SEQUENCE-OBSERVABILITY RED: distinct declarations \
                     on one source line have no machine-ordered pass credential"
                        .to_string(),
                );
            }
        }
    }
    if pending_cells.is_empty() {
        Ok(TwinDiscriminationOutcome::Complete)
    } else {
        Ok(TwinDiscriminationOutcome::Pending(pending_cells))
    }
}

fn assert_mapped_case_positive(source_file: &str, case: &str) -> Result<(), String> {
    let output = run_exact_e2e_case(source_file, case, None)?;
    let observed = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    if !output.status.success() || !observed.contains("running 1 test") {
        return Err(format!(
            "TOOTH-3B POSITIVE-CONTROL RED: declared A case must execute exactly once and pass \
             without a negative twin; exit={:?}",
            output.status.code()
        ));
    }
    Ok(())
}

fn assert_mapped_case_negative_twin(
    command: &str,
    evidence: &CoveredEvidenceDeclaration,
) -> Result<(), String> {
    run_negative_twin_at_declared_assertion(command, evidence).map(|_| ())
}

fn run_negative_twin_at_declared_assertion(
    command: &str,
    evidence: &CoveredEvidenceDeclaration,
) -> Result<String, String> {
    let (success, observed) = run_negative_twin_raw(evidence)?;
    if success {
        return Err(format!(
            "TOOTH-3B NEGATIVE-TWIN-NOT-RED RED: A entry {command:?} stayed green after its \
             declared key behavior fact was removed"
        ));
    }
    if !observed_at_declared_assertion(evidence, &observed) {
        return Err(
            "TOOTH-3B NEGATIVE-TWIN-WRONG-FAILURE-SITE RED: twin must fail at the declared \
             assertion line and marker, never in setup/parse/launcher"
                .to_string(),
        );
    }
    Ok(observed)
}

fn run_negative_twin_raw(evidence: &CoveredEvidenceDeclaration) -> Result<(bool, String), String> {
    let twin = &evidence.negative_twin;
    let output = run_exact_e2e_case_with_envs(
        &evidence.source_file,
        &evidence.case,
        &[(&twin.env_key, &twin.env_value)],
    )?;
    let observed = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    Ok((output.status.success(), observed))
}

fn run_twin_observation_raw(
    evidence: &CoveredEvidenceDeclaration,
    nonce: &str,
) -> Result<String, String> {
    let twin = &evidence.negative_twin;
    let scenario = std::env::var(TWIN_OBSERVATION_SCENARIO_ENV).ok();
    let child_nonce = match scenario.as_deref() {
        None => Some(nonce.to_string()),
        Some("missing") => None,
        Some("stale-nonce") => Some(format!("{nonce}-stale")),
        Some(other) => {
            return Err(format!(
                "TOOTH-3B NEGATIVE-TWIN-OBSERVATION-SCENARIO RED: unsupported verifier \
                 reachability scenario {other:?}"
            ));
        }
    };
    let mut envs = vec![(&twin.env_key[..], &twin.env_value[..])];
    if let Some(child_nonce) = child_nonce.as_ref() {
        envs.push((TWIN_OBSERVATION_NONCE_ENV, child_nonce));
    }
    let output = run_exact_e2e_case_with_envs(&evidence.source_file, &evidence.case, &envs)?;
    Ok(format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    ))
}

fn observed_at_declared_assertion(evidence: &CoveredEvidenceDeclaration, observed: &str) -> bool {
    let file = Path::new(&evidence.source_file)
        .file_name()
        .expect("validated evidence source filename")
        .to_string_lossy();
    let location = format!("{file}:{}:", evidence.assertion.line);
    observed.contains(&location) && observed.contains(&evidence.assertion.failure_marker)
}

fn run_exact_e2e_case(
    source_file: &str,
    case: &str,
    twin: Option<(&str, &str)>,
) -> Result<std::process::Output, String> {
    match twin {
        Some(pair) => run_exact_e2e_case_with_envs(source_file, case, &[pair]),
        None => run_exact_e2e_case_with_envs(source_file, case, &[]),
    }
}

fn run_exact_e2e_case_with_envs(
    source_file: &str,
    case: &str,
    envs: &[(&str, &str)],
) -> Result<std::process::Output, String> {
    let module = Path::new(source_file)
        .file_stem()
        .ok_or_else(|| {
            "TOOTH-3B NEGATIVE-TWIN-EXECUTOR RED: source has no module stem".to_string()
        })?
        .to_string_lossy();
    let mut command = Command::new(
        std::env::current_exe()
            .map_err(|error| format!("TOOTH-3B NEGATIVE-TWIN-EXECUTOR RED: {error}"))?,
    );
    command
        .arg(format!("cases::{module}::{case}"))
        .arg("--exact")
        .arg("--nocapture")
        .env_remove("TEAM_AGENT_COVERAGE_NEGATIVE_TWIN")
        .env_remove("TEAM_AGENT_COVERAGE_NEGATIVE_TWIN_EXECUTOR_CANARY")
        .env_remove(TWIN_OBSERVATION_NONCE_ENV)
        .env_remove("TMUX")
        .env_remove("TMUX_PANE");
    for (key, value) in envs {
        command.env(key, value);
    }
    command
        .output()
        .map_err(|error| format!("TOOTH-3B NEGATIVE-TWIN-EXECUTOR RED: {error}"))
}

fn assert_negative_twin_executor_canary() {
    let source_file = "crates/team-agent/tests/e2e/cases/gate_hole_061_red.rs";
    let source = std::fs::read_to_string(repo_root().join(source_file))
        .expect("read negative twin executor canary source");
    let syntax = rust_syntax_tokens(&source).expect("parse executor canary source");
    let functions = test_function_nodes(&syntax, "gate_hole_negative_twin_execution_canary_case");
    let assertions = assertion_nodes(&functions[0].body);
    let line_for = |marker: &str| {
        assertions
            .iter()
            .find(|node| string_literal_contains(&node.arguments, marker))
            .map(|node| node.line)
            .expect("executor canary panic marker")
    };
    let target_line = line_for("NEGATIVE-TWIN-TARGET-ASSERTION-CANARY");
    let setup_line = line_for("NEGATIVE-TWIN-SETUP-CANARY");
    let canary_evidence = CoveredEvidenceDeclaration {
        case: "gate_hole_negative_twin_execution_canary_case".to_string(),
        source_file: source_file.to_string(),
        invocation: InvocationDeclaration {
            runner: "run_ta".to_string(),
            line: 0,
            documented_argv: Vec::new(),
            literal_argv: Vec::new(),
        },
        binding: BindingDeclaration {
            name: "out".to_string(),
            line: 0,
        },
        assertion: AssertionDeclaration {
            macro_name: "assert".to_string(),
            line: target_line,
            operand: "condition".to_string(),
            behavior_fact: "stdout".to_string(),
            failure_marker: "NEGATIVE-TWIN-TARGET-ASSERTION-CANARY".to_string(),
        },
        negative_twin: NegativeTwinDeclaration {
            env_key: "TEAM_AGENT_COVERAGE_NEGATIVE_TWIN_EXECUTOR_CANARY".to_string(),
            env_value: "target".to_string(),
            operation: "remove_text_literal".to_string(),
            remove_literal: "canary".to_string(),
            replacement: "removed".to_string(),
        },
    };
    assert!(
        assert_mapped_case_negative_twin("executor-canary", &canary_evidence).is_ok(),
        "TOOTH-3B harness canary: target assertion exit must be accepted"
    );
    let mut wrong_site = canary_evidence.clone();
    wrong_site.negative_twin.env_value = "setup".to_string();
    assert_failure_signature(
        assert_mapped_case_negative_twin("executor-canary", &wrong_site),
        "NEGATIVE-TWIN-WRONG-FAILURE-SITE",
    );
    let mut no_red = canary_evidence;
    no_red.negative_twin.env_value = "not-triggered".to_string();
    assert_failure_signature(
        assert_mapped_case_negative_twin("executor-canary", &no_red),
        "NEGATIVE-TWIN-NOT-RED",
    );
    assert_ne!(target_line, setup_line);
}

fn assert_twin_observation_protocol_canary() {
    let nonce = "observation-canary";
    let expected = ["canary-first", "canary-second"]
        .into_iter()
        .map(str::to_string)
        .collect::<BTreeSet<_>>();
    let first = format!("{TWIN_OBSERVATION_PREFIX} nonce={nonce} cell=canary-first result=FAIL");
    let second = format!("{TWIN_OBSERVATION_PREFIX} nonce={nonce} cell=canary-second result=PASS");
    let valid = complete_twin_observations(&format!("{first}\n{second}"), nonce, &expected)
        .expect("TOOTH-3B observation canary: valid markers must parse")
        .expect("TOOTH-3B observation canary: valid markers must not become PENDING");
    assert_eq!(
        valid.get("canary-first"),
        Some(&TwinObservationResult::Fail)
    );
    assert_eq!(
        valid.get("canary-second"),
        Some(&TwinObservationResult::Pass)
    );
    assert_failure_signature(
        complete_twin_observations(&first.replace(nonce, "stale-canary"), nonce, &expected)
            .map(|_| ()),
        "NEGATIVE-TWIN-OBSERVATION-NONCE",
    );
    assert_failure_signature(
        complete_twin_observations(
            &first.replace("canary-first", "undeclared-cell"),
            nonce,
            &expected,
        )
        .map(|_| ()),
        "NEGATIVE-TWIN-OBSERVATION-CELL",
    );
    assert_failure_signature(
        complete_twin_observations(&format!("{first}\n{first}"), nonce, &expected).map(|_| ()),
        "NEGATIVE-TWIN-OBSERVATION-DUPLICATE",
    );
    assert_failure_signature(
        complete_twin_observations(
            &first.replace("result=FAIL", "result=UNKNOWN"),
            nonce,
            &expected,
        )
        .map(|_| ()),
        "NEGATIVE-TWIN-OBSERVATION-PROTOCOL",
    );
    assert_failure_signature(
        complete_twin_observations(&first, nonce, &expected).map(|_| ()),
        "NEGATIVE-TWIN-OBSERVATION-CELLSET",
    );
    assert_eq!(
        complete_twin_observations("no observation markers", nonce, &expected)
            .expect("TOOTH-3B observation canary: marker absence must be evaluable"),
        None,
        "TOOTH-3B observation canary: marker absence must remain typed PENDING input"
    );
}

fn assert_twin_discrimination_canary() {
    assert_twin_observation_protocol_canary();

    let source_file = "crates/team-agent/tests/e2e/cases/gate_hole_061_red.rs";
    let case = "gate_hole_twin_discrimination_canary_case";
    let source = std::fs::read_to_string(repo_root().join(source_file))
        .expect("read twin discrimination canary source");
    let syntax = rust_syntax_tokens(&source).expect("parse twin discrimination canary source");
    let functions = test_function_nodes(&syntax, case);
    let assertions = assertion_nodes(&functions[0].body);
    let line_for = |marker: &str| {
        assertions
            .iter()
            .find(|node| string_literal_contains(&node.arguments, marker))
            .map(|node| node.line)
            .expect("twin discrimination canary marker")
    };
    let evidence_for = |marker: &str, twin: &str| {
        let mut evidence = canary_evidence("assert_ne", "left", "stdout", line_for(marker));
        evidence.case = case.to_string();
        evidence.source_file = source_file.to_string();
        evidence.assertion.failure_marker = marker.to_string();
        evidence.negative_twin.env_value = twin.to_string();
        evidence
    };
    let first = evidence_for("NEGATIVE-TWIN-MATRIX-FIRST-CANARY", "matrix-first");
    let second = evidence_for("NEGATIVE-TWIN-MATRIX-SECOND-CANARY", "matrix-second");
    let honest = CoverageManifest {
        schema_version: "team-agent-skill-command-coverage-v3".to_string(),
        authority: None,
        commands: vec![
            covered_canary_entry("team-agent matrix-first", first.clone()),
            covered_canary_entry("team-agent matrix-second", second),
        ],
    };
    let honest_outcome = validate_observable_twin_discrimination(&honest)
        .expect("TOOTH-3B twin discrimination canary: recorded matrix must be evaluable");
    let TwinDiscriminationOutcome::Pending(pending_cells) = honest_outcome.clone() else {
        panic!("TOOTH-3B direction canary: the recorded later-assertion cell must remain PENDING");
    };
    assert_eq!(
        pending_cells
            .iter()
            .map(|cell| (cell.row, cell.column, cell.outcome.as_str()))
            .collect::<Vec<_>>(),
        vec![(0, 1, "PENDING-TWIN-OVERFLOW-INTO-LATER-ASSERTION")],
        "TOOTH-3B direction canary: the recorded upper cell must be identified positively"
    );
    let pending_verdict = GateVerdict::from_validation(Ok(honest_outcome));
    assert_eq!(pending_verdict.status, GateTerminalStatus::Pending);
    assert!(
        !pending_verdict.allows_success(),
        "TOOTH-3B direction canary: PENDING must select the non-green terminal branch"
    );
    assert_eq!(
        gate_verdict_value(&pending_verdict)["status"],
        Value::String("PENDING".to_string()),
        "TOOTH-3B direction canary: PENDING must survive in the structured verdict"
    );
    let green_verdict = GateVerdict::from_validation(Ok(TwinDiscriminationOutcome::Complete));
    assert!(green_verdict.allows_success());
    let red_verdict = GateVerdict::from_validation(Err("direction-canary-red".to_string()));
    assert_eq!(red_verdict.status, GateTerminalStatus::Red);
    assert!(!red_verdict.allows_success());

    let collision = CoverageManifest {
        schema_version: honest.schema_version.clone(),
        authority: None,
        commands: vec![
            covered_canary_entry("team-agent matrix-first", first.clone()),
            covered_canary_entry("team-agent matrix-collision", first),
        ],
    };
    assert_failure_signature(
        validate_observable_twin_discrimination(&collision).map(|_| ()),
        "NEGATIVE-TWIN-NONINDEPENDENT-ASSERTION",
    );
}

fn rust_syntax_tokens(source: &str) -> Result<Vec<RustToken>, String> {
    RustTokenParser {
        source,
        index: 0,
        line: 1,
    }
    .parse_sequence(None)
    .map_err(|failure| format!("TOOTH-3B COVERED-SYNTAX-PARSE RED: {failure}"))
}

struct RustTokenParser<'a> {
    source: &'a str,
    index: usize,
    line: usize,
}

impl RustTokenParser<'_> {
    fn parse_sequence(&mut self, closing: Option<u8>) -> Result<Vec<RustToken>, String> {
        let mut tokens = Vec::new();
        while self.index < self.source.len() {
            self.skip_space_and_comments()?;
            if self.index >= self.source.len() {
                break;
            }
            let byte = self.source.as_bytes()[self.index];
            if Some(byte) == closing {
                self.index += 1;
                return Ok(tokens);
            }
            if matches!(byte, b')' | b']' | b'}') {
                return Err(format!(
                    "unexpected closing delimiter on line {}",
                    self.line
                ));
            }
            let line = self.line;
            if let Some((delimiter, close)) = match byte {
                b'(' => Some(('(', b')')),
                b'[' => Some(('[', b']')),
                b'{' => Some(('{', b'}')),
                _ => None,
            } {
                self.index += 1;
                let nested = self.parse_sequence(Some(close))?;
                tokens.push(RustToken {
                    kind: RustTokenKind::Group {
                        delimiter,
                        tokens: nested,
                    },
                    line,
                });
                continue;
            }
            if let Some(value) = self.take_raw_string()? {
                tokens.push(RustToken {
                    kind: RustTokenKind::StringLiteral(value),
                    line,
                });
                continue;
            }
            if byte == b'"' {
                tokens.push(RustToken {
                    kind: RustTokenKind::StringLiteral(self.take_string()?),
                    line,
                });
                continue;
            }
            if byte == b'\'' {
                if self.take_char_literal()? {
                    tokens.push(RustToken {
                        kind: RustTokenKind::CharLiteral,
                        line,
                    });
                } else {
                    self.index += 1;
                    tokens.push(RustToken {
                        kind: RustTokenKind::Punct('\''),
                        line,
                    });
                }
                continue;
            }
            if byte.is_ascii_alphabetic() || byte == b'_' {
                let start = self.index;
                self.index += 1;
                while self
                    .source
                    .as_bytes()
                    .get(self.index)
                    .is_some_and(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
                {
                    self.index += 1;
                }
                tokens.push(RustToken {
                    kind: RustTokenKind::Ident(self.source[start..self.index].to_string()),
                    line,
                });
                continue;
            }
            if byte.is_ascii_digit() {
                let start = self.index;
                self.index += 1;
                while self
                    .source
                    .as_bytes()
                    .get(self.index)
                    .is_some_and(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
                {
                    self.index += 1;
                }
                tokens.push(RustToken {
                    kind: RustTokenKind::Number(self.source[start..self.index].to_string()),
                    line,
                });
                continue;
            }
            if byte.is_ascii() {
                self.index += 1;
                tokens.push(RustToken {
                    kind: RustTokenKind::Punct(byte as char),
                    line,
                });
                continue;
            }
            return Err(format!(
                "unsupported non-ASCII syntax token on line {}",
                self.line
            ));
        }
        if closing.is_some() {
            Err("unclosed delimiter".to_string())
        } else {
            Ok(tokens)
        }
    }

    fn skip_space_and_comments(&mut self) -> Result<(), String> {
        loop {
            while let Some(byte) = self.source.as_bytes().get(self.index) {
                if !byte.is_ascii_whitespace() {
                    break;
                }
                if *byte == b'\n' {
                    self.line += 1;
                }
                self.index += 1;
            }
            if self.source.as_bytes().get(self.index..self.index + 2) == Some(b"//") {
                while let Some(byte) = self.source.as_bytes().get(self.index) {
                    self.index += 1;
                    if *byte == b'\n' {
                        self.line += 1;
                        break;
                    }
                }
                continue;
            }
            if self.source.as_bytes().get(self.index..self.index + 2) == Some(b"/*") {
                self.index += 2;
                let mut depth = 1usize;
                while self.index < self.source.len() && depth > 0 {
                    if self.source.as_bytes().get(self.index..self.index + 2) == Some(b"/*") {
                        depth += 1;
                        self.index += 2;
                    } else if self.source.as_bytes().get(self.index..self.index + 2) == Some(b"*/")
                    {
                        depth -= 1;
                        self.index += 2;
                    } else {
                        if self.source.as_bytes()[self.index] == b'\n' {
                            self.line += 1;
                        }
                        self.index += 1;
                    }
                }
                if depth != 0 {
                    return Err("unclosed block comment".to_string());
                }
                continue;
            }
            return Ok(());
        }
    }

    fn take_raw_string(&mut self) -> Result<Option<String>, String> {
        let start = self.index;
        let bytes = self.source.as_bytes();
        let prefix = if bytes.get(start..start + 2) == Some(b"br") {
            2
        } else if bytes.get(start) == Some(&b'r') {
            1
        } else {
            return Ok(None);
        };
        let mut cursor = start + prefix;
        while bytes.get(cursor) == Some(&b'#') {
            cursor += 1;
        }
        if bytes.get(cursor) != Some(&b'"') {
            return Ok(None);
        }
        let hashes = cursor - start - prefix;
        let content_start = cursor + 1;
        cursor = content_start;
        loop {
            let Some(byte) = bytes.get(cursor) else {
                return Err("unclosed raw string".to_string());
            };
            if *byte == b'\n' {
                self.line += 1;
            }
            if *byte == b'"'
                && bytes
                    .get(cursor + 1..cursor + 1 + hashes)
                    .is_some_and(|tail| tail.iter().all(|byte| *byte == b'#'))
            {
                let value = self.source[content_start..cursor].to_string();
                self.index = cursor + 1 + hashes;
                return Ok(Some(value));
            }
            cursor += 1;
        }
    }

    fn take_string(&mut self) -> Result<String, String> {
        let start = self.index;
        self.index += 1;
        let mut escaped = false;
        while self.index < self.source.len() {
            let byte = self.source.as_bytes()[self.index];
            if byte == b'\n' {
                self.line += 1;
            }
            self.index += 1;
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                let raw = &self.source[start..self.index];
                return decode_rust_string(raw);
            }
        }
        Err("unclosed string literal".to_string())
    }

    fn take_char_literal(&mut self) -> Result<bool, String> {
        let start = self.index;
        let bytes = self.source.as_bytes();
        let Some(first) = bytes.get(start + 1) else {
            return Ok(false);
        };
        let mut cursor = start + 1;
        if *first == b'\\' {
            cursor += 2;
            if *bytes.get(start + 2).unwrap_or(&0) == b'u' {
                while bytes.get(cursor) != Some(&b'}') {
                    cursor += 1;
                    if cursor >= bytes.len() {
                        return Err("unclosed unicode char literal".to_string());
                    }
                }
                cursor += 1;
            } else if *bytes.get(start + 2).unwrap_or(&0) == b'x' {
                cursor = start + 5;
            }
        } else {
            let ch = self.source[start + 1..]
                .chars()
                .next()
                .ok_or_else(|| "unclosed char literal".to_string())?;
            cursor += ch.len_utf8();
        }
        if bytes.get(cursor) != Some(&b'\'') {
            return Ok(false);
        }
        self.index = cursor + 1;
        Ok(true)
    }
}

fn decode_rust_string(raw: &str) -> Result<String, String> {
    let content = raw
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .ok_or_else(|| "invalid quoted Rust string".to_string())?;
    let mut chars = content.chars().peekable();
    let mut decoded = String::new();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            decoded.push(ch);
            continue;
        }
        let escaped = chars
            .next()
            .ok_or_else(|| "trailing Rust string escape".to_string())?;
        match escaped {
            '\\' => decoded.push('\\'),
            '"' => decoded.push('"'),
            '\'' => decoded.push('\''),
            'n' => decoded.push('\n'),
            'r' => decoded.push('\r'),
            't' => decoded.push('\t'),
            '0' => decoded.push('\0'),
            'x' => {
                let hex = [chars.next(), chars.next()];
                let digits = hex
                    .into_iter()
                    .collect::<Option<String>>()
                    .ok_or_else(|| "short \\x escape".to_string())?;
                let value = u8::from_str_radix(&digits, 16)
                    .map_err(|_| "invalid \\x escape".to_string())?;
                decoded.push(value as char);
            }
            'u' => {
                if chars.next() != Some('{') {
                    return Err("invalid unicode escape".to_string());
                }
                let mut digits = String::new();
                for digit in chars.by_ref() {
                    if digit == '}' {
                        break;
                    }
                    digits.push(digit);
                }
                let value = u32::from_str_radix(&digits, 16)
                    .ok()
                    .and_then(char::from_u32)
                    .ok_or_else(|| "invalid unicode escape".to_string())?;
                decoded.push(value);
            }
            '\n' => {
                while chars.peek().is_some_and(|ch| ch.is_whitespace()) {
                    chars.next();
                }
            }
            other => return Err(format!("unsupported Rust string escape \\{other}")),
        }
    }
    Ok(decoded)
}

fn test_function_nodes(tokens: &[RustToken], case: &str) -> Vec<FunctionNode> {
    let mut functions = Vec::new();
    collect_test_function_nodes(tokens, case, &mut functions);
    functions
}

fn collect_test_function_nodes(
    tokens: &[RustToken],
    case: &str,
    functions: &mut Vec<FunctionNode>,
) {
    for index in 0..tokens.len() {
        if token_ident(tokens.get(index)) == Some("fn")
            && token_ident(tokens.get(index + 1)) == Some(case)
            && has_test_attribute(tokens, index)
        {
            let body = tokens[index + 2..]
                .iter()
                .find_map(|token| match &token.kind {
                    RustTokenKind::Group {
                        delimiter: '{',
                        tokens,
                    } => Some(tokens.clone()),
                    _ => None,
                });
            if let Some(body) = body {
                functions.push(FunctionNode { body });
            }
        }
        if let RustTokenKind::Group { tokens: nested, .. } = &tokens[index].kind {
            collect_test_function_nodes(nested, case, functions);
        }
    }
}

fn has_test_attribute(tokens: &[RustToken], fn_index: usize) -> bool {
    fn_index >= 2
        && matches!(tokens[fn_index - 2].kind, RustTokenKind::Punct('#'))
        && matches!(
            &tokens[fn_index - 1].kind,
            RustTokenKind::Group {
                delimiter: '[',
                tokens
            } if tokens.len() == 1 && token_ident(tokens.first()) == Some("test")
        )
}

fn module_declaration_count(tokens: &[RustToken], module: &str) -> usize {
    let mut count = 0;
    for index in 0..tokens.len() {
        if token_ident(tokens.get(index)) == Some("mod")
            && token_ident(tokens.get(index + 1)) == Some(module)
        {
            count += 1;
        }
        if let RustTokenKind::Group { tokens: nested, .. } = &tokens[index].kind {
            count += module_declaration_count(nested, module);
        }
    }
    count
}

fn run_ta_calls(tokens: &[RustToken]) -> Vec<RunTaCall> {
    let mut calls = Vec::new();
    collect_run_ta_calls(tokens, &mut calls);
    calls
}

fn collect_run_ta_calls(tokens: &[RustToken], calls: &mut Vec<RunTaCall>) {
    for index in 0..tokens.len() {
        let Some(runner) = token_ident(tokens.get(index)) else {
            if let RustTokenKind::Group { tokens: nested, .. } = &tokens[index].kind {
                collect_run_ta_calls(nested, calls);
            }
            continue;
        };
        if matches!(runner, "run_ta" | "run_ta_env") {
            if let Some(arguments) = token_group(tokens.get(index + 1), '(') {
                if let Some(argv) = literal_argv(arguments) {
                    let binding = binding_before(tokens, index);
                    calls.push(RunTaCall {
                        runner: runner.to_string(),
                        binding: binding.as_ref().map(|(name, _)| name.clone()),
                        binding_line: binding.map(|(_, line)| line),
                        line: tokens[index].line,
                        argv,
                        has_path_override: string_literal_in_tokens(arguments, "PATH"),
                    });
                }
            }
        }
        if let RustTokenKind::Group { tokens: nested, .. } = &tokens[index].kind {
            collect_run_ta_calls(nested, calls);
        }
    }
}

fn binding_before(tokens: &[RustToken], call_index: usize) -> Option<(String, usize)> {
    let start = tokens[..call_index]
        .iter()
        .rposition(|token| matches!(token.kind, RustTokenKind::Punct(';')))
        .map_or(0, |index| index + 1);
    let statement = &tokens[start..call_index];
    let let_index = statement
        .iter()
        .position(|token| token_ident(Some(token)) == Some("let"))?;
    let mut binding_index = let_index + 1;
    if token_ident(statement.get(binding_index)) == Some("mut") {
        binding_index += 1;
    }
    let binding = token_ident(statement.get(binding_index))?;
    statement[binding_index + 1..]
        .iter()
        .any(|token| matches!(token.kind, RustTokenKind::Punct('=')))
        .then(|| (binding.to_string(), statement[binding_index].line))
}

fn binding_nodes(tokens: &[RustToken]) -> Vec<(String, usize)> {
    let mut nodes = Vec::new();
    for index in 0..tokens.len() {
        if token_ident(tokens.get(index)) == Some("let") {
            let mut binding_index = index + 1;
            if token_ident(tokens.get(binding_index)) == Some("mut") {
                binding_index += 1;
            }
            if let Some(binding) = token_ident(tokens.get(binding_index)) {
                nodes.push((binding.to_string(), tokens[binding_index].line));
            }
        }
        if let RustTokenKind::Group { tokens: nested, .. } = &tokens[index].kind {
            nodes.extend(binding_nodes(nested));
        }
    }
    nodes
}

fn literal_argv(tokens: &[RustToken]) -> Option<Vec<String>> {
    for index in 0..tokens.len().saturating_sub(1) {
        if matches!(tokens[index].kind, RustTokenKind::Punct('&')) {
            if let Some(array) = token_group(tokens.get(index + 1), '[') {
                if let Some(argv) = literal_string_array(array) {
                    return Some(argv);
                }
            }
        }
    }
    None
}

fn literal_string_array(tokens: &[RustToken]) -> Option<Vec<String>> {
    let mut values = Vec::new();
    for token in tokens {
        match &token.kind {
            RustTokenKind::StringLiteral(value) => values.push(value.clone()),
            RustTokenKind::Punct(',') => {}
            _ => return None,
        }
    }
    (!values.is_empty()).then_some(values)
}

fn assertion_nodes(tokens: &[RustToken]) -> Vec<AssertionNode> {
    let mut assertions = Vec::new();
    collect_assertion_nodes(tokens, &mut assertions);
    assertions
}

fn top_level_assertion_nodes(tokens: &[RustToken]) -> Vec<AssertionNode> {
    let mut assertions = Vec::new();
    collect_assertion_nodes_at_current_level(tokens, &mut assertions);
    assertions
}

fn collect_assertion_nodes_at_current_level(
    tokens: &[RustToken],
    assertions: &mut Vec<AssertionNode>,
) {
    for index in 0..tokens.len() {
        if let (Some(name), Some(arguments)) = (
            token_ident(tokens.get(index)),
            (matches!(
                tokens.get(index + 1).map(|token| &token.kind),
                Some(RustTokenKind::Punct('!'))
            ))
            .then(|| token_group(tokens.get(index + 2), '('))
            .flatten(),
        ) {
            let path_qualified = index >= 2
                && matches!(tokens[index - 1].kind, RustTokenKind::Punct(':'))
                && matches!(tokens[index - 2].kind, RustTokenKind::Punct(':'));
            assertions.push(AssertionNode {
                name: name.to_string(),
                line: tokens[index].line,
                path_qualified,
                arguments: arguments.to_vec(),
            });
        }
    }
}

fn collect_assertion_nodes(tokens: &[RustToken], assertions: &mut Vec<AssertionNode>) {
    collect_assertion_nodes_at_current_level(tokens, assertions);
    for token in tokens {
        if let RustTokenKind::Group { tokens: nested, .. } = &token.kind {
            collect_assertion_nodes(nested, assertions);
        }
    }
}

fn assertion_operands(assertion: &AssertionNode) -> Option<Vec<&[RustToken]>> {
    let mut operands = Vec::new();
    let mut start = 0;
    for (index, token) in assertion.arguments.iter().enumerate() {
        if matches!(token.kind, RustTokenKind::Punct(',')) {
            operands.push(&assertion.arguments[start..index]);
            start = index + 1;
        }
    }
    operands.push(&assertion.arguments[start..]);
    match assertion.name.as_str() {
        "assert" if !operands.is_empty() => Some(operands),
        "assert_eq" | "assert_ne" if operands.len() >= 2 => Some(operands),
        _ => None,
    }
}

fn behavior_fact_in_tokens(
    tokens: &[RustToken],
    binding: &str,
    fact: &str,
) -> Result<bool, String> {
    if !matches!(
        fact,
        "stdout" | "stderr" | "exit_code" | "is_success" | "json" | "quick_start_launched"
    ) {
        return Err("TOOTH-3B COVERED-BEHAVIOR-FACT RED: legal facts are \
             stdout|stderr|exit_code|is_success|json|quick_start_launched"
            .to_string());
    }
    let mut rejected_contexts = BTreeSet::new();
    if behavior_fact_required_by_expression(tokens, binding, fact, &mut rejected_contexts) {
        return Ok(true);
    }
    if let Some(context) = rejected_contexts.into_iter().next() {
        return Err(format!(
            "TOOTH-3B COVERED-BINDING-NESTED-{context} RED: legal behavior evidence must be \
             required by the assertion condition/comparison expression; references confined \
             to nested diagnostic macros or closure bodies are not admitted"
        ));
    }
    Ok(false)
}

fn behavior_fact_required_by_expression(
    tokens: &[RustToken],
    binding: &str,
    fact: &str,
    rejected_contexts: &mut BTreeSet<&'static str>,
) -> bool {
    // This bounded semantic subset admits a direct binding fact (including transparent nested
    // groups) only when every top-level `||` alternative still requires it. Macro argument trees
    // and closure bodies are classified separately and never become behavior evidence merely
    // because they contain the same tokens.
    let or_branches = logical_or_branches(tokens);
    if or_branches.len() > 1 {
        let mut every_branch_requires_fact = true;
        for branch in or_branches {
            every_branch_requires_fact &=
                behavior_fact_required_by_expression(branch, binding, fact, rejected_contexts);
        }
        return every_branch_requires_fact;
    }

    let mut found = false;
    let mut index = 0;
    while index < tokens.len() {
        if let Some((macro_name, arguments)) = nested_macro_at(tokens, index) {
            if raw_behavior_fact_in_tokens(arguments, binding, fact) {
                rejected_contexts.insert(nested_macro_context(macro_name));
            }
            index += 3;
            continue;
        }
        if let Some(body) = closure_body_at(tokens, index) {
            if raw_behavior_fact_in_tokens(body, binding, fact) {
                rejected_contexts.insert("CLOSURE");
            }
            break;
        }
        if fact == "quick_start_launched"
            && token_ident(tokens.get(index)) == Some("quick_start_launched")
            && token_group(tokens.get(index + 1), '(')
                .is_some_and(|arguments| identifier_in_tokens(arguments, binding))
        {
            found = true;
            index += 2;
            continue;
        }
        if index + 2 < tokens.len()
            && token_ident(tokens.get(index)) == Some(binding)
            && matches!(tokens[index + 1].kind, RustTokenKind::Punct('.'))
            && token_ident(tokens.get(index + 2)) == Some(fact)
        {
            found = true;
            index += 3;
            continue;
        }
        if let RustTokenKind::Group { tokens: nested, .. } = &tokens[index].kind {
            found |= behavior_fact_required_by_expression(nested, binding, fact, rejected_contexts);
        }
        index += 1;
    }
    found
}

fn raw_behavior_fact_in_tokens(tokens: &[RustToken], binding: &str, fact: &str) -> bool {
    if fact == "quick_start_launched"
        && tokens.windows(2).any(|window| {
            token_ident(window.first()) == Some("quick_start_launched")
                && token_group(window.get(1), '(')
                    .is_some_and(|arguments| identifier_in_tokens(arguments, binding))
        })
    {
        return true;
    }
    if tokens.windows(3).any(|window| {
        token_ident(window.first()) == Some(binding)
            && matches!(window[1].kind, RustTokenKind::Punct('.'))
            && token_ident(window.get(2)) == Some(fact)
    }) {
        return true;
    }
    tokens.iter().any(|token| match &token.kind {
        RustTokenKind::Group { tokens: nested, .. } => {
            raw_behavior_fact_in_tokens(nested, binding, fact)
        }
        _ => false,
    })
}

fn logical_or_branches(tokens: &[RustToken]) -> Vec<&[RustToken]> {
    let mut branches = Vec::new();
    let mut start = 0;
    let mut index = 0;
    while index + 1 < tokens.len() {
        let is_double_pipe = matches!(tokens[index].kind, RustTokenKind::Punct('|'))
            && matches!(tokens[index + 1].kind, RustTokenKind::Punct('|'));
        if is_double_pipe && !is_expression_start(tokens, index) {
            branches.push(&tokens[start..index]);
            start = index + 2;
            index += 2;
            continue;
        }
        index += 1;
    }
    if start == 0 {
        vec![tokens]
    } else {
        branches.push(&tokens[start..]);
        branches
    }
}

fn is_expression_start(tokens: &[RustToken], index: usize) -> bool {
    if index == 0 {
        return true;
    }
    matches!(
        tokens[index - 1].kind,
        RustTokenKind::Punct(',') | RustTokenKind::Punct('=') | RustTokenKind::Punct('>')
    ) || token_ident(tokens.get(index - 1)) == Some("move")
}

fn nested_macro_at(tokens: &[RustToken], index: usize) -> Option<(&str, &[RustToken])> {
    let name = token_ident(tokens.get(index))?;
    matches!(
        tokens.get(index + 1).map(|token| &token.kind),
        Some(RustTokenKind::Punct('!'))
    )
    .then(|| token_group(tokens.get(index + 2), '('))
    .flatten()
    .map(|arguments| (name, arguments))
}

fn nested_macro_context(name: &str) -> &'static str {
    match name {
        "panic" => "PANIC",
        "format" | "format_args" => "FORMAT",
        "write" | "writeln" => "WRITE",
        "debug_assert" | "debug_assert_eq" | "debug_assert_ne" => "DEBUG-ASSERT",
        "assert" | "assert_eq" | "assert_ne" => "ASSERT",
        _ => "MACRO",
    }
}

fn closure_body_at(tokens: &[RustToken], index: usize) -> Option<&[RustToken]> {
    if !matches!(tokens.get(index)?.kind, RustTokenKind::Punct('|'))
        || !is_expression_start(tokens, index)
    {
        return None;
    }
    let closing = tokens[index + 1..]
        .iter()
        .position(|token| matches!(token.kind, RustTokenKind::Punct('|')))
        .map(|offset| index + 1 + offset)?;
    Some(&tokens[closing + 1..])
}

fn negative_twin_hook_lines(
    tokens: &[RustToken],
    evidence: &CoveredEvidenceDeclaration,
) -> Vec<usize> {
    let mut lines = Vec::new();
    collect_negative_twin_hook_lines(tokens, evidence, &mut lines);
    lines
}

fn collect_negative_twin_hook_lines(
    tokens: &[RustToken],
    evidence: &CoveredEvidenceDeclaration,
    lines: &mut Vec<usize>,
) {
    for index in 0..tokens.len() {
        if token_ident(tokens.get(index)) == Some("if") {
            if let Some((body_index, body)) =
                tokens[index + 1..]
                    .iter()
                    .enumerate()
                    .find_map(|(offset, token)| {
                        token_group(Some(token), '{').map(|body| (index + 1 + offset, body))
                    })
            {
                let condition = &tokens[index + 1..body_index];
                if negative_twin_condition_matches(condition, &evidence.negative_twin)
                    && negative_twin_body_matches(body, evidence)
                {
                    lines.push(tokens[index].line);
                }
            }
        }
        if let RustTokenKind::Group { tokens: nested, .. } = &tokens[index].kind {
            collect_negative_twin_hook_lines(nested, evidence, lines);
        }
    }
}

fn negative_twin_condition_matches(tokens: &[RustToken], twin: &NegativeTwinDeclaration) -> bool {
    syntax_atoms(tokens)
        == vec![
            "i:std".to_string(),
            "p::".to_string(),
            "p::".to_string(),
            "i:env".to_string(),
            "p::".to_string(),
            "p::".to_string(),
            "i:var".to_string(),
            "g:(".to_string(),
            format!("s:{}", twin.env_key),
            "g:)".to_string(),
            "p:.".to_string(),
            "i:as_deref".to_string(),
            "g:(".to_string(),
            "g:)".to_string(),
            "p:=".to_string(),
            "p:=".to_string(),
            "i:Ok".to_string(),
            "g:(".to_string(),
            format!("s:{}", twin.env_value),
            "g:)".to_string(),
        ]
}

fn negative_twin_body_matches(tokens: &[RustToken], evidence: &CoveredEvidenceDeclaration) -> bool {
    let field = evidence.assertion.behavior_fact.as_str();
    let expected = vec![
        format!("i:{}", evidence.binding.name),
        "p:.".to_string(),
        format!("i:{field}"),
        "p:=".to_string(),
        format!("i:{}", evidence.binding.name),
        "p:.".to_string(),
        format!("i:{field}"),
        "p:.".to_string(),
        "i:replacen".to_string(),
        "g:(".to_string(),
        format!("s:{}", evidence.negative_twin.remove_literal),
        "p:,".to_string(),
        format!("s:{}", evidence.negative_twin.replacement),
        "p:,".to_string(),
        "n:1".to_string(),
        "g:)".to_string(),
        "p:;".to_string(),
    ];
    let mut expected_with_trailing_comma = expected.clone();
    expected_with_trailing_comma.insert(expected.len() - 2, "p:,".to_string());
    let observed = syntax_atoms(tokens);
    observed == expected || observed == expected_with_trailing_comma
}

fn syntax_atoms(tokens: &[RustToken]) -> Vec<String> {
    let mut atoms = Vec::new();
    for token in tokens {
        match &token.kind {
            RustTokenKind::Ident(value) => atoms.push(format!("i:{value}")),
            RustTokenKind::StringLiteral(value) => atoms.push(format!("s:{value}")),
            RustTokenKind::Number(value) => atoms.push(format!("n:{value}")),
            RustTokenKind::CharLiteral => atoms.push("char".to_string()),
            RustTokenKind::Punct(value) => atoms.push(format!("p:{value}")),
            RustTokenKind::Group { delimiter, tokens } => {
                atoms.push(format!("g:{delimiter}"));
                atoms.extend(syntax_atoms(tokens));
                atoms.push(format!(
                    "g:{}",
                    match delimiter {
                        '(' => ')',
                        '[' => ']',
                        '{' => '}',
                        _ => '?',
                    }
                ));
            }
        }
    }
    atoms
}

fn binding_assigned_named_call(tokens: &[RustToken], binding: &str, function: &str) -> bool {
    for index in 0..tokens.len() {
        if token_ident(tokens.get(index)) == Some(function)
            && token_group(tokens.get(index + 1), '(').is_some()
            && binding_before(tokens, index).is_some_and(|(observed, _)| observed == binding)
        {
            return true;
        }
        if let RustTokenKind::Group { tokens: nested, .. } = &tokens[index].kind {
            if binding_assigned_named_call(nested, binding, function) {
                return true;
            }
        }
    }
    false
}

fn identifier_in_tokens(tokens: &[RustToken], identifier: &str) -> bool {
    tokens.iter().any(|token| match &token.kind {
        RustTokenKind::Ident(value) => value == identifier,
        RustTokenKind::Group { tokens, .. } => identifier_in_tokens(tokens, identifier),
        _ => false,
    })
}

fn string_literal_in_tokens(tokens: &[RustToken], expected: &str) -> bool {
    tokens.iter().any(|token| match &token.kind {
        RustTokenKind::StringLiteral(value) => value == expected,
        RustTokenKind::Group { tokens, .. } => string_literal_in_tokens(tokens, expected),
        _ => false,
    })
}

fn string_literal_contains(tokens: &[RustToken], expected: &str) -> bool {
    tokens.iter().any(|token| match &token.kind {
        RustTokenKind::StringLiteral(value) => value.contains(expected),
        RustTokenKind::Group { tokens, .. } => string_literal_contains(tokens, expected),
        _ => false,
    })
}

fn token_ident(token: Option<&RustToken>) -> Option<&str> {
    match token.map(|token| &token.kind) {
        Some(RustTokenKind::Ident(value)) => Some(value),
        _ => None,
    }
}

fn token_group(token: Option<&RustToken>, delimiter: char) -> Option<&[RustToken]> {
    match token.map(|token| &token.kind) {
        Some(RustTokenKind::Group {
            delimiter: observed,
            tokens,
        }) if *observed == delimiter => Some(tokens),
        _ => None,
    }
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("workspace root")
        .to_path_buf()
}
