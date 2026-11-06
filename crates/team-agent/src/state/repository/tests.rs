
use super::StateWriteIntent;

#[test]
fn clean_shutdown_releases_only_its_unchanged_receiver() -> Result<(), Box<dyn std::error::Error>> {
    use serde_json::json;
    for (clean, takeover) in [(false, false), (true, false), (true, true)] {
        let workspace = std::env::temp_dir().join(format!(
            "shutdown-receiver-{}", crate::provider::adapter::next_session_token()
        ));
        let path = super::helper_workspace_path(&workspace);
        std::fs::create_dir_all(path.parent().unwrap())?;
        let initial = json!({"active_team_key":"parent", "teams":{
            "parent":{"status":"alive","session_name":"team-parent",
                "agents":{"manager":{"status":"running","provider":"pi","pane_id":"%manager"}},
                "team_owner":{"pane_id":"%leader","owner_epoch":1},
                "leader_receiver":{"status":"attached","pane_id":"%leader","owner_epoch":1}},
            "child":{"status":"alive","session_name":"team-child","agents":{},
                "team_owner":{"pane_id":"%manager","owner_epoch":1},
                "leader_receiver":{"status":"attached","pane_id":"%manager","owner_epoch":1}}
        }});
        std::fs::write(&path, serde_json::to_vec(&initial)?)?;
        let repository = super::StateRepository::new(&workspace);
        let before = crate::state::persist::load_runtime_state(repository.workspace)?;
        let mut stopped = super::super::projection::project_top_level_view(&before, "child");
        if clean {
            stopped["status"] = json!("shutdown");
            stopped["teams"]["child"]["status"] = json!("shutdown");
        }
        if takeover {
            let mut rebound = before.clone();
            rebound["teams"]["child"]["team_owner"] = json!({"pane_id":"%new","owner_epoch":2});
            rebound["teams"]["child"]["leader_receiver"] = json!({"status":"attached","pane_id":"%new","owner_epoch":2});
            repository.save(StateWriteIntent::ClaimLeaderFreshCaller {
                team_key:"child",
                expected_owner:&before["teams"]["child"]["team_owner"],
                expected_receiver:&before["teams"]["child"]["leader_receiver"],
            }, &rebound)?;
        }
        let result = repository.save(StateWriteIntent::ShutdownTeam {team_key:Some("child"), clean}, &stopped);
        if takeover { assert!(matches!(result, Err(super::StateError::SaveConflict(_)))); }
        else { result?; }
        let saved = crate::state::persist::load_runtime_state(repository.workspace)?;
        assert_eq!(saved["teams"]["parent"], before["teams"]["parent"]);
        if takeover {
            assert_eq!(saved["teams"]["child"]["leader_receiver"]["pane_id"], "%new");
        } else if clean {
            assert!(saved["teams"]["child"]["leader_receiver"].is_null());
            assert_eq!(saved["teams"]["child"]["team_owner"], before["teams"]["child"]["team_owner"]);
        } else {
            assert_eq!(saved["teams"]["child"]["leader_receiver"], before["teams"]["child"]["leader_receiver"]);
        }
        std::fs::remove_dir_all(workspace)?;
    }
    Ok(())
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
        let before = crate::state::persist::load_runtime_state(repository.workspace)?;
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
        let saved = crate::state::persist::load_runtime_state(repository.workspace)?;
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
        // Root normalization retains null session fields that compact team
        // rows omit. Build an independent root oracle, not the compact schema.
        let mut expected_root = before["agents"].clone();
        if target == "team-a" {
            let row = &mut expected_root["worker"];
            row["status"] = json!("running");
            row["explicit_stop"] = json!(false);
            row["generation"] = json!(2);
            row["contract_instance"] = json!("instance-two");
            row["native_endpoint"] = json!("new-endpoint");
        }
        assert_eq!(saved["agents"], expected_root);
        // An ordinary bounded delta still cannot wake a held peer.
        repository.commit_messaging_checked(StateWriteIntent::MessagingDeliveryState { owner_team_id: Some(target) }, |selected| {
            selected["agents"]["peer"]["status"] = json!("running");
            selected["agents"]["peer"]["explicit_stop"] = json!(false);
            Ok(())
        })?;
        let ordinary = crate::state::persist::load_runtime_state(repository.workspace)?;
        assert_eq!(ordinary["teams"][target]["agents"], team["agents"]);
        assert_eq!(ordinary["agents"], saved["agents"]);
        std::fs::remove_dir_all(workspace)?;
    }
    Ok(())
}
