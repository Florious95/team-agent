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
fn mixed_contract_providers_do_not_migrate_legacy_cursor_names_or_state() {
    let (_sandbox, args) = fixture();
    for (name, provider) in [
        ("a-cursor", "cursor"), ("b-kiro", "kiro"), ("c-pi", "pi"),
        ("d-legacy-cursor", "cursor_agent"), ("e-legacy-agent", "agent"),
    ] {
        role(&args, name, provider);
    }
    let config = read_team(&args).unwrap().unwrap();
    assert_eq!(config.roles.len(), 2);
    assert_eq!(config.members.len(), 5);
    for member in &config.members {
        let expected = if ["cursor", "kiro"].contains(&member.provider.as_str()) {
            RuntimeFamily::Contract
        } else {
            RuntimeFamily::Legacy
        };
        assert_eq!(member.runtime, expected, "{}", member.provider);
    }
    for provider in ["cursor_agent", "agent"] {
        let state = serde_json::json!({"provider":provider,"pane_id":"%0","runtime_family":"legacy"});
        assert!(!team_agent::contract_runtime::framework::is_contract(&state));
        assert!(team_agent::contract_runtime::framework::require_legacy_role(None, Some(provider)).is_ok());
    }
    let current = serde_json::json!({"provider":"cursor","runtime_family":"contract"});
    assert!(team_agent::contract_runtime::framework::is_contract(&current));
    assert!(!args.workspace.join(".team").exists());
}

#[test]
fn legacy_cursor_only_config_stays_on_legacy_track() {
    let (_sandbox, args) = fixture();
    for provider in ["cursor_agent", "agent"] {
        role(&args, "worker", provider);
        assert!(read_team(&args).unwrap().is_none());
    }
    assert!(!args.workspace.join(".team").exists());
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

fn codex_role(args: &QuickStartArgs, name: &str, extra: &str) {
    std::fs::write(args.agents_dir.join("agents").join(format!("{name}.md")), format!(
        "---\nprovider: codex\nmodel: gpt-6-luna\ndangerously_skip_permissions: true\n{extra}---\nCodex role body\n"
    )).unwrap();
}

#[test]
fn bare_codex_stays_legacy_and_only_an_exact_opt_in_selects_the_contract_leaf() {
    use team_agent::model::yaml::Value;
    let (_sandbox, args) = fixture();
    codex_role(&args, "legacy-worker", "");
    codex_role(&args, "opted-out", "contract_runtime: false\n");
    // A team of bare/opted-out Codex roles never enters the contract path.
    assert!(read_team(&args).unwrap().is_none());
    codex_role(&args, "native-worker", "contract_runtime: true\n");
    let config = read_team(&args).unwrap().unwrap();
    assert_eq!(config.roles.len(), 1);
    assert_eq!(config.roles[0].id.as_str(), "native-worker");
    assert_eq!(config.roles[0].provider.as_str(), "codex_tui");
    let routes: Vec<_> = config.members.iter().map(|member| (member.id.as_str(), member.provider.as_str(), member.runtime)).collect();
    assert_eq!(routes, [
        ("legacy-worker", "codex", RuntimeFamily::Legacy),
        ("native-worker", "codex_tui", RuntimeFamily::Contract),
        ("opted-out", "codex", RuntimeFamily::Legacy),
    ]);
    // The compiled spec carries the leaf's distinct canonical id; legacy rows keep `codex`.
    let spec = team_agent::compiler::compile_team(&args.agents_dir).unwrap();
    let providers: Vec<_> = spec.get("agents").and_then(Value::as_list).unwrap().iter()
        .map(|agent| (agent.get("id").and_then(Value::as_str).unwrap(), agent.get("provider").and_then(Value::as_str).unwrap()))
        .collect();
    assert_eq!(providers, [("legacy-worker", "codex"), ("native-worker", "codex_tui"), ("opted-out", "codex")]);
    // Mutable lifecycle refuses only the contract declaration.
    let native = args.agents_dir.join("agents/native-worker.md");
    let legacy = args.agents_dir.join("agents/legacy-worker.md");
    assert!(team_agent::contract_runtime::framework::require_legacy_role(Some(&native), None).is_err());
    assert!(team_agent::contract_runtime::framework::require_legacy_role(Some(&legacy), None).is_ok());
    assert!(team_agent::contract_runtime::framework::require_legacy_role(None, Some("codex")).is_ok());
    assert!(team_agent::contract_runtime::framework::require_legacy_role(None, Some("codex_tui")).is_err());
    assert!(!args.workspace.join(".team").exists());
}

#[test]
fn malformed_or_misdirected_codex_opt_in_fails_closed() {
    let (_sandbox, args) = fixture();
    for (provider, extra, field) in [
        ("codex", "contract_runtime: \"yes\"\n", "contract_runtime"),
        ("codex", "contract_runtime: 1\n", "contract_runtime"),
        ("claude", "contract_runtime: true\n", "provider"),
        ("Codex", "contract_runtime: true\n", "provider"),
    ] {
        std::fs::write(args.agents_dir.join("agents/worker.md"), format!(
            "---\nprovider: {provider}\nmodel: gpt-6-luna\n{extra}---\nBody\n"
        )).unwrap();
        assert!(matches!(read_team(&args), Err(ConfigError::Invalid { field: actual, .. }) if actual == field), "{provider} {extra}");
        assert!(team_agent::compiler::compile_team(&args.agents_dir).is_err(), "{provider} {extra}");
    }
    // An unparseable header that opted in cannot fall back to a default worker.
    std::fs::write(args.agents_dir.join("agents/worker.md"), "---\nprovider: codex\ncontract_runtime: true\nmissing closing delimiter").unwrap();
    assert!(matches!(read_team(&args), Err(ConfigError::Invalid { field: "front matter", .. })));
    assert!(!args.workspace.join(".team").exists());
}

#[test]
fn legacy_role_named_leader_cannot_shadow_the_framework_route_of_a_contract_team() {
    let (_sandbox, args) = fixture();
    // A legacy-only team keeps its existing behavior: no contract selection.
    role(&args, "leader", "pi");
    assert!(read_team(&args).unwrap().is_none());
    // With a contract seat, `leader` is the framework's own forwarding route.
    // Refuse before compile/spawn instead of a late, opaque route Conflict.
    codex_role(&args, "onboarding-worker", "contract_runtime: true\n");
    assert!(matches!(
        read_team(&args),
        Err(ConfigError::Invalid { field: "reserved worker id", .. })
    ));
    assert!(!args.workspace.join(".team").exists());
}

fn pi_role(args: &QuickStartArgs, name: &str, extra: &str) {
    std::fs::write(args.agents_dir.join("agents").join(format!("{name}.md")), format!(
        "---\nprovider: pi\nmodel: openai-codex/gpt-6-luna\n{extra}---\nPi role body\n"
    )).unwrap();
}

#[test]
fn bare_pi_stays_legacy_and_only_an_exact_opt_in_selects_the_contract_leaf() {
    use team_agent::model::yaml::Value;
    let (_sandbox, args) = fixture();
    pi_role(&args, "legacy-worker", "");
    pi_role(&args, "opted-out", "contract_runtime: false\n");
    // Pi is the default provider and the leader: bare roles never move.
    assert!(read_team(&args).unwrap().is_none());
    pi_role(&args, "native-worker", "contract_runtime: true\neffort: high\n");
    // An omitted provider is the default `pi` and opts in the same way.
    std::fs::write(args.agents_dir.join("agents/default-worker.md"),
        "---\nmodel: openai-codex/gpt-6-luna\ncontract_runtime: true\n---\nDefault body\n").unwrap();
    let config = read_team(&args).unwrap().unwrap();
    let mut roles: Vec<_> = config.roles.iter().map(|role| (role.id.as_str(), role.provider.as_str())).collect();
    roles.sort();
    assert_eq!(roles, [("default-worker", "pi_tui"), ("native-worker", "pi_tui")]);
    let mut routes: Vec<_> = config.members.iter().map(|member| (member.id.as_str(), member.provider.as_str(), member.runtime)).collect();
    routes.sort_by_key(|route| route.0);
    assert_eq!(routes, [
        ("default-worker", "pi_tui", RuntimeFamily::Contract),
        ("legacy-worker", "pi", RuntimeFamily::Legacy),
        ("native-worker", "pi_tui", RuntimeFamily::Contract),
        ("opted-out", "pi", RuntimeFamily::Legacy),
    ]);
    // The compiled spec carries the leaf's distinct canonical id; legacy rows keep `pi`.
    let spec = team_agent::compiler::compile_team(&args.agents_dir).unwrap();
    let agents = spec.get("agents").and_then(Value::as_list).unwrap();
    let mut providers: Vec<_> = agents.iter()
        .map(|agent| (agent.get("id").and_then(Value::as_str).unwrap(), agent.get("provider").and_then(Value::as_str).unwrap()))
        .collect();
    providers.sort();
    assert_eq!(providers, [("default-worker", "pi_tui"), ("legacy-worker", "pi"), ("native-worker", "pi_tui"), ("opted-out", "pi")]);
    let native = agents.iter().find(|agent| agent.get("id").and_then(Value::as_str) == Some("native-worker")).unwrap();
    assert_eq!(native.get("effort").and_then(Value::as_str), Some("high"));
    let native = args.agents_dir.join("agents/native-worker.md");
    let legacy = args.agents_dir.join("agents/legacy-worker.md");
    assert!(team_agent::contract_runtime::framework::require_legacy_role(Some(&native), None).is_err());
    assert!(team_agent::contract_runtime::framework::require_legacy_role(Some(&legacy), None).is_ok());
    assert!(team_agent::contract_runtime::framework::require_legacy_role(None, Some("pi")).is_ok());
    assert!(team_agent::contract_runtime::framework::require_legacy_role(None, Some("pi_tui")).is_err());
    assert!(!args.workspace.join(".team").exists());
}

#[test]
fn pi_opt_in_refuses_effort_levels_pi_could_silently_clamp() {
    let (_sandbox, args) = fixture();
    for effort in ["xhigh", "max", "ultra"] {
        pi_role(&args, "worker", &format!("contract_runtime: true\neffort: {effort}\n"));
        let error = team_agent::compiler::compile_team(&args.agents_dir).unwrap_err().to_string();
        assert!(error.contains("Pi"), "{effort}: {error}");
    }
    pi_role(&args, "worker", "contract_runtime: true\neffort: medium\n");
    assert!(team_agent::compiler::compile_team(&args.agents_dir).is_ok());
    assert!(!args.workspace.join(".team").exists());
}
