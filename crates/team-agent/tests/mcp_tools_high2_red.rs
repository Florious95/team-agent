#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{json, Value};
use team_agent::mcp_server::TeamOrchestratorTools;
use team_agent::model::enums::ResultStatus;
use team_agent::model::ids::{AgentId, TeamKey};

fn tmp_workspace(tag: &str) -> std::path::PathBuf {
    static N: AtomicU64 = AtomicU64::new(0);
    let ws = std::env::temp_dir().join(format!(
        "ta-rs-mcp-high2-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(ws.join(".team").join("runtime")).unwrap();
    ws
}

fn tool_value(ok: team_agent::mcp_server::ToolOk) -> Value {
    serde_json::to_value(ok).unwrap()
}

#[test]
fn update_state_appends_note_saves_state_and_writes_file() {
    let ws = tmp_workspace("update-state");
    team_agent::state::persist::save_runtime_state(&ws, &json!({"notes": ["old note"]})).unwrap();
    let tools = TeamOrchestratorTools::with_identity(&ws, Some(AgentId::new("leader")), None);

    let value = tool_value(tools.update_state("new note").expect("update_state ok"));

    let state_file = value["state_file"].as_str().expect("state_file returned");
    assert_eq!(value["ok"], json!(true));
    assert!(std::path::Path::new(state_file).exists(), "team_state.md must be written");
    let state = team_agent::state::persist::load_runtime_state(&ws).unwrap();
    assert_eq!(state["notes"], json!(["old note", "new note"]));
    let text = std::fs::read_to_string(state_file).unwrap();
    assert!(text.contains("old note"));
    assert!(text.contains("new note"));
}

#[test]
fn report_result_fills_golden_defaults_before_delegate() {
    let ws = tmp_workspace("report-defaults");
    let tools = TeamOrchestratorTools::with_identity(&ws, None, None);

    let value = tool_value(
        tools
            .report_result(
                Some(&json!({})),
                None,
                ResultStatus::Success,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
            )
            .expect("report_result ok"),
    );

    assert_eq!(value["task_id"], json!("manual"));
    assert_eq!(value["agent_id"], json!("unknown"));
    let store = team_agent::message_store::MessageStore::open(&ws).unwrap();
    let conn = team_agent::db::schema::open_db(store.db_path()).unwrap();
    let envelope: String = conn
        .query_row("select envelope from results where task_id = 'manual'", [], |row| row.get(0))
        .unwrap();
    let stored: Value = serde_json::from_str(&envelope).unwrap();
    assert_eq!(stored["summary"], json!("completed"));
    assert_eq!(stored["task_id"], json!("manual"));
    assert_eq!(stored["agent_id"], json!("unknown"));
}

#[test]
fn request_human_uses_owner_team_scope_and_logs_unknown_identity() {
    // Post-#230 N31/N32 funnel (cr-approved I-3 + I-7):
    //
    // [OLD assertion] request_human did a raw `store.create_message(... None ...)` that
    // hardcoded `owner_team_id=NULL`, ignoring the tool's owner-team scope. The legacy
    // assertion was `owner_team_id == None` — i.e. "request_human never carries team
    // scope on its message row".
    //
    // [NEW assertion] request_human routes through the shared leader-delivery primitive,
    // which derives the message row's `owner_team_id` from the runtime state's
    // `active_team_key`. This is the SAME scope mechanism every other leader-bound
    // funnel caller uses (send_to_leader / report_result / broadcast / peer-mirror),
    // satisfying N12/N18/N30 (scope audit) and N31/N32 (single funnel). With no state
    // seeded, `active_team_key` falls back to the workspace dirname — assert that the
    // row carries the resolved scope (a non-empty string), not NULL.
    // Identity inference behavior (`sender="unknown"` when agent_id=None,
    // `mcp.identity_inference_failed` audit) is unchanged.
    let ws = tmp_workspace("request-human");
    let tools = TeamOrchestratorTools::with_identity(&ws, None, Some(TeamKey::new("teamA")));

    let value = tool_value(
        tools
            .request_human("need a decision", Some("task-1"), None)
            .expect("request_human ok"),
    );

    let message_id = value["message_id"].as_str().expect("message_id returned");
    let store = team_agent::message_store::MessageStore::open(&ws).unwrap();
    let conn = team_agent::db::schema::open_db(store.db_path()).unwrap();
    let (owner_team_id, sender): (Option<String>, String) = conn
        .query_row(
            "select owner_team_id, sender from messages where message_id = ?1",
            [message_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert!(
        owner_team_id.as_deref().is_some_and(|t| !t.is_empty()),
        "N31/N32 funnel: leader-bound row must carry a resolved owner_team_id (active_team_key fallback), not NULL; got {owner_team_id:?}"
    );
    assert_eq!(sender, "unknown");
    let events = team_agent::event_log::EventLog::new(&ws).tail(10).unwrap();
    assert!(events
        .iter()
        .any(|event| event["event"] == json!("mcp.identity_inference_failed")));
}
