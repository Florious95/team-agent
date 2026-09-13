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
        "provider": "codex", "status": "running",
        "capture_state": "pending_first_turn", "spawn_cwd": cwd,
        "spawned_at": "2026-09-13T00:00:00Z"
    }}})
}

fn rollout(path: &Path, cwd: &Path, id: &str, created_at: &str, identity: Option<&str>) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let meta = json!({"type": "session_meta", "payload": {
        "id": id, "cwd": cwd, "created_at": created_at
    }});
    let marker = json!({"text": identity.map(|id| {
        format!("You are Team Agent worker `{id}` with role `fixture`.")
    })});
    std::fs::write(path, format!("{meta}\n{marker}\n")).unwrap();
}

#[test]
#[serial(env)]
fn pending_codex_discovers_late_nested_cwd_and_home_sessions_after_repeated_empty_passes() {
    let env = hermetic_guard::HermeticTestEnv::enter("discovery-late");
    for storage in ["cwd", "home"] {
        let cwd = env.workspace(storage);
        let root = if storage == "cwd" {
            cwd.join("cache/nested")
        } else {
            env.home().join(".codex/sessions/2026/09/13")
        };
        let mut state = pending(&cwd);
        for _ in 0..2 {
            let report =
                capture_missing_provider_sessions_once(&mut state, &mut get_adapter, true, 0)
                    .unwrap();
            assert!(report.assigned.is_empty());
            assert!(report.capture_failures.is_empty());
            assert_eq!(state["agents"]["worker"]["capture_state"], "pending_first_turn");
            assert!(state["agents"]["worker"]["session_id"].is_null());
        }
        // These all look like session files to the actual collector. Each
        // must still fail its original generation, cwd, or identity guard.
        let foreign_cwd = env.workspace("foreign");
        rollout(
            &root.join("stale.jsonl"),
            &cwd,
            "stale",
            "2026-09-12T00:00:00Z",
            None,
        );
        rollout(
            &root.join("foreign.jsonl"),
            &foreign_cwd,
            "foreign",
            "2026-09-13T01:00:00Z",
            None,
        );
        rollout(
            &root.join("identity.jsonl"),
            &cwd,
            "other",
            "2026-09-13T01:00:00Z",
            Some("other"),
        );
        let report =
            capture_missing_provider_sessions_once(&mut state, &mut get_adapter, true, 0)
                .unwrap();
        assert!(report.assigned.is_empty());
        assert_eq!(report.candidate_count_by_agent.get("worker"), Some(&0));

        let target = root.join("late.jsonl");
        rollout(&target, &cwd, "late", "2026-09-13T01:00:00Z", None);
        let report =
            capture_missing_provider_sessions_once(&mut state, &mut get_adapter, true, 0)
                .unwrap();
        assert_eq!(report.assigned, vec!["worker"]);
        assert!(report.capture_failures.is_empty());
        assert_eq!(state["agents"]["worker"]["session_id"], "late");
        assert_eq!(state["agents"]["worker"]["rollout_path"], json!(target));
        assert_eq!(state["agents"]["worker"]["capture_state"], "captured");
    }
}

#[test]
#[serial(env)]
fn full_discovery_preserves_competitors_expected_id_and_cross_team_claims() {
    let env = hermetic_guard::HermeticTestEnv::enter("discovery-ownership");
    let cwd = env.workspace("ownership");
    let first = cwd.join("source/first.jsonl");
    let second = cwd.join("source/second.jsonl");
    for (path, id) in [(&first, "first"), (&second, "second")] {
        rollout(path, &cwd, id, "2026-09-13T01:00:00Z", None);
    }
    let mut state = pending(&cwd);
    let report =
        capture_missing_provider_sessions_once(&mut state, &mut get_adapter, false, 0)
            .unwrap();
    assert_eq!(report.candidate_count_by_agent.get("worker"), Some(&2));
    assert!(report.assigned.is_empty());
    assert_eq!(report.ambiguous.len(), 1);
    assert_eq!(report.ambiguous[0].agent_id, "worker");
    assert!(state["agents"]["worker"]["session_id"].is_null());

    state["agents"]["worker"]["_pending_session_id"] = json!("first");
    state["teams"] = json!({"other": {"agents": {"worker": {
        "provider": "codex", "status": "stopped",
        "session_id": "first", "rollout_path": first
    }}}});
    let report =
        capture_missing_provider_sessions_once(&mut state, &mut get_adapter, false, 0)
            .unwrap();
    assert_eq!(report.candidate_count_by_agent.get("worker"), Some(&1));
    assert!(report.assigned.is_empty());
    assert!(state["agents"]["worker"]["session_id"].is_null());

    // Remove only the synthetic claim: expected-id discovery may now bind
    // first, never the sibling that was also fully enumerated.
    state.as_object_mut().unwrap().remove("teams");
    let report =
        capture_missing_provider_sessions_once(&mut state, &mut get_adapter, true, 0)
            .unwrap();
    assert_eq!(report.assigned, vec!["worker"]);
    assert_eq!(state["agents"]["worker"]["session_id"], "first");
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
