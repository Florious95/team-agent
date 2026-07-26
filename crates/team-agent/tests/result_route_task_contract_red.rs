//! 0.5.61 `result_route` deterministic RED contracts.
//!
//! Requirement anchors: `result_route 与流水线结果消费` B01-B09/C01-C17.
//! Compiled from TP01-TP17 in the leader-approved station-3 analysis.
//!
//! This file deliberately uses only public task/result entry points plus the
//! durable SQLite observation surface already exposed to integration tests.
//! It does not inspect product source. Consumer checkpoint/casefile SSOT,
//! zero-report lifecycle closure, and typed failure-envelope injection remain
//! delayed until their public observation seams exist.

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

#[path = "support/hermetic.rs"]
mod hermetic_guard;

use std::sync::atomic::{AtomicU64, Ordering};

use rusqlite::OptionalExtension;
use serde_json::{json, Value};
use serial_test::serial;
use team_agent::db::schema::open_db;
use team_agent::mcp_server::wire::tools_contract;
use team_agent::mcp_server::TeamOrchestratorTools;
use team_agent::message_store::MessageStore;
use team_agent::messaging::report_result;
use team_agent::model::ids::{AgentId, TeamKey};
use team_agent::state::persist::{load_runtime_state, save_runtime_state};

const TEAM: &str = "team-route";
const WORKER: &str = "worker-route";

struct Case {
    _env: hermetic_guard::HermeticTestEnv,
    workspace: std::path::PathBuf,
}

impl Case {
    fn new(tag: &str) -> Self {
        static N: AtomicU64 = AtomicU64::new(0);
        let env = hermetic_guard::HermeticTestEnv::enter(tag);
        let workspace = env.workspace(&format!(
            "result-route-{tag}-{}",
            N.fetch_add(1, Ordering::Relaxed)
        ));
        MessageStore::open(&workspace).expect("initialize durable result store");
        Self {
            _env: env,
            workspace,
        }
    }

    fn seed_tasks(&self, tasks: &[Value]) {
        save_runtime_state(
            &self.workspace,
            &json!({
                "active_team_key": TEAM,
                "session_name": "team-result-route-contract",
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
        .expect("seed route contract state");
    }

    fn leader_tools(&self) -> TeamOrchestratorTools {
        TeamOrchestratorTools::with_identity(
            &self.workspace,
            Some(AgentId::new("leader")),
            Some(TeamKey::new(TEAM)),
        )
    }

    fn submit(&self, task_id: &str, result_id: &str, status: &str, summary: &str) -> Value {
        self.submit_extra(task_id, result_id, status, summary, json!({}))
    }

    fn submit_extra(
        &self,
        task_id: &str,
        result_id: &str,
        status: &str,
        summary: &str,
        extra: Value,
    ) -> Value {
        let mut envelope = json!({
            "schema_version": "result_envelope_v1",
            "result_id": result_id,
            "task_id": task_id,
            "agent_id": WORKER,
            "status": status,
            "summary": summary,
            "changes": [],
            "tests": [],
            "risks": [],
            "artifacts": [],
            "next_actions": []
        });
        for (key, value) in extra.as_object().expect("extra object") {
            envelope[key] = value.clone();
        }
        report_result(&self.workspace, &envelope).expect("report_result accepted")
    }

    fn result_count(&self, task_id: &str) -> i64 {
        let conn = self.conn();
        conn.query_row(
            "select count(*) from results where task_id = ?1",
            [task_id],
            |row| row.get(0),
        )
        .expect("count task results")
    }

    fn leader_message_count(&self, task_id: &str) -> i64 {
        let conn = self.conn();
        conn.query_row(
            "select count(*) from messages where task_id = ?1 and recipient = 'leader'",
            [task_id],
            |row| row.get(0),
        )
        .expect("count leader notifications")
    }

    fn stored_result(&self, result_id: &str) -> Option<(String, String, Value)> {
        let conn = self.conn();
        conn.query_row(
            "select task_id, status, envelope from results where result_id = ?1",
            [result_id],
            |row| {
                let envelope: String = row.get(2)?;
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    serde_json::from_str(&envelope).expect("stored result envelope JSON"),
                ))
            },
        )
        .optional()
        .expect("read stored result")
    }

    fn conn(&self) -> rusqlite::Connection {
        let store = MessageStore::open(&self.workspace).expect("open message store");
        open_db(store.db_path()).expect("open runtime DB")
    }
}

fn task(task_id: &str, route: Option<&str>) -> Value {
    let mut value = json!({
        "id": task_id,
        "title": format!("result route {task_id}"),
        "assignee": WORKER,
        "status": "pending"
    });
    if let Some(route) = route {
        value["result_route"] = json!(route);
    }
    value
}

fn report_result_contract() -> Value {
    tools_contract()
        .into_iter()
        .find(|tool| tool["name"] == json!("report_result"))
        .expect("tools/list contains report_result")
}

fn assign_task_contract() -> Value {
    tools_contract()
        .into_iter()
        .find(|tool| tool["name"] == json!("assign_task"))
        .expect("tools/list contains assign_task")
}

#[test]
fn tp01_route_catalog_is_public_and_closed_before_activation() {
    let contract = assign_task_contract();
    let catalog = &contract["inputSchema"]["properties"]["task"]["properties"]["result_route"];
    let variants = catalog["enum"]
        .as_array()
        .unwrap_or_else(|| panic!(
            "TP01 RED capability_missing: assign_task must expose the product-owned result_route catalog; schema={catalog}"
        ));
    assert_eq!(
        variants.len(),
        2,
        "TP01 RED catalog_not_closed: result_route is exactly the two-value product directory; catalog={variants:?}"
    );
    assert!(
        variants.iter().any(|value| value.as_str() == Some("leader"))
            && variants
                .iter()
                .any(|value| value.as_str() == Some("pipeline")),
        "TP01 RED route_semantics_missing: product catalog must expose leader and pipeline; catalog={variants:?}"
    );
    assert_eq!(
        catalog["default"],
        json!("leader"),
        "TP01 RED default_missing: omitted route must have machine-readable effective default leader"
    );
}

#[test]
#[serial(env)]
fn tp01_unknown_route_fails_loud_and_leaves_no_partial_task() {
    let case = Case::new("tp01-unknown");
    case.seed_tasks(&[]);
    let before = load_runtime_state(&case.workspace).expect("state before rejection");
    let before_messages: i64 = case
        .conn()
        .query_row("select count(*) from messages", [], |row| row.get(0))
        .expect("messages before rejection");

    let outcome = case.leader_tools().assign_task(
        &task("task-tp01-unknown", Some("side-channel")),
        Some("must reject before activation"),
    );
    assert!(
        outcome.is_err(),
        "TP01 RED unknown_route_accepted: malformed result_route must fail loud before task activation; outcome={outcome:?}"
    );

    let after = load_runtime_state(&case.workspace).expect("state after rejection");
    let after_messages: i64 = case
        .conn()
        .query_row("select count(*) from messages", [], |row| row.get(0))
        .expect("messages after rejection");
    assert_eq!(
        after, before,
        "TP01 route rejection partially polluted state"
    );
    assert_eq!(
        after_messages, before_messages,
        "TP01 route rejection partially emitted an assignment message"
    );
}

#[test]
#[serial(env)]
fn tp03_worker_result_route_is_an_atomic_contract_violation() {
    let case = Case::new("tp03-worker-override");
    case.seed_tasks(&[task("task-tp03", Some("leader"))]);
    let envelope = json!({
        "schema_version": "result_envelope_v1",
        "result_id": "res-tp03-worker-override",
        "task_id": "task-tp03",
        "agent_id": WORKER,
        "status": "success",
        "summary": "worker must not choose pipeline",
        "result_route": "pipeline",
        "changes": [],
        "tests": [],
        "risks": [],
        "artifacts": [],
        "next_actions": []
    });

    let outcome = report_result(&case.workspace, &envelope);
    assert!(
        outcome.is_err(),
        "TP03 RED unauthorized_route_input_accepted: worker payload result_route must be rejected, not ignored or used; outcome={outcome:?}"
    );
    assert_eq!(case.result_count("task-tp03"), 0, "TP03 partial result row");
    assert_eq!(
        case.leader_message_count("task-tp03"),
        0,
        "TP03 partial leader presentation"
    );
    assert_eq!(
        load_runtime_state(&case.workspace).expect("state after TP03")["tasks"][0]["result_route"],
        json!("leader"),
        "TP03 worker override changed task route"
    );
}

#[test]
#[serial(env)]
fn tp02_pipeline_is_durable_without_live_leader_result() {
    let case = Case::new("tp02-pipeline");
    case.seed_tasks(&[task("task-tp02", Some("pipeline"))]);
    case.submit(
        "task-tp02",
        "res-tp02-pipeline",
        "success",
        "TP02_PIPELINE_BACKING_TOKEN",
    );

    assert_eq!(
        case.result_count("task-tp02"),
        1,
        "TP02 positive backing: the pipeline result must really be durable"
    );
    assert!(
        case.stored_result("res-tp02-pipeline").is_some(),
        "TP02 positive backing: stable result_id must be pullable"
    );
    assert_eq!(
        case.leader_message_count("task-tp02"),
        0,
        "TP02 RED route_not_applied: pipeline result is durable/casefile-only and creates no live leader result"
    );
}

#[test]
#[serial(env)]
fn tp07_existing_result_id_dedupe_remains_a_route_independent_regression_lock() {
    for route in ["leader", "pipeline"] {
        let case = Case::new(&format!("tp07-dedupe-{route}"));
        let task_id = format!("task-tp07-{route}");
        let result_id = format!("res-tp07-{route}");
        case.seed_tasks(&[task(&task_id, Some(route))]);
        case.submit(&task_id, &result_id, "success", "first canonical result");
        let duplicate = case.submit(
            &task_id,
            &result_id,
            "failed",
            "must not replace first canonical result",
        );

        assert_eq!(
            case.result_count(&task_id),
            1,
            "TP07 regression: duplicate report_result must retain one canonical row for route={route}"
        );
        let (_, status, envelope) = case.stored_result(&result_id).expect("canonical result");
        assert_eq!(status, "success");
        assert_eq!(envelope["summary"], json!("first canonical result"));
        assert_eq!(
            duplicate.get("status").and_then(Value::as_str),
            Some("duplicate_ignored"),
            "TP07 regression: duplicate outcome stays explicit for route={route}"
        );
    }
}

#[test]
#[serial(env)]
fn tp10_custom_status_bytes_survive_the_durable_result_store() {
    for (index, status) in ["novel/outcome", "阶段-继续", "failed", "stage_result"]
        .into_iter()
        .enumerate()
    {
        let case = Case::new(&format!("tp10-status-{index}"));
        let task_id = format!("task-tp10-{index}");
        let result_id = format!("res-tp10-{index}");
        case.seed_tasks(&[task(&task_id, Some("pipeline"))]);
        case.submit(&task_id, &result_id, status, "opaque status regression");
        let (_, stored_status, stored) = case.stored_result(&result_id).expect("stored status");
        assert_eq!(
            stored_status, status,
            "TP10 status column must preserve the task-local string byte/semantic value"
        );
        assert_eq!(
            stored["status"],
            json!(status),
            "TP10 canonical envelope must preserve custom status"
        );
    }
}

#[test]
fn tp10_public_report_result_schema_has_no_global_business_status_enum() {
    let contract = report_result_contract();
    let status = &contract["inputSchema"]["properties"]["status"];
    assert!(
        status.get("enum").is_none(),
        "TP10 RED global_status_taxonomy_remains: report_result business status is an opaque string, not a framework enum; schema={status}"
    );
    assert_eq!(
        status.get("type"),
        Some(&json!("string")),
        "TP10 RED opaque_status_surface_missing: report_result status must accept arbitrary strings; schema={status}"
    );
}

#[test]
#[serial(env)]
fn tp11_pipeline_does_not_sniff_summary_or_failed_status() {
    for (index, (status, summary)) in [
        ("failed", "final timeout bounce stage_pass"),
        ("custom", "failed fallback final_review"),
        ("stage_result", "ordinary summary"),
    ]
    .into_iter()
    .enumerate()
    {
        let case = Case::new(&format!("tp11-sniff-{index}"));
        let task_id = format!("task-tp11-{index}");
        let result_id = format!("res-tp11-{index}");
        case.seed_tasks(&[task(&task_id, Some("pipeline"))]);
        case.submit(&task_id, &result_id, status, summary);
        assert_eq!(
            case.result_count(&task_id),
            1,
            "TP11 positive backing: anti-sniff input must reach durable storage"
        );
        assert_eq!(
            case.leader_message_count(&task_id),
            0,
            "TP11 RED text_sniff_or_route_missing: pipeline status/summary words must not create a live leader result; status={status:?} summary={summary:?}"
        );
    }
}

#[test]
#[serial(env)]
fn tp13_stage_outcome_is_only_an_opaque_pipeline_result_fact() {
    for (index, status) in ["stage_result", "stage_pass", "final_review", "bounce"]
        .into_iter()
        .enumerate()
    {
        let case = Case::new(&format!("tp13-stage-{index}"));
        let task_id = format!("task-tp13-{index}");
        let result_id = format!("res-tp13-{index}");
        case.seed_tasks(&[task(&task_id, Some("pipeline"))]);
        case.submit(&task_id, &result_id, status, "stage word is task-local");
        let (_, stored_status, _) = case.stored_result(&result_id).expect("stage result");
        assert_eq!(
            stored_status, status,
            "TP13 stage outcome changed in transit"
        );
        assert_eq!(
            case.leader_message_count(&task_id),
            0,
            "TP13 RED stage_plane_not_moved: stage outcome belongs to pipeline result storage, not live presentation; status={status}"
        );
    }
}

#[test]
#[serial(env)]
fn tp14_legacy_presentation_cannot_override_task_pipeline_route() {
    let case = Case::new("tp14-legacy-second-writer");
    case.seed_tasks(&[task("task-tp14", Some("pipeline"))]);
    case.submit_extra(
        "task-tp14",
        "res-tp14",
        "stage_result",
        "legacy presentation must not win",
        json!({
            "presentation": {
                "sink": "leader",
                "class": "stage_result",
                "case_id": "legacy-case"
            }
        }),
    );
    assert_eq!(
        case.result_count("task-tp14"),
        1,
        "TP14 positive backing: compatibility input still has one canonical result"
    );
    assert_eq!(
        case.leader_message_count("task-tp14"),
        0,
        "TP14 RED task_route_not_authoritative: old presentation metadata must not override task result_route=pipeline or form a second route writer"
    );
}

#[test]
#[serial(env)]
fn tp16_explicit_leader_and_pipeline_share_identity_and_dedupe_semantics() {
    for route in ["leader", "pipeline"] {
        let case = Case::new(&format!("tp16-shared-{route}"));
        let task_id = format!("task-tp16-{route}");
        let result_id = format!("res-tp16-{route}");
        case.seed_tasks(&[task(&task_id, Some(route))]);
        case.submit(&task_id, &result_id, "success", "shared canonical result");
        case.submit(&task_id, &result_id, "success", "duplicate");
        let (stored_task, _, stored) = case.stored_result(&result_id).expect("shared result");
        assert_eq!(stored_task, task_id);
        assert_eq!(stored["result_id"], json!(result_id));
        assert_eq!(case.result_count(&task_id), 1);
        let expected_live = i64::from(route == "leader");
        assert_eq!(
            case.leader_message_count(&task_id),
            expected_live,
            "TP16 RED route_changed_reliability: route changes only the consumer, not canonical identity/dedupe; route={route}"
        );
    }
}

#[test]
fn tp17_negative_oracle_canary_distinguishes_backing_absence_from_live_absence() {
    fn pipeline_negative_oracle(backing_results: usize, live_results: usize) -> bool {
        backing_results == 1 && live_results == 0
    }

    assert!(
        pipeline_negative_oracle(1, 0),
        "TP17 synthetic positive canary must accept durable backing plus zero live result"
    );
    assert!(
        !pipeline_negative_oracle(0, 0),
        "TP17 synthetic negative canary must reject vacuous no-op (no backing, no live result)"
    );
    assert!(
        !pipeline_negative_oracle(1, 1),
        "TP17 synthetic negative canary must reject an actually-live pipeline result"
    );
}

#[allow(dead_code)]
fn _hermetic_boundary_marker(_: &hermetic_guard::HermeticTestEnv) {}
