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
    let state = serde_json::json!({
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

    let freshness = compute_runtime_freshness(
        Path::new("/nonexistent/status-port-typed-provider-exit-test"),
        &state,
        &health_fixture(),
    );

    assert!(!freshness.provider_exited_agents.contains("pane_only"));
    assert!(freshness.provider_exited_agents.contains("typed_exit"));
}
