
use super::StateWriteIntent;

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
    let path = super::helper_workspace_path(&workspace);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, serde_json::to_vec(&initial)?)?;
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
    std::fs::write(&path, serde_json::to_vec(&changed_latest)?)?;
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

#[test]
fn stopped_start_commit_updates_exact_target_and_both_mirrors()
    -> Result<(), Box<dyn std::error::Error>> {
    use serde_json::json;
    for target in ["team-a", "team-b"] {
        let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_nanos();
        let workspace = std::env::temp_dir().join(format!("state-start-stopped-{target}-{nonce}"));
        let held = json!({"agent_id":"worker","provider":"grok_build","runtime_family":"contract",
            "status":"stopped","explicit_stop":true,"generation":1,"contract_instance":"instance-one",
            "session_id":"same-session","native_endpoint":"old-endpoint"});
        let peer = json!({"agent_id":"peer","provider":"pi","status":"stopped","explicit_stop":true,
            "session_id":"peer-session","generation":1});
        let control = json!({"agent_id":"control","provider":"pi","status":"running","session_id":"control-session"});
        let agents = json!({"worker":held,"peer":peer,"control":control});
        let initial = json!({"session_name":"team-a","active_team_key":"team-a","agents":agents,
            "teams":{"team-a":{"session_name":"team-a","status":"alive","agents":agents},
                     "team-b":{"session_name":"team-b","status":"alive","agents":agents}}});
        let path = super::helper_workspace_path(&workspace);
        std::fs::create_dir_all(path.parent().unwrap())?;
        std::fs::write(&path, serde_json::to_vec(&initial)?)?;
        let repository = super::StateRepository::new(&workspace);
        let before = repository.load_workspace()?;
        let other = if target == "team-a" { "team-b" } else { "team-a" };
        let committed = repository.commit(StateWriteIntent::StartAgent { team_key: target, agent_id: "worker" }, |selected| {
            let row = &mut selected["agents"]["worker"];
            row["status"] = json!("running");
            row["explicit_stop"] = json!(false);
            row["generation"] = json!(2);
            row["contract_instance"] = json!("instance-two");
            row["native_endpoint"] = json!("new-endpoint");
            // Start authority must not leak to an unrelated stopped peer.
            selected["agents"]["peer"]["status"] = json!("running");
            selected["agents"]["peer"]["explicit_stop"] = json!(false);
        })?;
        let saved = repository.load_workspace()?;
        let team = &saved["teams"][target];
        assert_eq!(team["agents"]["worker"]["status"], "running");
        assert_eq!(team["agents"]["worker"]["generation"], 2);
        assert_eq!(team["agents"]["worker"]["explicit_stop"], false);
        assert_eq!(team["agents"]["worker"]["session_id"], "same-session");
        assert_eq!(committed["agents"]["worker"], team["agents"]["worker"]);
        assert!(!crate::state::projection::agent_stopped_for_restart(&team["agents"]["worker"]));
        assert_eq!(team["agents"]["peer"], before["teams"][target]["agents"]["peer"]);
        assert_eq!(team["agents"]["control"], before["teams"][target]["agents"]["control"]);
        assert_eq!(saved["teams"][other], before["teams"][other]);
        let expected_root = if target == "team-a" { &team["agents"] } else { &before["agents"] };
        assert_eq!(&saved["agents"], expected_root);
        // An ordinary bounded delta still cannot wake a held peer.
        repository.commit(StateWriteIntent::ResultCollection { owner_team_id: Some(target) }, |selected| {
            selected["agents"]["peer"]["status"] = json!("running");
            selected["agents"]["peer"]["explicit_stop"] = json!(false);
        })?;
        let ordinary = repository.load_workspace()?;
        assert_eq!(ordinary["teams"][target]["agents"], team["agents"]);
        assert_eq!(ordinary["agents"], saved["agents"]);
        std::fs::remove_dir_all(workspace)?;
    }
    Ok(())
}
