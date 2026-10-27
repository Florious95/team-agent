//! 0.4.x provider effort MVP — focused unit/integration tests covering the
//! 10-step plan in .team/artifacts/provider-effort-mvp-plan.md.

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
            errors
                .iter()
                .any(|e| e.contains("provider_effort") && e.contains("unknown effort")),
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
            errors
                .iter()
                .any(|e| e.contains("/agents/0/effort") && e.contains("unknown effort")),
            "errors should mention unknown agent effort; got {errors:?}"
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
                !errors
                    .iter()
                    .any(|e| e.contains("/effort:") || e.contains("/provider_effort:")),
                "no effort error should be reported for valid high+claude; got {errors:?}"
            ),
        }
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
        assert!(
            line.contains("PATH=/usr/bin"),
            "PATH still exported; got {line}"
        );
    }

    #[test]
    fn team_provider_effort_high_does_not_produce_effort_error() {
        let yaml = base_team("\n  provider_effort: high");
        match validate_spec_yaml_str(&yaml) {
            Ok(()) => {}
            Err(errors) => assert!(
                !errors
                    .iter()
                    .any(|e| e.contains("provider_effort:") && e.contains("unknown effort")),
                "no provider_effort error should be reported for valid value; got {errors:?}"
            ),
        }
    }
}
