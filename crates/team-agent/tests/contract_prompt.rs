//! Content preservation is deterministic evidence, not native consumption.
#![cfg(unix)]
#[path = "../../team-agent-contract/tests/support/mod.rs"]
mod support;
use team_agent::cli::QuickStartArgs;
use team_agent::contract_runtime::{config::read_team_with_user_source, prompt::*};
use team_agent_contract::host::digest;

#[test]
fn all_four_layers_retain_whole_documents_and_exact_hash_ranges() {
    let sandbox = support::Sandbox::new();
    let workspace = sandbox.parent.join("project");
    let team_dir = workspace.join("roles");
    std::fs::create_dir_all(team_dir.join("agents")).unwrap();
    let user_path = sandbox.parent.join("user instructions 中文.md");
    let user = "USER-FIRST\r\nUser middle 'quote' 中文\r\nUSER-LAST\r\n\r\n";
    let project = "PROJECT-FIRST\nProject middle\nPROJECT-LAST\n";
    let role = "---\r\nprovider: kiro\r\nmodel: claude-sonnet-4.5\r\ndangerously_skip_permissions: true\r\n---\r\nROLE-FIRST\r\nRole middle\r\nROLE-LAST\r\n";
    let team = "---\nname: exact\ninstruction_files:\n  - 'extra 中文.md'\ninstructions: Inline team instruction\n---\nTEAM-FIRST\nTeam middle\nTEAM-LAST\n";
    let extra = "EXTRA-FIRST\nExtra middle\nEXTRA-LAST\n";
    std::fs::write(&user_path, user).unwrap();
    std::fs::write(workspace.join("AGENTS.md"), project).unwrap();
    std::fs::write(team_dir.join("agents/worker.md"), role).unwrap();
    std::fs::write(team_dir.join("TEAM.md"), team).unwrap();
    std::fs::write(team_dir.join("extra 中文.md"), extra).unwrap();
    let args = QuickStartArgs { workspace: workspace.clone(), agents_dir: team_dir,
        name: None, team_id: None, yes: false, json: true, detail: false, backend: None };
    let config = read_team_with_user_source(&args, || Source::read(&user_path).map(Some)).unwrap().unwrap();
    let prompt = &config.roles[0].prompt;
    assert_eq!(prompt.sources.iter().map(|s| s.layer).collect::<Vec<_>>(),
        vec![Layer::UserGlobal, Layer::Project, Layer::Role, Layer::TeamExplicit, Layer::TeamExplicit]);
    for (receipt, original) in prompt.sources.iter().zip([user, project, role, team, extra]) {
        assert_eq!(&prompt.text[receipt.start..receipt.end], original);
        assert_eq!(receipt.bytes, original.len());
        assert_eq!(receipt.sha256, digest(original.as_bytes()));
    }
    assert_eq!(prompt.sha256, digest(prompt.text.as_bytes()));
    assert_eq!(std::fs::read_to_string(&user_path).unwrap(), user);
    assert_eq!(std::fs::read_to_string(workspace.join("AGENTS.md")).unwrap(), project);
    assert!(!workspace.join(".kiro").exists());
    assert!(!workspace.join(".team").exists());
}

#[test]
fn no_contract_role_does_not_resolve_user_or_project_policy() {
    let sandbox = support::Sandbox::new();
    let team = sandbox.parent.join("roles");
    std::fs::create_dir_all(team.join("agents")).unwrap();
    std::fs::write(team.join("agents/worker.md"), "---\nprovider: pi\n---\nUnchanged legacy role\n").unwrap();
    let args = QuickStartArgs { workspace: sandbox.parent.clone(), agents_dir: team,
        name: None, team_id: None, yes: false, json: true, detail: false, backend: None };
    assert!(read_team_with_user_source(&args, || panic!("legacy routing must not load new user policy")).unwrap().is_none());
}

#[test]
fn missing_explicit_files_and_oversize_sources_fail_instead_of_being_dropped() {
    let sandbox = support::Sandbox::new();
    let large = sandbox.parent.join("large.md");
    std::fs::write(&large, vec![b'x'; MAX_PROMPT_BYTES + 1]).unwrap();
    assert!(Source::read(&large).is_err());
    let team = Source { path: sandbox.parent.join("TEAM.md"), contents: "Team instructions\n".into() };
    let meta = team_agent::model::yaml::loads("instructions:\n  - missing.md\n").unwrap();
    assert!(team_sources(&team, &meta).is_err());
    let role = Source { path: sandbox.parent.join("role.md"), contents: "x".repeat(MAX_PROMPT_BYTES) };
    assert!(assemble("worker", "role", Default::default(), None, None, &role, &[]).is_err());
}
