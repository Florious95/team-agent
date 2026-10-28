//! Pending discovery must keep its full candidate and ownership boundaries
//! while removing redundant directory traversal work.
#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

#[path = "support/hermetic.rs"]
mod hermetic_guard;

use std::path::Path;

use serde_json::{json, Value};
use serial_test::serial;
use team_agent::provider::session::capture::capture_missing_provider_sessions_once;
use team_agent::provider::get_adapter;

fn pending(cwd: &Path) -> Value {
    json!({"agents": {"worker": {
        "provider": "claude_code", "status": "running",
        "capture_state": "pending_first_turn", "spawn_cwd": cwd,
        "spawned_at": "2026-09-13T00:00:00Z"
    }}})
}

#[test]
#[serial(env)]
fn generic_claude_discovery_keeps_explicit_team_root_and_rejects_runtime_siblings() {
    let env = hermetic_guard::HermeticTestEnv::enter("discovery-claude");
    let cwd = env.workspace("claude");
    let root = cwd.join(".team/runtime/claude/projects");
    let owned = root.join("cwd/owned.jsonl");
    let sibling = cwd.join(".team/artifacts/other.jsonl");
    for path in [&owned, &sibling] {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let record = json!({
            "type": "user", "sessionId": "claude-owned", "cwd": cwd,
            "message": {"role": "user", "content":
                "You are Team Agent worker `worker` with role `fixture`."}
        });
        std::fs::write(path, format!("{record}\n")).unwrap();
    }
    let mut state = pending(&cwd);
    state["agents"]["worker"]["provider"] = json!("claude_code");
    state["agents"]["worker"]["spawned_at"] = json!("2020-01-01T00:00:00Z");
    state["agents"]["worker"]["claude_projects_root"] = json!(root);
    let report =
        capture_missing_provider_sessions_once(&mut state, &mut get_adapter, true, 0)
            .unwrap();
    assert_eq!(report.candidate_count_by_agent.get("worker"), Some(&1));
    assert_eq!(report.assigned, vec!["worker"]);
    assert_eq!(state["agents"]["worker"]["rollout_path"], json!(owned));
}
