#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[path = "support/hermetic.rs"]
mod hermetic_guard;
#[allow(dead_code)]
fn _hermetic_boundary_marker(_: &hermetic_guard::HermeticTestEnv) {}

use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::json;
use team_agent::mcp_server::TeamOrchestratorTools;
use team_agent::model::enums::ResultStatus;

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

#[test]
fn report_result_requires_framework_identity_before_delegate() {
    let ws = tmp_workspace("report-defaults");
    let tools = TeamOrchestratorTools::with_identity(&ws, None, None);

    let error = tools
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
        .expect_err("report_result must fail closed without a captured identity");

    assert_eq!(
        error.reason,
        team_agent::mcp_server::ToolErrorReason::McpScopeRefused
    );
    assert!(error.message.contains("identity_mismatch"));
    let store = team_agent::message_store::MessageStore::open(&ws).unwrap();
    let conn = team_agent::db::schema::open_db(store.db_path()).unwrap();
    let count: i64 = conn
        .query_row("select count(*) from results", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count, 0, "identity rejection must precede result persistence");
}
