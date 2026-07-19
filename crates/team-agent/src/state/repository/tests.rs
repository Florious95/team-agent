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
