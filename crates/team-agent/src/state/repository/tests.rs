
use super::{reapply_scope, ReapplyScope, StateWriteIntent};

#[test]
fn mcp_reapply_scope_matches_legacy_helpers() {
    assert!(
        reapply_scope(&StateWriteIntent::McpUpdateStateNote {
            team_key: Some("team-a"),
        }) == ReapplyScope::Team
    );
    assert!(
        reapply_scope(&StateWriteIntent::McpAssignTask {
            team_key: Some("team-a"),
            task_id: "task-a",
        }) == ReapplyScope::Root
    );
}

#[test]
fn optional_unmigrated_read_preserves_missing_corrupt_and_valid_shapes(
) -> Result<(), Box<dyn std::error::Error>> {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_nanos();
    let workspace = std::env::temp_dir().join(format!("state-repository-read-{nonce}"));
    let repository = super::StateRepository::new(&workspace);
    assert!(repository
        .load_workspace_if_exists_without_migrations()?
        .is_none());

    let path = super::helper_workspace_path(&workspace);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, b"{not-json")?;
    assert!(matches!(
        repository.load_workspace_if_exists_without_migrations(),
        Err(super::StateError::Json(_))
    ));

    std::fs::write(&path, br#"{"legacy":true}"#)?;
    assert_eq!(
        repository.load_workspace_if_exists_without_migrations()?,
        Some(serde_json::json!({"legacy": true}))
    );
    std::fs::remove_dir_all(workspace)?;
    Ok(())
}

#[test]
fn coordinator_observations_keep_legacy_identity_and_shape()
    -> Result<(), Box<dyn std::error::Error>> {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_nanos();
    let workspace =
        std::env::temp_dir().join(format!("state-repository-legacy-observation-{nonce}"));
    let initial = serde_json::json!({
        "active_team_key": "team",
        "agents": {"w1": {"agent_id": "w1", "provider": "codex", "status": "running"}}
    });
    crate::state::persist::save_runtime_state(&workspace, &initial)?;
    let repository = super::StateRepository::new(&workspace);
    let before = repository.load_workspace()?;

    let mut observed = before.clone();
    observed["coordinator"] = serde_json::json!({
        "abnormal_exit_watch": {"w1": {"last_error_observation_key": "error-1"}}
    });
    repository.commit_observations(
        StateWriteIntent::CoordinatorTick {
            team_key: "current",
        },
        &before,
        &observed,
    )?;
    let first = repository.load_workspace()?;
    assert!(
        first.get("teams").is_none(),
        "legacy observation must not synthesize teams.current"
    );
    assert_eq!(first["active_team_key"], serde_json::json!("team"));
    assert_eq!(
        first["coordinator"]["abnormal_exit_watch"]["w1"]["last_error_observation_key"],
        serde_json::json!("error-1")
    );

    let mut observed = first.clone();
    observed["coordinator"]["abnormal_exit_watch"]["w1"]["last_error_observation_key"] =
        serde_json::json!("error-2");
    repository.commit_observations(
        StateWriteIntent::CoordinatorTick {
            team_key: "current",
        },
        &first,
        &observed,
    )?;
    let second = repository.load_workspace()?;
    assert!(
        second.get("teams").is_none(),
        "legacy shape must remain root-only"
    );
    assert_eq!(second["active_team_key"], serde_json::json!("team"));
    assert_eq!(
        second["coordinator"]["abnormal_exit_watch"]["w1"]["last_error_observation_key"],
        serde_json::json!("error-2")
    );

    let mut changed_latest = second.clone();
    changed_latest["active_team_key"] = serde_json::json!("other");
    crate::state::persist::save_runtime_state(&workspace, &changed_latest)?;
    let err = repository
        .commit_observations(
            StateWriteIntent::CoordinatorTick {
                team_key: "current",
            },
            &second,
            &observed,
        )
        .expect_err("identity drift must refuse the stale legacy observation");
    assert!(matches!(err, super::StateError::SaveConflict(_)));

    std::fs::remove_dir_all(workspace)?;
    Ok(())
}
