use super::*;

fn health_fixture() -> crate::coordinator::HealthReport {
    crate::coordinator::HealthReport {
        ok: true,
        status: crate::coordinator::CoordinatorHealthStatus::Running,
        pid: Some(crate::coordinator::Pid::new(std::process::id())),
        metadata: None,
        metadata_ok: true,
        process_running: true,
        wire_metadata_ok: true,
        binary_identity_ok: true,
        binary_identity_relation: crate::coordinator::CoordinatorBinaryIdentityRelation::Same,
        service_available: true,
        metadata_mismatch_reason: None,
        current_binary_identity: crate::coordinator::CoordinatorBinaryIdentity {
            binary_path: "/test/team-agent".to_string(),
            binary_version: env!("CARGO_PKG_VERSION").to_string(),
        },
        schema: crate::coordinator::SchemaHealth {
            ok: true,
            schema_version: crate::db::schema::SCHEMA_VERSION,
            error: None,
            action: None,
        },
    }
}

#[test]
fn runtime_freshness_provider_exit_requires_typed_positive_fact() {
    let mut state = serde_json::json!({
        "active_team_key": "status-team",
        "agents": {
            "pane_only": {"provider": "codex", "rollout_path": "/fixture/pane", "spawn_epoch": 1},
            "typed_exit": {"provider": "codex", "rollout_path": "/fixture/typed", "spawn_epoch": 1}
        },
        "coordinator": {
            "abnormal_exit_watch": {
                "pane_only": {
                    "provider_process_dead": true,
                    "worker_provider_exited": false
                },
                "typed_exit": {
                    "provider_process_dead": true,
                    "worker_provider_exited": true
                }
            }
        }
    });

    for id in ["pane_only", "typed_exit"] {
        state["coordinator"]["abnormal_exit_watch"][id]["watch_identity"] =
            crate::state::abnormal_watch::observation_identity(&state, id).unwrap();
    }
    let freshness = compute_runtime_freshness(
        Path::new("/nonexistent/status-port-typed-provider-exit-test"),
        &state,
        &health_fixture(),
    );

    assert!(!freshness.provider_exited_agents.contains("pane_only"));
    assert!(freshness.provider_exited_agents.contains("typed_exit"));
}

#[test]
fn runtime_freshness_rejects_foreign_legacy_and_unknown_watch_identity() {
    let mut state = json!({
        "active_team_key": "a",
        "agents": {"w": {"provider": "codex", "rollout_path": "/fixture/w", "spawn_epoch": 1}},
        "coordinator": {"abnormal_exit_watch": {"w": {"worker_provider_exited": true}}}
    });
    let path = Path::new("/nonexistent/status-watch-188");
    assert!(compute_runtime_freshness(path, &state, &health_fixture())
        .provider_exited_agents
        .is_empty());
    let identity = crate::state::abnormal_watch::observation_identity(&state, "w").unwrap();
    state["coordinator"]["abnormal_exit_watch"]["w"]["watch_identity"] = identity;
    for (pointer, value) in [
        ("/active_team_key", json!("b")),
        ("/agents/w/provider", json!("claude")),
        ("/agents/w/spawn_epoch", json!(2)),
        ("/agents/w/rollout_path", json!(null)),
    ] {
        let mut changed = state.clone();
        *changed.pointer_mut(pointer).unwrap() = value;
        assert!(compute_runtime_freshness(path, &changed, &health_fixture())
            .provider_exited_agents
            .is_empty());
    }
}
