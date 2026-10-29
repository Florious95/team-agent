use super::*;
use std::os::unix::fs::DirBuilderExt;

struct Fixture { root: PathBuf, args: QuickStartArgs, role: PathBuf }
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("contract-override-{}-{}", std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        std::fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
        let directory = root.join("roles");
        std::fs::create_dir_all(directory.join("agents")).unwrap();
        std::fs::write(directory.join("TEAM.md"), "---\nname: test\n---\nBounded fixture\n").unwrap();
        let role = directory.join("agents/worker.md");
        std::fs::write(&role, "---\nprovider: cursor\nmodel: old-model\n---\nOld instructions\n").unwrap();
        let args = QuickStartArgs { workspace: root.clone(), agents_dir: directory, name: None,
            team_id: None, yes: false, json: false, detail: false, backend: Some("tmux".into()) };
        Self { root, args, role }
    }
}
impl Drop for Fixture { fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.root); } }

#[test]
fn selected_seat_ignores_sibling_native_admission_but_keeps_forwarding_routes() {
    let fixture = Fixture::new();
    std::fs::write(&fixture.role, "---\nprovider: grok\nmodel: grok-4.7\nauth_mode: subscription\ndangerously_skip_permissions: true\n---\nTarget\n").unwrap();
    let roles = fixture.args.agents_dir.join("agents");
    std::fs::write(roles.join("legacy.md"), "---\nprovider: claude\nprofile: claude-default\n---\nLegacy\n").unwrap();
    for rejected in ["profile: codex-default", "auth_mode: compatible_api", "dangerously_skip_permissions: invalid", "contract_runtime: invalid"] {
        std::fs::write(roles.join("retired.md"), format!("---\nprovider: codex\nmodel: native-model\n{rejected}\n---\nRetired sibling\n")).unwrap();
        assert!(read_team_inner(&fixture.args, || Ok(None), None, None).is_err());
        let config = read_team_inner(&fixture.args, || Ok(None), None, Some("worker")).unwrap().unwrap();
        assert_eq!(config.roles.len(), 1);
        assert_eq!(config.roles[0].provider.as_str(), "grok_build");
        assert!(config.roles[0].bypass);
        assert!(config.members.iter().any(|member| member.id == "legacy" && member.runtime == RuntimeFamily::Legacy));
        assert!(read_team_inner(&fixture.args, || Ok(None), None, Some("retired")).is_err());
    }
    assert!(read_team_inner(&fixture.args, || Ok(None), None, Some("missing")).unwrap().is_none());
    assert!(!fixture.root.join(".team").exists());
}

#[test]
fn virtual_role_change_is_readonly_and_binds_new_instruction_digest() {
    let fixture = Fixture::new();
    let before = std::fs::read(&fixture.role).unwrap();
    let path = fixture.role.canonicalize().unwrap();
    let contents = "---\nprovider: cursor\nmodel: new-model\n---\nNew instructions\n";
    let original = read_team_inner(&fixture.args, || Ok(None), None, None).unwrap().unwrap();
    let changed = read_team_inner(&fixture.args, || Ok(None), Some((&path, contents)), Some("worker")).unwrap().unwrap();
    assert_eq!(changed.roles[0].model, "new-model");
    assert_eq!(changed.roles[0].body, "New instructions\n");
    assert_ne!(changed.roles[0].prompt.sha256, original.roles[0].prompt.sha256);
    assert_eq!(std::fs::read(&fixture.role).unwrap(), before);
    assert!(!fixture.root.join(".team").exists());
    assert!(!fixture.root.join(".cursor").exists());
}

#[test]
fn virtual_api_profile_is_refused_without_file_effects() {
    let fixture = Fixture::new();
    let before = std::fs::read(&fixture.role).unwrap();
    let path = fixture.role.canonicalize().unwrap();
    let contents = "---\nprovider: cursor\nmodel: new-model\nprofile: unsupported\n---\nInstructions\n";
    assert!(matches!(read_team_inner(&fixture.args, || Ok(None), Some((&path, contents)), Some("worker")), Err(ConfigError::Unsupported(_))));
    assert_eq!(std::fs::read(&fixture.role).unwrap(), before);
    assert!(!fixture.root.join(".team").exists());
}

#[test]
fn virtual_role_override_cannot_match_a_different_source() {
    let fixture = Fixture::new();
    let other = fixture.root.join("other.md");
    let contents = "---\nprovider: cursor\nmodel: wrong-model\n---\nOther\n";
    let config = read_team_inner(&fixture.args, || Ok(None), Some((&other, contents)), Some("worker")).unwrap().unwrap();
    assert_eq!(config.roles[0].model, "old-model");
}
