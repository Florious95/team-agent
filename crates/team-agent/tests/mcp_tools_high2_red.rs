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
    // M4: no station record and no retired store may exist after the rejection.
    assert!(!ws.join(".team/runtime/team.db").exists());
    let inbox = team_agent::inbox_store::read(&ws, "leader", 10, None);
    assert!(
        inbox.map_or(true, |v| v["messages"].as_array().map_or(true, Vec::is_empty)),
        "identity rejection must precede result persistence"
    );
}
