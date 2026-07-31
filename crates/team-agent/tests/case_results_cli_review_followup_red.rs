//! Review-followup RED contract for two read-side invariants of
//! `team-agent results --case CASE_ID [--team TEAM]`.
//!
//! Requirement authority:
//! - `wiki/功能/F6 任务、结果与交付闭环.md`: task/result ownership remains
//!   scoped to its current team and a pull read does not become delivery.
//! - `wiki/功能/F10 治理边界与黑盒验收.md`: negative assertions must prove
//!   the request entered the real canonical path and produced the positive
//!   control outcome.
//! - reviewer-r11 `review://caseid-read/defect/implicit-team-scope`.
//! - reviewer-r11 `review://caseid-read/defect/read-creates-db`.

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

#[path = "support/hermetic.rs"]
mod hermetic_guard;

use std::fs;
use std::path::Path;
use std::process::Output;

use hermetic_guard::HermeticTestEnv;
use rusqlite::params;
use serde_json::{json, Value};
use serial_test::serial;
use team_agent::db::schema::open_db;
use team_agent::message_store::MessageStore;
use team_agent::state::persist::save_runtime_state;

const ACTIVE_TEAM: &str = "teamA";
const FOREIGN_TEAM: &str = "teamB";
const CASE_ID: &str = "case-shared";

#[test]
#[serial(case_results_cli)]
fn r10_implicit_team_uses_the_canonical_active_team_scope() {
    let env = HermeticTestEnv::enter("case-results-r10");
    let workspace = env.workspace("case-results-r10");
    seed_two_team_state(&workspace);
    seed_same_case_results_for_both_teams(&workspace);

    let explicit_foreign = results_body(
        run_results(&env, &workspace, CASE_ID, Some(FOREIGN_TEAM)),
        "R10-foreign-positive-control",
    );
    assert_eq!(
        result_ids(&explicit_foreign),
        ["res-team-b"],
        "R10 positive control: the foreign-team row must exist and remain readable when that team is explicitly selected"
    );

    let explicit_active = results_body(
        run_results(&env, &workspace, CASE_ID, Some(ACTIVE_TEAM)),
        "R10-active-positive-control",
    );
    assert_eq!(
        result_ids(&explicit_active),
        ["res-team-a"],
        "R10 positive control: explicit active-team selection must isolate the active row"
    );

    let implicit = results_body(
        run_results(&env, &workspace, CASE_ID, None),
        "R10-implicit-active-team",
    );
    assert_eq!(
        result_ids(&implicit),
        ["res-team-a"],
        "R10 RED implicit_team_scope_leak: omitting --team must still use the canonical active team and must not reveal same-case rows owned by another team; body={implicit}"
    );
}

#[test]
#[serial(case_results_cli)]
fn r11_reading_without_any_database_does_not_create_or_initialize_one() {
    let env = HermeticTestEnv::enter("case-results-r11");
    let workspace = env.workspace("case-results-r11");
    seed_two_team_state(&workspace);

    let before = database_artifacts(&workspace.join(".team"));
    assert!(
        before.is_empty(),
        "R11 setup: the no-database fixture must start without database artifacts; found={before:?}"
    );

    let body = results_body(
        run_results(&env, &workspace, "case-not-yet-reported", Some(ACTIVE_TEAM)),
        "R11-valid-read-positive-control",
    );
    assert_eq!(
        body["results"],
        json!([]),
        "R11 positive control: the valid canonical read must reach the result path and return an empty result set; body={body}"
    );

    let after = database_artifacts(&workspace.join(".team"));
    assert_eq!(
        after, before,
        "R11 RED read_created_database: results --case is a read-only pull and must not create or initialize any database file when no database existed; before={before:?} after={after:?}"
    );
}

fn seed_two_team_state(workspace: &Path) {
    save_runtime_state(
        workspace,
        &json!({
            "active_team_key": ACTIVE_TEAM,
            "team_key": ACTIVE_TEAM,
            "session_name": "case-results-review-followup",
            "agents": {},
            "tasks": [],
            "teams": {
                ACTIVE_TEAM: {
                    "team_key": ACTIVE_TEAM,
                    "session_name": "case-results-review-followup-a",
                    "agents": {},
                    "tasks": []
                },
                FOREIGN_TEAM: {
                    "team_key": FOREIGN_TEAM,
                    "session_name": "case-results-review-followup-b",
                    "agents": {},
                    "tasks": []
                }
            }
        }),
    )
    .expect("seed canonical active-team state");
}

fn seed_same_case_results_for_both_teams(workspace: &Path) {
    let store = MessageStore::open(workspace).expect("initialize result store");
    let conn = open_db(store.db_path()).expect("open result store");
    for (owner_team_id, result_id, created_at) in [
        (ACTIVE_TEAM, "res-team-a", "2026-07-31T00:00:01Z"),
        (FOREIGN_TEAM, "res-team-b", "2026-07-31T00:00:02Z"),
    ] {
        let envelope = json!({
            "schema_version": "result_envelope_v1",
            "result_id": result_id,
            "task_id": CASE_ID,
            "agent_id": "worker",
            "status": "ready",
            "summary": format!("{owner_team_id} result"),
            "changes": [],
            "tests": [],
            "risks": [],
            "artifacts": [],
            "next_actions": []
        });
        conn.execute(
            "insert into results(
                result_id, owner_team_id, task_id, agent_id, envelope, status, created_at
            ) values (?1, ?2, ?3, 'worker', ?4, 'ready', ?5)",
            params![
                result_id,
                owner_team_id,
                CASE_ID,
                envelope.to_string(),
                created_at
            ],
        )
        .expect("seed scoped result");
    }
}

fn run_results(
    env: &HermeticTestEnv,
    workspace: &Path,
    case_id: &str,
    team: Option<&str>,
) -> Output {
    let workspace_arg = workspace.to_str().expect("UTF-8 workspace");
    match team {
        Some(team) => env.run_cli(
            workspace,
            &[
                "results",
                "--case",
                case_id,
                "--team",
                team,
                "--workspace",
                workspace_arg,
                "--json",
            ],
        ),
        None => env.run_cli(
            workspace,
            &[
                "results",
                "--case",
                case_id,
                "--workspace",
                workspace_arg,
                "--json",
            ],
        ),
    }
}

fn results_body(output: Output, tooth: &str) -> Value {
    assert!(
        output.status.success(),
        "{tooth} RED valid_results_read_failed: status={:?} stdout={} stderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let body: Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "{tooth} RED results_read_non_json: {error}; stdout={}",
            String::from_utf8_lossy(&output.stdout)
        )
    });
    assert_eq!(
        body["ok"],
        json!(true),
        "{tooth} RED results_read_not_ok: body={body}"
    );
    assert!(
        body["results"].is_array(),
        "{tooth} RED results_array_missing: body={body}"
    );
    body
}

fn result_ids(body: &Value) -> Vec<&str> {
    body["results"]
        .as_array()
        .expect("results array")
        .iter()
        .map(|row| {
            row.get("result_id")
                .and_then(Value::as_str)
                .unwrap_or_else(|| panic!("result row lacks result_id: {row}"))
        })
        .collect()
}

fn database_artifacts(root: &Path) -> Vec<String> {
    let mut artifacts = Vec::new();
    collect_database_artifacts(root, root, &mut artifacts);
    artifacts.sort();
    artifacts
}

fn collect_database_artifacts(root: &Path, current: &Path, artifacts: &mut Vec<String>) {
    let entries = match fs::read_dir(current) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
        Err(error) => panic!("read database inventory {}: {error}", current.display()),
    };
    for entry in entries {
        let entry = entry.expect("read database inventory entry");
        let path = entry.path();
        let file_type = entry.file_type().expect("read inventory file type");
        if file_type.is_dir() {
            collect_database_artifacts(root, &path, artifacts);
            continue;
        }
        if !file_type.is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
        let bytes = fs::read(&path).expect("read possible database artifact");
        let database_named = name.ends_with(".db")
            || name.contains(".db-")
            || name.ends_with(".sqlite")
            || name.ends_with(".sqlite3");
        let sqlite_initialized = bytes.starts_with(b"SQLite format 3\0");
        if database_named || sqlite_initialized {
            let relative = path.strip_prefix(root).unwrap_or(&path);
            artifacts.push(format!(
                "{} ({} bytes, sqlite_header={sqlite_initialized})",
                relative.display(),
                bytes.len()
            ));
        }
    }
}
