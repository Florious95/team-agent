//! Ad-hoc roles still enter through the ordinary quick-start/add-agent transactions.
use super::*;
use crate::model::yaml::Value as Yaml;
use std::io::Write;

pub(super) fn creation_patch(
    id: &str,
    source: Option<&Path>,
    patch: &RoleConfigPatch,
) -> Result<RoleConfigPatch, CliError> {
    let meta = source
        .map(crate::compiler::read_front_matter)
        .transpose()
        .map_err(|e| refusal(e.to_string()))?
        .map(|(meta, _)| meta);
    let mut patch = patch.clone();
    if patch.provider.is_none() && meta.as_ref().and_then(|m| m.get("provider")).is_none() {
        patch.provider = Some("pi".into());
    }
    if patch.bypass.is_none()
        && meta
            .as_ref()
            .and_then(|m| m.get("dangerously_skip_permissions"))
            .is_none()
    {
        patch.bypass = Some(false);
    }
    if source.is_none() && patch.prompt.is_none() {
        patch.prompt = Some(format!(
            "You are {id}. Complete the tasks assigned by the leader.\n"
        ));
    }
    Ok(patch)
}

pub(super) fn initial_role(
    id: &str,
    source: Option<&Path>,
    patch: &RoleConfigPatch,
) -> Result<String, CliError> {
    let patch = creation_patch(id, source, patch)?;
    let text = source
        .map(std::fs::read_to_string)
        .transpose()?
        .unwrap_or_default();
    crate::lifecycle::role_config::patched_role(&text, &AgentId::new(id), &patch, source.is_none())
        .map_err(|e| refusal(e.to_string()))
}

pub(super) fn execute(
    args: &QuickStartArgs,
    agent: &str,
    role: &str,
) -> Result<CmdResult, CliError> {
    // Sources are durable recovery inputs, not a second runtime registry. Never
    // overwrite a user's document or a concurrent start's differently configured role.
    let team = args.team_id.as_deref().unwrap_or("current");
    let metadata = Yaml::Map(vec![("name".into(), Yaml::Str(team.into()))]);
    let team_text = format!("---\n{}---\n", crate::model::yaml::dumps(&metadata));
    std::fs::create_dir_all(args.agents_dir.join("agents"))?;
    create_source(&args.agents_dir.join("TEAM.md"), &team_text)?;
    create_source(
        &args.agents_dir.join("agents").join(format!("{agent}.md")),
        role,
    )?;
    cmd_quick_start(args)
}

fn create_source(path: &Path, text: &str) -> Result<(), CliError> {
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
    {
        Ok(mut file) => {
            file.write_all(text.as_bytes())?;
            file.sync_all()?;
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            if std::fs::symlink_metadata(path)?.is_file() && std::fs::read(path)? == text.as_bytes()
            {
                Ok(())
            } else {
                Err(refusal(format!(
                    "ad-hoc source changed: {}",
                    path.display()
                )))
            }
        }
        Err(error) => Err(error.into()),
    }
}

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

    #[test]
    #[serial_test::serial(env)]
    fn empty_workspace_admission_is_current_and_planning_is_readonly() {
        let root = Sandbox::new();
        let intent = parse_start(root.path(), &["worker-1", "--dry-run"]).unwrap();
        let plan = runtime::plan(intent).unwrap();
        assert_eq!(plan.scope.team.as_deref(), Some("current"));
        let Step::CreateAgent { args, agent, role } = &plan.steps[0] else {
            panic!()
        };
        assert_eq!(agent, "worker-1");
        assert_eq!(args.team_id.as_deref(), Some("current"));
        let (meta, body) = crate::compiler::split_front_matter(role);
        assert_eq!(meta.get("provider").and_then(Yaml::as_str), Some("pi"));
        assert_eq!(
            meta.get("dangerously_skip_permissions"),
            Some(&Yaml::Bool(false))
        );
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
            "team_owner":{"pane_id":"%0", "provider":"pi", "owner_epoch":1},
            "leader_receiver":{"pane_id":"%0", "status":"attached"}
        }}});
        let bytes = serde_json::to_vec(&state).unwrap();
        std::fs::write(&state_path, &bytes).unwrap();
        let intent = parse_start(root.path(), &["worker-1", "--dry-run"]).unwrap();
        let plan = runtime::plan(intent).unwrap();
        assert!(matches!(&plan.steps[0], Step::CreateAgent { agent, .. } if agent == "worker-1"));
        assert_eq!(plan.scope.team.as_deref(), Some("current"));
        assert_eq!(std::fs::read(&state_path).unwrap(), bytes);
        assert!(!root.path().join(".team/adhoc-worker-1").exists());
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
        let role = initial_role("worker-1", None, &intent.patch).unwrap();
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
            initial_role("worker-1", Some(&path), &RoleConfigPatch::default()).unwrap(),
            text
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
    }

    #[test]
    fn supplied_role_without_policy_gets_safe_defaults() {
        let root = Sandbox::new();
        let path = root.path().join("worker-1.md");
        std::fs::write(&path, "Review the changes\n").unwrap();
        let role = initial_role("worker-1", Some(&path), &RoleConfigPatch::default()).unwrap();
        let (meta, body) = crate::compiler::split_front_matter(&role);
        assert_eq!(meta.get("provider").and_then(Yaml::as_str), Some("pi"));
        assert_eq!(
            meta.get("dangerously_skip_permissions"),
            Some(&Yaml::Bool(false))
        );
        assert_eq!(body.trim(), "Review the changes");
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
