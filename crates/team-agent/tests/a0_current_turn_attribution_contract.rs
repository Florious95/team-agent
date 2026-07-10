//! Foundation-0 F0-1 RED contract: A0 current-turn result attribution.
//!
//! References:
//! - `.team/artifacts/foundation-0-slice-design.md` §3 A0 target semantics.
//! - `.team/artifacts/foundation-0-slice-design.md` §5 F0-1 RED design.
//!
//! User story: when a worker reports without an explicit task_id, Team Agent
//! attributes that result to the worker's current physical turn/message, not to a
//! stale startup task or another team's similarly named worker. If physical submit
//! never happened, the current turn is not armed and historical fallback is bounded.

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

#[path = "support/hermetic.rs"]
mod hermetic_guard;

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use rusqlite::params;
use serde_json::{json, Value};
use serial_test::serial;
use team_agent::db::schema::open_db;
use team_agent::mcp_server::TeamOrchestratorTools;
use team_agent::message_store::MessageStore;
use team_agent::model::enums::ResultStatus;
use team_agent::model::ids::{AgentId, TeamKey};
use team_agent::state::persist::save_runtime_state;

const WORKER: &str = "worker";
const TEAM_A: &str = "team-a";
const TEAM_B: &str = "team-b";
const TASK_INITIAL: &str = "task_initial";
const MSG_CURRENT: &str = "msg_a0_current";
const MSG_SUBMITTED: &str = "msg_a0_submitted";
const MSG_FAILED_BEFORE_SUBMIT: &str = "msg_a0_failed_before_submit";
const MSG_OLD_DELIVERED: &str = "msg_a0_old_delivered";
const MSG_ALPHA: &str = "msg_a0_alpha";
const MSG_BETA: &str = "msg_a0_beta";
const EXPLICIT_TASK: &str = "task_explicit_override";

#[test]
#[serial(env)]
fn old_task_initial_loses_to_current_direct_message_without_explicit_task_id() {
    let case = A0Case::new("old-task-initial-current-direct");
    case.seed_team(TEAM_A, Some(MSG_CURRENT), &[pending_task(TASK_INITIAL)]);
    case.insert_direct_message(TEAM_A, MSG_CURRENT, "delivered", 10);

    let body = case.report_without_task(TEAM_A, "A0_CURRENT_BEATS_TASK_INITIAL");

    assert_eq!(
        body.get("task_id").and_then(Value::as_str),
        Some(MSG_CURRENT),
        "F0-1 RED1: old nonterminal task_initial must not steal a no-task report when the worker has a newer current physical direct turn; body={body} rows={:?}",
        case.result_rows("A0_CURRENT_BEATS_TASK_INITIAL")
    );
    assert!(
        case.result_rows_for_task(TASK_INITIAL, "A0_CURRENT_BEATS_TASK_INITIAL")
            .is_empty(),
        "F0-1 RED1: stale task_initial must receive zero result rows when current_turn_message_id is reportable; rows={:?}",
        case.result_rows("A0_CURRENT_BEATS_TASK_INITIAL")
    );
}

#[test]
#[serial(env)]
fn physical_submit_before_delayed_verification_arms_current_turn_for_fast_report() {
    let case = A0Case::new("physical-submit-before-verification");
    case.seed_team(TEAM_A, Some(MSG_SUBMITTED), &[pending_task(TASK_INITIAL)]);
    case.insert_direct_message(TEAM_A, MSG_SUBMITTED, "submitted", 20);

    let body = case.report_without_task(TEAM_A, "A0_SUBMITTED_BEFORE_VERIFIED");

    assert_eq!(
        body.get("task_id").and_then(Value::as_str),
        Some(MSG_SUBMITTED),
        "F0-1 RED2: physical submit is the current-turn boundary; a fast report emitted before delayed verification returns must attach to the submitted message, not stale task_initial; body={body} rows={:?}",
        case.result_rows("A0_SUBMITTED_BEFORE_VERIFIED")
    );
}

#[test]
#[serial(env)]
fn failed_inject_before_physical_submit_does_not_arm_current_turn_or_cross_to_stale_fallback() {
    let case = A0Case::new("failed-before-physical-submit");
    case.seed_team(TEAM_A, None, &[pending_task(TASK_INITIAL)]);
    case.insert_direct_message(TEAM_A, MSG_OLD_DELIVERED, "delivered", 10);
    case.insert_direct_message(TEAM_A, MSG_FAILED_BEFORE_SUBMIT, "failed", 30);

    let body = case.report_without_task(TEAM_A, "A0_FAILED_BEFORE_SUBMIT");

    assert_ne!(
        body.get("task_id").and_then(Value::as_str),
        Some(MSG_FAILED_BEFORE_SUBMIT),
        "F0-1 RED3: an inject failure before physical submit must not arm current_turn for the failed message; body={body}"
    );
    assert!(
        case.result_rows_for_task(MSG_FAILED_BEFORE_SUBMIT, "A0_FAILED_BEFORE_SUBMIT")
            .is_empty(),
        "F0-1 RED3: failed-before-submit message must receive no result rows; rows={:?}",
        case.result_rows("A0_FAILED_BEFORE_SUBMIT")
    );
    assert!(
        case.result_rows_for_task(MSG_OLD_DELIVERED, "A0_FAILED_BEFORE_SUBMIT")
            .is_empty()
            && case
                .result_rows_for_task(TASK_INITIAL, "A0_FAILED_BEFORE_SUBMIT")
                .is_empty(),
        "F0-1 RED3: bounded fallback must not cross a newer non-reportable direct turn to stale delivered message or legacy task_initial; body={body} rows={:?}",
        case.result_rows("A0_FAILED_BEFORE_SUBMIT")
    );
    assert_eq!(
        body.get("task_id").and_then(Value::as_str),
        Some("manual"),
        "F0-1 RED3: with no current physical turn and a newer failed direct message, no-task report should become manual/invalid rather than attach to stale history; body={body}"
    );
}

#[test]
#[serial(env)]
fn sibling_team_same_worker_name_does_not_cross_owner_team_id() {
    let case = A0Case::new("sibling-owner-team");
    case.seed_two_teams(
        Some(MSG_ALPHA),
        Some(MSG_BETA),
        &[pending_task(TASK_INITIAL)],
        &[pending_task("task_initial_sibling")],
    );
    case.insert_direct_message(TEAM_A, MSG_ALPHA, "delivered", 10);
    case.insert_direct_message(TEAM_B, MSG_BETA, "delivered", 30);

    let body = case.report_without_task(TEAM_A, "A0_OWNER_SCOPE_ALPHA");

    assert_eq!(
        body.get("task_id").and_then(Value::as_str),
        Some(MSG_ALPHA),
        "F0-1 RED4: owner_team_id=team-a must read team-a current turn even when sibling team-b has the same worker name and a newer delivered message; body={body} rows={:?}",
        case.result_rows("A0_OWNER_SCOPE_ALPHA")
    );
    assert!(
        case.result_rows_for_task(MSG_BETA, "A0_OWNER_SCOPE_ALPHA")
            .is_empty(),
        "F0-1 RED4: no result row may attach to sibling owner_team_id/team-b message {MSG_BETA}; rows={:?}",
        case.result_rows("A0_OWNER_SCOPE_ALPHA")
    );
    assert_eq!(
        case.result_rows("A0_OWNER_SCOPE_ALPHA")[0]
            .owner_team_id
            .as_deref(),
        Some(TEAM_A),
        "F0-1 RED4: result row owner_team_id must preserve the worker MCP scope"
    );
}

#[test]
#[serial(env)]
fn explicit_task_id_wins_over_current_turn_inference() {
    let case = A0Case::new("explicit-task-id");
    case.seed_team(TEAM_A, Some(MSG_CURRENT), &[pending_task(TASK_INITIAL)]);
    case.insert_direct_message(TEAM_A, MSG_CURRENT, "delivered", 10);

    let body = case.report_with_task(TEAM_A, "A0_EXPLICIT_TASK_WINS", EXPLICIT_TASK);

    assert_eq!(
        body.get("task_id").and_then(Value::as_str),
        Some(EXPLICIT_TASK),
        "F0-1 RED5: explicit task_id supplied by the caller is the first inference source and must win over current_turn_message_id; body={body}"
    );
    assert!(
        case.result_rows_for_task(MSG_CURRENT, "A0_EXPLICIT_TASK_WINS")
            .is_empty(),
        "F0-1 RED5: current turn message must not receive the result when explicit task_id is present; rows={:?}",
        case.result_rows("A0_EXPLICIT_TASK_WINS")
    );
}

struct A0Case {
    _env: hermetic_guard::HermeticTestEnv,
    workspace: PathBuf,
}

impl A0Case {
    fn new(tag: &str) -> Self {
        static N: AtomicU64 = AtomicU64::new(0);
        let env = hermetic_guard::HermeticTestEnv::enter(tag);
        let workspace = env.workspace(&format!(
            "a0-current-turn-{tag}-{}",
            N.fetch_add(1, Ordering::Relaxed)
        ));
        MessageStore::open(&workspace).expect("initialize message store");
        Self {
            _env: env,
            workspace,
        }
    }

    fn seed_team(&self, team: &str, current_turn: Option<&str>, tasks: &[Value]) {
        self.seed_two_teams(current_turn, None, tasks, &[]);
        if team != TEAM_A {
            panic!("seed_team currently supports TEAM_A only; use seed_two_teams for siblings");
        }
    }

    fn seed_two_teams(
        &self,
        team_a_current: Option<&str>,
        team_b_current: Option<&str>,
        team_a_tasks: &[Value],
        team_b_tasks: &[Value],
    ) {
        let team_a = team_state(TEAM_A, team_a_current, team_a_tasks);
        let team_b = team_state(TEAM_B, team_b_current, team_b_tasks);
        let state = json!({
            "active_team_key": TEAM_A,
            "session_name": "team-a0-current-turn",
            "agents": {
                WORKER: agent_state(team_a_current)
            },
            "tasks": team_a_tasks,
            "teams": {
                TEAM_A: team_a,
                TEAM_B: team_b
            }
        });
        save_runtime_state(&self.workspace, &state).expect("save runtime state");
    }

    fn insert_direct_message(
        &self,
        owner_team_id: &str,
        message_id: &str,
        status: &str,
        created_offset_sec: i64,
    ) {
        let store = MessageStore::open(&self.workspace).expect("open message store");
        store
            .create_message_with_id(
                message_id,
                None,
                "leader",
                WORKER,
                &format!("direct message {message_id}"),
                None,
                false,
                Some(owner_team_id),
            )
            .expect("insert direct message");
        store
            .mark(
                message_id,
                status,
                (status == "failed").then_some("inject_failed_before_physical_submit"),
            )
            .expect("mark message");
        let conn = open_db(store.db_path()).expect("open db");
        let ts = format!("2026-07-10T12:00:{created_offset_sec:02}Z");
        conn.execute(
            "update messages set created_at = ?2, updated_at = ?2 where message_id = ?1",
            params![message_id, ts],
        )
        .expect("set message timestamp");
    }

    fn report_without_task(&self, owner_team_id: &str, summary: &str) -> Value {
        self.report(owner_team_id, summary, None)
    }

    fn report_with_task(&self, owner_team_id: &str, summary: &str, task_id: &str) -> Value {
        self.report(owner_team_id, summary, Some(task_id))
    }

    fn report(&self, owner_team_id: &str, summary: &str, task_id: Option<&str>) -> Value {
        let tools = TeamOrchestratorTools::with_identity(
            &self.workspace,
            Some(AgentId::new(WORKER)),
            Some(TeamKey::new(owner_team_id)),
        );
        let out = tools
            .report_result(
                None,
                Some(summary),
                ResultStatus::Success,
                None,
                None,
                None,
                None,
                None,
                task_id,
                None,
            )
            .map(|ok| Value::Object(ok.fields))
            .unwrap_or_else(|err| err.to_envelope());
        assert!(
            !out.is_null(),
            "F0-1 RED setup: report_result must return a structured body"
        );
        out
    }

    fn result_rows(&self, summary: &str) -> Vec<ResultRow> {
        let store = MessageStore::open(&self.workspace).expect("open message store");
        let conn = open_db(store.db_path()).expect("open db");
        let mut stmt = conn
            .prepare(
                "select owner_team_id, task_id, envelope
                 from results
                 where agent_id = ?1
                 order by created_at asc, result_id asc",
            )
            .expect("prepare result query");
        stmt.query_map(params![WORKER], |row| {
            Ok(ResultRow {
                owner_team_id: row.get(0)?,
                task_id: row.get(1)?,
                envelope: row.get(2)?,
            })
        })
        .expect("query results")
        .filter_map(Result::ok)
        .filter(|row| row.summary().as_deref() == Some(summary))
        .collect()
    }

    fn result_rows_for_task(&self, task_id: &str, summary: &str) -> Vec<ResultRow> {
        self.result_rows(summary)
            .into_iter()
            .filter(|row| row.task_id == task_id)
            .collect()
    }
}

#[derive(Debug)]
struct ResultRow {
    owner_team_id: Option<String>,
    task_id: String,
    envelope: String,
}

impl ResultRow {
    fn summary(&self) -> Option<String> {
        let value: Value = serde_json::from_str(&self.envelope).ok()?;
        value
            .get("summary")
            .and_then(Value::as_str)
            .map(ToString::to_string)
    }
}

fn team_state(team: &str, current_turn: Option<&str>, tasks: &[Value]) -> Value {
    json!({
        "team_key": team,
        "session_name": format!("team-{team}"),
        "agents": {
            WORKER: agent_state(current_turn)
        },
        "tasks": tasks,
        "coordinator": {}
    })
}

fn agent_state(current_turn: Option<&str>) -> Value {
    let mut agent = json!({
        "status": "running",
        "provider": "fake",
        "window": WORKER,
        "pane_id": "%7"
    });
    if let Some(message_id) = current_turn {
        agent["current_turn_message_id"] = json!(message_id);
    }
    agent
}

fn pending_task(task_id: &str) -> Value {
    json!({
        "id": task_id,
        "assignee": WORKER,
        "status": "pending"
    })
}
