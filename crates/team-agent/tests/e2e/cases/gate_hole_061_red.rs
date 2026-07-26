//! 0.5.61 gate-coverage-hole RED: the documented fresh-team command and first
//! message/result loop belong to the existing CLI E2E hard smoke.
//!
//! Requirement anchors:
//! - `skills/team-agent/SKILL.md` "Minimal Copy-Paste Team" and "Commands"
//! - F1 one-entry startup / stable team identity
//! - F4 end-to-end delivery truth and unique recipient
//! - F10 requirement-to-RED and anti-vacuous controls

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
        || result_summary_for_message(ws.path(), &message_id).is_some(),
        Duration::from_secs(10),
    );
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
fn tooth_3_every_skill_command_has_a_machine_checked_e2e_mapping() {
    assert_command_extractor_canary();

    let skill = std::fs::read_to_string(repo_root().join("skills/team-agent/SKILL.md"))
        .expect("read product Team Agent SKILL.md");
    let documented = extract_team_agent_commands(&skill);
    assert!(
        documented.contains("team-agent quick-start .team/current"),
        "TOOTH-3 harness canary: extractor missed the canonical quick-start command"
    );

    let manifest_path = repo_root().join(COVERAGE_MANIFEST);
    let raw = std::fs::read_to_string(&manifest_path).unwrap_or_else(|_| {
        panic!(
            "TOOTH-3 RED: machine-readable documented-command coverage manifest is missing: \
             {COVERAGE_MANIFEST}. It must map every executable `team-agent ...` command extracted \
             from SKILL.md to one or more cases in the existing `e2e` binary."
        )
    });
    let manifest: CoverageManifest = serde_json::from_str(&raw)
        .unwrap_or_else(|e| panic!("TOOTH-3 RED: invalid {COVERAGE_MANIFEST}: {e}"));
    assert_eq!(
        manifest.schema_version, "team-agent-skill-command-coverage-v1",
        "TOOTH-3 RED: unsupported command coverage manifest schema"
    );

    let listed = manifest
        .commands
        .iter()
        .map(|entry| entry.command.clone())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        listed, documented,
        "TOOTH-3 RED: SKILL.md executable commands and the machine coverage manifest drifted"
    );

    let e2e_tests = source_tree(&["tests/e2e/main.rs", "tests/e2e/cases"]);
    for entry in &manifest.commands {
        assert!(
            !entry.cases.is_empty(),
            "TOOTH-3 RED: documented command {:?} has no E2E case mapping",
            entry.command
        );
        for case in &entry.cases {
            assert!(
                !case.starts_with("tooth_") && e2e_tests.contains(&format!("fn {case}(")),
                "TOOTH-3 RED: documented command {:?} maps to missing/non-test E2E case {:?}",
                entry.command,
                case
            );
        }
    }
}

#[derive(Debug)]
struct MessageTruth {
    recipient: String,
    status: String,
    delivered_at: Option<String>,
}

impl MessageTruth {
    fn delivered(&self) -> bool {
        self.status == "delivered" && self.delivered_at.is_some()
    }
}

#[derive(serde::Deserialize)]
struct CoverageManifest {
    schema_version: String,
    commands: Vec<CoverageEntry>,
}

#[derive(serde::Deserialize)]
struct CoverageEntry {
    command: String,
    cases: Vec<String>,
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

fn result_summary_for_message(workspace: &Path, message_id: &str) -> Option<String> {
    let conn = Connection::open(workspace.join(".team/runtime/team.db")).ok()?;
    let mut stmt = conn
        .prepare("select envelope from results order by created_at desc")
        .ok()?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(0)).ok()?;
    let found = rows.filter_map(Result::ok).find_map(|raw| {
        let envelope: Value = serde_json::from_str(&raw).ok()?;
        envelope["summary"]
            .as_str()
            .filter(|summary| summary.contains(message_id))
            .map(str::to_string)
    });
    found
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
```
Use `team-agent status --json`; prose only.
"#;
    assert_eq!(
        extract_team_agent_commands(markdown),
        BTreeSet::from([
            "team-agent quick-start .team/current".to_string(),
            "team-agent status --json".to_string(),
        ])
    );
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
    let command = raw[start..]
        .trim_end_matches(|c: char| matches!(c, '.' | ',' | ';' | ':'))
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    Some(command)
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("workspace root")
        .to_path_buf()
}
