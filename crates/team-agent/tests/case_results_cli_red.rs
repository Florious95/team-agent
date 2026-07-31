//! RED contract for the read-only case result CLI:
//! `team-agent results --case CASE_ID [--team TEAM] [--workspace WS] [--json]`.
//!
//! Requirement authority:
//! - `.team/artifacts/CASEID-DESIGN-DECISION-20260730.md` §2-§3.
//! - `需求分析-report_result状态驱动交接.md` §1.1/§1.2/§5.2/§5.4.
//!
//! R1/R2/R4 deliberately cross the real stdio MCP `report_result` ingress,
//! SQLite persistence, and the public CLI read assembly.

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

#[path = "support/hermetic.rs"]
mod hermetic_guard;

#[path = "support/mcp_sim_harness.rs"]
#[allow(dead_code)]
mod mcp_sim_harness;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Output;

use hermetic_guard::HermeticTestEnv;
use mcp_sim_harness::spawn_mcp_client_without_catalog_check;
use rusqlite::Connection;
use serde_json::{json, Value};
use serial_test::serial;
use team_agent::db::migration::MANAGED_TABLE_LAYOUTS;
use team_agent::db::schema::{open_db, SCHEMA_VERSION};
use team_agent::message_store::MessageStore;
use team_agent::state::persist::{load_runtime_state, save_runtime_state};

const TEAM: &str = "teamA";
const WORKER: &str = "worker_a";

struct Case {
    env: HermeticTestEnv,
    workspace: PathBuf,
}

impl Case {
    fn new(tag: &str, tasks: &[Value]) -> Self {
        let env = HermeticTestEnv::enter(tag);
        let workspace = env.workspace(tag);
        MessageStore::open(&workspace).expect("initialize case result store");
        seed_state(&workspace, tasks);
        Self { env, workspace }
    }

    fn from_pre_feature_v4(tag: &str) -> Self {
        let env = HermeticTestEnv::enter(tag);
        let workspace = env.workspace(tag);
        fs::create_dir_all(workspace.join(".team")).expect("create historical .team");
        let conn =
            Connection::open(workspace.join(".team/team.db")).expect("create historical team.db");
        conn.execute_batch(include_str!("fixtures/case_results_pre_feature_v4.sql"))
            .expect("load frozen pre-feature v4 team.db inventory");
        drop(conn);
        seed_state(&workspace, &[task("case-pre-feature", Some("pipeline"))]);
        Self { env, workspace }
    }

    fn report(
        &self,
        task_id: &str,
        result_id: &str,
        status: &str,
        artifacts: Value,
        presentation: Option<Value>,
    ) {
        let mut envelope = json!({
            "schema_version": "result_envelope_v1",
            "result_id": result_id,
            "task_id": task_id,
            "agent_id": WORKER,
            "status": status,
            "summary": format!("CASE_RESULTS_{result_id}"),
            "changes": [],
            "tests": [],
            "risks": [],
            "artifacts": artifacts,
            "next_actions": []
        });
        if let Some(presentation) = presentation {
            envelope["presentation"] = presentation;
        }
        let mut worker = spawn_mcp_client_without_catalog_check(&self.workspace, WORKER, TEAM);
        let call = worker.call_tool("report_result", json!({"envelope": envelope}));
        assert!(
            !call.is_error,
            "fixture report_result must persist before the case reader is exercised; body={} raw={}",
            call.body,
            call.raw
        );
        assert_eq!(
            call.body["result_id"],
            json!(result_id),
            "fixture report_result must keep its stable result_id"
        );
    }

    fn run_results(&self, case_id: &str) -> Output {
        self.env.run_cli(
            &self.workspace,
            &[
                "results",
                "--case",
                case_id,
                "--team",
                TEAM,
                "--workspace",
                self.workspace.to_str().expect("UTF-8 workspace"),
                "--json",
            ],
        )
    }

    fn run_results_with_implicit_team(&self, case_id: &str) -> Output {
        self.env.run_cli(
            &self.workspace,
            &[
                "results",
                "--case",
                case_id,
                "--workspace",
                self.workspace.to_str().expect("UTF-8 workspace"),
                "--json",
            ],
        )
    }

    fn run_collect(&self) -> Output {
        self.env.run_cli(
            &self.workspace,
            &[
                "collect",
                "--team",
                TEAM,
                "--workspace",
                self.workspace.to_str().expect("UTF-8 workspace"),
                "--json",
            ],
        )
    }

    fn conn(&self) -> Connection {
        let store = MessageStore::open(&self.workspace).expect("open result store");
        open_db(store.db_path()).expect("open team.db")
    }

    fn checkpoint(&self) {
        self.conn()
            .execute_batch("pragma wal_checkpoint(truncate)")
            .expect("checkpoint hermetic fixture DB before snapshot");
    }

    fn snapshot(&self, name: &str) -> PathBuf {
        self.checkpoint();
        let destination = self.env.root().join(name);
        copy_tree(&self.workspace, &destination);
        destination
    }

    fn restore(&self, snapshot: &Path) {
        fs::remove_dir_all(&self.workspace).expect("remove mutated hermetic workspace");
        copy_tree(snapshot, &self.workspace);
    }
}

fn task(task_id: &str, route: Option<&str>) -> Value {
    let mut value = json!({
        "id": task_id,
        "title": format!("case results {task_id}"),
        "assignee": WORKER,
        "status": "pending"
    });
    if let Some(route) = route {
        value["result_route"] = json!(route);
    }
    value
}

fn seed_state(workspace: &Path, tasks: &[Value]) {
    fs::write(
        workspace.join("team.spec.yaml"),
        "version: 1\nteam:\n  name: case-results-contract\n",
    )
    .expect("seed collect-compatible team spec");
    save_runtime_state(
        workspace,
        &json!({
            "active_team_key": TEAM,
            "session_name": "case-results-contract",
            "agents": {
                WORKER: {"status": "running", "provider": "fake"}
            },
            "tasks": tasks,
            "teams": {
                TEAM: {
                    "agents": {
                        WORKER: {"status": "running", "provider": "fake"}
                    },
                    "tasks": tasks
                }
            }
        }),
    )
    .expect("seed case result runtime state");
}

#[test]
#[serial(case_results_cli)]
fn r1_artifacts_cross_the_report_store_read_bridge_byte_for_byte() {
    let case = Case::new("case-results-r1", &[task("case-r1", Some("pipeline"))]);
    let artifacts = json!([{
        "path": "artifact://r1/report.json",
        "description": "R1_DEEP_ARTIFACT_CANARY",
        "custom": {
            "level1": {
                "level2": {
                    "array": [1, true, null, {"leaf": "逐字保留🚀"}]
                }
            }
        }
    }]);
    let expected = serde_json::to_vec(&artifacts).expect("serialize submitted artifacts");
    case.report(
        "case-r1",
        "res-r1-artifacts",
        "artifact_ready/custom",
        artifacts,
        None,
    );

    let body = results_body(case.run_results("case-r1"), "R1");
    let row = result_by_id(&body, "res-r1-artifacts", "R1");
    let actual =
        serde_json::to_vec(&envelope(row)["artifacts"]).expect("serialize returned artifacts");
    assert_eq!(
        actual, expected,
        "R1 RED artifacts_projection_loss: the reader must return the exact nested artifacts bytes stored by report_result"
    );
}

#[test]
#[serial(case_results_cli)]
fn r2_status_comes_from_the_envelope_after_collect_rewrites_the_column() {
    let case = Case::new("case-results-r2", &[task("case-r2", Some("pipeline"))]);
    let custom_status = "red_gate/等待复核";
    case.report("case-r2", "res-r2-status", custom_status, json!([]), None);
    let collected = case.run_collect();
    assert!(
        collected.status.success(),
        "R2 setup: collect must run before reading; stdout={} stderr={}",
        String::from_utf8_lossy(&collected.stdout),
        String::from_utf8_lossy(&collected.stderr)
    );
    let stored_column: String = case
        .conn()
        .query_row(
            "select status from results where result_id = 'res-r2-status'",
            [],
            |row| row.get(0),
        )
        .expect("read post-collect status column");
    assert_eq!(
        stored_column, "collected",
        "R2 positive control: collect must really rewrite the status column"
    );

    let body = results_body(case.run_results("case-r2"), "R2");
    let row = result_by_id(&body, "res-r2-status", "R2");
    assert_eq!(
        envelope(row)["status"],
        json!(custom_status),
        "R2 RED status_column_leak: the reader must return envelope.status, not the collected bookkeeping column"
    );
}

#[test]
#[serial(case_results_cli)]
fn r3_two_reads_are_strictly_non_consuming_and_non_notifying() {
    let case = Case::new("case-results-r3", &[task("case-r3", Some("pipeline"))]);
    case.report(
        "case-r3",
        "res-r3-read-only",
        "pipeline_ready",
        json!([{"path": "artifact://r3/report.md"}]),
        None,
    );
    let snapshot = case.snapshot("case-results-r3-before-read");

    let control_collect = case.run_collect();
    assert!(
        control_collect.status.success(),
        "R3 setup: control collect failed; stdout={} stderr={}",
        String::from_utf8_lossy(&control_collect.stdout),
        String::from_utf8_lossy(&control_collect.stderr)
    );
    case.restore(&snapshot);

    let before = read_side_effects(&case.conn());
    let first = results_body(case.run_results("case-r3"), "R3-first");
    let second = results_body(case.run_results("case-r3"), "R3-second");
    assert_eq!(
        first, second,
        "R3 RED read_is_not_repeatable: two reads of one unchanged case must be identical"
    );
    let after = read_side_effects(&case.conn());
    assert_eq!(
        after.result_watchers, before.result_watchers,
        "R3 RED watcher_created: a read must not create or rebind result_watchers"
    );
    assert_eq!(
        after.leader_messages, before.leader_messages,
        "R3 RED leader_notified: a read must not enqueue any leader inbox message"
    );

    let after_collect = case.run_collect();
    assert_eq!(
        (after_collect.status.code(), &after_collect.stdout, &after_collect.stderr),
        (
            control_collect.status.code(),
            &control_collect.stdout,
            &control_collect.stderr
        ),
        "R3 RED result_consumed: collect output changed after two supposedly read-only results calls"
    );
}

#[test]
#[serial(case_results_cli)]
fn r4_case_lookup_matches_task_id_and_envelope_presentation_case_id() {
    let case_id = "case-r4-shared";
    let case = Case::new(
        "case-results-r4",
        &[task(case_id, Some("pipeline")), task("task-r4-other", None)],
    );
    case.report(
        case_id,
        "res-r4-task-id",
        "from_task_id",
        json!([{"path": "artifact://r4/task-id"}]),
        None,
    );
    case.report(
        "task-r4-other",
        "res-r4-envelope-case",
        "from_envelope_case_id",
        json!([{"path": "artifact://r4/envelope-case"}]),
        Some(json!({
            "sink": "casefile",
            "class": "stage_result",
            "case_id": case_id
        })),
    );

    let body = results_body(case.run_results(case_id), "R4");
    let ids = result_ids(&body);
    assert!(
        ids.contains(&"res-r4-task-id".to_string()),
        "R4 RED task_id_predicate_missing: pipeline case_id=task_id result was omitted; ids={ids:?}"
    );
    assert!(
        ids.contains(&"res-r4-envelope-case".to_string()),
        "R4 RED envelope_case_predicate_missing: presentation.case_id result with a different task_id was omitted; ids={ids:?}"
    );
    assert_eq!(
        ids.len(),
        2,
        "R4 case query must return exactly both matching rows; ids={ids:?}"
    );
}

#[test]
#[serial(case_results_cli)]
fn r5_missing_case_is_a_successful_empty_set() {
    let case = Case::new("case-results-r5", &[]);
    let output = case.run_results("case-does-not-exist");
    assert!(
        output.status.success(),
        "R5 RED empty_case_exit_nonzero: a not-yet-reported case is not a CLI failure; stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    let body: Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "R5 RED empty_case_non_json: --json must remain machine-readable: {error}; stdout={}",
            String::from_utf8_lossy(&output.stdout)
        )
    });
    assert_eq!(
        body["ok"],
        json!(true),
        "R5 RED empty_case_ok_false: empty case polling must report ok:true; body={body}"
    );
    assert_eq!(
        body["results"],
        json!([]),
        "R5 RED empty_case_not_empty_array: empty case polling must return results:[]; body={body}"
    );
}

#[test]
#[serial(case_results_cli)]
fn r6_all_case_rows_are_returned_in_created_at_result_id_order() {
    let case_id = "case-r6-shared";
    let case = Case::new("case-results-r6", &[task(case_id, Some("pipeline"))]);
    for result_id in ["res-r6-z", "res-r6-b", "res-r6-a"] {
        case.report(
            case_id,
            result_id,
            "opaque",
            json!([{"path": format!("artifact://r6/{result_id}")}]),
            None,
        );
    }
    let conn = case.conn();
    conn.execute(
        "update results set created_at = '2026-07-30T00:00:02Z' where result_id = 'res-r6-z'",
        [],
    )
    .expect("set later created_at");
    conn.execute(
        "update results set created_at = '2026-07-30T00:00:01Z' where result_id in ('res-r6-a','res-r6-b')",
        [],
    )
    .expect("set tied earlier created_at");
    drop(conn);

    let body = results_body(case.run_results(case_id), "R6");
    assert_eq!(
        result_ids(&body),
        ["res-r6-a", "res-r6-b", "res-r6-z"],
        "R6 RED latest_or_filtered_projection: return every matching row ordered by created_at then result_id"
    );
}

#[test]
#[serial(case_results_cli)]
fn r7_results_is_cli_only_and_real_tools_list_stays_at_thirteen() {
    let case = Case::new("case-results-r7", &[]);
    let mut client = spawn_mcp_client_without_catalog_check(&case.workspace, WORKER, TEAM);
    let tools = client.tools_list();
    let listed = tools
        .as_array()
        .unwrap_or_else(|| panic!("R7 RED tools_list_not_array: real stdio tools/list={tools}"));
    assert_eq!(
        listed.len(),
        13,
        "R7 RED mcp_surface_grew: case result reads are CLI-only; real tools/list={tools}"
    );

    let help = case.env.run_cli(&case.workspace, &["results", "--help"]);
    assert_results_help(&help, "R7");
}

#[test]
#[serial(case_results_cli)]
fn r8_pre_feature_v4_database_is_read_without_schema_migration_or_backup() {
    let results_layout = MANAGED_TABLE_LAYOUTS
        .iter()
        .find(|(table, _)| *table == "results")
        .map(|(_, columns)| *columns)
        .expect("R8 product manifest must declare results");
    assert_eq!(
        results_layout.len(),
        7,
        "R8 RED results_schema_growth: the read API must not add a results column; layout={results_layout:?}"
    );
    assert_eq!(
        SCHEMA_VERSION, 4,
        "R8 RED schema_version_bump: the read API must not require a schema migration"
    );

    let case = Case::from_pre_feature_v4("case-results-r8");
    let before_backups = migration_backups(&case.workspace);
    let body = results_body(case.run_results("case-pre-feature"), "R8");
    assert!(
        result_ids(&body).contains(&"res-pre-feature-v4".to_string()),
        "R8 RED historical_row_unreadable: the pre-feature v4 result must remain queryable; body={body}"
    );
    let after_backups = migration_backups(&case.workspace);
    assert_eq!(
        after_backups, before_backups,
        "R8 RED migration_backup_created: a read-only feature must not create team.db.pre-migration backups"
    );
}

#[test]
#[serial(case_results_cli)]
fn r9_results_has_no_to_option_and_never_sends() {
    let case = Case::new("case-results-r9", &[task("case-r9", Some("pipeline"))]);
    case.report(
        "case-r9",
        "res-r9-no-send",
        "ready",
        json!([{"path": "artifact://r9/report.md"}]),
        None,
    );
    let help = case.env.run_cli(&case.workspace, &["results", "--help"]);
    let help_text = assert_results_help(&help, "R9");
    assert!(
        !help_text.contains("--to"),
        "R9 RED to_option_exposed: results is a pull command and must not own recipient selection; help={help_text}"
    );

    let before = send_side_effects(&case.conn());
    let bad_to = case.env.run_cli(
        &case.workspace,
        &[
            "results",
            "--case",
            "case-r9",
            "--to",
            WORKER,
            "--workspace",
            case.workspace.to_str().expect("UTF-8 workspace"),
            "--json",
        ],
    );
    assert!(
        !bad_to.status.success()
            || serde_json::from_slice::<Value>(&bad_to.stdout)
                .ok()
                .and_then(|body| body.get("ok").and_then(Value::as_bool))
                == Some(false),
        "R9 RED to_option_accepted: results must reject --to; stdout={} stderr={}",
        String::from_utf8_lossy(&bad_to.stdout),
        String::from_utf8_lossy(&bad_to.stderr)
    );
    let _ = results_body(case.run_results("case-r9"), "R9");
    let after = send_side_effects(&case.conn());
    assert_eq!(
        after, before,
        "R9 RED read_sent_message: results must not create message, schedule, token, or leader-notification rows"
    );
}

#[test]
#[serial(case_results_cli)]
fn r10_implicit_active_team_scope_hides_foreign_case_rows() {
    let case_id = "case-r10-owner-scope";
    let case = Case::new("case-results-r10", &[task(case_id, Some("pipeline"))]);
    case.report(
        case_id,
        "res-r10-current-team",
        "ready",
        json!([{"path": "artifact://r10/current-team"}]),
        None,
    );

    let foreign_envelope = json!({
        "schema_version": "result_envelope_v1",
        "result_id": "res-r10-foreign-team",
        "task_id": case_id,
        "agent_id": "worker_b",
        "status": "ready",
        "summary": "R10_FOREIGN_TEAM_CANARY",
        "changes": [],
        "tests": [],
        "risks": [],
        "artifacts": [{"path": "artifact://r10/foreign-team"}],
        "next_actions": []
    });
    case.conn()
        .execute(
            "insert into results(
                result_id, owner_team_id, task_id, agent_id, envelope, status, created_at
             ) values (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![
                "res-r10-foreign-team",
                "teamB",
                case_id,
                "worker_b",
                serde_json::to_string(&foreign_envelope).expect("serialize foreign result"),
                "ready",
                "2026-07-31T00:00:00Z"
            ],
        )
        .expect("seed same-case foreign-team result");

    let mut state = load_runtime_state(&case.workspace).expect("load active-team fixture state");
    state["teams"]["teamB"] = json!({
        "status": "alive",
        "session_name": "case-results-team-b",
        "agents": {
            "worker_b": {"status": "running", "provider": "fake"}
        },
        "tasks": [task(case_id, Some("pipeline"))]
    });
    save_runtime_state(&case.workspace, &state)
        .expect("seed a second independently selectable active team");

    let inventory = case
        .conn()
        .prepare(
            "select owner_team_id, result_id
             from results
             where task_id = ?1
             order by owner_team_id, result_id",
        )
        .and_then(|mut statement| {
            statement
                .query_map([case_id], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()
        })
        .expect("read complete same-case owner inventory");
    assert_eq!(
        inventory,
        [
            (TEAM.to_string(), "res-r10-current-team".to_string()),
            ("teamB".to_string(), "res-r10-foreign-team".to_string()),
        ],
        "R10 positive control: both canonical-team and foreign-team rows must exist before the public read"
    );

    let team_a_body = results_body(case.run_results_with_implicit_team(case_id), "R10");
    let team_a_ids = result_ids(&team_a_body);
    assert_eq!(
        team_a_ids,
        ["res-r10-current-team"],
        "R10 RED active_team_a_scope_not_consumed: omitting --team must return only rows owned by canonical active teamA; ids={team_a_ids:?}"
    );

    state["active_team_key"] = json!("teamB");
    state["session_name"] = json!("case-results-team-b");
    state["agents"] = state["teams"]["teamB"]["agents"].clone();
    state["tasks"] = state["teams"]["teamB"]["tasks"].clone();
    save_runtime_state(&case.workspace, &state)
        .expect("switch the independent canonical active-team source to teamB");
    let persisted =
        load_runtime_state(&case.workspace).expect("reload switched active-team source");
    assert_eq!(
        persisted["active_team_key"],
        json!("teamB"),
        "R10 positive control: the canonical active-team source must independently switch to teamB"
    );

    let team_b_body = results_body(case.run_results_with_implicit_team(case_id), "R10");
    let team_b_ids = result_ids(&team_b_body);
    assert_eq!(
        team_b_ids,
        ["res-r10-foreign-team"],
        "R10 RED active_team_b_scope_not_consumed: after the canonical active source switches, the same implicit public query must return only teamB rows; ids={team_b_ids:?}"
    );
}

fn results_body(output: Output, tooth: &str) -> Value {
    assert!(
        output.status.success(),
        "{tooth} RED results_cli_missing_or_failed: status={:?} stdout={} stderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let body: Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "{tooth} RED results_cli_missing_or_non_json: {error}; stdout={}",
            String::from_utf8_lossy(&output.stdout)
        )
    });
    assert_eq!(
        body["ok"],
        json!(true),
        "{tooth} RED results_cli_not_ok: body={body}"
    );
    assert!(
        body["results"].is_array(),
        "{tooth} RED results_array_missing: body={body}"
    );
    body
}

fn result_by_id<'a>(body: &'a Value, result_id: &str, tooth: &str) -> &'a Value {
    body["results"]
        .as_array()
        .expect("results array")
        .iter()
        .find(|row| row_result_id(row) == Some(result_id))
        .unwrap_or_else(|| {
            panic!("{tooth} RED matching_result_missing: result_id={result_id}; body={body}")
        })
}

fn result_ids(body: &Value) -> Vec<String> {
    body["results"]
        .as_array()
        .expect("results array")
        .iter()
        .map(|row| {
            row_result_id(row)
                .unwrap_or_else(|| panic!("result row lacks result_id: {row}"))
                .to_string()
        })
        .collect()
}

fn row_result_id(row: &Value) -> Option<&str> {
    row.get("result_id")
        .and_then(Value::as_str)
        .or_else(|| envelope(row).get("result_id").and_then(Value::as_str))
}

fn envelope(row: &Value) -> &Value {
    row.get("envelope")
        .filter(|value| value.is_object())
        .unwrap_or(row)
}

fn assert_results_help(output: &Output, tooth: &str) -> String {
    assert!(
        output.status.success(),
        "{tooth} RED results_cli_help_missing: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        text.contains("usage: team-agent results --case"),
        "{tooth} RED results_cli_help_missing: help={text}"
    );
    text
}

#[derive(Debug, PartialEq, Eq)]
struct ReadSideEffects {
    result_watchers: i64,
    leader_messages: i64,
}

fn read_side_effects(conn: &Connection) -> ReadSideEffects {
    ReadSideEffects {
        result_watchers: table_count(conn, "result_watchers"),
        leader_messages: conn
            .query_row(
                "select count(*) from messages where recipient = 'leader'",
                [],
                |row| row.get(0),
            )
            .expect("count leader messages"),
    }
}

fn send_side_effects(conn: &Connection) -> Vec<(&'static str, i64)> {
    [
        "messages",
        "scheduled_events",
        "delivery_tokens",
        "leader_notification_log",
    ]
    .into_iter()
    .map(|table| (table, table_count(conn, table)))
    .collect()
}

fn table_count(conn: &Connection, table: &str) -> i64 {
    conn.query_row(&format!("select count(*) from {table}"), [], |row| {
        row.get(0)
    })
    .unwrap_or_else(|error| panic!("count {table}: {error}"))
}

fn migration_backups(workspace: &Path) -> Vec<String> {
    let mut names = fs::read_dir(workspace.join(".team"))
        .expect("read .team")
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().to_string())
        .filter(|name| name.starts_with("team.db.pre-migration-") && name.ends_with(".bak"))
        .collect::<Vec<_>>();
    names.sort();
    names
}

fn copy_tree(source: &Path, destination: &Path) {
    fs::create_dir_all(destination).expect("create snapshot destination");
    for entry in fs::read_dir(source).expect("read snapshot source") {
        let entry = entry.expect("read snapshot entry");
        let from = entry.path();
        let to = destination.join(entry.file_name());
        if entry.file_type().expect("snapshot file type").is_dir() {
            copy_tree(&from, &to);
        } else {
            fs::copy(&from, &to).expect("copy snapshot file");
        }
    }
}
