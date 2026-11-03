//! Ad-hoc roles still enter through the ordinary quick-start/add-agent transactions.
use super::*;
use crate::model::yaml::Value as Yaml;
use std::io::Write;

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::super::tests::Sandbox;
    use super::*;

    fn parse_start(root: &Path, tokens: &[&str]) -> Result<Intent, CliError> {
        let tokens: Vec<String> = tokens.iter().map(|s| s.to_string()).collect();
        let checked = crate::cli::arguments::parse("intent-start", &tokens)?;
        super::super::lifecycle_intent("start", Verb::Start, &checked.tokens, root, false, false)
    }

    fn resolved_start(step: &Step) -> (QuickStartArgs, String, String) {
        let Step::UnifiedStart { request, .. } = step else { panic!("expected unified start") };
        let selected = crate::state::persist::runtime_state_path(&request.workspace).is_file().then(||
            crate::state::selector::resolve_active_team_readonly(&request.workspace, request.team.as_deref(),
                crate::state::selector::SelectorMode::RuntimeOnly).unwrap());
        let declaration = crate::lifecycle::seat_start::resolve(request, selected.as_ref()).unwrap();
        let args = QuickStartArgs { workspace: request.workspace.clone(),
            agents_dir: crate::lifecycle::seat_start::bootstrap_directory(request), name: None,
            team_id: request.team.clone(), yes: false, json: true, detail: true, backend: None };
        (args, request.agent.to_string(), declaration.text)
    }

    #[test]
    #[serial_test::serial(env)]
    fn empty_workspace_admission_is_current_and_planning_is_readonly() {
        let root = Sandbox::new();
        let intent = parse_start(root.path(), &["worker-1", "--dry-run"]).unwrap();
        let plan = runtime::plan(intent).unwrap();
        assert_eq!(plan.scope.team.as_deref(), Some("current"));
        let (args, agent, role) = resolved_start(&plan.steps[0]);
        assert_eq!(agent, "worker-1");
        assert_eq!(args.team_id.as_deref(), Some("current"));
        let (meta, body) = crate::compiler::split_front_matter(&role);
        assert_eq!(
            meta.get("provider").and_then(Yaml::as_str),
            Some(default_provider(None).as_str())
        );
        assert_eq!(
            meta.get("dangerously_skip_permissions"),
            Some(&Yaml::Bool(false))
        );
        runtime::recheck(&plan, &plan.steps[0], false).unwrap();
        assert!(meta.get("model").is_none());
        assert!(body.contains("worker-1"));
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
    }

    #[test]
    #[serial_test::serial(env)]
    fn leader_only_bootstrap_creates_first_worker_without_role_sources() {
        let root = Sandbox::new();
        let state_path = crate::state::persist::runtime_state_path(root.path());
        std::fs::create_dir_all(state_path.parent().unwrap()).unwrap();
        let state = json!({"active_team_key":"current", "team_key":"current", "teams":{"current":{
            "agents":{}, "session_name":null, "tasks":{},
            "team_owner":{"pane_id":"%0", "provider":"claude", "owner_epoch":1},
            "leader_receiver":{"pane_id":"%0", "status":"attached"}
        }}});
        let bytes = serde_json::to_vec(&state).unwrap();
        std::fs::write(&state_path, &bytes).unwrap();
        let intent = parse_start(root.path(), &["worker-1", "--dry-run"]).unwrap();
        let plan = runtime::plan(intent).unwrap();
        let (_, agent, role) = resolved_start(&plan.steps[0]);
        assert_eq!(agent, "worker-1");
        let (meta, _) = crate::compiler::split_front_matter(&role);
        assert_eq!(meta.get("provider").and_then(Yaml::as_str), Some("claude"));
        assert_eq!(plan.scope.team.as_deref(), Some("current"));
        assert_eq!(std::fs::read(&state_path).unwrap(), bytes);
        assert!(!root.path().join(".team/start-source").exists());
    }

    #[test]
    fn trailing_prompt_and_options_customize_the_generated_role() {
        let root = Sandbox::new();
        let intent = parse_start(
            root.path(),
            &[
                "worker-1",
                "--provider",
                "claude",
                "--model",
                "sonnet",
                "--bypass",
                "true",
                "Review",
                "the",
                "changes",
            ],
        )
        .unwrap();
        assert_eq!(intent.patch.prompt.as_deref(), Some("Review the changes"));
        let role = initial_role("worker-1", None, &intent.patch, "pi").unwrap();
        let (meta, body) = crate::compiler::split_front_matter(&role);
        assert_eq!(meta.get("provider").and_then(Yaml::as_str), Some("claude"));
        assert_eq!(meta.get("model").and_then(Yaml::as_str), Some("sonnet"));
        assert_eq!(
            meta.get("dangerously_skip_permissions"),
            Some(&Yaml::Bool(true))
        );
        assert_eq!(body.trim(), "Review the changes");
    }

    #[test]
    fn literal_trailing_options_remain_prompt_text() {
        let root = Sandbox::new();
        let intent = parse_start(
            root.path(),
            &["worker-1", "--", "--provider=bogus", "--help"],
        )
        .unwrap();
        assert_eq!(intent.patch.provider, None);
        assert_eq!(
            intent.patch.prompt.as_deref(),
            Some("--provider=bogus --help")
        );
        assert!(parse_start(root.path(), &["worker-1", "text", "--prompt", "other"]).is_err());
    }

    #[test]
    fn existing_role_policy_and_source_are_preserved() {
        let root = Sandbox::new();
        let path = root.path().join("worker-1.md");
        let text = "---\nname: worker-1\nprovider: claude\nmodel: sonnet\ndangerously_skip_permissions: true\n---\nOriginal task\n";
        std::fs::write(&path, text).unwrap();
        assert_eq!(
            initial_role("worker-1", Some(&path), &RoleConfigPatch::default(), "pi").unwrap(),
            text
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
    }

    #[test]
    fn supplied_role_without_policy_gets_safe_defaults() {
        let root = Sandbox::new();
        let path = root.path().join("worker-1.md");
        std::fs::write(&path, "Review the changes\n").unwrap();
        let role =
            initial_role("worker-1", Some(&path), &RoleConfigPatch::default(), "pi").unwrap();
        let (meta, body) = crate::compiler::split_front_matter(&role);
        assert_eq!(meta.get("provider").and_then(Yaml::as_str), Some("pi"));
        assert_eq!(
            meta.get("dangerously_skip_permissions"),
            Some(&Yaml::Bool(false))
        );
        assert_eq!(body.trim(), "Review the changes");
    }

    #[test]
    #[serial_test::serial(env)]
    fn refused_initial_start_removes_sources_and_allows_changed_retry() {
        let root = Sandbox::new();
        let mut plan = runtime::plan(parse_start(root.path(), &["worker-1"]).unwrap()).unwrap();
        let (args, agent, role) = resolved_start(&plan.steps.remove(0));
        assert!(execute_with(&args, &agent, &role, |_| Err(refusal("preflight failed"))).is_err());
        assert!(!args.agents_dir.exists());
        assert!(!crate::state::persist::runtime_state_path(root.path()).exists());
        let changed = initial_role(&agent, None, &RoleConfigPatch::default(), "claude").unwrap();
        execute_with(&args, &agent, &changed, |_| {
            Ok(CmdResult::from_json(json!({"ok":true}), true))
        })
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(args.agents_dir.join("agents/worker-1.md")).unwrap(),
            changed
        );
    }

    #[test]
    #[serial_test::serial(env)]
    fn failed_provider_admission_never_seeds_a_seat_or_role_source() {
        let root = Sandbox::new();
        let intent = parse_start(
            root.path(),
            &[
                "worker-1",
                "--provider",
                "pi",
                "--model",
                "team-agent-tests/missing-model",
            ],
        )
        .unwrap();
        let mut plan = runtime::plan(intent).unwrap();
        let (args, agent, role) = resolved_start(&plan.steps.remove(0));
        assert!(execute(&args, &agent, &role).is_err());
        assert!(!crate::state::persist::runtime_state_path(root.path()).exists());
        assert!(!args.agents_dir.exists());
    }

    #[test]
    #[serial_test::serial(env)]
    fn partial_start_keeps_sources_adopted_by_runtime() {
        let root = Sandbox::new();
        let mut plan = runtime::plan(parse_start(root.path(), &["worker-1"]).unwrap()).unwrap();
        let (args, agent, role) = resolved_start(&plan.steps.remove(0));
        let result = execute_with(&args, &agent, &role, |_| {
            let state = json!({"active_team_key":"current", "team_key":"current", "teams":{"current":{
                "team_key":"current", "status":"alive", "agents":{"worker-1":{"status":"starting"}}
            }}});
            let state_path = crate::state::persist::runtime_state_path(root.path());
            std::fs::create_dir_all(state_path.parent().unwrap()).unwrap();
            std::fs::write(state_path, serde_json::to_vec(&state).unwrap()).unwrap();
            Ok(CmdResult::from_json(json!({"ok":false,"status":"partial"}), true))
        }).unwrap();
        assert_eq!(result.exit, ExitCode::Error);
        assert!(source_adopted(&args, &agent));
        assert_eq!(
            std::fs::read_to_string(args.agents_dir.join("agents/worker-1.md")).unwrap(),
            role
        );
    }

    #[test]
    fn managed_sources_never_overwrite_changed_documents() {
        let root = Sandbox::new();
        let path = root.path().join("role.md");
        create_source(&path, "original").unwrap();
        create_source(&path, "original").unwrap();
        assert!(create_source(&path, "changed").is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "original");
    }
}
