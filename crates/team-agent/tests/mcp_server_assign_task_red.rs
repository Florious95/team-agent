#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[path = "support/hermetic.rs"]
mod hermetic_guard;
#[allow(dead_code)]
fn _hermetic_boundary_marker(_: &hermetic_guard::HermeticTestEnv) {}

use serde_json::{json, Value};
use team_agent::mcp_server::TeamOrchestratorTools;
use team_agent::message_store::MessageStore;
use team_agent::model::ids::{AgentId, TeamKey};

fn unique_ws(tag: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!("ta-rs-mcp-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&path).unwrap();
    path
}

#[test]
fn assign_task_registers_state_and_delegates_message_delivery() {
    let ws = unique_ws("assign-task-223");
    team_agent::state::persist::save_runtime_state(
        &ws,
        &json!({
            "active_team_key": "teamA",
            "session_name": "team-a",
            "agents": {
                "worker-1": {"status": "running", "provider": "codex"}
            },
            "tasks": [{"id": "task-old", "assignee": "worker-1", "accepted_result_id": "res-1"}],
            "teams": {
                "teamA": {
                    "agents": {
                        "worker-1": {"status": "running", "provider": "codex"}
                    }
                }
            }
        }),
    )
    .unwrap();
    let tools = TeamOrchestratorTools::with_identity(
        &ws,
        Some(AgentId::new("leader")),
        Some(TeamKey::new("teamA")),
    );
    let ok = tools
        .assign_task(
            &json!({"id": "task-223", "title": "Fix collect floor", "assignee": "worker-1"}),
            Some("please fix collect"),
        )
        .expect("assign_task ok");
    assert!(
        ok.fields.get("task_id").is_none(),
        "assign_task return is compacted send_message result only"
    );
    assert!(
        ok.fields.get("to").is_none(),
        "assign_task must not inject extra `to`"
    );
    assert_eq!(ok.fields.get("status"), Some(&json!("accepted")));
    let message_id = ok
        .fields
        .get("message_id")
        .and_then(Value::as_str)
        .expect("message_id");

    let state = team_agent::state::persist::load_runtime_state(&ws).unwrap();
    assert_eq!(state["tasks"][0]["id"], json!("task-old"));
    assert_eq!(
        state["tasks"][1]["id"],
        json!("task-223"),
        "new task appends at the end"
    );
    assert!(
        state["tasks"][1].get("status").is_none(),
        "missing status is stored as-is"
    );
    assert!(
        state["teams"]["teamA"].get("status").is_none(),
        "existing team missing status is not backfilled"
    );
    assert_eq!(state["teams"]["teamA"]["tasks"][0]["id"], json!("task-223"));
    assert!(state["teams"]["teamA"]["tasks"][0].get("status").is_none());

    let store = MessageStore::open(&ws).unwrap();
    let conn = team_agent::db::schema::open_db(store.db_path()).unwrap();
    let row: (String, String, String, String, String) = conn
        .query_row(
            "select task_id, sender, recipient, content, owner_team_id from messages where message_id = ?1",
            [message_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .unwrap();
    assert_eq!(
        row,
        (
            "task-223".to_string(),
            "leader".to_string(),
            "worker-1".to_string(),
            "please fix collect".to_string(),
            "teamA".to_string(),
        )
    );
}

#[test]
fn assign_task_updates_existing_fields_and_uses_golden_content_fallbacks() {
    let ws = unique_ws("assign-task-update-223");
    team_agent::state::persist::save_runtime_state(
        &ws,
        &json!({
            "active_team_key": "teamA",
            "session_name": "team-a",
            "agents": {
                "worker-1": {"status": "running", "provider": "codex"}
            },
            "tasks": [{
                "id": "task-keep",
                "assignee": "worker-1",
                "accepted_result_id": "res-keep",
                "status": "done"
            }],
            "teams": {
                "teamA": {
                    "status": "alive",
                    "agents": {
                        "worker-1": {"status": "running", "provider": "codex"}
                    },
                    "tasks": [{
                        "id": "task-keep",
                        "assignee": "worker-1",
                        "accepted_result_id": "res-keep"
                    }]
                }
            }
        }),
    )
    .unwrap();
    let tools = TeamOrchestratorTools::with_identity(
        &ws,
        Some(AgentId::new("leader")),
        Some(TeamKey::new("teamA")),
    );
    let task = json!({
        "id": "task-keep",
        "description": "description wins",
        "title": "title loses",
        "assignee": "worker-1"
    });
    let ok = tools.assign_task(&task, None).expect("assign ok");
    let message_id = ok
        .fields
        .get("message_id")
        .and_then(Value::as_str)
        .expect("message_id");

    let state = team_agent::state::persist::load_runtime_state(&ws).unwrap();
    assert_eq!(
        state["tasks"].as_array().unwrap().len(),
        1,
        "existing task updates in place"
    );
    assert_eq!(state["tasks"][0]["description"], json!("description wins"));
    assert_eq!(state["tasks"][0]["accepted_result_id"], json!("res-keep"));
    assert_eq!(state["tasks"][0]["status"], json!("done"));
    assert_eq!(
        state["teams"]["teamA"]["tasks"][0]["accepted_result_id"],
        json!("res-keep")
    );

    let store = MessageStore::open(&ws).unwrap();
    let conn = team_agent::db::schema::open_db(store.db_path()).unwrap();
    let content: String = conn
        .query_row(
            "select content from messages where message_id = ?1",
            [message_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(content, "description wins");

    let fallback = json!({"id": "task-json", "assignee": "worker-1", "priority": 2});
    let ok = tools
        .assign_task(&fallback, None)
        .expect("json fallback assign ok");
    let fallback_message_id = ok
        .fields
        .get("message_id")
        .and_then(Value::as_str)
        .expect("message_id");
    let fallback_content: String = conn
        .query_row(
            "select content from messages where message_id = ?1",
            [fallback_message_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        fallback_content,
        r#"{"id": "task-json", "assignee": "worker-1", "priority": 2}"#
    );
}
