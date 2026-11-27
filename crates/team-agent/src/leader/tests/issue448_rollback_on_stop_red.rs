//! Issue #448 RED (publication level): the managed launcher's exit-time
//! `InceptionRegistryPublication::restore()` must succeed after a clean
//! `team-agent stop` already unregistered the published entry.
//!
//! Chain under test is the production one: `incept_launched_leader` (managed
//! launch publication) -> `cli::leader_port::unregister_after_shutdown_success`
//! (the `team-agent stop` success hook) -> `publication.restore()` (launcher
//! exit). See `.team/artifacts/real-incident-cards/issue-448-rollback-on-stop/CONTRACT.md`.

use super::*;
use serde_json::json;

const TEAM: &str = "current";

/// Restores HOME and removes the scratch root even when an assertion panics.
struct HomeGuard(Option<std::ffi::OsString>, PathBuf);

impl Drop for HomeGuard {
    fn drop(&mut self) {
        match self.0.take() {
            Some(home) => std::env::set_var("HOME", home),
            None => std::env::remove_var("HOME"),
        }
        let _ = std::fs::remove_dir_all(&self.1);
    }
}

fn hermetic(tag: &str) -> (HomeGuard, PathBuf) {
    let root = std::env::temp_dir().join(format!(
        "ta-i448-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let home = root.join("home");
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    let workspace = std::fs::canonicalize(workspace).unwrap();
    let guard = HomeGuard(std::env::var_os("HOME"), root);
    std::env::set_var("HOME", &home);
    (guard, workspace)
}

fn incept(ws: &Path) -> super::super::lease::InceptionRegistryPublication {
    let identity = LeaderIdentity {
        leader_session_uuid: uuid("fp", &ws.to_string_lossy(), "tester", TEAM),
        leader_session_uuid_source: LeaderSessionUuidSource::Derived,
        machine_fingerprint: "fp".into(),
        workspace_abspath: ws.to_path_buf(),
        os_user: "tester".into(),
        team_id: TeamKey::new(TEAM),
    };
    let observed = crate::transport::PaneInfo {
        pane_id: PaneId::new("%448"),
        session: SessionName::new("team-agent-leader-claude_code-i448"),
        window_index: None,
        window_name: Some(WindowName::new("claude_code")),
        pane_index: None,
        tty: None,
        current_command: None,
        current_path: Some(ws.to_path_buf()),
        active: true,
        pane_pid: Some(4480),
        leader_env: BTreeMap::new(),
    };
    super::super::lease::incept_launched_leader(
        ws,
        &identity,
        Provider::ClaudeCode,
        &observed,
        "/private/tmp/ta-i448-unit.sock",
        &json!({}),
        false,
        LeaderStartMode::ManagedTmuxClient,
    )
    .expect("SETUP: managed inception succeeds")
    .expect("SETUP: managed inception returns its registry publication")
}

fn entry_path(ws: &Path) -> PathBuf {
    crate::leader::registry::registry_dir()
        .unwrap()
        .join(format!("{}__{TEAM}.json", crate::leader::registry::workspace_hash(ws)))
}

#[test]
#[serial_test::serial(env)]
fn launcher_restore_after_clean_stop_without_preimage_is_ok() {
    let (_home, ws) = hermetic("none");
    let publication = incept(&ws);
    let path = entry_path(&ws);
    assert!(path.is_file(), "SETUP: managed launch published {}", path.display());

    let removed = crate::cli::leader_port::unregister_after_shutdown_success(&ws, Some(TEAM));
    assert_eq!(removed.as_deref(), Some(path.as_path()), "SETUP: stop hook unregistered");

    let restored = publication.restore();
    assert!(
        restored.is_ok(),
        "#448: launcher exit after a clean `team-agent stop` must not fail the registry \
         rollback; got {restored:?}"
    );
    assert!(!path.exists(), "#448: rollback must keep the unbound entry absent");
}

#[test]
#[serial_test::serial(env)]
fn launcher_restore_after_clean_stop_with_preimage_is_ok_and_does_not_resurrect() {
    let (_home, ws) = hermetic("preimage");
    let path = entry_path(&ws);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let preimage = b"{\n  \"stale_prior_discovery_record\": true\n}\n";
    std::fs::write(&path, preimage).unwrap();
    let publication = incept(&ws);
    assert_ne!(std::fs::read(&path).unwrap(), preimage, "SETUP: launch replaced preimage");

    assert!(
        crate::cli::leader_port::unregister_after_shutdown_success(&ws, Some(TEAM)).is_some(),
        "SETUP: stop hook unregistered"
    );

    let restored = publication.restore();
    assert!(
        matches!(restored, Ok(outcome) if outcome != crate::leader::registry::RegistryRollback::Restored),
        "#448: after canonical unbind the launcher's receipt is no longer current; restore \
         must be a benign non-restoring Ok, got {restored:?}"
    );
    assert!(
        !path.exists(),
        "#448: launcher rollback must not resurrect a stale preimage after `team-agent stop`"
    );
}

#[test]
#[serial_test::serial(env)]
fn launcher_restore_without_stop_still_rolls_back_own_entry() {
    let (_home, ws) = hermetic("control");
    let publication = incept(&ws);
    let path = entry_path(&ws);
    assert!(path.is_file(), "SETUP: managed launch published");
    assert_eq!(
        publication.restore().unwrap(),
        crate::leader::registry::RegistryRollback::Restored,
        "control: a still-current launcher entry is rolled back on exit"
    );
    assert!(!path.exists(), "control: own entry removed");
}
