//! 0.4.x provider effort MVP — focused unit/integration tests covering the
//! 10-step plan in .team/artifacts/provider-effort-mvp-plan.md.

use team_agent::model::enums::{Provider, ProviderEffort};

/// Step 1: enum parse/as_str/is_supported_by/is_claude_only round-trip.
#[test]
fn provider_effort_enum_parse_round_trip() {
    for s in ["low", "medium", "high", "xhigh", "max"] {
        let parsed = ProviderEffort::parse(s).unwrap_or_else(|| panic!("must parse {s}"));
        assert_eq!(parsed.as_str(), s, "as_str round-trip for {s}");
    }
    assert!(ProviderEffort::parse("turbo").is_none());
    assert!(ProviderEffort::parse("").is_none());
    assert!(ProviderEffort::parse(" high ").is_some(), "trim whitespace");
}

#[test]
fn provider_effort_max_is_claude_only() {
    assert!(ProviderEffort::Max.is_claude_only());
    for e in [
        ProviderEffort::Low,
        ProviderEffort::Medium,
        ProviderEffort::High,
        ProviderEffort::XHigh,
    ] {
        assert!(!e.is_claude_only(), "{} must not be claude-only", e.as_str());
    }
}

#[test]
fn provider_effort_support_matrix() {
    // Claude/ClaudeCode: all 5 levels.
    for e in [
        ProviderEffort::Low,
        ProviderEffort::Medium,
        ProviderEffort::High,
        ProviderEffort::XHigh,
        ProviderEffort::Max,
    ] {
        assert!(e.is_supported_by(Provider::Claude), "Claude must support {}", e.as_str());
        assert!(
            e.is_supported_by(Provider::ClaudeCode),
            "ClaudeCode must support {}",
            e.as_str()
        );
    }
    // Codex: 4 levels, NOT max.
    for e in [
        ProviderEffort::Low,
        ProviderEffort::Medium,
        ProviderEffort::High,
        ProviderEffort::XHigh,
    ] {
        assert!(e.is_supported_by(Provider::Codex), "Codex must support {}", e.as_str());
    }
    assert!(
        !ProviderEffort::Max.is_supported_by(Provider::Codex),
        "Codex must NOT support max"
    );
    // Copilot/Gemini/Fake: none.
    for provider in [Provider::Copilot, Provider::GeminiCli, Provider::Fake] {
        for e in [
            ProviderEffort::Low,
            ProviderEffort::High,
            ProviderEffort::Max,
        ] {
            assert!(
                !e.is_supported_by(provider),
                "{provider:?} must NOT support effort"
            );
        }
    }
}

/// Steps 5 + 6: adapter argv contains adjacent effort flag.
mod adapter_argv {
    use team_agent::provider::{get_adapter, McpConfig, ProviderCommandContext};
    use team_agent::model::enums::{AuthMode, Provider, ProviderEffort};

    fn ctx<'a>(effort: Option<ProviderEffort>) -> ProviderCommandContext<'a> {
        ProviderCommandContext {
            auth_mode: AuthMode::Subscription,
            mcp_config: None,
            system_prompt: Some("you are X"),
            model: None,
            tools: &[],
            profile_launch: None,
            agent_id_hint: Some("dev"),
            effort,
        }
    }

    #[test]
    fn claude_fresh_argv_contains_adjacent_effort_high() {
        let adapter = get_adapter(Provider::Claude);
        let plan = adapter
            .build_command_plan(ctx(Some(ProviderEffort::High)))
            .expect("claude plan");
        let argv = &plan.argv;
        let pos = argv
            .iter()
            .position(|a| a == "--effort")
            .unwrap_or_else(|| panic!("--effort missing in {argv:?}"));
        assert_eq!(argv[pos + 1], "high", "--effort must be followed by 'high'");
    }

    #[test]
    fn claude_fresh_argv_omits_effort_when_none() {
        let adapter = get_adapter(Provider::Claude);
        let plan = adapter
            .build_command_plan(ctx(None))
            .expect("claude plan");
        assert!(
            !plan.argv.iter().any(|a| a == "--effort"),
            "no --effort when effort is None; got {:?}",
            plan.argv
        );
    }

    #[test]
    fn codex_fresh_argv_contains_adjacent_model_reasoning_effort_xhigh() {
        let adapter = get_adapter(Provider::Codex);
        let plan = adapter
            .build_command_plan(ctx(Some(ProviderEffort::XHigh)))
            .expect("codex plan");
        let argv = &plan.argv;
        let kv = format!("model_reasoning_effort={}", "xhigh");
        let pos = argv
            .iter()
            .position(|a| a == &kv)
            .unwrap_or_else(|| panic!("{kv} missing in {argv:?}"));
        assert_eq!(argv[pos - 1], "-c", "-c must precede {kv}");
    }

    #[test]
    fn codex_fresh_argv_omits_effort_when_none() {
        let adapter = get_adapter(Provider::Codex);
        let plan = adapter
            .build_command_plan(ctx(None))
            .expect("codex plan");
        assert!(
            !plan.argv.iter().any(|a| a.starts_with("model_reasoning_effort=")),
            "no model_reasoning_effort=… when None; got {:?}",
            plan.argv
        );
    }

    /// Effort=max on Claude survives an MCP-config code path.
    #[test]
    fn claude_effort_with_mcp_config_still_includes_effort_flag() {
        let cfg = McpConfig { raw: serde_json::json!({}) };
        let mut c = ctx(Some(ProviderEffort::Max));
        c.mcp_config = Some(&cfg);
        let adapter = get_adapter(Provider::Claude);
        let plan = adapter.build_command_plan(c).expect("claude plan");
        let pos = plan
            .argv
            .iter()
            .position(|a| a == "--effort")
            .unwrap_or_else(|| panic!("--effort missing in {:?}", plan.argv));
        assert_eq!(plan.argv[pos + 1], "max");
    }
}

/// Step 3 spec validation tests.
mod spec_validation {
    use team_agent::model::{spec::validate_spec, yaml};

    fn validate_spec_yaml_str(yaml_s: &str) -> Result<(), Vec<String>> {
        let loaded = yaml::loads(yaml_s).map_err(|e| vec![format!("yaml parse: {e}")])?;
        match validate_spec(&loaded, std::path::Path::new("/tmp")) {
            Ok(()) => Ok(()),
            Err(team_agent::model::ModelError::Validation(msg)) => {
                let lines: Vec<String> = msg.lines().map(|s| s.to_string()).collect();
                Err(lines)
            }
            Err(other) => Err(vec![format!("{other}")]),
        }
    }

    fn base_team(extra: &str) -> String {
        format!(
            r#"version: 1
team:
  name: t
  mode: supervisor_worker
  objective: o
  workspace: /tmp/ws{extra}
leader:
  id: leader
  role: leader
  provider: codex
  model: null
  tools: [fs_read]
  context_policy:
    keep_user_thread: true
    receive_worker_outputs: business_messages_and_short_summaries
    max_worker_result_tokens: 2000
agents:
  - id: dev
    role: dev
    provider: claude
    model: null
    working_directory: /tmp/ws
    system_prompt:
      inline: hi
      file: null
    tools: [fs_read]
    permission_mode: restricted
    preferred_for: [dev]
    avoid_for: []
    output_contract:
      format: result_envelope_v1
      required_fields: [task_id, status, summary, artifacts]
routing:
  default_assignee: dev
  rules: []
communication:
  ack: required
  retries:
    max: 3
    backoff_seconds: 5
runtime:
  fast: false
context: {{}}
tasks: []
"#
        )
    }

    #[test]
    fn provider_effort_unknown_literal_rejected() {
        let yaml = base_team("\n  provider_effort: turbo");
        let result = validate_spec_yaml_str(&yaml);
        let errors = result.expect_err("must reject unknown effort");
        assert!(
            errors.iter().any(|e| e.contains("provider_effort") && e.contains("unknown effort")),
            "errors should mention unknown provider_effort; got {errors:?}"
        );
    }

    #[test]
    fn agent_effort_unknown_literal_rejected() {
        let mut yaml = base_team("");
        yaml = yaml.replace(
            "preferred_for: [dev]",
            "preferred_for: [dev]\n    effort: turbo",
        );
        let result = validate_spec_yaml_str(&yaml);
        let errors = result.expect_err("must reject unknown agent effort");
        assert!(
            errors.iter().any(|e| e.contains("/agents/0/effort") && e.contains("unknown effort")),
            "errors should mention unknown agent effort; got {errors:?}"
        );
    }

    #[test]
    fn agent_effort_max_on_codex_rejected() {
        let mut yaml = base_team("");
        yaml = yaml
            .replace("provider: claude", "provider: codex")
            .replace(
                "preferred_for: [dev]",
                "preferred_for: [dev]\n    effort: max",
            );
        let result = validate_spec_yaml_str(&yaml);
        let errors = result.expect_err("must reject max + codex");
        assert!(
            errors.iter().any(|e| e.contains("/agents/0/effort") && e.contains("only supported by claude")),
            "errors should mention claude-only constraint; got {errors:?}"
        );
    }

    #[test]
    fn agent_effort_high_on_claude_does_not_produce_effort_error() {
        // The base_team fixture lacks some unrelated required fields
        // (communication/runtime/context schemas) so full validate_spec may
        // return other errors. We assert NO effort-related error is in the
        // list when effort=high is on a claude agent.
        let mut yaml = base_team("");
        yaml = yaml.replace(
            "preferred_for: [dev]",
            "preferred_for: [dev]\n    effort: high",
        );
        match validate_spec_yaml_str(&yaml) {
            Ok(()) => {}
            Err(errors) => assert!(
                !errors.iter().any(|e| e.contains("/effort:") || e.contains("/provider_effort:")),
                "no effort error should be reported for valid high+claude; got {errors:?}"
            ),
        }
    }

    #[test]
    fn claude_env_unset_includes_claude_effort_in_provider_env_unsets() {
        // Use the public leader_env_unset_for_provider helper which is a
        // pub-facing wrapper around provider_env_unsets — single source of
        // truth for the Claude/ClaudeCode env-unset block.
        use team_agent::leader::leader_env_unset_for_provider;
        use team_agent::provider::Provider;
        let unsets = leader_env_unset_for_provider(Provider::Claude);
        assert!(
            unsets.iter().any(|k| k == "CLAUDE_EFFORT"),
            "CLAUDE_EFFORT must be in Claude provider_env_unsets (single source); got {unsets:?}"
        );
        let cc_unsets = leader_env_unset_for_provider(Provider::ClaudeCode);
        assert!(
            cc_unsets.iter().any(|k| k == "CLAUDE_EFFORT"),
            "CLAUDE_EFFORT must be in ClaudeCode provider_env_unsets; got {cc_unsets:?}"
        );
    }

    #[test]
    fn leader_shell_wrapper_drops_env_keys_present_in_env_unset() {
        // Regression: the shell wrapper must SKIP env exports whose key is
        // in env_unset, otherwise inherited env carrying that key (e.g.
        // CLAUDE_EFFORT from launching shell, preserved by worker_spawn_env
        // whitelist) would be re-introduced after the `unset KEY &&` line.
        use std::collections::BTreeMap;
        use std::path::Path;
        use team_agent::tmux_backend::leader_shell_wrapper_command;
        let mut env = BTreeMap::new();
        env.insert("CLAUDE_EFFORT".to_string(), "high".to_string());
        env.insert("PATH".to_string(), "/usr/bin".to_string());
        let line = leader_shell_wrapper_command(
            &["claude".to_string()],
            Path::new("/tmp"),
            &env,
            &["CLAUDE_EFFORT".to_string()],
            "claude",
        );
        assert!(
            line.contains("unset CLAUDE_EFFORT &&"),
            "wrapper must emit `unset CLAUDE_EFFORT &&`; got {line}"
        );
        assert!(
            !line.contains("CLAUDE_EFFORT=high"),
            "wrapper MUST NOT re-export CLAUDE_EFFORT after unsetting it; got {line}"
        );
        // Other env keys still exported.
        assert!(line.contains("PATH=/usr/bin"), "PATH still exported; got {line}");
    }

    #[test]
    fn team_provider_effort_high_does_not_produce_effort_error() {
        let yaml = base_team("\n  provider_effort: high");
        match validate_spec_yaml_str(&yaml) {
            Ok(()) => {}
            Err(errors) => assert!(
                !errors.iter().any(|e| e.contains("provider_effort:") && e.contains("unknown effort")),
                "no provider_effort error should be reported for valid value; got {errors:?}"
            ),
        }
    }
}
