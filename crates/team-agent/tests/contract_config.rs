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
fn pi_contract_keeps_all_instruction_layers_at_launch_and_preserves_legacy_pi() {
    use team_agent::contract_runtime::prompt::{Layer, Source};
    use team_agent::model::yaml::Value;
    let (_sandbox, args) = fixture();
    std::fs::write(args.workspace.join("AGENTS.md"), "project instructions").unwrap();
    let role = "---\nprovider: pi_tui\nmodel: openai-codex/gpt-6-luna\neffort: ultra\n---\nPi role instructions\n";
    std::fs::write(args.agents_dir.join("agents/worker.md"), role).unwrap();
    let config = read_team_with_user_source(&args, || Ok(Some(Source {
        path: args.workspace.join("global.md"), contents: "global instructions".into(),
    }))).unwrap().unwrap();
    let worker = &config.roles[0];
    assert_eq!(worker.provider.as_str(), "pi_tui");
    assert_eq!(worker.effort.as_deref(), Some("ultra"));
    assert!(!worker.bypass);
    assert_eq!(worker.prompt.sources.iter().map(|source| source.layer).collect::<Vec<_>>(),
        [Layer::UserGlobal, Layer::Project, Layer::Role, Layer::TeamExplicit]);
    for content in ["global instructions", "project instructions", role, "A bounded task."] {
        assert!(worker.prompt.text.contains(content));
    }
    let spec = team_agent::compiler::compile_team(&args.agents_dir).unwrap();
    let compiled = &spec.get("agents").and_then(Value::as_list).unwrap()[0];
    assert_eq!(compiled.get("provider").and_then(Value::as_str), Some("pi_tui"));
    assert!(!args.workspace.join(".team").exists());
    std::fs::write(args.agents_dir.join("agents/worker.md"), role.replace("pi_tui", "pi")).unwrap();
    assert!(read_team(&args).unwrap().is_none());
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
fn mixed_contract_providers_route_every_cursor_name_without_migrating_state() {
    let (_sandbox, args) = fixture();
    for (name, provider) in [
        ("a-cursor", "cursor"), ("b-kiro", "kiro"), ("c-pi", "pi"),
        ("d-cursor-agent", "cursor_agent"), ("e-agent", "agent"), ("f-cursor-agent", "cursor-agent"),
    ] {
        role(&args, name, provider);
    }
    let config = read_team(&args).unwrap().unwrap();
    assert_eq!(config.roles.len(), 5);
    assert_eq!(config.members.len(), 6);
    for member in &config.members {
        let expected = if member.id == "c-pi" { RuntimeFamily::Legacy } else { RuntimeFamily::Contract };
        assert_eq!(member.runtime, expected, "{}", member.id);
        if member.id != "b-kiro" && member.id != "c-pi" {
            assert_eq!(member.provider, "cursor", "{}", member.id);
        }
    }
    for provider in ["cursor_agent", "cursor-agent", "agent"] {
        // A declaration selects the leaf; a persisted legacy row is never reclassified.
        assert!(team_agent::contract_runtime::framework::require_legacy_role(None, Some(provider)).is_err());
        let state = serde_json::json!({"provider":provider,"pane_id":"%0","runtime_family":"legacy"});
        assert!(!team_agent::contract_runtime::framework::is_contract(&state));
    }
    let current = serde_json::json!({"provider":"cursor","runtime_family":"contract"});
    assert!(team_agent::contract_runtime::framework::is_contract(&current));
    assert!(!args.workspace.join(".team").exists());
}

#[test]
fn every_cursor_name_alone_selects_the_contract_leaf() {
    let (_sandbox, args) = fixture();
    for provider in ["cursor_agent", "cursor-agent", "agent"] {
        role(&args, "worker", provider);
        let config = read_team(&args).unwrap().unwrap();
        assert_eq!(config.roles.len(), 1, "{provider}");
        assert_eq!(config.roles[0].provider.as_str(), "cursor", "{provider}");
        assert_eq!(config.members[0].runtime, RuntimeFamily::Contract, "{provider}");
    }
    assert!(!args.workspace.join(".team").exists());
}

#[test]
fn pre_upgrade_cursor_agent_row_admits_only_teardown_and_never_blocks_siblings() {
    use team_agent::contract_runtime::framework::require_legacy_lifecycle;
    use team_agent::state::{persist, selector};
    let (_sandbox, args) = fixture();
    std::fs::write(args.agents_dir.join("agents/pi-worker.md"),
        "---\nprovider: pi\nmodel: openai-codex/gpt-6-luna\n---\nWorker.\n").unwrap();
    for retired in ["cursor_agent", "agent"] {
        // The role file now declares the contract leaf; the recorded row is legacy.
        role(&args, "retired", retired);
        persist::save_runtime_state(&args.workspace, &serde_json::json!({
            "team_key": "demo", "active_team_key": "demo", "team_dir": args.agents_dir,
            "agents": {"pi-worker": {"provider": "pi"}, "retired": {"provider": retired}},
        })).unwrap();
        selector::resolve_active_team_readonly(
            &args.workspace, None, selector::SelectorMode::RuntimeOnly,
        ).expect("the guard must examine a real selected team");
        for operation in ["stop-agent", "remove-agent"] {
            require_legacy_lifecycle(&args.workspace, None, Some("retired"), operation)
                .unwrap_or_else(|error| panic!("{retired} {operation}: {error}"));
        }
        for operation in ["start-agent", "reset-agent", "fork-agent", "clone-agent"] {
            let error = require_legacy_lifecycle(&args.workspace, None, Some("retired"), operation)
                .unwrap_err().to_string();
            assert!(error.contains("legacy cursor_agent seat"), "{operation}: {error}");
        }
        require_legacy_lifecycle(&args.workspace, None, Some("pi-worker"), "start-agent")
            .unwrap_or_else(|error| panic!("sibling blocked by {retired}: {error}"));
    }
}

#[test]
fn pre_upgrade_codex_and_grok_rows_admit_only_teardown_and_never_block_siblings() {
    use team_agent::contract_runtime::framework::{is_contract, require_legacy_lifecycle};
    use team_agent::state::{persist, selector};
    let (_sandbox, args) = fixture();
    std::fs::write(args.agents_dir.join("agents/copilot-worker.md"),
        "---\nprovider: copilot\nmodel: gpt-6-luna\n---\nWorker.\n").unwrap();
    for (retired, public) in [
        ("codex", "codex"), ("codex_cli", "codex"), ("codex-cli", "codex"), ("Codex", "codex"),
        ("grok", "grok"), ("Grok", "grok"),
    ] {
        // The role file now declares the contract leaf; the recorded row is legacy.
        role(&args, "retired", public);
        persist::save_runtime_state(&args.workspace, &serde_json::json!({
            "team_key": "demo", "active_team_key": "demo", "team_dir": args.agents_dir,
            "agents": {"copilot-worker": {"provider": "copilot"}, "retired": {"provider": retired}},
        })).unwrap();
        selector::resolve_active_team_readonly(
            &args.workspace, None, selector::SelectorMode::RuntimeOnly,
        ).expect("the guard must examine a real selected team");
        assert!(!is_contract(&serde_json::json!({"provider": retired})), "{retired}");
        for operation in ["stop-agent", "remove-agent"] {
            require_legacy_lifecycle(&args.workspace, None, Some("retired"), operation)
                .unwrap_or_else(|error| panic!("{retired} {operation}: {error}"));
        }
        let expected = format!(
            "legacy {public} seat has no runtime in this release; stop or remove it, then add it again (provider: {public})"
        );
        for operation in ["start-agent", "reset-agent", "fork-agent", "clone-agent"] {
            let error = require_legacy_lifecycle(&args.workspace, None, Some("retired"), operation)
                .unwrap_err().to_string();
            assert_eq!(error, expected, "{retired} {operation}");
        }
        require_legacy_lifecycle(&args.workspace, None, Some("copilot-worker"), "start-agent")
            .unwrap_or_else(|error| panic!("sibling blocked by {retired}: {error}"));
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

fn codex_role(args: &QuickStartArgs, name: &str, extra: &str) {
    std::fs::write(args.agents_dir.join("agents").join(format!("{name}.md")), format!(
        "---\nprovider: codex\nmodel: gpt-6-luna\ndangerously_skip_permissions: true\n{extra}---\nCodex role body\n"
    )).unwrap();
}

#[test]
fn every_codex_and_grok_name_selects_the_contract_leaf_without_opt_in() {
    use team_agent::model::yaml::Value;
    let (_sandbox, args) = fixture();
    // `codex`, its CLI names and `grok` declare their leaves on their own;
    // `contract_runtime: false` cannot keep them on an excised legacy worker engine.
    codex_role(&args, "bare-worker", "");
    codex_role(&args, "opted-out", "contract_runtime: false\n");
    codex_role(&args, "native-worker", "contract_runtime: true\n");
    for (name, provider) in [("cli-worker", "codex_cli"), ("dash-worker", "codex-cli"), ("grok-worker", "grok")] {
        std::fs::write(args.agents_dir.join("agents").join(format!("{name}.md")), format!(
            "---\nprovider: {provider}\nmodel: gpt-6-luna\n---\nRole body\n"
        )).unwrap();
    }
    let expected = [
        ("bare-worker", "codex_tui"), ("cli-worker", "codex_tui"), ("dash-worker", "codex_tui"),
        ("grok-worker", "grok_build"), ("native-worker", "codex_tui"), ("opted-out", "codex_tui"),
    ];
    let config = read_team(&args).unwrap().unwrap();
    let mut roles: Vec<_> = config.roles.iter().map(|role| (role.id.as_str(), role.provider.as_str())).collect();
    roles.sort();
    assert_eq!(roles, expected);
    let mut routes: Vec<_> = config.members.iter().map(|member| (member.id.as_str(), member.provider.as_str(), member.runtime)).collect();
    routes.sort_by_key(|route| route.0);
    assert_eq!(routes, expected.map(|(id, provider)| (id, provider, RuntimeFamily::Contract)));
    // The compiled spec carries the leaf's distinct canonical id.
    let spec = team_agent::compiler::compile_team(&args.agents_dir).unwrap();
    let mut providers: Vec<_> = spec.get("agents").and_then(Value::as_list).unwrap().iter()
        .map(|agent| (agent.get("id").and_then(Value::as_str).unwrap(), agent.get("provider").and_then(Value::as_str).unwrap()))
        .collect();
    providers.sort();
    assert_eq!(providers, expected);
    // Mutable lifecycle refuses every Codex/Grok declaration.
    for (name, _) in expected {
        let path = args.agents_dir.join(format!("agents/{name}.md"));
        assert!(team_agent::contract_runtime::framework::require_legacy_role(Some(&path), None).is_err(), "{name}");
    }
    for provider in ["codex", "codex_cli", "codex-cli", "grok", "codex_tui", "grok_build"] {
        assert!(team_agent::contract_runtime::framework::require_legacy_role(None, Some(provider)).is_err(), "{provider}");
    }
    // A persisted pre-upgrade row keeps its key and is never reclassified.
    for provider in ["codex", "codex_cli", "codex-cli", "grok"] {
        assert!(!team_agent::contract_runtime::framework::is_contract(&serde_json::json!({"provider":provider})), "{provider}");
    }
    for provider in ["codex_tui", "grok_build"] {
        assert!(team_agent::contract_runtime::framework::is_contract(&serde_json::json!({"provider":provider})), "{provider}");
    }
    assert!(!args.workspace.join(".team").exists());
}

#[test]
fn malformed_or_misdirected_codex_opt_in_fails_closed() {
    let (_sandbox, args) = fixture();
    for (provider, extra, field) in [
        ("codex", "contract_runtime: \"yes\"\n", "contract_runtime"),
        ("codex", "contract_runtime: 1\n", "contract_runtime"),
        ("pi", "contract_runtime: true\n", "provider"),
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
