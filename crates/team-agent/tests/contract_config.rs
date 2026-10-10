//! Contract routing is a read-only opt-in; it must not replace legacy teams.
#![cfg(unix)]
#[path = "../../team-agent-contract/tests/support/mod.rs"]
mod support;
use std::path::Path;
use team_agent::cli::QuickStartArgs;
use team_agent::contract_runtime::config::{
    read_team_with_user_source, ConfigError, RuntimeFamily, TeamConfig,
};

fn read_team(args: &QuickStartArgs) -> Result<Option<TeamConfig>, ConfigError> {
    read_team_with_user_source(args, || Ok(None))
}
fn args(workspace: &Path) -> QuickStartArgs {
    QuickStartArgs {
        workspace: workspace.into(),
        agents_dir: workspace.join("roles"),
        name: None,
        team_id: None,
        yes: false,
        json: true,
        detail: false,
        backend: None,
    }
}
fn fixture() -> (support::Sandbox, QuickStartArgs) {
    let sandbox = support::Sandbox::new();
    let workspace = sandbox.parent.join("workspace");
    let args = args(&workspace);
    std::fs::create_dir_all(args.agents_dir.join("agents")).unwrap();
    std::fs::write(
        args.agents_dir.join("TEAM.md"),
        "---\nname: demo\n---\nA bounded task.\n",
    )
    .unwrap();
    (sandbox, args)
}
fn role(args: &QuickStartArgs, name: &str, provider: &str) {
    std::fs::write(args.agents_dir.join("agents").join(format!("{name}.md")), format!(
        "---\nprovider: {provider}\nmodel: claude-sonnet-4.5\ndangerously_skip_permissions: true\n---\nFirst line\nSecond line 中文 'quotes'\n"
    )).unwrap();
}

#[test]
fn public_compiler_accepts_kiro_without_legacy_effort_inheritance() {
    use team_agent::model::yaml::Value;
    let (_sandbox, args) = fixture();
    std::fs::write(
        args.agents_dir.join("TEAM.md"),
        "---\nname: demo\nprovider_effort: ultra\n---\nA bounded task.\n",
    )
    .unwrap();
    role(&args, "worker", "kiro");
    let spec = team_agent::compiler::compile_team(&args.agents_dir).unwrap();
    let worker = &spec.get("agents").and_then(Value::as_list).unwrap()[0];
    assert_eq!(worker.get("provider").and_then(Value::as_str), Some("kiro"));
    assert!(worker.get("effort").is_none());
    assert!(!args.workspace.join(".team").exists());
}

#[test]
fn mutable_kiro_role_is_refused_before_legacy_fallback_or_role_writes() {
    let (_sandbox, args) = fixture();
    role(&args, "worker", "kiro");
    let path = args.agents_dir.join("agents/worker.md");
    let before = std::fs::read(&path).unwrap();
    assert!(
        team_agent::contract_runtime::framework::require_legacy_role(Some(&path), None).is_err()
    );
    assert!(
        team_agent::contract_runtime::framework::require_legacy_role(None, Some("kiro")).is_err()
    );
    assert!(team_agent::contract_runtime::framework::require_legacy_role(None, Some("pi")).is_ok());
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert!(!args.workspace.join(".team").exists());
}

#[test]
fn kiro_selection_preserves_role_bytes_and_has_no_runtime_effects() {
    let (_sandbox, args) = fixture();
    role(&args, "worker", "kiro");
    let config = read_team(&args).unwrap().unwrap();
    assert_eq!(config.selector, "demo");
    assert_eq!(config.roles[0].id.as_str(), "worker");
    assert_eq!(config.roles[0].model, "claude-sonnet-4.5");
    assert!(config.roles[0].bypass);
    assert_eq!(config.roles[0].effort, None);
    assert_eq!(
        config.roles[0].body,
        "First line\nSecond line 中文 'quotes'\n"
    );
    assert!(!args.workspace.join(".team").exists());
    assert!(!args.workspace.join(".kiro").exists());
    let again = read_team(&args).unwrap().unwrap();
    assert_eq!(again.scope, config.scope);
    let mut other = args.clone();
    other.team_id = Some("other".into());
    assert_ne!(read_team(&other).unwrap().unwrap().scope, config.scope);
}

#[test]
fn legacy_and_leader_only_inputs_delegate_without_new_requirements() {
    let (_sandbox, args) = fixture();
    assert!(read_team(&args).unwrap().is_none());
    role(&args, "worker", "pi");
    assert!(read_team(&args).unwrap().is_none());
    std::fs::write(args.agents_dir.join("agents/worker.md"), "---\nprovider: pi\n---\n---\nprovider: kiro\n---\nExample in a role body, not a provider declaration.\n").unwrap();
    assert!(read_team(&args).unwrap().is_none());
    std::fs::remove_file(args.agents_dir.join("agents/worker.md")).unwrap();
    std::fs::remove_dir(args.agents_dir.join("agents")).unwrap();
    assert!(read_team(&args).unwrap().is_none());
    assert!(!args.workspace.join(".team").exists());
}

#[test]
fn mixed_team_routes_each_role_and_malformed_kiro_still_refuses() {
    let (_sandbox, args) = fixture();
    role(&args, "new-worker", "kiro");
    role(&args, "old-worker", "pi");
    let config = read_team(&args).unwrap().unwrap();
    assert_eq!(config.roles.len(), 1);
    assert_eq!(config.members.len(), 2);
    assert_eq!(config.members[0].runtime, RuntimeFamily::Contract);
    assert_eq!(config.members[1].runtime, RuntimeFamily::Legacy);
    assert_eq!(config.members[1].provider, "pi");
    std::fs::remove_file(args.agents_dir.join("agents/old-worker.md")).unwrap();
    std::fs::write(
        args.agents_dir.join("agents/new-worker.md"),
        "---\nprovider: kiro\nmodel: claude-sonnet-4.5\nmissing closing delimiter",
    )
    .unwrap();
    assert!(matches!(
        read_team(&args),
        Err(ConfigError::Invalid {
            field: "front matter",
            ..
        })
    ));
    assert!(!args.workspace.join(".team").exists());
    assert!(!args.workspace.join(".kiro").exists());
}

#[test]
fn safe_bypass_default_and_model_identity_api_profile_admission() {
    let (_sandbox, args) = fixture();
    for header in [
        "provider: kiro\ndangerously_skip_permissions: true", // no native model ID
        "provider: kiro\nmodel: claude-sonnet-4.5\ndangerously_skip_permissions: yes",
        "provider: kiro\nmodel: claude-sonnet-4.5\ndangerously_skip_permissions: true\nprofile: inherited",
        "provider: kiro\nmodel: claude-sonnet-4.5\ndangerously_skip_permissions: true\nauth_mode: compatible_api",
    ] {
        std::fs::write(args.agents_dir.join("agents/worker.md"), format!("---\n{header}\n---\nRole body\n")).unwrap();
        assert!(read_team(&args).is_err());
    }
    std::fs::write(
        args.agents_dir.join("agents/worker.md"),
        "---\nprovider: kiro\nmodel: claude-sonnet-4.5\n---\nSafe default role\n",
    )
    .unwrap();
    assert!(!read_team(&args).unwrap().unwrap().roles[0].bypass);
    role(&args, "worker", "kiro");
    role(&args, "another", "kiro");
    std::fs::write(args.agents_dir.join("agents/another.md"), "---\nprovider: kiro\nagent_id: worker\nmodel: claude-sonnet-4.5\ndangerously_skip_permissions: true\n---\nRole\n").unwrap();
    assert!(matches!(
        read_team(&args),
        Err(ConfigError::Invalid {
            field: "duplicate agent id",
            ..
        })
    ));
}

#[test]
fn worker_named_leader_is_not_a_leader_launcher_and_non_tmux_refuses() {
    let (_sandbox, mut args) = fixture();
    role(&args, "worker", "kiro");
    args.backend = Some("conpty".into());
    assert!(matches!(read_team(&args), Err(ConfigError::Unsupported(_))));
    args.backend = None;
    // Leader launch is its own public entry, not a worker with a reserved name.
    role(&args, "leader", "kiro");
    assert!(matches!(
        read_team(&args),
        Err(ConfigError::Invalid {
            field: "reserved worker id",
            ..
        })
    ));
    assert!(!args.workspace.join(".team").exists());
    assert!(!args.workspace.join(".kiro").exists());
}
