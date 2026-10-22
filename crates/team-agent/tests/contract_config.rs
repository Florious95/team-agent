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
    let model = if provider == "cursor" { "gpt-5.6-luna-xhigh" } else { "claude-sonnet-4.5" };
    std::fs::write(args.agents_dir.join("agents").join(format!("{name}.md")), format!(
        "---\nprovider: {provider}\nmodel: {model}\ndangerously_skip_permissions: true\n---\nFirst line\nSecond line 中文 'quotes'\n"
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
fn cursor_compiler_preserves_native_id_and_does_not_inherit_team_effort() {
    use team_agent::model::yaml::Value;
    let (_sandbox, args) = fixture();
    std::fs::write(args.agents_dir.join("TEAM.md"),
        "---\nname: demo\nprovider_effort: ultra\n---\nA bounded task.\n").unwrap();
    role(&args, "worker", "cursor");
    let before = std::fs::read(args.agents_dir.join("agents/worker.md")).unwrap();
    let spec = team_agent::compiler::compile_team(&args.agents_dir).unwrap();
    let worker = &spec.get("agents").and_then(Value::as_list).unwrap()[0];
    assert_eq!(worker.get("provider").and_then(Value::as_str), Some("cursor"));
    assert_eq!(worker.get("model").and_then(Value::as_str), Some("gpt-5.6-luna-xhigh"));
    assert!(worker.get("effort").is_none());
    let config = read_team(&args).unwrap().unwrap();
    assert_eq!(config.roles[0].provider.as_str(), "cursor");
    assert_eq!(config.roles[0].effort, None);
    assert_eq!(std::fs::read(args.agents_dir.join("agents/worker.md")).unwrap(), before);
    for path in [".team", ".cursor", ".kiro"] {
        assert!(!args.workspace.join(path).exists());
    }
}

#[test]
fn mixed_contract_providers_reject_retired_cursor_names_without_migration() {
    let (_sandbox, args) = fixture();
    for (name, provider) in [
        ("a-cursor", "cursor"), ("b-kiro", "kiro"), ("c-pi", "pi"),
    ] {
        role(&args, name, provider);
    }
    let config = read_team(&args).unwrap().unwrap();
    assert_eq!(config.roles.len(), 2);
    assert_eq!(config.members.len(), 3);
    for member in &config.members {
        let expected = if ["cursor", "kiro"].contains(&member.provider.as_str()) {
            RuntimeFamily::Contract
        } else {
            RuntimeFamily::Legacy
        };
        assert_eq!(member.runtime, expected, "{}", member.provider);
    }
    for provider in ["cursor_agent", "agent"] {
        role(&args, "retired-worker", provider);
        let path = args.agents_dir.join("agents/retired-worker.md");
        let before = std::fs::read(&path).unwrap();
        assert!(read_team(&args).is_err(), "mixed team accepted {provider}");
        assert!(team_agent::compiler::compile_team(&args.agents_dir).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), before);
        std::fs::remove_file(path).unwrap();
        let state = serde_json::json!({"provider":provider,"pane_id":"%0","runtime_family":"legacy"});
        assert!(!team_agent::contract_runtime::framework::is_contract(&state));
        assert!(team_agent::contract_runtime::framework::require_legacy_role(None, Some(provider)).is_err());
    }
    let current = serde_json::json!({"provider":"cursor","runtime_family":"contract"});
    assert!(team_agent::contract_runtime::framework::is_contract(&current));
    assert!(!args.workspace.join(".team").exists());
}

#[test]
fn retired_cursor_names_refuse_at_each_configuration_entry_without_writes() {
    use team_agent::model::{enums::Provider, spec::validate_spec, yaml};
    let (_sandbox, args) = fixture();
    for provider in ["cursor_agent", "agent"] {
        role(&args, "worker", provider);
        let path = args.agents_dir.join("agents/worker.md");
        let before = std::fs::read(&path).unwrap();
        assert!(read_team(&args).is_err(), "read_team delegated {provider}");
        assert!(team_agent::compiler::compile_role_agent(
            &path,
            &yaml::Value::Map(Vec::new()),
            args.workspace.to_str().unwrap(),
        ).is_err(), "compile_role accepted {provider}");
        assert!(team_agent::compiler::compile_team(&args.agents_dir).is_err());
        assert!(serde_json::from_value::<Provider>(serde_json::json!(provider)).is_err());
        let raw = include_str!("../src/model/testdata/team.spec.yaml")
            .replace("provider: codex", &format!("provider: {provider}"));
        let spec = yaml::loads(&raw).unwrap();
        let error = validate_spec(&spec, &args.workspace).unwrap_err().to_string();
        assert!(error.contains(provider), "spec rejected for a different reason: {error}");
        for explicit in [None, Some("pi"), Some(provider)] {
            assert!(team_agent::contract_runtime::framework::require_legacy_role(
                Some(&path), explicit,
            ).is_err(), "retired file {provider} bypassed with {explicit:?}");
        }
        assert!(team_agent::contract_runtime::framework::require_legacy_role(None, Some(provider)).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), before);
        std::fs::write(&path, format!("---\nprovider: {provider}\nmodel: native-id\nmissing closing delimiter")).unwrap();
        assert!(read_team(&args).is_err(), "malformed {provider} fell back to legacy");
        assert!(team_agent::contract_runtime::framework::require_legacy_role(Some(&path), None).is_err());
    }
    for path in [".team", ".cursor", ".kiro"] {
        assert!(!args.workspace.join(path).exists());
    }
}

#[test]
fn pi_and_claude_roles_keep_nested_and_body_provider_examples() {
    use team_agent::model::yaml::Value;
    let (_sandbox, args) = fixture();
    for (provider, model) in [("pi", "openai-codex/gpt-6-luna"), ("claude", "claude-sonnet-4.5")] {
        let path = args.agents_dir.join("agents/worker.md");
        let body = "Examples are ordinary role content.\nprovider: agent\n---\nprovider: cursor_agent\n";
        let raw = format!("---\nprovider: {provider}\nmodel: {model}\nexample:\n  provider: agent\nnotes: |\n  provider: cursor_agent\n---\n{body}");
        std::fs::write(&path, &raw).unwrap();
        let compiled = team_agent::compiler::compile_role_agent(
            &path, &Value::Map(Vec::new()), args.workspace.to_str().unwrap(),
        ).unwrap_or_else(|error| panic!("valid {provider} role refused: {error}"));
        assert_eq!(compiled.agent.get("provider").and_then(Value::as_str), Some(provider));
        assert!(read_team(&args).unwrap().is_none());
        assert!(team_agent::contract_runtime::framework::require_legacy_role(Some(&path), None).is_ok());
        assert!(team_agent::compiler::compile_team(&args.agents_dir).is_ok());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), raw);
    }
    assert!(!args.workspace.join(".team").exists());
}

#[test]
fn pi_and_claude_lifecycle_guards_ignore_retired_siblings() {
    use team_agent::contract_runtime::framework::require_legacy_lifecycle;
    use team_agent::state::{persist, selector};
    let (_sandbox, args) = fixture();
    for (id, provider, model) in [
        ("pi-worker", "pi", "openai-codex/gpt-6-luna"),
        ("claude-worker", "claude", "claude-sonnet-4.5"),
    ] {
        std::fs::write(args.agents_dir.join(format!("agents/{id}.md")),
            format!("---\nprovider: {provider}\nmodel: {model}\n---\nWorker.\n")).unwrap();
    }
    for retired in ["cursor_agent", "agent"] {
        role(&args, "retired", retired);
        persist::save_runtime_state(&args.workspace, &serde_json::json!({
            "team_key": "demo", "active_team_key": "demo", "team_dir": args.agents_dir,
            "agents": {
                "pi-worker": {"provider": "pi"},
                "claude-worker": {"provider": "claude"},
                "retired": {"provider": retired},
            },
        })).unwrap();
        let selected = selector::resolve_active_team_readonly(
            &args.workspace, None, selector::SelectorMode::RuntimeOnly,
        ).expect("the public guard must examine a real selected team");
        assert_eq!(selected.state["agents"]["retired"]["provider"], retired);
        let path = persist::runtime_state_path(&args.workspace);
        let before = std::fs::read(&path).unwrap();
        for id in ["pi-worker", "claude-worker"] {
            require_legacy_lifecycle(&args.workspace, None, Some(id), "start-agent")
                .unwrap_or_else(|error| panic!("{id} blocked by sibling {retired}: {error}"));
        }
        for target in [Some("retired"), None] {
            let error = require_legacy_lifecycle(&args.workspace, None, target, "restart")
                .unwrap_err().to_string();
            assert!(error.contains("retired provider"), "{error}");
        }
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }
}

#[test]
fn cursor_mutation_malformed_declarations_and_api_profiles_refuse_without_writes() {
    let (_sandbox, args) = fixture();
    role(&args, "worker", "cursor");
    let path = args.agents_dir.join("agents/worker.md");
    let before = std::fs::read(&path).unwrap();
    assert!(team_agent::contract_runtime::framework::require_legacy_role(Some(&path), None).is_err());
    assert!(team_agent::contract_runtime::framework::require_legacy_role(None, Some("cursor")).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), before);
    for header in [
        "provider: Cursor\nmodel: gpt-5.6-luna-xhigh",
        "provider: cursor\ndangerously_skip_permissions: true",
        "provider: cursor\nmodel: gpt-5.6-luna-xhigh\nauth_mode: compatible_api",
        "provider: cursor\nmodel: gpt-5.6-luna-xhigh\nprofile: inherited",
    ] {
        std::fs::write(&path, format!("---\n{header}\n---\nRole\n")).unwrap();
        assert!(read_team(&args).is_err(), "{header}");
    }
    std::fs::write(&path, "---\nprovider: cursor\nmodel: gpt-5.6-luna-xhigh\nmissing closing delimiter").unwrap();
    assert!(matches!(read_team(&args), Err(ConfigError::Invalid { field: "front matter", .. })));
    for path in [".team", ".cursor", ".kiro"] {
        assert!(!args.workspace.join(path).exists());
    }
}

#[test]
fn paused_contract_roles_do_not_probe_or_materialize_runtime() {
    let (_sandbox, args) = fixture();
    for provider in ["cursor", "kiro"] {
        role(&args, "worker", provider);
        let path = args.agents_dir.join("agents/worker.md");
        let text = std::fs::read_to_string(&path).unwrap().replace(
            "dangerously_skip_permissions: true", "dangerously_skip_permissions: true\npaused: true",
        );
        std::fs::write(&path, text).unwrap();
        let config = read_team(&args).unwrap().unwrap();
        assert!(config.roles[0].paused);
        team_agent::contract_runtime::backend::preflight(&config).unwrap();
        for path in [".team", ".cursor", ".kiro"] {
            assert!(!args.workspace.join(path).exists());
        }
    }
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
